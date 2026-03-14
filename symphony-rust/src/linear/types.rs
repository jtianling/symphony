use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GraphQLResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQLError {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct IssuesData {
    pub issues: IssuesConnection,
}

#[derive(Debug, Deserialize)]
pub struct IssuesConnection {
    #[serde(default)]
    pub nodes: Vec<LinearIssue>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
pub struct PageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NodesData {
    #[serde(default)]
    pub nodes: Vec<Option<LinearIssue>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinearIssue {
    pub id: String,
    pub identifier: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<serde_json::Value>,
    #[serde(rename = "branchName")]
    pub branch_name: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
    pub state: Option<StateNode>,
    pub labels: Option<LabelsConnection>,
    pub relations: Option<RelationsConnection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateNode {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelsConnection {
    #[serde(default)]
    pub nodes: Vec<LabelNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelNode {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelationsConnection {
    #[serde(default)]
    pub nodes: Vec<RelationNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelationNode {
    #[serde(rename = "relatedIssue")]
    pub related_issue: Option<RelatedIssue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelatedIssue {
    pub id: String,
    pub identifier: String,
    pub state: Option<StateNode>,
}
