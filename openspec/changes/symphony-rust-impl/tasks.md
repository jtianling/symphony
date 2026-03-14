## 1. Project Scaffolding

- [x] 1.1 Create `symphony-rust/` directory with `Cargo.toml` including all dependencies (tokio, axum, reqwest, serde, serde_yaml, liquid, tracing, tracing-subscriber, notify, clap, thiserror, chrono, uuid)
- [x] 1.2 Create `src/main.rs` with minimal tokio runtime setup and clap CLI argument parsing (positional workflow path, optional `--port`)
- [x] 1.3 Create `src/lib.rs` with module declarations
- [x] 1.4 Create `src/error.rs` with top-level `SymphonyError` enum using thiserror
- [x] 1.5 Create `src/domain.rs` with core types: Issue, WorkflowDefinition, RunAttempt, LiveSession, RetryEntry, OrchestratorState

## 2. Workflow Loader and Config

- [x] 2.1 Create `src/workflow.rs` with WORKFLOW.md loader: front matter split, YAML parsing, error types (missing_workflow_file, workflow_parse_error, workflow_front_matter_not_a_map)
- [x] 2.2 Create `src/config.rs` with typed config structs (TrackerConfig, PollingConfig, WorkspaceConfig, HooksConfig, AgentConfig, CodexConfig, ServerConfig) with serde defaults
- [x] 2.3 Implement `$VAR` environment variable resolution for api_key and path fields
- [x] 2.4 Implement `~` home directory expansion for path values
- [x] 2.5 Implement per-state concurrency map normalization (lowercase keys, ignore invalid values)
- [x] 2.6 Implement dispatch preflight validation (tracker.kind, api_key, project_slug, codex.command)
- [x] 2.7 Implement filesystem watch on WORKFLOW.md using `notify` crate, sending reload messages to orchestrator
- [x] 2.8 Write tests for workflow loading, config parsing, defaults, env resolution, validation

## 3. Domain Model and Issue Types

- [x] 3.1 Define `Issue` struct with all normalized fields (id, identifier, title, description, priority, state, branch_name, url, labels, blocked_by, created_at, updated_at)
- [x] 3.2 Define `BlockerRef` struct (id, identifier, state)
- [x] 3.3 Define workspace key sanitization function (replace non `[A-Za-z0-9._-]` with `_`)
- [x] 3.4 Write tests for issue type conversions and workspace key sanitization

## 4. Linear Issue Tracker Client

- [x] 4.1 Create `src/linear/client.rs` with async reqwest-based GraphQL client (auth header, timeout 30s, endpoint config)
- [x] 4.2 Create `src/linear/queries.rs` with GraphQL query strings for: candidate fetch (project slugId filter, active states, pagination), state refresh by IDs (variable type `[ID!]`), fetch by states
- [x] 4.3 Create `src/linear/types.rs` with serde types for Linear GraphQL responses (nodes, pageInfo, edges)
- [x] 4.4 Create `src/linear/adapter.rs` with normalization: labels to lowercase, blockers from inverse `blocks` relations, priority int-or-null, ISO-8601 timestamps
- [x] 4.5 Implement cursor-based pagination with page size 50
- [x] 4.6 Implement error mapping: linear_api_request, linear_api_status, linear_graphql_errors, linear_unknown_payload, linear_missing_end_cursor
- [x] 4.7 Implement empty state list short-circuit (no API call)
- [x] 4.8 Write tests for normalization, pagination, error mapping

## 5. Prompt Builder

- [x] 5.1 Create `src/prompt.rs` with Liquid template rendering using strict mode
- [x] 5.2 Implement template context assembly: issue object (with nested labels/blockers) and attempt variable
- [x] 5.3 Implement fallback prompt for empty template body ("You are working on an issue from Linear.")
- [x] 5.4 Implement continuation turn prompt logic (first turn = full prompt, subsequent = continuation guidance)
- [x] 5.5 Write tests for rendering, strict unknown variable rejection, fallback prompt, continuation prompts

## 6. Workspace Manager

- [x] 6.1 Create `src/workspace.rs` with workspace path computation (root + sanitized identifier)
- [x] 6.2 Implement workspace creation (mkdir if not exists, track created_now flag)
- [x] 6.3 Implement workspace root containment validation (absolute path prefix check)
- [x] 6.4 Implement hook execution via `bash -lc` with configurable timeout (hooks.timeout_ms, default 60000)
- [x] 6.5 Implement hook lifecycle: after_create (fatal on fail), before_run (fatal on fail), after_run (ignore fail), before_remove (ignore fail)
- [x] 6.6 Implement workspace cleanup (before_remove hook + directory removal)
- [x] 6.7 Write tests for path computation, sanitization, containment validation, hook execution

## 7. Codex App-Server Client

- [x] 7.1 Create `src/codex/app_server.rs` with subprocess launch via `bash -lc <command>` using tokio::process
- [x] 7.2 Implement line-delimited JSON reading from stdout with BufReader, partial line buffering
- [x] 7.3 Implement stderr drain (log, do not parse as protocol)
- [x] 7.4 Implement startup handshake: initialize request/response, initialized notification, thread/start, turn/start
- [x] 7.5 Implement read_timeout_ms enforcement for request/response exchanges
- [x] 7.6 Implement turn_timeout_ms enforcement for streaming turn processing
- [x] 7.7 Create `src/codex/events.rs` with event parsing: turn/completed, turn/failed, turn/cancelled, token usage, rate limits, approvals, tool calls
- [x] 7.8 Implement approval auto-approve policy (command execution + file changes)
- [x] 7.9 Implement unsupported tool call rejection (return failure response, continue session)
- [x] 7.10 Implement user-input-required detection and hard failure
- [x] 7.11 Implement token extraction: prefer absolute thread totals, track deltas
- [x] 7.12 Create `src/codex/tools.rs` with optional linear_graphql tool handler
- [x] 7.13 Write tests for handshake, event parsing, timeout enforcement, approval handling

## 8. Agent Runner

- [x] 8.1 Create `src/agent_runner.rs` with worker task function: create workspace, build prompt, launch codex session
- [x] 8.2 Implement multi-turn loop: run turn, check issue state, continue or exit up to max_turns
- [x] 8.3 Implement codex update forwarding to orchestrator via message channel
- [x] 8.4 Implement error handling: workspace errors, hook errors, prompt errors, session errors
- [x] 8.5 Implement session cleanup (stop app-server process, run after_run hook)
- [x] 8.6 Write tests for worker lifecycle, multi-turn flow, error paths

## 9. Orchestrator

- [x] 9.1 Create `src/orchestrator.rs` with actor-style mpsc message loop
- [x] 9.2 Create `src/orchestrator/state.rs` with OrchestratorState struct (running, claimed, retry_attempts, completed, codex_totals, codex_rate_limits)
- [x] 9.3 Create `src/orchestrator/dispatch.rs` with candidate selection rules: active/not-terminal, not claimed, slots available, per-state limits, Todo blocker check
- [x] 9.4 Implement dispatch sort: priority asc (null last), created_at oldest, identifier lexicographic
- [x] 9.5 Implement global and per-state concurrency slot computation
- [x] 9.6 Create `src/orchestrator/reconciliation.rs` with stall detection and tracker state refresh
- [x] 9.7 Implement stall timeout: compute elapsed since last_codex_timestamp or started_at, terminate if > stall_timeout_ms, skip if <= 0
- [x] 9.8 Implement terminal/active/other state transition handling (terminate + cleanup / update / terminate)
- [x] 9.9 Create `src/orchestrator/retry.rs` with retry queue: cancel existing timer, compute backoff, schedule timer
- [x] 9.10 Implement backoff formula: continuation = 1000ms fixed, failure = min(10000 * 2^(attempt-1), max_retry_backoff_ms)
- [x] 9.11 Implement retry handler: fetch candidates, find issue, dispatch or requeue or release
- [x] 9.12 Implement poll tick sequence: reconcile, validate, fetch, sort, dispatch, notify
- [x] 9.13 Implement startup terminal workspace cleanup
- [x] 9.14 Implement worker exit handling: normal (continuation retry) and abnormal (backoff retry)
- [x] 9.15 Implement codex update message handling: update live session, tokens, rate limits
- [x] 9.16 Implement workflow reload message handling: update effective config
- [x] 9.17 Write tests for dispatch selection, sorting, concurrency, reconciliation, retry backoff, state transitions

## 10. HTTP Server (Optional Extension)

- [x] 10.1 Create `src/http/server.rs` with axum server setup, loopback bind, port from CLI/config
- [x] 10.2 Create `src/http/routes.rs` with route definitions: GET /, GET /api/v1/state, GET /api/v1/:identifier, POST /api/v1/refresh
- [x] 10.3 Implement GET /api/v1/state with snapshot from orchestrator state
- [x] 10.4 Implement GET /api/v1/:identifier with issue-specific detail or 404
- [x] 10.5 Implement POST /api/v1/refresh with poll trigger message to orchestrator
- [x] 10.6 Create `src/http/dashboard.rs` with server-rendered HTML dashboard
- [x] 10.7 Implement 405 for unsupported methods and JSON error envelope
- [x] 10.8 Write tests for API endpoints and error responses

## 11. CLI and Host Lifecycle

- [x] 11.1 Implement clap CLI with positional workflow path and optional `--port`
- [x] 11.2 Implement startup sequence: configure logging, validate config, start file watch, start orchestrator, optionally start HTTP server
- [x] 11.3 Implement SIGINT/SIGTERM signal handling with tokio::signal for graceful shutdown
- [x] 11.4 Implement exit codes: 0 for normal, non-zero for startup failure
- [x] 11.5 Write integration test for CLI startup and shutdown

## 12. Integration and Conformance

- [x] 12.1 Create end-to-end test with mock Linear API and mock codex app-server subprocess
- [x] 12.2 Verify all SPEC section 17 test matrix items are covered
- [x] 12.3 Run clippy with pedantic warnings, fix all issues
- [x] 12.4 Run `cargo fmt` and verify formatting
- [x] 12.5 Test with a real WORKFLOW.md from the Elixir implementation
