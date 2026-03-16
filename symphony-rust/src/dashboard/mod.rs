mod render;
mod sparkline;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Instant};

use crate::config::SymphonyConfig;
use crate::http::StateProvider;

pub use render::render_panel;
pub use sparkline::{compute_sparkline, rolling_tps, update_token_samples};

const SPARKLINE_COLUMNS: usize = 48;

pub fn spawn_dashboard(
    state_provider: Arc<StateProvider>,
    mut watch_rx: watch::Receiver<()>,
    config: SymphonyConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut writer = BufWriter::new(tokio::io::stdout());
        let started_at = Instant::now();
        let mut gate = RenderThrottle::new(config.observability.render_interval_ms);
        let mut token_samples = Vec::new();
        let max_agents = config.agent.max_concurrent_agents;

        if gate.request(0) {
            let _ = render_once(
                &mut writer,
                state_provider.as_ref(),
                &mut token_samples,
                0,
                max_agents,
            )
            .await;
        }

        loop {
            match gate.next_render_at_ms() {
                Some(deadline_ms) => {
                    let deadline =
                        started_at + Duration::from_millis(u64::try_from(deadline_ms).unwrap_or(0));

                    tokio::select! {
                        result = watch_rx.changed() => {
                            if result.is_err() {
                                break;
                            }

                            let now_ms = elapsed_ms(started_at);
                            if gate.request(now_ms)
                                && render_once(
                                    &mut writer,
                                    state_provider.as_ref(),
                                    &mut token_samples,
                                    now_ms,
                                    max_agents,
                                )
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        _ = sleep_until(deadline) => {
                            let now_ms = elapsed_ms(started_at);
                            if gate.flush_if_ready(now_ms)
                                && render_once(
                                    &mut writer,
                                    state_provider.as_ref(),
                                    &mut token_samples,
                                    now_ms,
                                    max_agents,
                                )
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                None => {
                    if watch_rx.changed().await.is_err() {
                        break;
                    }

                    let now_ms = elapsed_ms(started_at);
                    if gate.request(now_ms)
                        && render_once(
                            &mut writer,
                            state_provider.as_ref(),
                            &mut token_samples,
                            now_ms,
                            max_agents,
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    })
}

async fn render_once(
    writer: &mut BufWriter<tokio::io::Stdout>,
    state_provider: &StateProvider,
    token_samples: &mut Vec<(i64, u64)>,
    now_ms: i64,
    max_agents: u32,
) -> std::io::Result<()> {
    let snapshot = state_provider.snapshot();
    let updated_samples =
        sparkline::update_token_samples(token_samples, now_ms, snapshot.codex_totals.total_tokens);
    *token_samples = updated_samples;
    let sparkline = sparkline::compute_sparkline(
        token_samples,
        sparkline::ROLLING_WINDOW_MS,
        SPARKLINE_COLUMNS,
    );
    let tps = sparkline::rolling_tps(token_samples, now_ms, snapshot.codex_totals.total_tokens);
    let panel = render::render_panel_with_context(&snapshot, tps, &sparkline, max_agents);

    writer.write_all(b"\x1b[H\x1b[2J").await?;
    writer.write_all(panel.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

fn elapsed_ms(started_at: Instant) -> i64 {
    i64::try_from(started_at.elapsed().as_millis()).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone)]
struct RenderThrottle {
    interval_ms: i64,
    last_render_at_ms: Option<i64>,
    next_render_at_ms: Option<i64>,
    pending: bool,
}

impl RenderThrottle {
    fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms: i64::try_from(interval_ms.max(1)).unwrap_or(i64::MAX),
            last_render_at_ms: None,
            next_render_at_ms: None,
            pending: false,
        }
    }

    fn request(&mut self, now_ms: i64) -> bool {
        match self.last_render_at_ms {
            None => {
                self.last_render_at_ms = Some(now_ms);
                self.pending = false;
                self.next_render_at_ms = None;
                true
            }
            Some(last_render_at_ms)
                if now_ms.saturating_sub(last_render_at_ms) >= self.interval_ms =>
            {
                self.last_render_at_ms = Some(now_ms);
                self.pending = false;
                self.next_render_at_ms = None;
                true
            }
            Some(last_render_at_ms) => {
                self.pending = true;
                self.next_render_at_ms = Some(last_render_at_ms.saturating_add(self.interval_ms));
                false
            }
        }
    }

    fn next_render_at_ms(&self) -> Option<i64> {
        self.next_render_at_ms
    }

    fn flush_if_ready(&mut self, now_ms: i64) -> bool {
        let Some(deadline_ms) = self.next_render_at_ms else {
            return false;
        };

        if !self.pending || now_ms < deadline_ms {
            return false;
        }

        self.pending = false;
        self.next_render_at_ms = None;
        self.last_render_at_ms = Some(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::RenderThrottle;

    #[test]
    fn rapid_updates_are_coalesced() {
        let mut throttle = RenderThrottle::new(16);

        assert!(throttle.request(0));
        assert!(!throttle.request(5));
        assert_eq!(throttle.next_render_at_ms(), Some(16));
        assert!(!throttle.request(10));
        assert_eq!(throttle.next_render_at_ms(), Some(16));
        assert!(throttle.flush_if_ready(16));
        assert_eq!(throttle.next_render_at_ms(), None);
    }
}
