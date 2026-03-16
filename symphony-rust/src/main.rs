#![allow(clippy::type_complexity)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::anyhow;
use clap::Parser;
use tokio::signal;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;
use tracing_subscriber::{fmt, EnvFilter};

use symphony::config::{ObservabilityConfig, SymphonyConfig};
use symphony::dashboard::spawn_dashboard;
use symphony::error::SymphonyError;
use symphony::http::{HttpServer, StateProvider};
use symphony::logging::build_file_appender;
use symphony::orchestrator::{Orchestrator, OrchestratorMsg};
use symphony::prompt::PromptBuilder;
use symphony::tracker::build_tracker;
use symphony::workflow::{load_workflow, watch_workflow};
use symphony::workspace::{default_workspace_root, WorkspaceManager};

const GUARDRAILS_ACKNOWLEDGEMENT_FLAG: &str =
    "i-understand-that-this-will-be-running-without-the-usual-guardrails";
const ANSI_BRIGHT_RED: &str = "\x1b[1;31m";
const ANSI_RESET: &str = "\x1b[0m";

#[derive(Parser, Debug)]
#[command(
    name = "symphony",
    about = "Orchestrate coding agents for project work"
)]
struct Cli {
    #[arg(default_value = "./WORKFLOW.md")]
    workflow_path: String,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long = GUARDRAILS_ACKNOWLEDGEMENT_FLAG)]
    i_understand_that_this_will_be_running_without_the_usual_guardrails:
        bool,
    #[arg(long)]
    logs_root: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<(), SymphonyError> {
    let cli = Cli::parse();
    if !cli.i_understand_that_this_will_be_running_without_the_usual_guardrails {
        eprintln!("{}", acknowledgement_banner());
        return Err(SymphonyError::ConfigValidation(
            "missing required guardrails acknowledgement flag".into(),
        ));
    }
    let logs_root = validate_logs_root(cli.logs_root.as_deref())?;

    let workflow_definition = load_workflow(&cli.workflow_path)?;
    let config = SymphonyConfig::from_yaml_value(&workflow_definition.config)?;
    config.validate()?;
    init_tracing(&config.observability, logs_root)?;

    let tracker = build_tracker(&config.tracker)?;
    let workspace_root = default_workspace_root(config.workspace.root.as_deref());
    let workspace_manager = Arc::new(WorkspaceManager::new(workspace_root, config.hooks.clone())?);
    let prompt_builder = Arc::new(PromptBuilder);
    let state_provider = Arc::new(StateProvider::new());

    let mut orchestrator = Orchestrator::new(
        config.clone(),
        workflow_definition.prompt_template,
        workspace_manager,
        prompt_builder,
        tracker,
    );
    orchestrator.set_state_provider(Arc::clone(&state_provider));
    let mut dashboard_handle = config.observability.dashboard_enabled.then(|| {
        spawn_dashboard(
            Arc::clone(&state_provider),
            state_provider.subscribe(),
            config.clone(),
        )
    });

    let orchestrator_tx = orchestrator.sender();
    let (reload_tx, mut reload_rx) = mpsc::channel(16);
    let workflow_watcher = watch_workflow(PathBuf::from(&cli.workflow_path), reload_tx)?;
    let reload_msg_tx = orchestrator_tx.clone();
    let workflow_forwarder = tokio::spawn(async move {
        while let Some(reload) = reload_rx.recv().await {
            if reload_msg_tx
                .send(OrchestratorMsg::WorkflowReload(reload.definition))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let (server_handle, server_shutdown_tx) = start_http_server(
        config
            .server
            .host
            .clone()
            .unwrap_or_else(|| "127.0.0.1".into()),
        cli.port.or(config.server.port),
        state_provider,
        orchestrator_tx.clone(),
    );
    let orchestrator_handle = tokio::spawn(async move {
        orchestrator.run().await;
    });

    let result = run_host(
        orchestrator_tx,
        orchestrator_handle,
        server_handle,
        server_shutdown_tx,
    )
    .await;
    abort_dashboard(&mut dashboard_handle);

    workflow_forwarder.abort();
    drop(workflow_watcher);

    result
}

fn validate_logs_root(logs_root: Option<&str>) -> Result<Option<&str>, SymphonyError> {
    match logs_root {
        Some(value) if value.trim().is_empty() => Err(SymphonyError::ConfigValidation(
            "--logs-root must not be empty".into(),
        )),
        Some(value) => Ok(Some(value)),
        None => Ok(None),
    }
}

fn init_tracing(
    observability: &ObservabilityConfig,
    logs_root: Option<&str>,
) -> Result<(), SymphonyError> {
    let file_filter = build_env_filter()?;
    let console_filter = if observability.dashboard_enabled {
        EnvFilter::builder()
            .with_default_directive(LevelFilter::WARN.into())
            .from_env_lossy()
    } else {
        build_env_filter()?
    };
    let file_appender = build_file_appender(observability, logs_root)?;
    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(move || file_appender.clone())
        .with_filter(file_filter);
    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(console_filter);

    tracing_subscriber::registry()
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))
}

fn build_env_filter() -> Result<EnvFilter, SymphonyError> {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))
}

fn acknowledgement_banner() -> String {
    let lines = [
        "Symphony is an engineering preview.",
        "Codex will run without the usual guardrails.",
        "It may automatically run AI agents, modify code, and interact with external services.",
        "Symphony is not a supported product.",
        "To proceed, re-run with:",
        "  --i-understand-that-this-will-be-running-without-the-usual-guardrails",
    ];
    let width = lines.iter().map(|line| line.chars().count()).max().unwrap_or(0);
    let border = format!("┌{}┐", "─".repeat(width + 2));
    let body = lines
        .iter()
        .map(|line| {
            let padding = " ".repeat(width.saturating_sub(line.chars().count()));
            format!("│ {line}{padding} │")
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "{ANSI_BRIGHT_RED}{border}\n{body}\n└{}┘{ANSI_RESET}",
        "─".repeat(width + 2)
    )
}

fn start_http_server(
    host: String,
    port: Option<u16>,
    state_provider: Arc<StateProvider>,
    msg_tx: mpsc::Sender<OrchestratorMsg>,
) -> (
    Option<JoinHandle<Result<(), SymphonyError>>>,
    Option<oneshot::Sender<()>>,
) {
    match port {
        Some(port) => {
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let handle = tokio::spawn(async move {
                HttpServer::new(host, port)
                    .start_with_shutdown(state_provider, msg_tx, async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
            });
            (Some(handle), Some(shutdown_tx))
        }
        None => (None, None),
    }
}

async fn run_host(
    orchestrator_tx: mpsc::Sender<OrchestratorMsg>,
    orchestrator_handle: JoinHandle<()>,
    server_handle: Option<JoinHandle<Result<(), SymphonyError>>>,
    server_shutdown_tx: Option<oneshot::Sender<()>>,
) -> Result<(), SymphonyError> {
    let mut orchestrator_handle = orchestrator_handle;
    let mut server_handle = server_handle;
    let mut server_shutdown_tx = server_shutdown_tx;

    if server_handle.is_some() {
        tokio::select! {
            () = shutdown_signal() => {}
            result = &mut orchestrator_handle => {
                request_http_shutdown(&mut server_shutdown_tx);
                if let Some(handle) = server_handle.take() {
                    await_http_server(handle).await?;
                }
                return unexpected_orchestrator_exit(result);
            }
            result = async {
                match server_handle.as_mut() {
                    Some(handle) => Some(handle.await),
                    None => None,
                }
            } => {
                request_orchestrator_shutdown(&orchestrator_tx).await;
                await_orchestrator(orchestrator_handle).await?;
                return match result {
                    Some(result) => unexpected_http_server_exit(result),
                    None => Err(SymphonyError::Internal(anyhow!(
                        "http server task missing"
                    ))),
                };
            }
        }
    } else {
        tokio::select! {
            () = shutdown_signal() => {}
            result = &mut orchestrator_handle => {
                return unexpected_orchestrator_exit(result);
            }
        }
    }

    request_orchestrator_shutdown(&orchestrator_tx).await;
    request_http_shutdown(&mut server_shutdown_tx);
    await_orchestrator(orchestrator_handle).await?;

    if let Some(handle) = server_handle.take() {
        await_http_server(handle).await?;
    }

    Ok(())
}

async fn request_orchestrator_shutdown(msg_tx: &mpsc::Sender<OrchestratorMsg>) {
    if msg_tx.send(OrchestratorMsg::Shutdown).await.is_err() {
        warn!("failed to send orchestrator shutdown");
    }
}

fn request_http_shutdown(shutdown_tx: &mut Option<oneshot::Sender<()>>) {
    if let Some(tx) = shutdown_tx.take() {
        let _ = tx.send(());
    }
}

fn abort_dashboard(handle: &mut Option<JoinHandle<()>>) {
    if let Some(handle) = handle.take() {
        handle.abort();
    }
}

async fn await_orchestrator(handle: JoinHandle<()>) -> Result<(), SymphonyError> {
    handle
        .await
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))
}

async fn await_http_server(
    handle: JoinHandle<Result<(), SymphonyError>>,
) -> Result<(), SymphonyError> {
    handle
        .await
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))?
}

fn unexpected_orchestrator_exit(
    result: Result<(), tokio::task::JoinError>,
) -> Result<(), SymphonyError> {
    result.map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))?;

    Err(SymphonyError::Internal(anyhow!(
        "orchestrator exited before shutdown signal"
    )))
}

fn unexpected_http_server_exit(
    result: Result<Result<(), SymphonyError>, tokio::task::JoinError>,
) -> Result<(), SymphonyError> {
    match result.map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))? {
        Ok(()) => Err(SymphonyError::Internal(anyhow!(
            "http server exited before shutdown signal"
        ))),
        Err(error) => Err(error),
    }
}

async fn shutdown_signal() {
    let ctrl_c = signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        tokio::select! {
            result = ctrl_c => {
                match result {
                    Ok(()) => tracing::info!("received SIGINT"),
                    Err(error) => warn!(error = %error, "failed to listen for SIGINT"),
                }
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM");
            }
        }
    }

    #[cfg(not(unix))]
    {
        match ctrl_c.await {
            Ok(()) => tracing::info!("received SIGINT"),
            Err(error) => warn!(error = %error, "failed to listen for SIGINT"),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use symphony::error::SymphonyError;

    use super::{acknowledgement_banner, validate_logs_root, Cli};

    #[test]
    // SPEC 17.7: CLI defaults the positional workflow path to `./WORKFLOW.md`.
    fn cli_parses_default_workflow_path() {
        let cli = Cli::parse_from(["symphony"]);

        assert_eq!(cli.workflow_path, "./WORKFLOW.md");
        assert_eq!(cli.port, None);
        assert!(!cli.i_understand_that_this_will_be_running_without_the_usual_guardrails);
        assert_eq!(cli.logs_root, None);
    }

    #[test]
    fn cli_parses_guardrails_flag_with_other_arguments() {
        let cli = Cli::parse_from([
            "symphony",
            "--i-understand-that-this-will-be-running-without-the-usual-guardrails",
            "./custom.md",
            "--port",
            "8080",
        ]);

        assert_eq!(cli.workflow_path, "./custom.md");
        assert_eq!(cli.port, Some(8080));
        assert!(cli.i_understand_that_this_will_be_running_without_the_usual_guardrails);
        assert_eq!(cli.logs_root, None);
    }

    #[test]
    fn cli_parses_logs_root_override() {
        let cli = Cli::parse_from([
            "symphony",
            "./custom.md",
            "--logs-root",
            "/tmp/symphony-logs",
        ]);

        assert_eq!(cli.workflow_path, "./custom.md");
        assert_eq!(cli.logs_root.as_deref(), Some("/tmp/symphony-logs"));
    }

    #[test]
    fn validate_logs_root_rejects_empty_value() {
        let cli = Cli::parse_from(["symphony", "./custom.md", "--logs-root", ""]);

        let error = validate_logs_root(cli.logs_root.as_deref()).unwrap_err();

        assert!(matches!(
            error,
            SymphonyError::ConfigValidation(message)
                if message == "--logs-root must not be empty"
        ));
    }

    #[test]
    fn acknowledgement_banner_is_not_empty() {
        assert!(!acknowledgement_banner().is_empty());
    }
}
