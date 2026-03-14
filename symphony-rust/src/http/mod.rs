mod dashboard;
mod routes;
mod server;

use std::sync::{Arc, RwLock};

use chrono::Utc;
use tracing::warn;

use crate::orchestrator::{AggregateTokens, SnapshotCounts, StateSnapshot};

pub use dashboard::render_dashboard;
pub use routes::{create_router, AppState};
pub use server::HttpServer;

pub struct StateProvider {
    state: Arc<RwLock<StateSnapshot>>,
}

impl StateProvider {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(empty_snapshot())),
        }
    }

    pub fn update(&self, snapshot: StateSnapshot) {
        match self.state.write() {
            Ok(mut guard) => {
                *guard = snapshot;
            }
            Err(error) => {
                warn!("state provider write lock poisoned");
                *error.into_inner() = snapshot;
            }
        }
    }

    pub fn snapshot(&self) -> StateSnapshot {
        match self.state.read() {
            Ok(guard) => guard.clone(),
            Err(error) => {
                warn!("state provider read lock poisoned");
                error.into_inner().clone()
            }
        }
    }
}

impl Default for StateProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn empty_snapshot() -> StateSnapshot {
    StateSnapshot {
        generated_at: Utc::now(),
        counts: SnapshotCounts {
            running: 0,
            claimed: 0,
            completed: 0,
            retrying: 0,
        },
        running: Vec::new(),
        retrying: Vec::new(),
        codex_totals: AggregateTokens::default(),
        rate_limits: None,
    }
}
