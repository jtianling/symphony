use thiserror::Error;

#[derive(Debug, Error)]
pub enum SymphonyError {
    #[error("missing_workflow_file: {path}")]
    MissingWorkflowFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("workflow_parse_error: {message}")]
    WorkflowParseError {
        message: String,
        #[source]
        source: Option<serde_yaml::Error>,
    },
    #[error("workflow_front_matter_not_a_map")]
    WorkflowFrontMatterNotAMap,
    #[error("workflow load error: {0}")]
    WorkflowLoad(String),
    #[error("workflow watch error: {0}")]
    WorkflowWatch(String),
    #[error("config parse error: {0}")]
    ConfigParse(String),
    #[error("config validation error: {0}")]
    ConfigValidation(String),
    #[error("tracker error: {0}")]
    Tracker(String),
    #[error("workspace error: {0}")]
    Workspace(String),
    #[error("ssh error: {0}")]
    Ssh(String),
    #[error("prompt error: {0}")]
    Prompt(String),
    #[error("prompt render error: {0}")]
    PromptRender(String),
    #[error("codex error: {0}")]
    Codex(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("linear_api_request: {0}")]
    LinearApiRequest(String),
    #[error("linear_api_status: status={status}, body={body}")]
    LinearApiStatus { status: u16, body: String },
    #[error("linear_graphql_errors: {messages:?}")]
    LinearGraphqlErrors { messages: Vec<String> },
    #[error("linear_unknown_payload: {0}")]
    LinearUnknownPayload(String),
    #[error("linear_missing_end_cursor")]
    LinearMissingEndCursor,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}
