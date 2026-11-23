//! Tokio runtime metrics, sampled once a second.
//!
//! Built with `--cfg tokio_unstable`, poll, blocking pool and local queue
//! metrics are recorded as well.

use std::time::Duration;

use ::metrics::{Unit, counter, describe_counter, describe_gauge, gauge};
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
/// Built with `RUSTFLAGS="--cfg tokio_unstable"`, these are recorded too:
///
/// | Name                                              | Type    | What it is                                         |
/// | ------------------------------------------------- | ------- | -------------------------------------------------- |
/// | `tokio_runtime_poll_duration_mean_seconds`        | gauge   | moving average of one task poll, over all workers  |
/// | `tokio_runtime_poll_duration_mean_max_seconds`    | gauge   | the same for the worker with the slowest polls     |
/// | `tokio_runtime_polls_total`                       | counter | task polls, summed over workers                    |
/// | `tokio_runtime_steals_total`                      | counter | tasks idle workers took from busy workers' queues  |
/// | `tokio_runtime_local_queue_depth`                 | gauge   | tasks waiting in workers' local queues, summed     |
/// | `tokio_runtime_local_queue_depth_max`             | gauge   | the same for the worker with the longest queue     |
/// | `tokio_runtime_blocking_threads`                  | gauge   | threads in the `spawn_blocking` pool               |
/// | `tokio_runtime_blocking_threads_idle`             | gauge   | of those, threads with nothing to do               |
/// | `tokio_runtime_blocking_queue_depth`              | gauge   | `spawn_blocking` tasks waiting for a thread        |
///
/// # Units
///
/// As across this crate: durations in seconds, ratios from 0 to 1, and
/// counts. The exception is `tokio_runtime_worker_busy_microseconds_total`:
/// `metrics` counters are integers, and whole seconds would drop the short
/// busy periods that make up most of a worker's time.
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
/// With `tokio_unstable`:
///
/// - **Slow polls:** a poll should take microseconds. A mean in the
///   milliseconds, or a `_max` far above the mean, points to a task doing
///   synchronous work between `.await`s.
/// - **Blocking pool saturation:** a `blocking_queue_depth` above 0 means
///   `spawn_blocking` work (file I/O, DNS, hashing) is waiting for a thread;
///   `blocking_threads` at the pool's limit with no idle threads confirms it.
/// - **Uneven load:** `local_queue_depth_max` well above
///   `local_queue_depth / workers` means one worker has a backlog the others
///   aren't stealing. A high `rate(tokio_runtime_steals_total[1m])` means
///   work keeps landing on a few workers and the rest are rebalancing it.
///
/// Rates of the counters (`rate(...[1m])` in Prometheus) give busy time per
/// second, across all workers, and parks per second.
///
/// # Limits
///
/// Without `tokio_unstable`, only metrics Tokio exposes on stable Rust are
/// recorded. Poll counts,
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
    describe();
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

/// Describe each metric's unit and meaning to the installed recorder, for
/// exporters that publish them (Prometheus `# HELP` and `# UNIT` lines).
fn describe() {
    describe_gauge!(
        "tokio_runtime_workers",
        Unit::Count,
        "Tokio runtime worker threads."
    );
    describe_gauge!(
        "tokio_runtime_live_tasks",
        Unit::Count,
        "Tokio tasks spawned and not yet finished."
    );
    describe_gauge!(
        "tokio_runtime_global_queue_depth",
        Unit::Count,
        "Tokio tasks waiting in the global queue for a worker."
    );
    describe_gauge!(
        "tokio_runtime_worker_busy_ratio",
        "Share of the last second the average Tokio worker was busy, from 0 to 1."
    );
    describe_gauge!(
        "tokio_runtime_worker_busy_ratio_max",
        "Share of the last second the busiest Tokio worker was busy, from 0 to 1."
    );
    describe_gauge!(
        "tokio_runtime_worker_busy_ratio_min",
        "Share of the last second the least busy Tokio worker was busy, from 0 to 1."
    );
    describe_counter!(
        "tokio_runtime_worker_busy_microseconds_total",
        Unit::Microseconds,
        "Time Tokio workers have spent busy, summed over workers."
    );
    describe_counter!(
        "tokio_runtime_worker_parks_total",
        Unit::Count,
        "Times Tokio workers have parked with nothing to do."
    );
    #[cfg(tokio_unstable)]
    describe_unstable();
}

#[cfg(tokio_unstable)]
/// Describe the metrics Tokio only exposes with `--cfg tokio_unstable`.
fn describe_unstable() {
    describe_gauge!(
        "tokio_runtime_poll_duration_mean_seconds",
        Unit::Seconds,
        "Moving average of one Tokio task poll, over all workers."
    );
    describe_gauge!(
        "tokio_runtime_poll_duration_mean_max_seconds",
        Unit::Seconds,
        "Moving average of one Tokio task poll, on the worker with the slowest polls."
    );
    describe_counter!(
        "tokio_runtime_polls_total",
        Unit::Count,
        "Tokio task polls, summed over workers."
    );
    describe_counter!(
        "tokio_runtime_steals_total",
        Unit::Count,
        "Tokio tasks idle workers took from other workers' queues."
    );
    describe_gauge!(
        "tokio_runtime_local_queue_depth",
        Unit::Count,
        "Tokio tasks waiting in workers' local queues, summed over workers."
    );
    describe_gauge!(
        "tokio_runtime_local_queue_depth_max",
        Unit::Count,
        "Tokio tasks waiting in the longest worker local queue."
    );
    describe_gauge!(
        "tokio_runtime_blocking_threads",
        Unit::Count,
        "Threads in the Tokio spawn_blocking pool."
    );
    describe_gauge!(
        "tokio_runtime_blocking_threads_idle",
        Unit::Count,
        "Idle threads in the Tokio spawn_blocking pool."
    );
    describe_gauge!(
        "tokio_runtime_blocking_queue_depth",
        Unit::Count,
        "Tokio spawn_blocking tasks waiting for a thread."
    );
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
    #[cfg(tokio_unstable)]
    record_unstable(metrics);
}

#[cfg(tokio_unstable)]
#[expect(
    clippy::cast_precision_loss,
    reason = "gauges are f64; counts this large are not exact anyway"
)]
/// Record the metrics Tokio only exposes with `--cfg tokio_unstable`.
fn record_unstable(metrics: &RuntimeMetrics) {
    gauge!("tokio_runtime_poll_duration_mean_seconds")
        .set(metrics.mean_poll_duration.as_secs_f64());
    gauge!("tokio_runtime_poll_duration_mean_max_seconds")
        .set(metrics.mean_poll_duration_worker_max.as_secs_f64());
    counter!("tokio_runtime_polls_total").increment(metrics.total_polls_count);
    counter!("tokio_runtime_steals_total").increment(metrics.total_steal_count);
    gauge!("tokio_runtime_local_queue_depth").set(metrics.total_local_queue_depth as f64);
    gauge!("tokio_runtime_local_queue_depth_max").set(metrics.max_local_queue_depth as f64);
    gauge!("tokio_runtime_blocking_threads").set(metrics.blocking_threads_count as f64);
    gauge!("tokio_runtime_blocking_threads_idle").set(metrics.idle_blocking_threads_count as f64);
    gauge!("tokio_runtime_blocking_queue_depth").set(metrics.blocking_queue_depth as f64);
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

    #[cfg(tokio_unstable)]
    #[test]
    fn records_unstable_metrics() {
        use metrics_util::debugging::DebuggingRecorder;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        ::metrics::with_local_recorder(&recorder, || record(&RuntimeMetrics::default()));
        let names: Vec<_> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(key, ..)| key.key().name().to_owned())
            .collect();
        for name in [
            "tokio_runtime_poll_duration_mean_seconds",
            "tokio_runtime_polls_total",
            "tokio_runtime_local_queue_depth_max",
            "tokio_runtime_blocking_queue_depth",
        ] {
            assert!(names.iter().any(|n| n == name), "{name} not recorded");
        }
    }

    #[test]
    fn every_recorded_metric_is_described() {
        use metrics_util::debugging::DebuggingRecorder;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        ::metrics::with_local_recorder(&recorder, || {
            describe();
            record(&RuntimeMetrics::default());
        });
        let snapshot = snapshotter.snapshot().into_vec();
        assert!(
            snapshot.len() >= 8,
            "only {} metrics recorded",
            snapshot.len()
        );
        for (key, _, description, _) in snapshot {
            assert!(
                description.is_some(),
                "{} has no description",
                key.key().name()
            );
        }
    }

    #[tokio::test]
    async fn exporter_samples_until_aborted() {
        let handle = export_runtime_metrics();
        assert!(!handle.is_finished());
        handle.abort();
    }
}
