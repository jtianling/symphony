mod dashboard;
mod routes;
mod server;

use std::sync::{Arc, RwLock};

use chrono::Utc;
use tokio::sync::{broadcast, watch};
use tracing::warn;

use crate::orchestrator::{AggregateTokens, SnapshotCounts, StateSnapshot};

pub use dashboard::render_dashboard;
pub use routes::{create_router, AppState};
pub use server::HttpServer;

pub struct StateProvider {
    state: Arc<RwLock<StateSnapshot>>,
    watch_tx: watch::Sender<()>,
    broadcast_tx: broadcast::Sender<StateSnapshot>,
}

impl StateProvider {
    pub fn new() -> Self {
        let (watch_tx, _) = watch::channel(());
        let (broadcast_tx, _) = broadcast::channel(32);

        Self {
            state: Arc::new(RwLock::new(empty_snapshot())),
            watch_tx,
            broadcast_tx,
        }
    }

    pub fn update(&self, snapshot: StateSnapshot) {
        let event_snapshot = snapshot.clone();

        match self.state.write() {
            Ok(mut guard) => {
                *guard = snapshot;
            }
            Err(error) => {
                warn!("state provider write lock poisoned");
                *error.into_inner() = snapshot;
            }
        }

        self.watch_tx.send_replace(());
        let _ = self.broadcast_tx.send(event_snapshot);
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

    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.watch_tx.subscribe()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<StateSnapshot> {
        self.broadcast_tx.subscribe()
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
