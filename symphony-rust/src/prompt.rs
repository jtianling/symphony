use liquid::model::{Array, Object, Value};
use liquid::ParserBuilder;

use crate::domain::Issue;
use crate::error::SymphonyError;

pub const DEFAULT_PROMPT: &str = "You are working on an issue from Linear.";

#[derive(Debug, Default)]
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn render(
        &self,
        template: &str,
        issue: &Issue,
        attempt: Option<u32>,
    ) -> Result<String, SymphonyError> {
        let trimmed_template = template.trim();
        if trimmed_template.is_empty() {
            return Ok(DEFAULT_PROMPT.to_string());
        }

        let parser = ParserBuilder::with_stdlib()
            .build()
            .map_err(|error| SymphonyError::PromptRender(error.to_string()))?;
        let parsed_template = parser
            .parse(trimmed_template)
            .map_err(|error| SymphonyError::PromptRender(error.to_string()))?;

        parsed_template
            .render(&build_context(issue, attempt))
            .map_err(|error| SymphonyError::PromptRender(error.to_string()))
    }

    pub fn build_prompt(
        &self,
        template: &str,
        issue: &Issue,
        attempt: Option<u32>,
        turn_number: u32,
    ) -> Result<String, SymphonyError> {
        if turn_number == 1 {
            return self.render(template, issue, attempt);
        }

        Ok(format!(
            "Continue working on issue {}. Check the current state and make further progress. This is turn {}.",
            issue.identifier, turn_number
        ))
    }
}

fn build_context(issue: &Issue, attempt: Option<u32>) -> Object {
    let mut issue_object = Object::new();
    issue_object.insert("id".into(), Value::scalar(issue.id.clone()));
    issue_object.insert("identifier".into(), Value::scalar(issue.identifier.clone()));
    issue_object.insert("title".into(), Value::scalar(issue.title.clone()));
    issue_object.insert(
        "description".into(),
        optional_string_value(issue.description.as_deref()),
    );
    issue_object.insert("priority".into(), optional_i32_value(issue.priority));
    issue_object.insert("state".into(), Value::scalar(issue.state.clone()));
    issue_object.insert(
        "branch_name".into(),
        optional_string_value(issue.branch_name.as_deref()),
    );
    issue_object.insert("url".into(), optional_string_value(issue.url.as_deref()));
    issue_object.insert("labels".into(), labels_value(&issue.labels));
    issue_object.insert("blocked_by".into(), blockers_value(issue));
    issue_object.insert(
        "created_at".into(),
        optional_string_value(issue.created_at.as_deref()),
    );
    issue_object.insert(
        "updated_at".into(),
        optional_string_value(issue.updated_at.as_deref()),
    );

    let mut context = Object::new();
    context.insert("issue".into(), Value::Object(issue_object));
    context.insert("attempt".into(), optional_u32_value(attempt));
    context
}

fn labels_value(labels: &[String]) -> Value {
    Value::Array(labels.iter().cloned().map(Value::scalar).collect::<Array>())
}

fn blockers_value(issue: &Issue) -> Value {
    let blockers = issue
        .blocked_by
        .iter()
        .map(|blocker| {
            let mut blocker_object = Object::new();
            blocker_object.insert("id".into(), Value::scalar(blocker.id.clone()));
            blocker_object.insert(
                "identifier".into(),
                Value::scalar(blocker.identifier.clone()),
            );
            blocker_object.insert("state".into(), Value::scalar(blocker.state.clone()));
            Value::Object(blocker_object)
        })
        .collect::<Array>();

    Value::Array(blockers)
}

fn optional_string_value(value: Option<&str>) -> Value {
    match value {
        Some(value) => Value::scalar(value.to_owned()),
        None => Value::Nil,
    }
}

fn optional_i32_value(value: Option<i32>) -> Value {
    match value {
        Some(value) => Value::scalar(i64::from(value)),
        None => Value::Nil,
    }
}

fn optional_u32_value(value: Option<u32>) -> Value {
    match value {
        Some(value) => Value::scalar(i64::from(value)),
        None => Value::Nil,
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{BlockerRef, Issue};

    use super::{PromptBuilder, DEFAULT_PROMPT};

    fn sample_issue() -> Issue {
        Issue {
            id: "issue-id".to_string(),
            identifier: "SYM-123".to_string(),
            title: "Implement prompt builder".to_string(),
            description: Some("Build prompt context".to_string()),
            priority: Some(2),
            state: "In Progress".to_string(),
            branch_name: Some("feature/sym-123".to_string()),
            url: Some("https://linear.app/symphony/issue/SYM-123".to_string()),
            labels: vec!["rust".to_string(), "backend".to_string()],
            blocked_by: vec![BlockerRef {
                id: "blocker-id".to_string(),
                identifier: "SYM-100".to_string(),
                state: "Todo".to_string(),
            }],
            created_at: Some("2026-03-14T00:00:00Z".to_string()),
            updated_at: Some("2026-03-14T01:00:00Z".to_string()),
        }
    }

    #[test]
    // SPEC 17.1: prompt templates render `issue` variables from normalized issue context.
    fn render_includes_issue_variables() -> Result<(), crate::error::SymphonyError> {
        let prompt_builder = PromptBuilder;
        let issue = sample_issue();
        let template = "{{ issue.identifier }}|{{ issue.title }}|{{ issue.labels | join: ', ' }}";

        let rendered = prompt_builder.render(template, &issue, None)?;

        assert_eq!(rendered, "SYM-123|Implement prompt builder|rust, backend");

        Ok(())
    }

    #[test]
    // SPEC 17.1: empty prompt templates fall back to the documented default prompt.
    fn render_returns_default_prompt_for_empty_template() -> Result<(), crate::error::SymphonyError>
    {
        let prompt_builder = PromptBuilder;
        let issue = sample_issue();

        let rendered = prompt_builder.render("", &issue, None)?;

        assert_eq!(rendered, DEFAULT_PROMPT);

        Ok(())
    }

    #[test]
    // SPEC 17.1: whitespace-only templates also fall back to the default prompt.
    fn render_returns_default_prompt_for_whitespace_template(
    ) -> Result<(), crate::error::SymphonyError> {
        let prompt_builder = PromptBuilder;
        let issue = sample_issue();

        let rendered = prompt_builder.render("   \n\t  ", &issue, None)?;

        assert_eq!(rendered, DEFAULT_PROMPT);

        Ok(())
    }

    #[test]
    // SPEC 17.5: continuation turns use the implementation-defined follow-up prompt.
    fn build_prompt_returns_continuation_prompt_for_later_turns(
    ) -> Result<(), crate::error::SymphonyError> {
        let prompt_builder = PromptBuilder;
        let issue = sample_issue();

        let rendered = prompt_builder.build_prompt("{{ issue.title }}", &issue, None, 3)?;

        assert_eq!(
            rendered,
            "Continue working on issue SYM-123. Check the current state and make further progress. This is turn 3."
        );

        Ok(())
    }

    #[test]
    // SPEC 17.1: first-turn prompt construction preserves the full workflow template.
    fn build_prompt_uses_full_template_on_first_turn() -> Result<(), crate::error::SymphonyError> {
        let prompt_builder = PromptBuilder;
        let issue = sample_issue();

        let rendered = prompt_builder.build_prompt(
            "{{ issue.identifier }}: {{ issue.title }}",
            &issue,
            None,
            1,
        )?;

        assert_eq!(rendered, "SYM-123: Implement prompt builder");

        Ok(())
    }

    #[test]
    // SPEC 17.1: prompt templates render `attempt` when retry context is present.
    fn render_includes_attempt_variable() -> Result<(), crate::error::SymphonyError> {
        let prompt_builder = PromptBuilder;
        let issue = sample_issue();

        let rendered = prompt_builder.render("Attempt {{ attempt }}", &issue, Some(2))?;

        assert_eq!(rendered, "Attempt 2");

        Ok(())
    }
}
