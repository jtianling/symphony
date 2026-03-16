use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::config::TrackerConfig;
use crate::domain::{BlockerRef, Issue};
use crate::error::SymphonyError;

use super::adapter::{normalize_issue, normalize_issue_ref};
use super::queries::{
    CANDIDATE_FETCH_QUERY, CREATE_COMMENT_MUTATION, FETCH_BY_STATES_QUERY, STATE_LOOKUP_QUERY,
    STATE_REFRESH_QUERY, UPDATE_STATE_MUTATION, VIEWER_QUERY,
};
use super::types::{
    CommentCreateData, GraphQLResponse, IssueUpdateData, IssuesData, LinearIssue, NodesData,
    StateLookupData, ViewerData,
};

const DEFAULT_ENDPOINT: &str = "https://api.linear.app/graphql";
const REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct LinearClient {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl LinearClient {
    pub fn new(api_key: impl Into<String>) -> Result<Self, SymphonyError> {
        Self::with_endpoint(api_key, DEFAULT_ENDPOINT)
    }

    pub fn with_endpoint(
        api_key: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Result<Self, SymphonyError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|error| SymphonyError::LinearApiRequest(error.to_string()))?;

        Ok(Self {
            client,
            api_key: api_key.into(),
            endpoint: endpoint.into(),
        })
    }

    pub fn from_config(config: &TrackerConfig) -> Result<Self, SymphonyError> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| SymphonyError::ConfigValidation("tracker.api_key is required".into()))?;
        let endpoint = config
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_ENDPOINT);

        Self::with_endpoint(api_key.to_owned(), endpoint.to_owned())
    }

    pub async fn execute(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, SymphonyError> {
        let response = self
            .client
            .post(&self.endpoint)
            .header(reqwest::header::AUTHORIZATION, &self.api_key)
            .json(&json!({
                "query": query,
                "variables": variables,
            }))
            .send()
            .await
            .map_err(|error| SymphonyError::LinearApiRequest(error.to_string()))?;

        let status = response.status();

        if !status.is_success() {
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) => error.to_string(),
            };

            return Err(SymphonyError::LinearApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        response
            .json::<Value>()
            .await
            .map_err(|error| SymphonyError::LinearUnknownPayload(error.to_string()))
    }

    pub async fn fetch_candidates(
        &self,
        config: &TrackerConfig,
    ) -> Result<Vec<Issue>, SymphonyError> {
        if config.active_states.is_empty() {
            return Ok(Vec::new());
        }

        let project_slug = project_slug(config)?;
        let assignee_id = self.resolve_assignee_filter(config).await?;
        let issues = self
            .fetch_paginated_issues(project_slug, &config.active_states, CANDIDATE_FETCH_QUERY)
            .await?;
        let filtered_issues = filter_issues_by_assignee(issues, assignee_id.as_deref());

        Ok(filtered_issues.iter().map(normalize_issue).collect())
    }

    pub async fn fetch_issues_by_states(
        &self,
        project_slug: &str,
        states: &[String],
    ) -> Result<Vec<BlockerRef>, SymphonyError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }

        let issues = self
            .fetch_paginated_issues(project_slug, states, FETCH_BY_STATES_QUERY)
            .await?;

        Ok(issues.iter().map(normalize_issue_ref).collect())
    }

    pub async fn refresh_issue_states(
        &self,
        issue_ids: &[String],
    ) -> Result<Vec<BlockerRef>, SymphonyError> {
        if issue_ids.is_empty() {
            return Ok(Vec::new());
        }

        let data: NodesData = self
            .execute_typed(STATE_REFRESH_QUERY, json!({ "ids": issue_ids }))
            .await?;

        Ok(data
            .nodes
            .iter()
            .filter_map(|issue| issue.as_ref())
            .map(normalize_issue_ref)
            .collect())
    }

    pub async fn create_comment(&self, issue_id: &str, body: &str) -> Result<(), SymphonyError> {
        let data: CommentCreateData = self
            .execute_typed(
                CREATE_COMMENT_MUTATION,
                json!({
                    "issueId": issue_id,
                    "body": body,
                }),
            )
            .await?;

        if data.comment_create.success {
            return Ok(());
        }

        Err(SymphonyError::Tracker("comment_create_failed".into()))
    }

    pub async fn update_issue_state(
        &self,
        issue_id: &str,
        state_name: &str,
    ) -> Result<(), SymphonyError> {
        let state_id = self.resolve_state_id(issue_id, state_name).await?;
        let data: IssueUpdateData = self
            .execute_typed(
                UPDATE_STATE_MUTATION,
                json!({
                    "issueId": issue_id,
                    "stateId": state_id,
                }),
            )
            .await?;

        if data.issue_update.success {
            return Ok(());
        }

        Err(SymphonyError::Tracker("issue_update_failed".into()))
    }

    async fn execute_typed<T>(&self, query: &str, variables: Value) -> Result<T, SymphonyError>
    where
        T: DeserializeOwned,
    {
        let payload = self.execute(query, variables).await?;
        decode_graphql_response(payload)
    }

    async fn fetch_paginated_issues(
        &self,
        project_slug: &str,
        states: &[String],
        query: &str,
    ) -> Result<Vec<LinearIssue>, SymphonyError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }

        let mut issues = Vec::new();
        let mut after: Option<String> = None;

        loop {
            let data: IssuesData = self
                .execute_typed(
                    query,
                    json!({
                        "projectSlug": project_slug,
                        "states": states,
                        "after": after.clone(),
                    }),
                )
                .await?;

            let has_next_page = data.issues.page_info.has_next_page;
            let end_cursor = data.issues.page_info.end_cursor.clone();

            issues.extend(data.issues.nodes);

            if !has_next_page {
                break;
            }

            after = Some(end_cursor.ok_or(SymphonyError::LinearMissingEndCursor)?);
        }

        Ok(issues)
    }

    async fn resolve_state_id(
        &self,
        issue_id: &str,
        state_name: &str,
    ) -> Result<String, SymphonyError> {
        let data: StateLookupData = self
            .execute_typed(
                STATE_LOOKUP_QUERY,
                json!({
                    "issueId": issue_id,
                    "stateName": state_name,
                }),
            )
            .await?;

        data.issue
            .and_then(|issue| issue.team)
            .and_then(|team| team.states.nodes.into_iter().next())
            .map(|state| state.id)
            .ok_or_else(|| SymphonyError::Tracker("state_not_found".into()))
    }

    async fn resolve_assignee_filter(
        &self,
        config: &TrackerConfig,
    ) -> Result<Option<String>, SymphonyError> {
        let assignee = config
            .assignee
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        match assignee {
            None => Ok(None),
            Some("me") => self.fetch_viewer_id().await.map(Some),
            Some(value) => Ok(Some(value.to_owned())),
        }
    }

    async fn fetch_viewer_id(&self) -> Result<String, SymphonyError> {
        let data: ViewerData = self.execute_typed(VIEWER_QUERY, json!({})).await?;

        data.viewer
            .map(|viewer| viewer.id)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| SymphonyError::Tracker("viewer_not_found".into()))
    }
}

pub async fn fetch_candidates(config: &TrackerConfig) -> Result<Vec<Issue>, SymphonyError> {
    let client = LinearClient::from_config(config)?;
    client.fetch_candidates(config).await
}

pub async fn fetch_issues_by_states(
    config: &TrackerConfig,
    states: &[String],
) -> Result<Vec<BlockerRef>, SymphonyError> {
    if states.is_empty() {
        return Ok(Vec::new());
    }

    let client = LinearClient::from_config(config)?;
    client
        .fetch_issues_by_states(project_slug(config)?, states)
        .await
}

pub async fn refresh_issue_states(
    config: &TrackerConfig,
    issue_ids: &[String],
) -> Result<Vec<BlockerRef>, SymphonyError> {
    if issue_ids.is_empty() {
        return Ok(Vec::new());
    }

    let client = LinearClient::from_config(config)?;
    client.refresh_issue_states(issue_ids).await
}

fn project_slug(config: &TrackerConfig) -> Result<&str, SymphonyError> {
    config
        .project_slug
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SymphonyError::ConfigValidation("tracker.project_slug is required".into()))
}

fn decode_graphql_response<T>(payload: Value) -> Result<T, SymphonyError>
where
    T: DeserializeOwned,
{
    let response: GraphQLResponse<T> = serde_json::from_value(payload)
        .map_err(|error| SymphonyError::LinearUnknownPayload(error.to_string()))?;

    if let Some(errors) = response.errors.filter(|errors| !errors.is_empty()) {
        return Err(SymphonyError::LinearGraphqlErrors {
            messages: errors.into_iter().map(|error| error.message).collect(),
        });
    }

    response
        .data
        .ok_or_else(|| SymphonyError::LinearUnknownPayload("missing GraphQL data".into()))
}

fn filter_issues_by_assignee(
    issues: Vec<LinearIssue>,
    assignee_id: Option<&str>,
) -> Vec<LinearIssue> {
    let Some(assignee_id) = assignee_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return issues;
    };

    issues
        .into_iter()
        .filter(|issue| {
            issue
                .assignee
                .as_ref()
                .map(|assignee| assignee.id.trim() == assignee_id)
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use wiremock::matchers::{body_partial_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{fetch_issues_by_states, LinearClient};
    use crate::config::TrackerConfig;
    use crate::error::SymphonyError;
    use crate::linear::queries::{
        CANDIDATE_FETCH_QUERY, CREATE_COMMENT_MUTATION, FETCH_BY_STATES_QUERY, STATE_LOOKUP_QUERY,
        UPDATE_STATE_MUTATION, VIEWER_QUERY,
    };
    use crate::linear::types::IssuesData;

    fn candidate_issue_json(id: &str, identifier: &str, assignee: Option<&str>) -> Value {
        json!({
            "id": id,
            "identifier": identifier,
            "title": identifier,
            "description": null,
            "priority": null,
            "branchName": null,
            "url": null,
            "createdAt": null,
            "updatedAt": null,
            "state": { "name": "Todo" },
            "assignee": assignee.map(|value| json!({ "id": value })).unwrap_or(Value::Null),
            "labels": { "nodes": [] },
            "inverseRelations": { "nodes": [] }
        })
    }

    #[tokio::test]
    // SPEC 17.3: empty state filters return without issuing a Linear API call.
    async fn fetch_issues_by_states_short_circuits_on_empty_states() {
        let config = TrackerConfig::default();

        let result = fetch_issues_by_states(&config, &[]).await;

        match result {
            Ok(issues) => assert!(issues.is_empty()),
            Err(error) => panic!("expected empty result, got {error}"),
        }
    }

    #[tokio::test]
    // SPEC 17.3: request transport failures map to the typed Linear request error.
    async fn maps_request_errors() -> Result<(), SymphonyError> {
        let client = LinearClient::with_endpoint("linear-key", "http://127.0.0.1:9/graphql")?;
        let error = match client.execute("query { viewer { id } }", json!({})).await {
            Ok(_) => panic!("expected request failure"),
            Err(error) => error,
        };

        assert!(matches!(error, SymphonyError::LinearApiRequest(_)));

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.3: non-200 responses map to the typed Linear status error.
    async fn maps_status_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(header("authorization", "linear-key"))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream failure"))
            .mount(&server)
            .await;

        let error = match client.execute("query { viewer { id } }", json!({})).await {
            Ok(_) => panic!("expected status failure"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::LinearApiStatus { status: 500, .. }
        ));

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.3: GraphQL `errors` payloads map to the typed Linear GraphQL error.
    async fn maps_graphql_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{ "message": "bad query" }]
            })))
            .mount(&server)
            .await;

        let error = match client
            .fetch_issues_by_states("project", &[String::from("Todo")])
            .await
        {
            Ok(_) => panic!("expected graphql failure"),
            Err(error) => error,
        };

        assert!(matches!(error, SymphonyError::LinearGraphqlErrors { .. }));

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.3: malformed GraphQL payloads map to the typed unknown-payload error.
    async fn maps_unknown_payload_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "issues": { "unexpected": true } }
            })))
            .mount(&server)
            .await;

        let error = match client
            .execute_typed::<IssuesData>(
                FETCH_BY_STATES_QUERY,
                json!({
                    "projectSlug": "project",
                    "states": ["Todo"],
                    "after": null,
                }),
            )
            .await
        {
            Ok(_) => panic!("expected payload failure"),
            Err(error) => error,
        };

        assert!(matches!(error, SymphonyError::LinearUnknownPayload(_)));

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.3: pagination requires `endCursor` when `hasNextPage` is true.
    async fn maps_missing_end_cursor_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issues": {
                        "nodes": [],
                        "pageInfo": {
                            "hasNextPage": true,
                            "endCursor": null
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let error = match client
            .fetch_issues_by_states("project", &[String::from("Todo")])
            .await
        {
            Ok(_) => panic!("expected missing cursor failure"),
            Err(error) => error,
        };

        assert!(matches!(error, SymphonyError::LinearMissingEndCursor));

        Ok(())
    }

    #[tokio::test]
    // SPEC 17.3: paginated fetches preserve issue order across pages and use `projectSlug`.
    async fn paginates_issue_queries() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(header("authorization", "linear-key"))
            .and(body_partial_json(json!({
                "variables": {
                    "projectSlug": "project",
                    "states": ["Todo"],
                    "after": null
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issues": {
                        "nodes": [
                            {
                                "id": "1",
                                "identifier": "SYM-1",
                                "state": { "name": "Todo" }
                            }
                        ],
                        "pageInfo": {
                            "hasNextPage": true,
                            "endCursor": "cursor-1"
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(header("authorization", "linear-key"))
            .and(body_partial_json(json!({
                "variables": {
                    "projectSlug": "project",
                    "states": ["Todo"],
                    "after": "cursor-1"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issues": {
                        "nodes": [
                            {
                                "id": "2",
                                "identifier": "SYM-2",
                                "state": { "name": "In Progress" }
                            }
                        ],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let issues = client
            .fetch_issues_by_states("project", &[String::from("Todo")])
            .await?;

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "SYM-1");
        assert_eq!(issues[1].identifier, "SYM-2");

        Ok(())
    }

    #[tokio::test]
    async fn fetch_candidates_returns_all_issues_without_assignee_filter(
    ) -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;
        let config = TrackerConfig {
            project_slug: Some("project".into()),
            active_states: vec!["Todo".into()],
            ..TrackerConfig::default()
        };

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": CANDIDATE_FETCH_QUERY,
                "variables": {
                    "projectSlug": "project",
                    "states": ["Todo"],
                    "after": null
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issues": {
                        "nodes": [
                            candidate_issue_json("1", "SYM-1", Some("user-1")),
                            candidate_issue_json("2", "SYM-2", None)
                        ],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let issues = client.fetch_candidates(&config).await?;

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].identifier, "SYM-1");
        assert_eq!(issues[1].identifier, "SYM-2");

        Ok(())
    }

    #[tokio::test]
    async fn fetch_candidates_filters_by_explicit_assignee_id() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;
        let config = TrackerConfig {
            project_slug: Some("project".into()),
            assignee: Some("user-1".into()),
            active_states: vec!["Todo".into()],
            ..TrackerConfig::default()
        };

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": CANDIDATE_FETCH_QUERY,
                "variables": {
                    "projectSlug": "project",
                    "states": ["Todo"],
                    "after": null
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issues": {
                        "nodes": [
                            candidate_issue_json("1", "SYM-1", Some("user-1")),
                            candidate_issue_json("2", "SYM-2", Some("user-2")),
                            candidate_issue_json("3", "SYM-3", None)
                        ],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let issues = client.fetch_candidates(&config).await?;

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "SYM-1");

        Ok(())
    }

    #[tokio::test]
    async fn fetch_candidates_resolves_me_to_viewer_id() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;
        let config = TrackerConfig {
            project_slug: Some("project".into()),
            assignee: Some("me".into()),
            active_states: vec!["Todo".into()],
            ..TrackerConfig::default()
        };

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": VIEWER_QUERY,
                "variables": {}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "viewer": {
                        "id": "viewer-1"
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": CANDIDATE_FETCH_QUERY,
                "variables": {
                    "projectSlug": "project",
                    "states": ["Todo"],
                    "after": null
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issues": {
                        "nodes": [
                            candidate_issue_json("1", "SYM-1", Some("viewer-1")),
                            candidate_issue_json("2", "SYM-2", Some("user-2")),
                            candidate_issue_json("3", "SYM-3", None)
                        ],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let issues = client.fetch_candidates(&config).await?;

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "SYM-1");

        Ok(())
    }

    #[tokio::test]
    async fn fetch_candidates_propagates_viewer_query_failures() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;
        let config = TrackerConfig {
            project_slug: Some("project".into()),
            assignee: Some("me".into()),
            active_states: vec!["Todo".into()],
            ..TrackerConfig::default()
        };

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": VIEWER_QUERY,
                "variables": {}
            })))
            .respond_with(ResponseTemplate::new(500).set_body_string("viewer failed"))
            .mount(&server)
            .await;

        let error = match client.fetch_candidates(&config).await {
            Ok(_) => panic!("expected viewer query failure"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::LinearApiStatus { status: 500, .. }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn create_comment_succeeds() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(header("authorization", "linear-key"))
            .and(body_partial_json(json!({
                "query": CREATE_COMMENT_MUTATION,
                "variables": {
                    "issueId": "issue-1",
                    "body": "hello"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "commentCreate": {
                        "success": true
                    }
                }
            })))
            .mount(&server)
            .await;

        client.create_comment("issue-1", "hello").await
    }

    #[tokio::test]
    async fn create_comment_propagates_api_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;

        let error = match client.create_comment("issue-1", "hello").await {
            Ok(_) => panic!("expected api failure"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::LinearApiStatus { status: 502, .. }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn create_comment_propagates_graphql_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{ "message": "comment failed" }]
            })))
            .mount(&server)
            .await;

        let error = match client.create_comment("issue-1", "hello").await {
            Ok(_) => panic!("expected graphql failure"),
            Err(error) => error,
        };

        assert!(matches!(error, SymphonyError::LinearGraphqlErrors { .. }));

        Ok(())
    }

    #[tokio::test]
    async fn create_comment_rejects_unsuccessful_mutation() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "commentCreate": {
                        "success": false
                    }
                }
            })))
            .mount(&server)
            .await;

        let error = match client.create_comment("issue-1", "hello").await {
            Ok(_) => panic!("expected unsuccessful mutation"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::Tracker(ref message) if message == "comment_create_failed"
        ));

        Ok(())
    }

    #[tokio::test]
    async fn update_issue_state_succeeds() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": STATE_LOOKUP_QUERY,
                "variables": {
                    "issueId": "issue-1",
                    "stateName": "Done"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issue": {
                        "team": {
                            "states": {
                                "nodes": [
                                    { "id": "state-1" }
                                ]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": UPDATE_STATE_MUTATION,
                "variables": {
                    "issueId": "issue-1",
                    "stateId": "state-1"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issueUpdate": {
                        "success": true
                    }
                }
            })))
            .mount(&server)
            .await;

        client.update_issue_state("issue-1", "Done").await
    }

    #[tokio::test]
    async fn update_issue_state_errors_when_state_is_missing() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": STATE_LOOKUP_QUERY,
                "variables": {
                    "issueId": "issue-1",
                    "stateName": "Missing"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issue": {
                        "team": {
                            "states": {
                                "nodes": []
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let error = match client.update_issue_state("issue-1", "Missing").await {
            Ok(_) => panic!("expected missing state failure"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::Tracker(ref message) if message == "state_not_found"
        ));

        Ok(())
    }

    #[tokio::test]
    async fn update_issue_state_propagates_api_errors() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": STATE_LOOKUP_QUERY,
                "variables": {
                    "issueId": "issue-1",
                    "stateName": "Done"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issue": {
                        "team": {
                            "states": {
                                "nodes": [
                                    { "id": "state-1" }
                                ]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": UPDATE_STATE_MUTATION,
                "variables": {
                    "issueId": "issue-1",
                    "stateId": "state-1"
                }
            })))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        let error = match client.update_issue_state("issue-1", "Done").await {
            Ok(_) => panic!("expected api failure"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::LinearApiStatus { status: 503, .. }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn update_issue_state_rejects_unsuccessful_mutation() -> Result<(), SymphonyError> {
        let server = MockServer::start().await;
        let client = LinearClient::with_endpoint("linear-key", server.uri())?;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": STATE_LOOKUP_QUERY,
                "variables": {
                    "issueId": "issue-1",
                    "stateName": "Done"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issue": {
                        "team": {
                            "states": {
                                "nodes": [
                                    { "id": "state-1" }
                                ]
                            }
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "query": UPDATE_STATE_MUTATION,
                "variables": {
                    "issueId": "issue-1",
                    "stateId": "state-1"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "issueUpdate": {
                        "success": false
                    }
                }
            })))
            .mount(&server)
            .await;

        let error = match client.update_issue_state("issue-1", "Done").await {
            Ok(_) => panic!("expected unsuccessful mutation"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            SymphonyError::Tracker(ref message) if message == "issue_update_failed"
        ));

        Ok(())
    }
}
