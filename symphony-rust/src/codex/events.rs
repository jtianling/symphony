use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum CodexEvent {
    TurnCompleted {
        turn_id: String,
    },
    TurnFailed {
        turn_id: String,
        error: String,
    },
    TurnCancelled {
        turn_id: String,
    },
    TokenUsage {
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    RateLimit {
        payload: Value,
    },
    ApprovalRequest {
        id: Value,
        payload: Value,
    },
    ToolCall {
        id: Value,
        name: String,
        params: Value,
    },
    UserInputRequired,
    Unknown(Value),
}

pub fn parse_event(msg: &Value) -> CodexEvent {
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return CodexEvent::Unknown(msg.clone());
    };

    match method {
        "turn/completed" => CodexEvent::TurnCompleted {
            turn_id: extract_turn_id(msg),
        },
        "turn/failed" => CodexEvent::TurnFailed {
            turn_id: extract_turn_id(msg),
            error: extract_error(msg),
        },
        "turn/cancelled" => CodexEvent::TurnCancelled {
            turn_id: extract_turn_id(msg),
        },
        "thread/tokenUsage/updated" => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            if let Some((input_tokens, output_tokens, total_tokens)) = extract_token_usage(&params)
            {
                CodexEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                    total_tokens,
                }
            } else {
                CodexEvent::Unknown(msg.clone())
            }
        }
        "codex/rateLimit" => CodexEvent::RateLimit {
            payload: msg.get("params").cloned().unwrap_or(Value::Null),
        },
        "approval/required" => CodexEvent::ApprovalRequest {
            id: msg.get("id").cloned().unwrap_or(Value::Null),
            payload: msg.get("params").cloned().unwrap_or(Value::Null),
        },
        "tool/call" | "item/tool/call" => CodexEvent::ToolCall {
            id: msg.get("id").cloned().unwrap_or(Value::Null),
            name: extract_tool_name(msg),
            params: extract_tool_params(msg),
        },
        "user/inputRequired" => CodexEvent::UserInputRequired,
        _ => CodexEvent::Unknown(msg.clone()),
    }
}

fn extract_turn_id(msg: &Value) -> String {
    let params = msg.get("params").unwrap_or(&Value::Null);

    [
        params.get("turnId"),
        params.get("turn_id"),
        params.get("turn").and_then(|turn| turn.get("id")),
        msg.get("turnId"),
        msg.get("turn_id"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .unwrap_or_default()
    .to_owned()
}

fn extract_error(msg: &Value) -> String {
    let params = msg.get("params").unwrap_or(&Value::Null);

    [
        params.get("error").and_then(Value::as_str),
        params
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str),
        params.get("message").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .find(|value| !value.is_empty())
    .unwrap_or("turn_failed")
    .to_owned()
}

fn extract_tool_name(msg: &Value) -> String {
    let params = msg.get("params").unwrap_or(&Value::Null);

    [
        params.get("name"),
        params.get("toolName"),
        params.get("tool_name"),
        params.get("tool").and_then(|tool| tool.get("name")),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .unwrap_or_default()
    .to_owned()
}

fn extract_tool_params(msg: &Value) -> Value {
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    [
        params.get("params").cloned(),
        params.get("arguments").cloned(),
        params.get("input").cloned(),
    ]
    .into_iter()
    .flatten()
    .next()
    .unwrap_or(params)
}

fn extract_token_usage(payload: &Value) -> Option<(u64, u64, u64)> {
    let input_tokens = find_u64(
        payload,
        &[
            &["input_tokens"],
            &["inputTokens"],
            &["tokenUsage", "input_tokens"],
            &["tokenUsage", "inputTokens"],
            &["total_token_usage", "input_tokens"],
            &["total_token_usage", "inputTokens"],
        ],
    )?;
    let output_tokens = find_u64(
        payload,
        &[
            &["output_tokens"],
            &["outputTokens"],
            &["tokenUsage", "output_tokens"],
            &["tokenUsage", "outputTokens"],
            &["total_token_usage", "output_tokens"],
            &["total_token_usage", "outputTokens"],
        ],
    )?;
    let total_tokens = find_u64(
        payload,
        &[
            &["total_tokens"],
            &["totalTokens"],
            &["tokenUsage", "total_tokens"],
            &["tokenUsage", "totalTokens"],
            &["total_token_usage", "total_tokens"],
            &["total_token_usage", "totalTokens"],
        ],
    )?;

    Some((input_tokens, output_tokens, total_tokens))
}

fn find_u64(payload: &Value, paths: &[&[&str]]) -> Option<u64> {
    paths.iter().find_map(|path| {
        path.iter()
            .try_fold(payload, |current, key| current.get(*key))
            .and_then(to_u64)
    })
}

fn to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_event, CodexEvent};

    #[test]
    // SPEC 17.5: `turn/completed` events accept the documented `turnId` payload.
    fn parses_turn_completed_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": { "turnId": "turn-1" }
        }));

        assert_eq!(
            event,
            CodexEvent::TurnCompleted {
                turn_id: "turn-1".into()
            }
        );
    }

    #[test]
    // SPEC 17.5: `turn/failed` events accept nested turn IDs and error payloads.
    fn parses_turn_failed_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "method": "turn/failed",
            "params": {
                "turn": { "id": "turn-2" },
                "error": { "message": "boom" }
            }
        }));

        assert_eq!(
            event,
            CodexEvent::TurnFailed {
                turn_id: "turn-2".into(),
                error: "boom".into()
            }
        );
    }

    #[test]
    // SPEC 17.5: `turn/cancelled` events accept snake_case turn IDs.
    fn parses_turn_cancelled_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "method": "turn/cancelled",
            "params": { "turn_id": "turn-3" }
        }));

        assert_eq!(
            event,
            CodexEvent::TurnCancelled {
                turn_id: "turn-3".into()
            }
        );
    }

    #[test]
    // SPEC 17.5: usage telemetry is extracted from compatible nested payload variants.
    fn parses_token_usage_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "method": "thread/tokenUsage/updated",
            "params": {
                "total_token_usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15
                }
            }
        }));

        assert_eq!(
            event,
            CodexEvent::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15
            }
        );
    }

    #[test]
    // SPEC 17.5: rate-limit telemetry is extracted from the documented event envelope.
    fn parses_rate_limit_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "method": "codex/rateLimit",
            "params": { "remaining": 42 }
        }));

        assert_eq!(
            event,
            CodexEvent::RateLimit {
                payload: json!({ "remaining": 42 })
            }
        );
    }

    #[test]
    // SPEC 17.5: approval requests preserve response IDs and payloads for auto-approval.
    fn parses_approval_request_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "approval/required",
            "params": { "kind": "command_execution" }
        }));

        assert_eq!(
            event,
            CodexEvent::ApprovalRequest {
                id: json!(11),
                payload: json!({ "kind": "command_execution" })
            }
        );
    }

    #[test]
    // SPEC 17.5: tool calls accept both name and arguments payloads for rejection/handling.
    fn parses_tool_call_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tool/call",
            "params": {
                "name": "linear_graphql",
                "params": { "query": "query Test { viewer { id } }" }
            }
        }));

        assert_eq!(
            event,
            CodexEvent::ToolCall {
                id: json!(12),
                name: "linear_graphql".into(),
                params: json!({ "query": "query Test { viewer { id } }" })
            }
        );
    }

    #[test]
    // SPEC 17.5: user-input-required signals are surfaced explicitly to the runner.
    fn parses_user_input_required_event() {
        let event = parse_event(&json!({
            "jsonrpc": "2.0",
            "method": "user/inputRequired"
        }));

        assert_eq!(event, CodexEvent::UserInputRequired);
    }

    #[test]
    // SPEC 17.5: unknown events are ignored without losing the original payload.
    fn unknown_event_preserves_payload() {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "other/event",
            "params": { "value": 1 }
        });
        let event = parse_event(&payload);

        assert_eq!(event, CodexEvent::Unknown(payload));
    }
}
