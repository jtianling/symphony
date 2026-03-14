use std::collections::HashMap;
use std::env;

use serde::de::Deserializer;
use serde::Deserialize;
use serde_yaml::Value;

use crate::error::SymphonyError;

const DEFAULT_POLLING_INTERVAL_MS: u64 = 30_000;
const DEFAULT_STALL_TIMEOUT_MS: i64 = 600_000;
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_CONCURRENT_AGENTS: u32 = 10;
const DEFAULT_MAX_TURNS: u32 = 5;
const DEFAULT_MAX_RETRY_BACKOFF_MS: u64 = 300_000;
const DEFAULT_CODEX_COMMAND: &str = "codex app-server";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub kind: Option<String>,
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub project_slug: Option<String>,
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
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: default_codex_command(),
            approval_policy: None,
            sandbox: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SymphonyConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub codex: CodexConfig,
    pub server: ServerConfig,
}

impl SymphonyConfig {
    pub fn from_yaml_value(value: &Value) -> Result<Self, SymphonyError> {
        let mut config = serde_yaml::from_value::<Self>(value.clone())
            .map_err(|error| SymphonyError::ConfigParse(error.to_string()))?;

        config.resolve_runtime_values();

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

        if tracker_kind != "linear" {
            return Err(SymphonyError::ConfigValidation(
                "tracker.kind must equal \"linear\"".into(),
            ));
        }

        let api_key = self
            .tracker
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SymphonyError::ConfigValidation("tracker.api_key is required".into()))?;

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

        self.codex
            .command
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SymphonyError::ConfigValidation("codex.command is required".into()))?;

        Ok(())
    }

    fn resolve_runtime_values(&mut self) {
        self.tracker.api_key = self.tracker.api_key.as_deref().and_then(resolve_env_var);

        self.workspace.root = self
            .workspace
            .root
            .as_deref()
            .and_then(resolve_env_var)
            .map(|value| expand_tilde(&value));

        self.agent.max_concurrent_agents_by_state =
            normalize_state_limit_map(self.agent.max_concurrent_agents_by_state.clone());
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

fn deserialize_state_limit_map<'de, D>(deserializer: D) -> Result<HashMap<String, u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Option::<HashMap<String, Value>>::deserialize(deserializer)?
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

fn parse_yaml_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .and_then(|parsed| u32::try_from(parsed).ok()),
        Value::String(string) => string.trim().parse::<u32>().ok(),
        _ => None,
    }
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

    use serde_yaml::Value;

    use super::{expand_tilde, resolve_env_var, SymphonyConfig};
    use crate::error::SymphonyError;

    #[test]
    // SPEC 17.1: config defaults apply when optional values are missing.
    fn config_defaults_are_applied() {
        let value: Value = serde_yaml::from_str("{}").unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        assert_eq!(config.polling.interval_ms, 30_000);
        assert_eq!(config.polling.stall_timeout_ms, 600_000);
        assert_eq!(config.hooks.timeout_ms, 60_000);
        assert_eq!(config.agent.max_concurrent_agents, 10);
        assert_eq!(config.agent.max_turns, 5);
        assert_eq!(config.agent.max_retry_backoff_ms, 300_000);
        assert_eq!(config.tracker.active_states, vec!["Todo", "In Progress"]);
        assert_eq!(config.tracker.terminal_states, vec!["Done", "Canceled"]);
        assert_eq!(config.codex.command.as_deref(), Some("codex app-server"));
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
        let value: Value = serde_yaml::from_str(&yaml).unwrap();
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
    // SPEC 17.1: `~` path expansion works for workspace paths.
    fn tilde_expansion_uses_home_directory() {
        let home = env::var("HOME").unwrap();

        assert_eq!(expand_tilde("~/workspace"), format!("{home}/workspace"));
    }

    #[test]
    // SPEC 17.1: per-state concurrency overrides are normalized and invalid values ignored.
    fn per_state_limits_are_normalized_and_filtered() {
        let value: Value = serde_yaml::from_str(
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
        let value: Value = serde_yaml::from_str("{}").unwrap();
        let config = SymphonyConfig::from_yaml_value(&value).unwrap();

        let error = config.validate().unwrap_err();

        assert!(matches!(error, SymphonyError::ConfigValidation(_)));
    }

    #[test]
    // SPEC 17.1: `tracker.api_key` is required during typed config validation.
    fn validation_fails_when_api_key_is_missing() {
        let value: Value = serde_yaml::from_str(
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
        let value: Value = serde_yaml::from_str(
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
        let value: Value = serde_yaml::from_str(
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
}
