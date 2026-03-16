use std::collections::HashMap;
use std::env;

use serde::de::Deserializer;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;

use crate::error::SymphonyError;

const DEFAULT_POLLING_INTERVAL_MS: u64 = 30_000;
const DEFAULT_STALL_TIMEOUT_MS: i64 = 600_000;
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_CONCURRENT_AGENTS: u32 = 10;
const DEFAULT_MAX_TURNS: u32 = 20;
const DEFAULT_MAX_RETRY_BACKOFF_MS: u64 = 300_000;
const DEFAULT_CODEX_COMMAND: &str = "codex app-server";
const DEFAULT_TURN_TIMEOUT_MS: u64 = 3_600_000;
const DEFAULT_READ_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_CODEX_STALL_TIMEOUT_MS: i64 = 300_000;
const DEFAULT_DASHBOARD_ENABLED: bool = true;
const DEFAULT_REFRESH_MS: u64 = 1_000;
const DEFAULT_RENDER_INTERVAL_MS: u64 = 16;
const DEFAULT_LOG_MAX_BYTES: u64 = 10_485_760;
const DEFAULT_LOG_MAX_FILES: u32 = 5;
const DEFAULT_SERVER_HOST: &str = "127.0.0.1";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub kind: Option<String>,
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub project_slug: Option<String>,
    pub assignee: Option<String>,
    #[serde(default = "default_active_states")]
    pub active_states: Vec<String>,
    #[serde(default = "default_terminal_states")]
    pub terminal_states: Vec<String>,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            kind: None,
            api_key: None,
            endpoint: None,
            project_slug: None,
            assignee: None,
            active_states: default_active_states(),
            terminal_states: default_terminal_states(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PollingConfig {
    #[serde(
        default = "default_polling_interval_ms",
        deserialize_with = "deserialize_polling_interval_ms"
    )]
    pub interval_ms: u64,
    #[serde(
        default = "default_stall_timeout_ms",
        deserialize_with = "deserialize_stall_timeout_ms"
    )]
    pub stall_timeout_ms: i64,
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            interval_ms: default_polling_interval_ms(),
            stall_timeout_ms: default_stall_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkerConfig {
    pub ssh_hosts: Vec<String>,
    pub max_concurrent_agents_per_host: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    #[serde(
        default = "default_hook_timeout_ms",
        deserialize_with = "deserialize_hook_timeout_ms"
    )]
    pub timeout_ms: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: default_hook_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    #[serde(
        default = "default_max_concurrent_agents",
        deserialize_with = "deserialize_max_concurrent_agents"
    )]
    pub max_concurrent_agents: u32,
    #[serde(default, deserialize_with = "deserialize_state_limit_map")]
    pub max_concurrent_agents_by_state: HashMap<String, u32>,
    #[serde(
        default = "default_max_turns",
        deserialize_with = "deserialize_max_turns"
    )]
    pub max_turns: u32,
    #[serde(
        default = "default_max_retry_backoff_ms",
        deserialize_with = "deserialize_max_retry_backoff_ms"
    )]
    pub max_retry_backoff_ms: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: default_max_concurrent_agents(),
            max_concurrent_agents_by_state: HashMap::new(),
            max_turns: default_max_turns(),
            max_retry_backoff_ms: default_max_retry_backoff_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CodexConfig {
    #[serde(default = "default_codex_command")]
    pub command: Option<String>,
    #[serde(
        default = "default_turn_timeout_ms",
        deserialize_with = "deserialize_turn_timeout_ms"
    )]
    pub turn_timeout_ms: Option<u64>,
    #[serde(
        default = "default_read_timeout_ms",
        deserialize_with = "deserialize_read_timeout_ms"
    )]
    pub read_timeout_ms: Option<u64>,
    #[serde(
        default = "default_codex_stall_timeout_ms",
        deserialize_with = "deserialize_codex_stall_timeout_ms"
    )]
    pub stall_timeout_ms: Option<i64>,
    pub approval_policy: Option<JsonValue>,
    pub sandbox: Option<String>,
    pub thread_sandbox: Option<String>,
    #[serde(default, deserialize_with = "deserialize_json_value_option")]
    pub turn_sandbox_policy: Option<JsonValue>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: default_codex_command(),
            turn_timeout_ms: default_turn_timeout_ms(),
            read_timeout_ms: default_read_timeout_ms(),
            stall_timeout_ms: default_codex_stall_timeout_ms(),
            approval_policy: None,
            sandbox: None,
            thread_sandbox: None,
            turn_sandbox_policy: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    #[serde(default = "default_server_host")]
    pub host: Option<String>,
    pub port: Option<u16>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_server_host(),
            port: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ObservabilityConfig {
    #[serde(default = "default_dashboard_enabled")]
    pub dashboard_enabled: bool,
    #[serde(
        default = "default_refresh_ms",
        deserialize_with = "deserialize_refresh_ms"
    )]
    pub refresh_ms: u64,
    #[serde(
        default = "default_render_interval_ms",
        deserialize_with = "deserialize_render_interval_ms"
    )]
    pub render_interval_ms: u64,
    #[serde(
        default = "default_log_max_bytes",
        deserialize_with = "deserialize_log_max_bytes"
    )]
    pub log_max_bytes: u64,
    #[serde(
        default = "default_log_max_files",
        deserialize_with = "deserialize_log_max_files"
    )]
    pub log_max_files: u32,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            dashboard_enabled: default_dashboard_enabled(),
            refresh_ms: default_refresh_ms(),
            render_interval_ms: default_render_interval_ms(),
            log_max_bytes: default_log_max_bytes(),
            log_max_files: default_log_max_files(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SymphonyConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub worker: WorkerConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub codex: CodexConfig,
    pub server: ServerConfig,
    #[serde(default)]
    pub observability: ObservabilityConfig,
}

impl SymphonyConfig {
    pub fn from_yaml_value(value: &YamlValue) -> Result<Self, SymphonyError> {
        let polling_stall_timeout_set = yaml_path_exists(value, &["polling", "stall_timeout_ms"]);
        let codex_stall_timeout_set = yaml_path_exists(value, &["codex", "stall_timeout_ms"]);
        let mut config = serde_yaml::from_value::<Self>(value.clone())
            .map_err(|error| SymphonyError::ConfigParse(error.to_string()))?;

        config.resolve_runtime_values(polling_stall_timeout_set, codex_stall_timeout_set);

        Ok(config)
    }

    pub fn validate(&self) -> Result<(), SymphonyError> {
        let tracker_kind = self
            .tracker
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .ok_or_else(|| SymphonyError::ConfigValidation("tracker.kind is required".into()))?;

        if !matches!(tracker_kind, "linear" | "memory") {
            return Err(SymphonyError::ConfigValidation(
                "tracker.kind must equal \"linear\" or \"memory\"".into(),
            ));
        }

        if tracker_kind == "linear" {
            let api_key = self
                .tracker
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    SymphonyError::ConfigValidation("tracker.api_key is required".into())
                })?;

            if api_key.is_empty() {
                return Err(SymphonyError::ConfigValidation(
                    "tracker.api_key is required".into(),
                ));
            }

            self.tracker
                .project_slug
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    SymphonyError::ConfigValidation("tracker.project_slug is required".into())
                })?;
        }

        self.codex
            .command
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SymphonyError::ConfigValidation("codex.command is required".into()))?;

        Ok(())
    }

    fn resolve_runtime_values(
        &mut self,
        polling_stall_timeout_set: bool,
        codex_stall_timeout_set: bool,
    ) {
        self.tracker.api_key = self
            .tracker
            .api_key
            .as_deref()
            .and_then(resolve_env_var)
            .or_else(|| {
                if self.tracker.kind.as_deref() == Some("linear") {
                    std::env::var("LINEAR_API_KEY")
                        .ok()
                        .filter(|v| !v.is_empty())
                } else {
                    None
                }
            });

        self.tracker.assignee = self
            .tracker
            .assignee
            .as_deref()
            .and_then(resolve_env_var)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                if self.tracker.kind.as_deref() == Some("linear") {
                    std::env::var("LINEAR_ASSIGNEE")
                        .ok()
                        .filter(|value| !value.trim().is_empty())
                } else {
                    None
                }
            });

        self.workspace.root = self
            .workspace
            .root
            .as_deref()
            .and_then(resolve_env_var)
            .map(|value| expand_tilde(&value));

        self.agent.max_concurrent_agents_by_state =
            normalize_state_limit_map(self.agent.max_concurrent_agents_by_state.clone());

        if polling_stall_timeout_set && !codex_stall_timeout_set {
            self.codex.stall_timeout_ms = Some(self.polling.stall_timeout_ms);
        }
    }
}

pub fn resolve_env_var(value: &str) -> Option<String> {
    if let Some(name) = value.strip_prefix('$') {
        return env::var(name).ok().filter(|resolved| !resolved.is_empty());
    }

    Some(value.to_owned())
}

pub fn expand_tilde(path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_owned();
    }

    let Some(home_dir) = home_dir() else {
        return path.to_owned();
    };

    if path == "~" {
        return home_dir;
    }

    format!("{home_dir}{}", &path[1..])
}

fn normalize_state_limit_map(input: HashMap<String, u32>) -> HashMap<String, u32> {
    input
        .into_iter()
        .filter(|(_, value)| *value > 0)
        .map(|(key, value)| (key.to_lowercase(), value))
        .collect()
}

fn default_active_states() -> Vec<String> {
    vec!["Todo".into(), "In Progress".into()]
}

fn default_terminal_states() -> Vec<String> {
    vec!["Done".into(), "Canceled".into()]
}

fn default_polling_interval_ms() -> u64 {
    DEFAULT_POLLING_INTERVAL_MS
}

fn default_stall_timeout_ms() -> i64 {
    DEFAULT_STALL_TIMEOUT_MS
}

fn default_hook_timeout_ms() -> u64 {
    DEFAULT_HOOK_TIMEOUT_MS
}

fn default_max_concurrent_agents() -> u32 {
    DEFAULT_MAX_CONCURRENT_AGENTS
}

fn default_max_turns() -> u32 {
    DEFAULT_MAX_TURNS
}

fn default_max_retry_backoff_ms() -> u64 {
    DEFAULT_MAX_RETRY_BACKOFF_MS
}

fn default_codex_command() -> Option<String> {
    Some(DEFAULT_CODEX_COMMAND.into())
}

fn default_turn_timeout_ms() -> Option<u64> {
    Some(DEFAULT_TURN_TIMEOUT_MS)
}

fn default_read_timeout_ms() -> Option<u64> {
    Some(DEFAULT_READ_TIMEOUT_MS)
}

fn default_codex_stall_timeout_ms() -> Option<i64> {
    Some(DEFAULT_CODEX_STALL_TIMEOUT_MS)
}

fn default_dashboard_enabled() -> bool {
    DEFAULT_DASHBOARD_ENABLED
}

fn default_refresh_ms() -> u64 {
    DEFAULT_REFRESH_MS
}

fn default_render_interval_ms() -> u64 {
    DEFAULT_RENDER_INTERVAL_MS
}

fn default_log_max_bytes() -> u64 {
    DEFAULT_LOG_MAX_BYTES
}

fn default_log_max_files() -> u32 {
    DEFAULT_LOG_MAX_FILES
}

fn default_server_host() -> Option<String> {
    Some(DEFAULT_SERVER_HOST.into())
}

fn home_dir() -> Option<String> {
    env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.is_empty())
        })
}

fn deserialize_polling_interval_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_u64_or_default(
        deserializer,
        default_polling_interval_ms(),
    ))
}

fn deserialize_stall_timeout_ms<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_i64_or_default(
        deserializer,
        default_stall_timeout_ms(),
    ))
}

fn deserialize_hook_timeout_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = parse_i64_or_default(
        deserializer,
        i64::try_from(default_hook_timeout_ms()).unwrap_or(60_000),
    );
    Ok(if value > 0 {
        u64::try_from(value).unwrap_or(default_hook_timeout_ms())
    } else {
        default_hook_timeout_ms()
    })
}

fn deserialize_max_concurrent_agents<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_u32_or_default(
        deserializer,
        default_max_concurrent_agents(),
    ))
}

fn deserialize_max_turns<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_u32_or_default(deserializer, default_max_turns()))
}

fn deserialize_max_retry_backoff_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_u64_or_default(
        deserializer,
        default_max_retry_backoff_ms(),
    ))
}

fn deserialize_turn_timeout_ms<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(parse_u64_or_default(
        deserializer,
        default_turn_timeout_ms().unwrap_or(DEFAULT_TURN_TIMEOUT_MS),
    )))
}

fn deserialize_read_timeout_ms<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(parse_u64_or_default(
        deserializer,
        default_read_timeout_ms().unwrap_or(DEFAULT_READ_TIMEOUT_MS),
    )))
}

fn deserialize_codex_stall_timeout_ms<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(parse_i64_or_default(
        deserializer,
        default_codex_stall_timeout_ms().unwrap_or(DEFAULT_CODEX_STALL_TIMEOUT_MS),
    )))
}

fn deserialize_refresh_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_positive_u64_or_default(
        deserializer,
        default_refresh_ms(),
    ))
}

fn deserialize_render_interval_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_positive_u64_or_default(
        deserializer,
        default_render_interval_ms(),
    ))
}

fn deserialize_log_max_bytes<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_positive_u64_or_default(
        deserializer,
        default_log_max_bytes(),
    ))
}

fn deserialize_log_max_files<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(parse_u32_or_default(deserializer, default_log_max_files()))
}

fn deserialize_state_limit_map<'de, D>(deserializer: D) -> Result<HashMap<String, u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<HashMap<String, YamlValue>>::deserialize(deserializer)?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| {
            parse_yaml_u32(&value)
                .filter(|limit| *limit > 0)
                .map(|limit| (key.to_lowercase(), limit))
        })
        .collect();

    Ok(values)
}

fn deserialize_json_value_option<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<YamlValue>::deserialize(deserializer)?
        .as_ref()
        .map(yaml_to_json_value))
}

fn parse_u64_or_default<'de, D>(deserializer: D, default: u64) -> u64
where
    D: Deserializer<'de>,
{
    Option::<IntegerLike>::deserialize(deserializer)
        .ok()
        .flatten()
        .and_then(|value| value.to_i64())
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(default)
}

fn parse_positive_u64_or_default<'de, D>(deserializer: D, default: u64) -> u64
where
    D: Deserializer<'de>,
{
    Option::<IntegerLike>::deserialize(deserializer)
        .ok()
        .flatten()
        .and_then(|value| value.to_i64())
        .filter(|value| *value > 0)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(default)
}

fn parse_i64_or_default<'de, D>(deserializer: D, default: i64) -> i64
where
    D: Deserializer<'de>,
{
    Option::<IntegerLike>::deserialize(deserializer)
        .ok()
        .flatten()
        .and_then(|value| value.to_i64())
        .unwrap_or(default)
}

fn parse_u32_or_default<'de, D>(deserializer: D, default: u32) -> u32
where
    D: Deserializer<'de>,
{
    Option::<IntegerLike>::deserialize(deserializer)
        .ok()
        .flatten()
        .and_then(|value| value.to_i64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_yaml_u32(value: &YamlValue) -> Option<u32> {
    match value {
        YamlValue::Number(number) => number
            .as_i64()
            .and_then(|parsed| u32::try_from(parsed).ok()),
        YamlValue::String(string) => string.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn yaml_to_json_value(value: &YamlValue) -> JsonValue {
    match value {
        YamlValue::Null => JsonValue::Null,
        YamlValue::Bool(boolean) => JsonValue::Bool(*boolean),
        YamlValue::Number(number) => {
            if let Some(integer) = number.as_i64() {
                JsonValue::from(integer)
            } else if let Some(unsigned) = number.as_u64() {
                JsonValue::from(unsigned)
            } else if let Some(float) = number.as_f64() {
                JsonValue::from(float)
            } else {
                JsonValue::Null
            }
        }
        YamlValue::String(string) => JsonValue::String(string.clone()),
        YamlValue::Sequence(values) => {
            JsonValue::Array(values.iter().map(yaml_to_json_value).collect())
        }
        YamlValue::Mapping(values) => JsonValue::Object(
            values
                .iter()
                .map(|(key, value)| (yaml_key_to_string(key), yaml_to_json_value(value)))
                .collect(),
        ),
        YamlValue::Tagged(tagged) => yaml_to_json_value(&tagged.value),
    }
}

fn yaml_key_to_string(value: &YamlValue) -> String {
    match value {
        YamlValue::Null => "null".into(),
        YamlValue::Bool(boolean) => boolean.to_string(),
        YamlValue::Number(number) => number.to_string(),
        YamlValue::String(string) => string.clone(),
        YamlValue::Sequence(_) | YamlValue::Mapping(_) | YamlValue::Tagged(_) => {
            serde_yaml::to_string(value)
                .unwrap_or_default()
                .trim()
                .to_owned()
        }
    }
}

fn yaml_path_exists(value: &YamlValue, path: &[&str]) -> bool {
    let mut current = value;

    for key in path {
        let Some(mapping) = current.as_mapping() else {
            return false;
        };
        let Some(next) = mapping.get(YamlValue::String((*key).into())) else {
            return false;
        };
        current = next;
    }

    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum IntegerLike {
    Integer(i64),
    String(String),
}

impl IntegerLike {
    fn to_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::String(value) => value.trim().parse::<i64>().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use serde_json::Value as JsonValue;
    use serde_yaml::Value as YamlValue;

    use super::{expand_tilde, resolve_env_var, yaml_to_json_value, ServerConfig, SymphonyConfig};
    use crate::error::SymphonyError;

    #[test]
    // SPEC 17.1: config defaults apply when optional values are missing.
    fn config_defaults_are_applied() {
        let value: YamlValue = serde_yaml::from_str("{}").unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.polling.interval_ms, 30_000);
        assert_eq!(config.polling.stall_timeout_ms, 600_000);
        assert_eq!(config.hooks.timeout_ms, 60_000);
        assert_eq!(config.agent.max_concurrent_agents, 10);
        assert_eq!(config.agent.max_turns, 20);
        assert_eq!(config.agent.max_retry_backoff_ms, 300_000);
        assert_eq!(config.tracker.active_states, vec!["Todo", "In Progress"]);
        assert_eq!(config.tracker.terminal_states, vec!["Done", "Canceled"]);
        assert_eq!(config.tracker.assignee, None);
        assert_eq!(config.codex.command.as_deref(), Some("codex app-server"));
        assert_eq!(config.codex.turn_timeout_ms, Some(3_600_000));
        assert_eq!(config.codex.read_timeout_ms, Some(5_000));
        assert_eq!(config.codex.stall_timeout_ms, Some(300_000));
        assert_eq!(config.server.host.as_deref(), Some("127.0.0.1"));
        assert!(config.observability.dashboard_enabled);
        assert_eq!(config.observability.refresh_ms, 1_000);
        assert_eq!(config.observability.render_interval_ms, 16);
        assert_eq!(config.observability.log_max_bytes, 10_485_760);
        assert_eq!(config.observability.log_max_files, 5);
    }

    #[test]
    fn codex_stall_timeout_overrides_polling() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
polling:
  stall_timeout_ms: 600000
codex:
  stall_timeout_ms: 120000
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.polling.stall_timeout_ms, 600_000);
        assert_eq!(config.codex.stall_timeout_ms, Some(120_000));
    }

    #[test]
    fn polling_stall_timeout_fallback() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
polling:
  stall_timeout_ms: 600000
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.codex.stall_timeout_ms, Some(600_000));
    }

    #[test]
    fn codex_timeout_string_values_parsed() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
codex:
  turn_timeout_ms: "3600000"
  read_timeout_ms: "5000"
  stall_timeout_ms: "300000"
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.codex.turn_timeout_ms, Some(3_600_000));
        assert_eq!(config.codex.read_timeout_ms, Some(5_000));
        assert_eq!(config.codex.stall_timeout_ms, Some(300_000));
    }

    #[test]
    fn config_approval_policy_string_value() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
codex:
  approval_policy: "auto"
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(
            config.codex.approval_policy,
            Some(JsonValue::String("auto".into()))
        );
    }

    #[test]
    fn config_approval_policy_map_value() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
codex:
  approval_policy:
    reject:
      sandbox_approval: true
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();
        let expected: YamlValue = serde_yaml::from_str(
            r#"
reject:
  sandbox_approval: true
"#,
        )
        .unwrap();

        assert_eq!(
            config.codex.approval_policy,
            Some(yaml_to_json_value(&expected))
        );
    }

    #[test]
    fn config_turn_sandbox_policy_map() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
codex:
  turn_sandbox_policy:
    type: workspaceWrite
    writableRoots:
      - /tmp/ws
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();
        let expected: YamlValue = serde_yaml::from_str(
            r#"
type: workspaceWrite
writableRoots:
  - /tmp/ws
"#,
        )
        .unwrap();

        assert_eq!(
            config.codex.turn_sandbox_policy,
            Some(yaml_to_json_value(&expected))
        );
    }

    #[test]
    fn observability_config_parses_custom_values() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
observability:
  dashboard_enabled: false
  refresh_ms: 2000
  render_interval_ms: 32
  log_max_bytes: 5242880
  log_max_files: 3
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert!(!config.observability.dashboard_enabled);
        assert_eq!(config.observability.refresh_ms, 2_000);
        assert_eq!(config.observability.render_interval_ms, 32);
        assert_eq!(config.observability.log_max_bytes, 5_242_880);
        assert_eq!(config.observability.log_max_files, 3);
    }

    #[test]
    fn observability_config_parses_string_encoded_integers() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
observability:
  refresh_ms: "5000"
  render_interval_ms: "24"
  log_max_bytes: "1048576"
  log_max_files: "7"
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.observability.refresh_ms, 5_000);
        assert_eq!(config.observability.render_interval_ms, 24);
        assert_eq!(config.observability.log_max_bytes, 1_048_576);
        assert_eq!(config.observability.log_max_files, 7);
    }

    #[test]
    fn observability_config_invalid_numeric_values_fallback_to_defaults() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
observability:
  refresh_ms: 0
  render_interval_ms: -1
  log_max_bytes: 0
  log_max_files: -3
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.observability.refresh_ms, 1_000);
        assert_eq!(config.observability.render_interval_ms, 16);
        assert_eq!(config.observability.log_max_bytes, 10_485_760);
        assert_eq!(config.observability.log_max_files, 5);
    }

    #[test]
    fn assignee_config_parses_literal_value() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
tracker:
  assignee: user-uuid-123
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.tracker.assignee.as_deref(), Some("user-uuid-123"));
    }

    #[test]
    // SPEC 17.1: `$VAR` resolution works for tracker API key values.
    fn env_resolution_uses_variable_value() {
        let key = format!("SYMPHONY_TEST_KEY_{}", std::process::id());
        env::set_var(&key, "secret");

        let yaml = format!(
            r#"
tracker:
  api_key: ${key}
"#
        );
        let value: YamlValue = serde_yaml::from_str(&yaml).unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.tracker.api_key.as_deref(), Some("secret"));

        env::remove_var(key);
    }

    #[test]
    // SPEC 17.1: empty `$VAR` values are treated as missing runtime config.
    fn env_resolution_treats_empty_value_as_missing() {
        let key = format!("SYMPHONY_EMPTY_KEY_{}", std::process::id());
        env::set_var(&key, "");

        let resolved = resolve_env_var(&format!("${key}"));

        assert_eq!(resolved, None);

        env::remove_var(key);
    }

    #[test]
    fn assignee_env_resolution_uses_variable_value() {
        let key = format!("SYMPHONY_ASSIGNEE_KEY_{}", std::process::id());
        env::set_var(&key, "viewer-123");

        let yaml = format!(
            r#"
tracker:
  assignee: ${key}
"#
        );
        let value: YamlValue = serde_yaml::from_str(&yaml).unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.tracker.assignee.as_deref(), Some("viewer-123"));

        env::remove_var(key);
    }

    #[test]
    fn assignee_uses_linear_assignee_fallback() {
        let key = format!("SYMPHONY_LINEAR_ASSIGNEE_{}", std::process::id());
        env::set_var(&key, "fallback-viewer");
        env::set_var("LINEAR_ASSIGNEE", "fallback-viewer");

        let value: YamlValue = serde_yaml::from_str(
            r#"
tracker:
  kind: linear
  assignee: $SYMPHONY_LINEAR_ASSIGNEE_MISSING
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.tracker.assignee.as_deref(), Some("fallback-viewer"));

        env::remove_var(key);
        env::remove_var("LINEAR_ASSIGNEE");
    }

    #[test]
    fn assignee_empty_env_resolves_to_none() {
        let key = format!("SYMPHONY_ASSIGNEE_EMPTY_{}", std::process::id());
        env::set_var(&key, "");
        env::remove_var("LINEAR_ASSIGNEE");

        let yaml = format!(
            r#"
tracker:
  assignee: ${key}
"#
        );
        let value: YamlValue = serde_yaml::from_str(&yaml).unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.tracker.assignee, None);

        env::remove_var(key);
    }

    #[test]
    // SPEC 17.1: `~` path expansion works for workspace paths.
    fn tilde_expansion_uses_home_directory() {
        let home = env::var("HOME").unwrap();

        assert_eq!(expand_tilde("~/workspace"), format!("{home}/workspace"));
    }

    #[test]
    // SPEC 17.1: per-state concurrency overrides are normalized and invalid values ignored.
    fn per_state_limits_are_normalized_and_filtered() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
agent:
  max_concurrent_agents_by_state:
    Todo: 3
    IN_PROGRESS: "2"
    blocked: 0
    invalid: nope
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(
            config.agent.max_concurrent_agents_by_state.get("todo"),
            Some(&3)
        );
        assert_eq!(
            config
                .agent
                .max_concurrent_agents_by_state
                .get("in_progress"),
            Some(&2)
        );
        assert!(!config
            .agent
            .max_concurrent_agents_by_state
            .contains_key("blocked"));
        assert!(!config
            .agent
            .max_concurrent_agents_by_state
            .contains_key("invalid"));
    }

    #[test]
    // SPEC 17.1: `tracker.kind` is required during typed config validation.
    fn validation_fails_when_tracker_kind_is_missing() {
        let value: YamlValue = serde_yaml::from_str("{}").unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        let error = config.validate().unwrap_err();

        assert!(matches!(error, SymphonyError::ConfigValidation(_)));
    }

    #[test]
    // SPEC 17.1: `tracker.api_key` is required during typed config validation.
    fn validation_fails_when_api_key_is_missing() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
tracker:
  kind: linear
  project_slug: proj
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        let error = config.validate().unwrap_err();

        assert!(matches!(error, SymphonyError::ConfigValidation(_)));
    }

    #[test]
    // SPEC 17.1: `tracker.project_slug` is required during typed config validation.
    fn validation_fails_when_project_slug_is_missing() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
tracker:
  kind: linear
  api_key: token
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        let error = config.validate().unwrap_err();

        assert!(matches!(error, SymphonyError::ConfigValidation(_)));
    }

    #[test]
    // SPEC 17.1: `tracker.kind` validation enforces the currently supported kind.
    fn validation_fails_when_tracker_kind_is_not_linear() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
tracker:
  kind: github
  api_key: token
  project_slug: proj
codex:
  command: codex app-server
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        let error = config.validate().unwrap_err();

        assert!(matches!(error, SymphonyError::ConfigValidation(_)));
    }

    #[test]
    // SPEC 17.1: `memory` tracker validation skips Linear-specific required fields.
    fn validation_allows_memory_tracker_without_linear_fields() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
tracker:
  kind: memory
codex:
  command: codex app-server
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert!(config.validate().is_ok());
    }

    #[test]
    fn worker_config_parses_ssh_hosts() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
worker:
  ssh_hosts:
    - host1
    - host2:2222
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.worker.ssh_hosts, vec!["host1", "host2:2222"]);
        assert_eq!(config.worker.max_concurrent_agents_per_host, None);
    }

    #[test]
    fn worker_config_defaults_when_section_is_missing() {
        let value: YamlValue = serde_yaml::from_str("{}").unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert!(config.worker.ssh_hosts.is_empty());
        assert_eq!(config.worker.max_concurrent_agents_per_host, None);
    }

    #[test]
    fn worker_config_preserves_empty_ssh_hosts() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
worker:
  ssh_hosts: []
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert!(config.worker.ssh_hosts.is_empty());
        assert_eq!(config.worker.max_concurrent_agents_per_host, None);
    }

    #[test]
    fn worker_config_parses_per_host_limit() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
worker:
  max_concurrent_agents_per_host: 3
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.worker.ssh_hosts, Vec::<String>::new());
        assert_eq!(config.worker.max_concurrent_agents_per_host, Some(3));
    }

    #[test]
    fn server_config_default_host_is_localhost() {
        let config = ServerConfig::default();

        assert_eq!(config.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.port, None);
    }

    #[test]
    fn server_config_parses_custom_host_from_yaml() {
        let value: YamlValue = serde_yaml::from_str(
            r#"
server:
  host: 0.0.0.0
"#,
        )
        .unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.server.host.as_deref(), Some("0.0.0.0"));
    }
}
