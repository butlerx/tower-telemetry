//! Tokio runtime metrics, sampled once a second.
//!
//! Only metrics Tokio exposes without `tokio_unstable` are recorded; poll,
//! steal and local queue metrics need it.

use std::time::Duration;

use ::metrics::{counter, gauge};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio_metrics::{RuntimeMetrics, RuntimeMonitor};

/// How often the runtime is sampled.
const INTERVAL: Duration = Duration::from_secs(1);

/// Sample the current Tokio runtime once a second, for as long as it runs,
/// and record what it's doing through the [`metrics`] facade.
///
/// Spawns a task on the current runtime and returns its handle. Call it
/// once per runtime, inside it, after installing your `metrics` exporter:
///
/// ```
/// use tower_telemetry::metrics::export_runtime_metrics;
///
/// #[tokio::main]
/// async fn main() {
///     // install a `metrics` exporter first
///     let sampler = export_runtime_metrics();
///     // ... serve until shutdown ...
///     sampler.abort();
/// }
/// ```
///
/// # Metrics
///
/// None have labels: add your service's and environment's as the exporter's
/// global labels. The first sample is taken a second after the call.
///
/// | Name                                          | Type    | What it is                                         |
/// | --------------------------------------------- | ------- | -------------------------------------------------- |
/// | `tokio_runtime_workers`                       | gauge   | worker threads                                     |
/// | `tokio_runtime_live_tasks`                    | gauge   | tasks spawned and not yet finished                 |
/// | `tokio_runtime_global_queue_depth`            | gauge   | tasks waiting in the global queue for a worker    |
/// | `tokio_runtime_worker_busy_ratio`             | gauge   | share of the last second the average worker was busy, 0 to 1 |
/// | `tokio_runtime_worker_busy_ratio_max`         | gauge   | the same for the busiest worker                    |
/// | `tokio_runtime_worker_busy_ratio_min`         | gauge   | the same for the least busy worker                 |
/// | `tokio_runtime_worker_busy_microseconds_total` | counter | time workers have spent busy, summed over workers |
/// | `tokio_runtime_worker_parks_total`            | counter | times workers have parked, with nothing to do     |
///
/// # Reading them
///
/// - **Saturation:** a mean busy ratio near 1 means the runtime has no
///   spare capacity; it's a good signal to autoscale on, better than CPU,
///   which also counts blocking threads and other processes.
/// - **Blocking:** one worker's `_max` pinned at 1 while the mean stays low
///   points to a task blocking its thread (synchronous I/O, a long
///   computation, a held lock).
/// - **Backlog:** a growing `global_queue_depth` means tasks are spawned
///   faster than workers pick them up.
/// - **Leaks:** `live_tasks` that only climbs points to tasks that never
///   finish.
///
/// Rates of the counters (`rate(...[1m])` in Prometheus) give busy time per
/// second, across all workers, and parks per second.
///
/// # Limits
///
/// Only metrics Tokio exposes on stable Rust are recorded. Poll counts,
/// steals, local queue depths and scheduling delays need the
/// `tokio_unstable` cfg and aren't sampled. Calling this twice on one
/// runtime double-counts the counters, and samplers on two runtimes write to
/// the same unlabelled series.
///
/// The task stops by itself when the runtime shuts down. Abort the handle
/// to stop it sooner; dropping it leaves the task running.
///
/// # Panics
///
/// Panics when called outside a Tokio runtime.
///
/// [`metrics`]: ::metrics
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
/// Record one sample of the runtime's metrics.
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

/// The share of `elapsed` the average worker was busy, from 0 to 1.
fn mean_worker_busy_ratio(total_busy: Duration, elapsed: Duration, workers: usize) -> f64 {
    let workers = u32::try_from(workers.max(1)).unwrap_or(u32::MAX);
    (duration_ratio(total_busy, elapsed) / f64::from(workers)).clamp(0.0, 1.0)
}

/// `duration` as a share of `elapsed`; 0 when no time elapsed.
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
