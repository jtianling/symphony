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
    pub assignee: Option<AssigneeNode>,
    pub labels: Option<LabelsConnection>,
    #[serde(rename = "inverseRelations")]
    pub inverse_relations: Option<InverseRelationsConnection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateNode {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssigneeNode {
    pub id: String,
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
pub struct InverseRelationsConnection {
    #[serde(default)]
    pub nodes: Vec<InverseRelationNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InverseRelationNode {
    #[serde(rename = "type")]
    pub relation_type: Option<String>,
    pub issue: Option<RelatedIssue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelatedIssue {
    pub id: String,
    pub identifier: String,
    pub state: Option<StateNode>,
}

#[derive(Debug, Deserialize)]
pub struct CommentCreateData {
    #[serde(rename = "commentCreate")]
    pub comment_create: MutationResult,
}

#[derive(Debug, Deserialize)]
pub struct IssueUpdateData {
    #[serde(rename = "issueUpdate")]
    pub issue_update: MutationResult,
}

#[derive(Debug, Deserialize)]
pub struct MutationResult {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct StateLookupData {
    pub issue: Option<StateLookupIssue>,
}

#[derive(Debug, Deserialize)]
pub struct StateLookupIssue {
    pub team: Option<StateLookupTeam>,
}

#[derive(Debug, Deserialize)]
pub struct StateLookupTeam {
    pub states: StateLookupConnection,
}

#[derive(Debug, Deserialize)]
pub struct StateLookupConnection {
    #[serde(default)]
    pub nodes: Vec<StateLookupNode>,
}

#[derive(Debug, Deserialize)]
pub struct StateLookupNode {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct ViewerData {
    pub viewer: Option<ViewerNode>,
}

#[derive(Debug, Deserialize)]
pub struct ViewerNode {
    pub id: String,
}
