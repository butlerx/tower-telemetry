//! Tokio runtime metrics, sampled once a second.
//!
//! Only metrics Tokio exposes without `tokio_unstable` are recorded; poll,
//! steal and local queue metrics need it.

use std::time::Duration;

use ::metrics::{counter, gauge};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio_metrics::{RuntimeMetrics, RuntimeMonitor};

const INTERVAL: Duration = Duration::from_secs(1);

/// Sample the current Tokio runtime once a second, for as long as it runs:
/// `tokio_runtime_workers`, `tokio_runtime_live_tasks`,
/// `tokio_runtime_global_queue_depth`, `tokio_runtime_worker_busy_ratio`
/// (mean, `_max` and `_min`), `tokio_runtime_worker_busy_microseconds_total`
/// and `tokio_runtime_worker_parks_total`.
///
/// # Panics
///
/// Panics when called outside a Tokio runtime.
#[must_use = "dropping the handle detaches the sampler; keep it to abort it"]
pub fn export_runtime_metrics() -> JoinHandle<()> {
    let monitor = RuntimeMonitor::new(&Handle::current());
    tokio::spawn(async move {
        let mut intervals = monitor.intervals();
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            match intervals.next() {
                Some(metrics) => record(&metrics),
                None => return,
            }
        }
    })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "gauges are f64; counts this large are not exact anyway"
)]
fn record(metrics: &RuntimeMetrics) {
    let elapsed = metrics.elapsed;
    gauge!("tokio_runtime_workers").set(metrics.workers_count as f64);
    gauge!("tokio_runtime_live_tasks").set(metrics.live_tasks_count as f64);
    gauge!("tokio_runtime_global_queue_depth").set(metrics.global_queue_depth as f64);
    gauge!("tokio_runtime_worker_busy_ratio").set(mean_worker_busy_ratio(
        metrics.total_busy_duration,
        elapsed,
        metrics.workers_count,
    ));
    gauge!("tokio_runtime_worker_busy_ratio_max")
        .set(duration_ratio(metrics.max_busy_duration, elapsed).clamp(0.0, 1.0));
    gauge!("tokio_runtime_worker_busy_ratio_min")
        .set(duration_ratio(metrics.min_busy_duration, elapsed).clamp(0.0, 1.0));
    // `metrics` counters are integers: microseconds keep sub-second busy
    // periods that whole seconds would drop.
    counter!("tokio_runtime_worker_busy_microseconds_total")
        .increment(u64::try_from(metrics.total_busy_duration.as_micros()).unwrap_or(u64::MAX));
    counter!("tokio_runtime_worker_parks_total").increment(metrics.total_park_count);
}

fn mean_worker_busy_ratio(total_busy: Duration, elapsed: Duration, workers: usize) -> f64 {
    let workers = u32::try_from(workers.max(1)).unwrap_or(u32::MAX);
    (duration_ratio(total_busy, elapsed) / f64::from(workers)).clamp(0.0, 1.0)
}

fn duration_ratio(duration: Duration, elapsed: Duration) -> f64 {
    if elapsed.is_zero() {
        0.0
    } else {
        duration.as_secs_f64() / elapsed.as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn no_elapsed_time_is_idle() {
        assert_close(duration_ratio(Duration::from_secs(1), Duration::ZERO), 0.0);
    }

    #[test]
    fn busy_ratio_is_averaged_over_workers() {
        assert_close(
            mean_worker_busy_ratio(Duration::from_millis(1_500), Duration::from_secs(1), 2),
            0.75,
        );
    }

    #[test]
    fn busy_ratio_is_clamped() {
        assert_close(
            mean_worker_busy_ratio(Duration::from_secs(5), Duration::from_secs(1), 1),
            1.0,
        );
    }

    #[tokio::test]
    async fn exporter_samples_until_aborted() {
        let handle = export_runtime_metrics();
        assert!(!handle.is_finished());
        handle.abort();
    }
}
