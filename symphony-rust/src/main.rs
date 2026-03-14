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
use tracing_subscriber::EnvFilter;

use symphony::config::SymphonyConfig;
use symphony::error::SymphonyError;
use symphony::http::{HttpServer, StateProvider};
use symphony::linear::client::LinearClient;
use symphony::orchestrator::{Orchestrator, OrchestratorMsg};
use symphony::prompt::PromptBuilder;
use symphony::workflow::{load_workflow, watch_workflow};
use symphony::workspace::{default_workspace_root, WorkspaceManager};

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
    init_tracing()?;

    let workflow_definition = load_workflow(&cli.workflow_path)?;
    let config = SymphonyConfig::from_yaml_value(&workflow_definition.config)?;
    config.validate()?;

    let tracker_client = Arc::new(LinearClient::from_config(&config.tracker)?);
    let workspace_root = default_workspace_root(config.workspace.root.as_deref());
    let workspace_manager = Arc::new(WorkspaceManager::new(workspace_root, config.hooks.clone())?);
    let prompt_builder = Arc::new(PromptBuilder);
    let state_provider = Arc::new(StateProvider::new());

    let mut orchestrator = Orchestrator::new(
        config.clone(),
        workflow_definition.prompt_template,
        workspace_manager,
        prompt_builder,
        tracker_client,
    );
    orchestrator.set_state_provider(Arc::clone(&state_provider));

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

    workflow_forwarder.abort();
    drop(workflow_watcher);

    result
}

fn init_tracing() -> Result<(), SymphonyError> {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))?;

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init()
        .map_err(|error| SymphonyError::Internal(anyhow!(error.to_string())))
}

fn start_http_server(
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
                HttpServer::new(port)
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

    use super::Cli;

    #[test]
    // SPEC 17.7: CLI defaults the positional workflow path to `./WORKFLOW.md`.
    fn cli_parses_default_workflow_path() {
        let cli = Cli::parse_from(["symphony"]);

        assert_eq!(cli.workflow_path, "./WORKFLOW.md");
        assert_eq!(cli.port, None);
    }

    #[test]
    // SPEC 17.7: CLI accepts an explicit workflow path and optional port flag.
    fn cli_parses_explicit_path_and_port() {
        let cli = Cli::parse_from(["symphony", "./custom.md", "--port", "8080"]);

        assert_eq!(cli.workflow_path, "./custom.md");
        assert_eq!(cli.port, Some(8080));
    }
}
