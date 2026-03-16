use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::error::SymphonyError;
use crate::linear::LinearClient;

#[derive(Debug, Clone)]
pub struct LinearGraphqlTool {
    client: Arc<LinearClient>,
}

impl LinearGraphqlTool {
    pub fn new(client: Arc<LinearClient>) -> Self {
        Self { client }
    }

    pub async fn handle(&self, arguments: Value) -> Value {
        match normalize_arguments(arguments) {
            Ok((query, variables)) => match self.client.execute(&query, variables).await {
                Ok(payload) => graphql_response(payload),
                Err(error) => failure_response(tool_error_payload(error)),
            },
            Err(error) => failure_response(tool_error_payload(error)),
        }
    }
}

fn normalize_arguments(arguments: Value) -> Result<(String, Value), SymphonyError> {
    match arguments {
        Value::String(query) => {
            let normalized = normalize_query(&query)?;
            Ok((normalized, Value::Object(Map::new())))
        }
        Value::Object(arguments) => {
            let query = normalize_query_value(arguments.get("query"))?;
            let variables = arguments
                .get("variables")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));

            if !matches!(variables, Value::Object(_)) {
                return Err(SymphonyError::Codex(
                    "linear_graphql_invalid_variables".into(),
                ));
            }

            Ok((query, variables))
        }
        _ => Err(SymphonyError::Codex(
            "linear_graphql_invalid_arguments".into(),
        )),
    }
}

fn normalize_query_value(query: Option<&Value>) -> Result<String, SymphonyError> {
    match query {
        Some(Value::String(query)) => normalize_query(query),
        _ => Err(SymphonyError::Codex("linear_graphql_empty_query".into())),
    }
}

fn normalize_query(query: &str) -> Result<String, SymphonyError> {
    let normalized = query.trim();
    validate_query(normalized)?;
    Ok(normalized.to_owned())
}

fn graphql_response(payload: Value) -> Value {
    let success = payload
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| errors.is_empty())
        .unwrap_or(true);
    dynamic_tool_response(success, encode_payload(&payload))
}

fn failure_response(payload: Value) -> Value {
    dynamic_tool_response(false, encode_payload(&payload))
}

fn dynamic_tool_response(success: bool, output: String) -> Value {
    json!({
        "success": success,
        "output": output,
        "contentItems": [{
            "type": "inputText",
            "text": output,
        }],
    })
}

fn encode_payload(payload: &Value) -> String {
    serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
}

fn tool_error_payload(error: SymphonyError) -> Value {
    match error {
        SymphonyError::Codex(message) => tool_validation_error_payload(&message),
        SymphonyError::ConfigValidation(message) if message == "tracker.api_key is required" => {
            json!({
                "error": {
                    "message": "Symphony is missing Linear auth. Set `linear.api_key` in `WORKFLOW.md` or export `LINEAR_API_KEY`."
                }
            })
        }
        SymphonyError::LinearApiStatus { status, .. } => json!({
            "error": {
                "message": format!("Linear GraphQL request failed with HTTP {status}."),
                "status": status,
            }
        }),
        SymphonyError::LinearApiRequest(reason) => json!({
            "error": {
                "message": "Linear GraphQL request failed before receiving a successful response.",
                "reason": reason,
            }
        }),
        other => json!({
            "error": {
                "message": "Linear GraphQL tool execution failed.",
                "reason": other.to_string(),
            }
        }),
    }
}

fn tool_validation_error_payload(message: &str) -> Value {
    let message = match message {
        "linear_graphql_empty_query" => {
            "`linear_graphql` requires a non-empty `query` string."
        }
        "linear_graphql_invalid_arguments" => {
            "`linear_graphql` expects either a GraphQL query string or an object with `query` and optional `variables`."
        }
        "linear_graphql_invalid_variables" => {
            "`linear_graphql.variables` must be a JSON object when provided."
        }
        "linear_graphql_multiple_operations" => {
            "`linear_graphql` accepts exactly one GraphQL operation per request."
        }
        _ => "Linear GraphQL tool execution failed.",
    };

    json!({
        "error": {
            "message": message,
        }
    })
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
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::LinearGraphqlTool;
    use crate::linear::LinearClient;

    fn build_tool(client: LinearClient) -> LinearGraphqlTool {
        LinearGraphqlTool::new(Arc::new(client))
    }

    fn decode_output(response: &serde_json::Value) -> serde_json::Value {
        serde_json::from_str(response["output"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn rejects_empty_query() {
        let client =
            LinearClient::with_endpoint("linear-key", "http://127.0.0.1:9/graphql").unwrap();
        let tool = build_tool(client);
        let response = tool.handle(json!("")).await;
        let output = decode_output(&response);

        assert_eq!(response["success"], json!(false));
        assert_eq!(
            output,
            json!({
                "error": {
                    "message": "`linear_graphql` requires a non-empty `query` string."
                }
            })
        );
    }

    #[tokio::test]
    async fn rejects_non_object_variables() {
        let client =
            LinearClient::with_endpoint("linear-key", "http://127.0.0.1:9/graphql").unwrap();
        let tool = build_tool(client);
        let response = tool
            .handle(json!({
                "query": "query Viewer { viewer { id } }",
                "variables": "invalid",
            }))
            .await;
        let output = decode_output(&response);

        assert_eq!(response["success"], json!(false));
        assert_eq!(
            output,
            json!({
                "error": {
                    "message": "`linear_graphql.variables` must be a JSON object when provided."
                }
            })
        );
    }

    #[tokio::test]
    async fn rejects_multiple_operations() {
        let client =
            LinearClient::with_endpoint("linear-key", "http://127.0.0.1:9/graphql").unwrap();
        let tool = build_tool(client);
        let response = tool
            .handle(json!({
                "query": "query Viewer { viewer { id } } mutation Update { updateIssue { success } }",
            }))
            .await;
        let output = decode_output(&response);

        assert_eq!(response["success"], json!(false));
        assert_eq!(
            output,
            json!({
                "error": {
                    "message": "`linear_graphql` accepts exactly one GraphQL operation per request."
                }
            })
        );
    }

    #[tokio::test]
    async fn executes_object_input_against_linear() {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri()).unwrap();
        let tool = build_tool(client);

        Mock::given(method("POST"))
            .and(header("authorization", "linear-key"))
            .and(body_partial_json(json!({
                "query": "query Viewer { viewer { id } }",
                "variables": {
                    "includeTeams": false
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "viewer": {
                        "id": "usr_123"
                    }
                }
            })))
            .mount(&server)
            .await;

        let response = tool
            .handle(json!({
                "query": "query Viewer { viewer { id } }",
                "variables": {
                    "includeTeams": false
                }
            }))
            .await;

        assert_eq!(response["success"], json!(true));
        assert_eq!(
            decode_output(&response),
            json!({
                "data": {
                    "viewer": {
                        "id": "usr_123"
                    }
                }
            })
        );
        assert_eq!(
            response["contentItems"],
            json!([{
                "type": "inputText",
                "text": response["output"],
            }])
        );
    }

    #[tokio::test]
    async fn executes_raw_query_string_with_empty_variables() {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri()).unwrap();
        let tool = build_tool(client);

        Mock::given(method("POST"))
            .and(header("authorization", "linear-key"))
            .and(body_partial_json(json!({
                "query": "query Viewer { viewer { id } }",
                "variables": {}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "viewer": {
                        "id": "usr_456"
                    }
                }
            })))
            .mount(&server)
            .await;

        let response = tool
            .handle(json!("  query Viewer { viewer { id } }  "))
            .await;

        assert_eq!(response["success"], json!(true));
        assert_eq!(
            decode_output(&response),
            json!({
                "data": {
                    "viewer": {
                        "id": "usr_456"
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn marks_graphql_error_payloads_as_failures() {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri()).unwrap();
        let tool = build_tool(client);

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": null,
                "errors": [{
                    "message": "Unknown field `nope`"
                }]
            })))
            .mount(&server)
            .await;

        let response = tool
            .handle(json!({
                "query": "mutation BadMutation { nope }"
            }))
            .await;

        assert_eq!(response["success"], json!(false));
        assert_eq!(
            decode_output(&response),
            json!({
                "data": null,
                "errors": [{
                    "message": "Unknown field `nope`"
                }]
            })
        );
    }

    #[tokio::test]
    async fn maps_transport_failures_to_error_envelopes() {
        let client =
            LinearClient::with_endpoint("linear-key", "http://127.0.0.1:9/graphql").unwrap();
        let tool = build_tool(client);

        let response = tool
            .handle(json!({
                "query": "query Viewer { viewer { id } }"
            }))
            .await;
        let output = decode_output(&response);

        assert_eq!(response["success"], json!(false));
        assert_eq!(
            output["error"]["message"],
            json!("Linear GraphQL request failed before receiving a successful response.")
        );
    }
}
