use serde_json::Value;

use crate::error::SymphonyError;

#[derive(Debug, Clone, Default)]
pub struct LinearGraphqlTool;

impl LinearGraphqlTool {
    pub async fn handle(&self, query: &str, variables: Value) -> Result<Value, SymphonyError> {
        validate_query(query)?;

        if !matches!(variables, Value::Object(_)) {
            return Err(SymphonyError::Codex(
                "linear_graphql_invalid_variables".into(),
            ));
        }

        Err(SymphonyError::Codex(
            "linear_graphql_not_implemented".into(),
        ))
    }
}

fn validate_query(query: &str) -> Result<(), SymphonyError> {
    let normalized = query.trim();

    if normalized.is_empty() {
        return Err(SymphonyError::Codex("linear_graphql_empty_query".into()));
    }

    let operation_count = normalized.match_indices("query ").count()
        + normalized.match_indices("mutation ").count()
        + normalized.match_indices("subscription ").count();

    if operation_count > 1 {
        return Err(SymphonyError::Codex(
            "linear_graphql_multiple_operations".into(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::LinearGraphqlTool;
    use crate::error::SymphonyError;

    #[tokio::test]
    async fn rejects_empty_query() {
        let tool = LinearGraphqlTool;
        let error = tool.handle("", json!({})).await.err();

        assert!(matches!(
            error,
            Some(SymphonyError::Codex(message))
                if message == "linear_graphql_empty_query"
        ));
    }

    #[tokio::test]
    async fn rejects_non_object_variables() {
        let tool = LinearGraphqlTool;
        let error = tool
            .handle("query Viewer { viewer { id } }", json!("invalid"))
            .await
            .err();

        assert!(matches!(
            error,
            Some(SymphonyError::Codex(message))
                if message == "linear_graphql_invalid_variables"
        ));
    }

    #[tokio::test]
    async fn rejects_multiple_operations() {
        let tool = LinearGraphqlTool;
        let error = tool
            .handle(
                "query Viewer { viewer { id } } mutation Update { updateIssue { success } }",
                json!({}),
            )
            .await
            .err();

        assert!(matches!(
            error,
            Some(SymphonyError::Codex(message))
                if message == "linear_graphql_multiple_operations"
        ));
    }
}
