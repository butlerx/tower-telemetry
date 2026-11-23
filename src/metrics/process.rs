//! Process metrics: panics and, with `process-metrics`, CPU, memory, file
//! descriptors and threads.

use std::panic;

use ::metrics::{Unit, counter, describe_counter};

/// Count panics in the counter `name`, then run the previous panic hook, so
/// panics still print (or reach your error reporter) as before.
///
/// ```
/// use tower_telemetry::metrics::install_panic_counter;
///
/// // After installing your exporter, and after any other panic hook (such
/// // as an error reporter's), since this one wraps whatever is set.
/// install_panic_counter("process_panics_total");
/// ```
///
/// The counter is set to 0 at once, so it exists before the first panic and
/// `increase()` alerts work from the start. A panic inside a Tokio task
/// counts too, even though Tokio catches it and the process keeps running:
/// that's what makes this worth alerting on.
///
/// Call it once; each call wraps the hook again and counts every panic once
/// more.
pub fn install_panic_counter(name: &'static str) {
    describe_counter!(
        name,
        Unit::Count,
        "Panics in the process, including those Tokio caught inside tasks."
    );
    counter!(name).absolute(0);
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        counter!(name).increment(1);
        previous(info);
    }));
}

/// How often [`export_process_metrics`] samples the process.
#[cfg(feature = "process-metrics")]
pub const PROCESS_METRICS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Sample the process every [`PROCESS_METRICS_INTERVAL`], for as long as the
/// runtime runs, and record the standard Prometheus process metrics through
/// the [`metrics`] facade.
///
/// Spawns a task on the current Tokio runtime and returns its handle. Call it
/// once per process, after installing your `metrics` exporter:
///
/// ```
/// use tower_telemetry::metrics::export_process_metrics;
///
/// #[tokio::main]
/// async fn main() {
///     // install a `metrics` exporter first
///     let sampler = export_process_metrics();
///     // ... serve until shutdown ...
///     sampler.abort();
/// }
/// ```
///
/// # Metrics
///
/// The names and units Prometheus client libraries use, so existing
/// dashboards and alerts work unchanged. None have labels. The first sample
/// is taken at once.
///
/// | Name                               | Type    | What it is                                 |
/// | ---------------------------------- | ------- | ------------------------------------------ |
/// | `process_cpu_seconds_total`        | counter | user and system CPU time, in seconds       |
/// | `process_resident_memory_bytes`    | gauge   | resident memory (RSS)                      |
/// | `process_virtual_memory_bytes`     | gauge   | virtual memory                             |
/// | `process_virtual_memory_max_bytes` | gauge   | virtual memory limit (not on Windows)      |
/// | `process_open_fds`                 | gauge   | open file descriptors (handles on Windows) |
/// | `process_max_fds`                  | gauge   | file descriptor limit                      |
/// | `process_threads`                  | gauge   | OS threads (not on Windows)                |
/// | `process_start_time_seconds`       | gauge   | start time, in seconds since the Unix epoch |
///
/// A metric the platform can't report is left out rather than set to 0.
///
/// # Reading them
///
/// - **CPU:** `rate(process_cpu_seconds_total[1m])` is cores in use; compare
///   it with the container's CPU limit.
/// - **Memory leaks:** `process_resident_memory_bytes` that only climbs.
/// - **File descriptor exhaustion:** `process_open_fds / process_max_fds`
///   near 1 means `accept` and `open` are about to fail with `EMFILE`.
/// - **Restarts:** `changes(process_start_time_seconds[1h])`.
///
/// # Limits
///
/// Reading the process's stats is a few small reads of `/proc` on Linux and
/// system calls elsewhere: fast enough to run on a worker thread every five
/// seconds. Calling this twice double-counts `process_cpu_seconds_total`.
///
/// The task stops by itself when the runtime shuts down. Abort the handle to
/// stop it sooner; dropping it leaves the task running.
///
/// # Panics
///
/// Panics when called outside a Tokio runtime.
///
/// [`metrics`]: ::metrics
#[cfg(feature = "process-metrics")]
#[must_use = "dropping the handle detaches the sampler; keep it to abort it"]
pub fn export_process_metrics() -> tokio::task::JoinHandle<()> {
    let collector = metrics_process::Collector::default();
    collector.describe();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PROCESS_METRICS_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            collector.collect();
        }
    })
}

#[cfg(all(test, feature = "process-metrics"))]
mod tests {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    use super::*;

    #[test]
    fn records_process_metrics() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        ::metrics::with_local_recorder(&recorder, || {
            metrics_process::Collector::default().collect();
        });
        let snapshot = snapshotter.snapshot().into_vec();
        let value = |name: &str| {
            snapshot
                .iter()
                .find(|(key, ..)| key.key().name() == name)
                .map(|(.., value)| value)
        };
        assert!(matches!(
            value("process_resident_memory_bytes"),
            Some(DebugValue::Gauge(bytes)) if bytes.into_inner() > 0.0
        ));
        assert!(matches!(
            value("process_cpu_seconds_total"),
            Some(DebugValue::Counter(_))
        ));
        assert!(value("process_start_time_seconds").is_some());
        assert!(value("process_open_fds").is_some());
    }

    #[test]
    fn process_metrics_are_described() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        ::metrics::with_local_recorder(&recorder, || {
            let collector = metrics_process::Collector::default();
            collector.describe();
            collector.collect();
        });
        for (key, _, description, _) in snapshotter.snapshot().into_vec() {
            assert!(
                description.is_some(),
                "{} has no description",
                key.key().name()
            );
        }
    }

    #[tokio::test]
    async fn exporter_samples_until_aborted() {
        let handle = export_process_metrics();
        assert!(!handle.is_finished());
        handle.abort();
    }
}
