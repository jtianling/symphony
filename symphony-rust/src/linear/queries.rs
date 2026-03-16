pub const CANDIDATE_FETCH_QUERY: &str = r#"
query($projectSlug: String!, $states: [String!]!, $after: String) {
  issues(
    filter: {
      project: { slugId: { eq: $projectSlug } }
      state: { name: { in: $states } }
    }
    first: 50
    after: $after
    orderBy: createdAt
  ) {
    nodes {
      id
      identifier
      title
      description
      priority
      branchName
      url
      createdAt
      updatedAt
      state { name }
      assignee { id }
      labels { nodes { name } }
      inverseRelations(first: 50) {
        nodes {
          type
          issue {
            id
            identifier
            state { name }
          }
        }
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

pub const VIEWER_QUERY: &str = r#"
query SymphonyViewer {
  viewer {
    id
  }
}
"#;

pub const STATE_REFRESH_QUERY: &str = r#"
query($ids: [ID!]!) {
  nodes(ids: $ids) {
    ... on Issue {
      id
      identifier
      state { name }
    }
  }
}
"#;

pub const FETCH_BY_STATES_QUERY: &str = r#"
query($projectSlug: String!, $states: [String!]!, $after: String) {
  issues(
    filter: {
      project: { slugId: { eq: $projectSlug } }
      state: { name: { in: $states } }
    }
    first: 50
    after: $after
  ) {
    nodes {
      id
      identifier
      state { name }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

pub const CREATE_COMMENT_MUTATION: &str = r#"
mutation SymphonyCreateComment($issueId: String!, $body: String!) {
  commentCreate(input: {issueId: $issueId, body: $body}) {
    success
  }
}
"#;

pub const UPDATE_STATE_MUTATION: &str = r#"
mutation SymphonyUpdateIssueState($issueId: String!, $stateId: String!) {
  issueUpdate(id: $issueId, input: {stateId: $stateId}) {
    success
  }
}
"#;

pub const STATE_LOOKUP_QUERY: &str = r#"
query SymphonyResolveStateId($issueId: String!, $stateName: String!) {
  issue(id: $issueId) {
    team {
      states(filter: {name: {eq: $stateName}}, first: 1) {
        nodes {
          id
        }
      }
    }
  }
}
"#;
