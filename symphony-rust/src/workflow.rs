use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde_yaml::{Mapping, Value};
use tokio::sync::mpsc;

use crate::domain::WorkflowDefinition;
use crate::error::SymphonyError;

const DEBOUNCE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct WorkflowReloadMsg {
    pub definition: WorkflowDefinition,
}

pub fn load_workflow(path: impl AsRef<Path>) -> Result<WorkflowDefinition, SymphonyError> {
    let path = path.as_ref();
    let content =
        fs::read_to_string(path).map_err(|source| SymphonyError::MissingWorkflowFile {
            path: path.display().to_string(),
            source,
        })?;

    let (config, prompt_template) = parse_workflow_content(&content)?;

    Ok(WorkflowDefinition {
        config,
        prompt_template,
    })
}

pub fn watch_workflow(
    path: PathBuf,
    sender: mpsc::Sender<WorkflowReloadMsg>,
) -> Result<RecommendedWatcher, SymphonyError> {
    let workflow_path = absolute_path(path)?;
    let watched_name = workflow_path
        .file_name()
        .map(|name| name.to_os_string())
        .ok_or_else(|| {
            SymphonyError::WorkflowWatch("workflow path must include a file name".into())
        })?;
    let watch_root = workflow_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            SymphonyError::WorkflowWatch("workflow path must include a parent directory".into())
        })?;
    let last_event = Arc::new(Mutex::new(Instant::now() - DEBOUNCE_WINDOW));
    let watched_path = workflow_path.clone();
    let sender_clone = sender.clone();
    let debounce_state = Arc::clone(&last_event);

    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else {
                return;
            };

            let matches_target = event.paths.iter().any(|event_path| {
                event_path == &watched_path
                    || event_path
                        .file_name()
                        .map(|name| name == watched_name.as_os_str())
                        .unwrap_or(false)
            });

            if !matches_target {
                return;
            }

            let mut guard = match debounce_state.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };

            if guard.elapsed() < DEBOUNCE_WINDOW {
                return;
            }

            *guard = Instant::now();
            drop(guard);

            if let Ok(definition) = load_workflow(&watched_path) {
                let _ = sender_clone.blocking_send(WorkflowReloadMsg { definition });
            }
        },
        Config::default(),
    )
    .map_err(|error| SymphonyError::WorkflowWatch(error.to_string()))?;

    watcher
        .watch(&watch_root, RecursiveMode::NonRecursive)
        .map_err(|error| SymphonyError::WorkflowWatch(error.to_string()))?;

    Ok(watcher)
}

fn parse_workflow_content(content: &str) -> Result<(Value, String), SymphonyError> {
    if !content.starts_with("---") {
        return Ok((empty_config_map(), content.trim().to_owned()));
    }

    let mut lines = content.lines();
    let Some(first_line) = lines.next() else {
        return Ok((empty_config_map(), String::new()));
    };

    if first_line != "---" {
        return Ok((empty_config_map(), content.trim().to_owned()));
    }

    let mut yaml_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut in_front_matter = true;

    for line in lines {
        if in_front_matter && line == "---" {
            in_front_matter = false;
            continue;
        }

        if in_front_matter {
            yaml_lines.push(line);
        } else {
            body_lines.push(line);
        }
    }

    if in_front_matter {
        return Err(SymphonyError::WorkflowParseError {
            message: "missing closing front matter delimiter".into(),
            source: None,
        });
    }

    let config = if yaml_lines.iter().all(|line| line.trim().is_empty()) {
        empty_config_map()
    } else {
        let parsed = serde_yaml::from_str::<Value>(&yaml_lines.join("\n")).map_err(|source| {
            SymphonyError::WorkflowParseError {
                message: source.to_string(),
                source: Some(source),
            }
        })?;

        match parsed {
            Value::Mapping(mapping) => Value::Mapping(mapping),
            _ => return Err(SymphonyError::WorkflowFrontMatterNotAMap),
        }
    };

    Ok((config, body_lines.join("\n").trim().to_owned()))
}

fn empty_config_map() -> Value {
    Value::Mapping(Mapping::new())
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, SymphonyError> {
    if path.is_absolute() {
        return Ok(path);
    }

    std::env::current_dir()
        .map(|current_dir| current_dir.join(path))
        .map_err(|error| SymphonyError::WorkflowWatch(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_yaml::Value;
    use tempfile::tempdir;

    use super::load_workflow;
    use crate::error::SymphonyError;

    #[test]
    // SPEC 17.1: workflow front matter and Markdown prompt body are split end-to-end.
    fn loads_workflow_with_front_matter_and_body() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
tracker:
  kind: linear
  api_key: token
---

# Prompt

Do work.
"#,
        )
        .unwrap();

        let workflow = load_workflow(&path).unwrap();

        assert_eq!(
            workflow.config["tracker"]["kind"],
            Value::String("linear".into())
        );
        assert_eq!(workflow.prompt_template, "# Prompt\n\nDo work.");
    }

    #[test]
    // SPEC 17.1: prompt-only workflow files default to an empty config map.
    fn loads_workflow_without_front_matter() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("WORKFLOW.md");
        fs::write(&path, "\n# Prompt\n\nShip it.\n").unwrap();

        let workflow = load_workflow(&path).unwrap();

        assert_eq!(workflow.config, Value::Mapping(Default::default()));
        assert_eq!(workflow.prompt_template, "# Prompt\n\nShip it.");
    }

    #[test]
    // SPEC 17.1: non-map YAML front matter returns a typed workflow error.
    fn errors_when_front_matter_is_not_a_map() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
- linear
---
body
"#,
        )
        .unwrap();

        let error = load_workflow(&path).unwrap_err();

        assert!(matches!(error, SymphonyError::WorkflowFrontMatterNotAMap));
    }

    #[test]
    // SPEC 17.1: invalid YAML front matter returns a typed workflow parse error.
    fn errors_when_yaml_is_invalid() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("WORKFLOW.md");
        fs::write(
            &path,
            r#"---
tracker: [1
---
body
"#,
        )
        .unwrap();

        let error = load_workflow(&path).unwrap_err();

        assert!(matches!(error, SymphonyError::WorkflowParseError { .. }));
    }
}
