pub const ROLLING_WINDOW_MS: i64 = 600_000;

const SPARKLINE_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub fn compute_sparkline(samples: &[(i64, u64)], window_ms: i64, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }

    if samples.is_empty() {
        return String::from_iter(std::iter::repeat_n(SPARKLINE_BLOCKS[0], columns));
    }

    let end_ms = samples.last().map(|(timestamp, _)| *timestamp).unwrap_or(0);
    let start_ms = end_ms.saturating_sub(window_ms.max(1));
    let mut buckets = vec![0_u64; columns];
    let mut previous_total = 0_u64;

    for (index, (timestamp, total_tokens)) in samples.iter().enumerate() {
        if *timestamp < start_ms {
            previous_total = *total_tokens;
            continue;
        }

        let delta = if index == 0 {
            *total_tokens
        } else {
            total_tokens.saturating_sub(previous_total)
        };
        previous_total = *total_tokens;

        let offset_ms = timestamp.saturating_sub(start_ms);
        let bucket_index = usize::try_from(
            (offset_ms.saturating_mul(columns as i64) / window_ms.max(1))
                .min(columns.saturating_sub(1) as i64),
        )
        .unwrap_or(columns.saturating_sub(1));

        buckets[bucket_index] = buckets[bucket_index].saturating_add(delta);
    }

    buckets_to_sparkline(&buckets)
}

pub fn rolling_tps(samples: &[(i64, u64)], now_ms: i64, current_tokens: u64) -> f64 {
    let Some((first_ts, first_total)) = samples.first().copied() else {
        return 0.0;
    };

    let elapsed_ms = now_ms.saturating_sub(first_ts);
    if elapsed_ms <= 0 {
        return 0.0;
    }

    current_tokens.saturating_sub(first_total) as f64 / (elapsed_ms as f64 / 1000.0)
}

pub fn update_token_samples(
    samples: &[(i64, u64)],
    now_ms: i64,
    total_tokens: u64,
) -> Vec<(i64, u64)> {
    let min_timestamp = now_ms.saturating_sub(ROLLING_WINDOW_MS);
    let mut updated = samples
        .iter()
        .copied()
        .filter(|(timestamp, _)| *timestamp >= min_timestamp)
        .collect::<Vec<_>>();

    updated.push((now_ms, total_tokens));
    updated
}

fn buckets_to_sparkline(buckets: &[u64]) -> String {
    let Some(max_bucket) = buckets.iter().copied().max() else {
        return String::new();
    };

    if max_bucket == 0 {
        return String::from_iter(std::iter::repeat_n(SPARKLINE_BLOCKS[0], buckets.len()));
    }

    buckets
        .iter()
        .map(|bucket| {
            if *bucket == 0 {
                return SPARKLINE_BLOCKS[0];
            }

            let scale = ((*bucket as f64 / max_bucket as f64)
                * (SPARKLINE_BLOCKS.len() as f64 - 1.0))
                .round() as usize;

            SPARKLINE_BLOCKS[scale.min(SPARKLINE_BLOCKS.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{compute_sparkline, rolling_tps, update_token_samples, ROLLING_WINDOW_MS};

    #[test]
    fn compute_sparkline_handles_no_samples() {
        assert_eq!(compute_sparkline(&[], ROLLING_WINDOW_MS, 4), "▁▁▁▁");
    }

    #[test]
    fn compute_sparkline_handles_single_sample() {
        let sparkline = compute_sparkline(&[(1_000, 50)], ROLLING_WINDOW_MS, 4);

        assert_eq!(sparkline.chars().count(), 4);
        assert!(sparkline.ends_with('█'));
    }

    #[test]
    fn compute_sparkline_handles_multiple_samples_within_window() {
        let sparkline = compute_sparkline(
            &[(10_000, 10), (20_000, 30), (30_000, 90), (40_000, 120)],
            40_000,
            4,
        );

        assert_eq!(sparkline.chars().count(), 4);
        assert!(sparkline.contains('█'));
    }

    #[test]
    fn update_token_samples_prunes_samples_outside_window() {
        let samples = vec![(0, 10), (ROLLING_WINDOW_MS - 1, 20)];
        let updated = update_token_samples(&samples, ROLLING_WINDOW_MS + 10, 30);

        assert_eq!(
            updated,
            vec![(ROLLING_WINDOW_MS - 1, 20), (ROLLING_WINDOW_MS + 10, 30)]
        );
    }

    #[test]
    fn rolling_tps_uses_sample_window() {
        let samples = vec![(1_000, 100), (2_000, 150)];

        assert_eq!(rolling_tps(&samples, 3_000, 200), 50.0);
    }
}
