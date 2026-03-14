use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::debug;

use crate::agent_runner::WorkerOutcome;

use super::OrchestratorMsg;

const CONTINUATION_DELAY_MS: u64 = 1_000;
const FAILURE_BASE_DELAY_MS: u64 = 10_000;

#[derive(Debug, Default)]
pub struct RetryQueue {
    entries: HashMap<String, RetryTimer>,
}

#[derive(Debug)]
struct RetryTimer {
    issue_id: String,
    attempt: u32,
    fire_at: Instant,
    handle: tokio::task::JoinHandle<()>,
}

impl RetryQueue {
    pub fn schedule(
        &mut self,
        issue_id: &str,
        attempt: u32,
        delay: Duration,
        msg_tx: mpsc::Sender<OrchestratorMsg>,
    ) {
        self.cancel(issue_id);

        let issue_id_owned = issue_id.to_owned();
        let issue_id_for_task = issue_id_owned.clone();
        let fire_at = Instant::now() + delay;
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = msg_tx
                .send(OrchestratorMsg::RetryTimer {
                    issue_id: issue_id_for_task,
                })
                .await;
        });

        self.entries.insert(
            issue_id_owned.clone(),
            RetryTimer {
                issue_id: issue_id_owned,
                attempt,
                fire_at,
                handle,
            },
        );
    }

    pub fn cancel(&mut self, issue_id: &str) {
        if let Some(timer) = self.entries.remove(issue_id) {
            debug!(
                issue_id = %timer.issue_id,
                attempt = timer.attempt,
                "cancelled retry timer"
            );
            timer.handle.abort();
        }
    }

    pub fn clear(&mut self) {
        for (_, timer) in self.entries.drain() {
            debug!(
                issue_id = %timer.issue_id,
                attempt = timer.attempt,
                "clearing retry timer"
            );
            timer.handle.abort();
        }
    }

    pub fn attempt(&self, issue_id: &str) -> Option<u32> {
        self.entries.get(issue_id).map(|entry| entry.attempt)
    }

    pub fn fire_at(&self, issue_id: &str) -> Option<Instant> {
        self.entries.get(issue_id).map(|entry| entry.fire_at)
    }

    pub fn contains(&self, issue_id: &str) -> bool {
        self.entries.contains_key(issue_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn remove(&mut self, issue_id: &str) -> Option<(u32, Instant)> {
        self.entries.remove(issue_id).map(|entry| {
            entry.handle.abort();
            (entry.attempt, entry.fire_at)
        })
    }
}

pub fn compute_retry_delay(outcome: &WorkerOutcome, attempt: u32, max_backoff_ms: u64) -> Duration {
    match outcome {
        WorkerOutcome::Normal => Duration::from_millis(CONTINUATION_DELAY_MS),
        WorkerOutcome::Failure(_) => {
            let capped_attempt = attempt.max(1).saturating_sub(1);
            let multiplier = 1_u64
                .checked_shl(capped_attempt.min(31))
                .unwrap_or(u64::MAX);
            let delay_ms = FAILURE_BASE_DELAY_MS.saturating_mul(multiplier);
            Duration::from_millis(delay_ms.min(max_backoff_ms))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::compute_retry_delay;
    use crate::agent_runner::WorkerOutcome;

    #[test]
    // SPEC 17.4: normal worker exits schedule the fixed continuation retry delay.
    fn compute_retry_delay_returns_fixed_continuation_delay() {
        let delay = compute_retry_delay(&WorkerOutcome::Normal, 99, 300_000);

        assert_eq!(delay, Duration::from_millis(1_000));
    }

    #[test]
    // SPEC 17.4: failure retries use 10s-based exponential backoff.
    fn compute_retry_delay_grows_exponentially_for_failures() {
        let delay = compute_retry_delay(&WorkerOutcome::Failure("boom".into()), 3, 300_000);

        assert_eq!(delay, Duration::from_millis(40_000));
    }

    #[test]
    // SPEC 17.4: retry backoff is capped by `agent.max_retry_backoff_ms`.
    fn compute_retry_delay_caps_at_max_backoff() {
        let delay = compute_retry_delay(&WorkerOutcome::Failure("boom".into()), 10, 30_000);

        assert_eq!(delay, Duration::from_millis(30_000));
    }
}
