#![deny(clippy::all)]
#![allow(
    clippy::assigning_clones,
    clippy::cast_precision_loss,
    clippy::default_trait_access,
    clippy::format_collect,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::missing_errors_doc,
    clippy::missing_fields_in_debug,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::needless_raw_string_hashes,
    clippy::redundant_closure_for_method_calls,
    clippy::single_match_else,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::unchecked_time_subtraction,
    clippy::unnecessary_wraps,
    clippy::unused_async
)]
// These targeted allowances keep pedantic lint noise from overwhelming the
// service code where signatures are constrained by serde, tokio, and internal APIs.

pub mod agent_runner;
pub mod codex;
pub mod config;
pub mod dashboard;
pub mod domain;
pub mod error;
pub mod http;
pub mod linear;
pub mod logging;
pub mod orchestrator;
pub mod prompt;
pub mod ssh;
pub mod tracker;
pub mod workflow;
pub mod workspace;
