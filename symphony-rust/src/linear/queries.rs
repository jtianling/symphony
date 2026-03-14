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
      labels { nodes { name } }
      relations(filter: { type: { eq: "blocks" } }) {
        nodes {
          relatedIssue {
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
