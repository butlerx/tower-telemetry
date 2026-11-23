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
/// install_panic_counter("process.panics");
/// ```
///
/// OpenTelemetry defines no panic metric; `process.panics` follows its
/// naming (Prometheus with recommended naming: `process_panics_total`).
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
/// runtime runs, and record OpenTelemetry's process metrics through the
/// [`metrics`] facade.
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
/// OpenTelemetry's process metric names, which exporters adapt: Prometheus
/// with recommended naming turns `process.memory.usage` into
/// `process_memory_usage_bytes`. None have labels except `cpu.mode`. The
/// first sample is taken at once.
///
/// | Name                                 | Type    | Unit    | What it is                                    | In the conventions |
/// | ------------------------------------ | ------- | ------- | --------------------------------------------- | ------------------ |
/// | `process.cpu.time`                   | counter | seconds | CPU time, by `cpu.mode`: `user` or `system`   | yes                |
/// | `process.memory.usage`               | gauge   | bytes   | resident memory (RSS)                         | yes                |
/// | `process.memory.virtual`             | gauge   | bytes   | committed virtual memory                      | yes                |
/// | `process.memory.virtual.limit`       | gauge   | bytes   | virtual memory limit (not on Windows)         | no                 |
/// | `process.unix.file_descriptor.count` | gauge   | count   | open file descriptors (Unix)                  | yes                |
/// | `process.unix.file_descriptor.limit` | gauge   | count   | file descriptor limit (Unix)                  | no                 |
/// | `process.windows.handle.count`       | gauge   | count   | open handles (Windows)                        | yes                |
/// | `process.thread.count`               | gauge   | count   | OS threads (not on Windows)                   | yes                |
/// | `process.uptime`                     | gauge   | seconds | time since the process started                | yes                |
///
/// The two limits aren't in the conventions; they're named in their style.
/// On Windows `process.cpu.time` has no `cpu.mode`: only the total is
/// available. A metric the platform can't report is left out rather than
/// set to 0.
///
/// # Reading them
///
/// - **CPU:** `sum(rate(process_cpu_time_seconds_total[1m]))` is cores in
///   use; compare it with the container's CPU limit.
/// - **Where CPU goes:** high `system` time relative to `user` points to
///   the kernel: many small reads and writes, syscalls in a hot loop, or
///   lock contention. High `user` time is your own code.
/// - **Memory leaks:** `process.memory.usage` that only climbs.
/// - **File descriptor exhaustion:** `count / limit` near 1 means `accept`
///   and `open` are about to fail with `EMFILE`.
/// - **Restarts:** `process.uptime` dropping back to 0.
///
/// # Limits
///
/// CPU time is recorded in whole seconds, since `metrics` counters are
/// integers; the error is at most one second per `rate()` window. Uptime is
/// whole seconds too: the start time the OS reports is.
///
/// Reading the process's stats is a few small reads of `/proc` on Linux and
/// system calls elsewhere: fast enough to run on a worker thread every five
/// seconds.
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
    sampler::describe();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PROCESS_METRICS_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            sampler::record();
        }
    })
}

/// Reading the process's stats and recording them.
#[cfg(feature = "process-metrics")]
mod sampler {
    use std::time::{SystemTime, UNIX_EPOCH};

    use ::metrics::{Unit, counter, describe_counter, describe_gauge, gauge};
    use opentelemetry_semantic_conventions::metric::{
        PROCESS_CPU_TIME, PROCESS_MEMORY_USAGE, PROCESS_MEMORY_VIRTUAL, PROCESS_THREAD_COUNT,
        PROCESS_UNIX_FILE_DESCRIPTOR_COUNT, PROCESS_UPTIME, PROCESS_WINDOWS_HANDLE_COUNT,
    };

    /// Virtual memory limit; not in the conventions.
    pub(super) const PROCESS_MEMORY_VIRTUAL_LIMIT: &str = "process.memory.virtual.limit";
    /// File descriptor limit; not in the conventions.
    pub(super) const PROCESS_UNIX_FILE_DESCRIPTOR_LIMIT: &str =
        "process.unix.file_descriptor.limit";

    /// Describe each metric's unit and meaning to the installed recorder.
    pub(super) fn describe() {
        describe_counter!(
            PROCESS_CPU_TIME,
            Unit::Seconds,
            "CPU time spent by the process, by mode: user or system."
        );
        describe_gauge!(
            PROCESS_MEMORY_USAGE,
            Unit::Bytes,
            "Physical memory in use by the process (resident set size)."
        );
        describe_gauge!(
            PROCESS_MEMORY_VIRTUAL,
            Unit::Bytes,
            "Virtual memory committed by the process."
        );
        describe_gauge!(
            PROCESS_MEMORY_VIRTUAL_LIMIT,
            Unit::Bytes,
            "Most virtual memory the process may use."
        );
        describe_gauge!(
            PROCESS_UNIX_FILE_DESCRIPTOR_COUNT,
            Unit::Count,
            "File descriptors open in the process."
        );
        describe_gauge!(
            PROCESS_UNIX_FILE_DESCRIPTOR_LIMIT,
            Unit::Count,
            "Most file descriptors the process may open."
        );
        describe_gauge!(
            PROCESS_WINDOWS_HANDLE_COUNT,
            Unit::Count,
            "Handles held by the process."
        );
        describe_gauge!(
            PROCESS_THREAD_COUNT,
            Unit::Count,
            "OS threads in the process."
        );
        describe_gauge!(
            PROCESS_UPTIME,
            Unit::Seconds,
            "Time since the process started."
        );
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "gauges are f64; values this large are not exact anyway"
    )]
    /// Record one sample of the process's stats.
    pub(super) fn record() {
        let stats = metrics_process::collector::collect();
        record_cpu_time(stats.cpu_seconds_total);
        if let Some(bytes) = stats.resident_memory_bytes {
            gauge!(PROCESS_MEMORY_USAGE).set(bytes as f64);
        }
        if let Some(bytes) = stats.virtual_memory_bytes {
            gauge!(PROCESS_MEMORY_VIRTUAL).set(bytes as f64);
        }
        if let Some(bytes) = stats.virtual_memory_max_bytes {
            gauge!(PROCESS_MEMORY_VIRTUAL_LIMIT).set(bytes as f64);
        }
        let open = if cfg!(windows) {
            PROCESS_WINDOWS_HANDLE_COUNT
        } else {
            PROCESS_UNIX_FILE_DESCRIPTOR_COUNT
        };
        if let Some(count) = stats.open_fds {
            gauge!(open).set(count as f64);
        }
        if let Some(count) = stats.max_fds.filter(|_| cfg!(unix)) {
            gauge!(PROCESS_UNIX_FILE_DESCRIPTOR_LIMIT).set(count as f64);
        }
        if let Some(count) = stats.threads {
            gauge!(PROCESS_THREAD_COUNT).set(count as f64);
        }
        if let Some(started) = stats.start_time_seconds {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            gauge!(PROCESS_UPTIME).set(now.saturating_sub(started) as f64);
        }
    }

    /// Record user and system CPU time so far, in whole seconds, from
    /// `getrusage`. Records nothing if it fails.
    #[cfg(unix)]
    fn record_cpu_time(_total: Option<f64>) {
        use nix::sys::resource::{UsageWho, getrusage};
        use opentelemetry_semantic_conventions::attribute::CPU_MODE;

        let Ok(usage) = getrusage(UsageWho::RUSAGE_SELF) else {
            return;
        };
        for (mode, time) in [("user", usage.user_time()), ("system", usage.system_time())] {
            let seconds = u64::try_from(time.tv_sec()).unwrap_or(0);
            counter!(PROCESS_CPU_TIME, CPU_MODE => mode).absolute(seconds);
        }
    }

    /// Record total CPU time so far, in whole seconds: Windows doesn't
    /// report it by mode here.
    #[cfg(not(unix))]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "CPU seconds are positive and far below u64::MAX"
    )]
    fn record_cpu_time(total: Option<f64>) {
        if let Some(seconds) = total {
            counter!(PROCESS_CPU_TIME).absolute(seconds as u64);
        }
    }
}

#[cfg(all(test, feature = "process-metrics"))]
mod tests {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
    use opentelemetry_semantic_conventions::metric::{
        PROCESS_CPU_TIME, PROCESS_MEMORY_USAGE, PROCESS_UPTIME,
    };

    use super::*;

    /// Describe and record one sample into a local recorder.
    fn sample() -> Snapshotter {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        ::metrics::with_local_recorder(&recorder, || {
            sampler::describe();
            sampler::record();
        });
        snapshotter
    }

    #[test]
    fn records_process_metrics() {
        let snapshot = sample().snapshot().into_vec();
        let value = |name: &str| {
            snapshot
                .iter()
                .find(|(key, ..)| key.key().name() == name)
                .map(|(.., value)| value)
        };
        assert!(matches!(
            value(PROCESS_MEMORY_USAGE),
            Some(DebugValue::Gauge(bytes)) if bytes.into_inner() > 0.0
        ));
        assert!(value(PROCESS_UPTIME).is_some());
        #[cfg(unix)]
        {
            use opentelemetry_semantic_conventions::metric::PROCESS_UNIX_FILE_DESCRIPTOR_COUNT;
            assert!(value(PROCESS_UNIX_FILE_DESCRIPTOR_COUNT).is_some());
            assert!(value(sampler::PROCESS_UNIX_FILE_DESCRIPTOR_LIMIT).is_some());
        }
    }

    #[test]
    fn every_process_metric_is_described_and_named_otel_style() {
        let snapshot = sample().snapshot().into_vec();
        assert!(
            snapshot.len() >= 5,
            "only {} metrics recorded",
            snapshot.len()
        );
        for (key, _, description, _) in snapshot {
            let name = key.key().name();
            assert!(description.is_some(), "{name} has no description");
            assert!(
                name.starts_with("process."),
                "{name} isn't an OpenTelemetry name"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn records_cpu_time_by_mode() {
        let mut modes: Vec<_> = sample()
            .snapshot()
            .into_vec()
            .into_iter()
            .filter(|(key, ..)| key.key().name() == PROCESS_CPU_TIME)
            .map(|(key, ..)| {
                key.key()
                    .labels()
                    .map(|label| format!("{}={}", label.key(), label.value()))
                    .collect::<Vec<_>>()
            })
            .collect();
        modes.sort();
        assert_eq!(modes, [["cpu.mode=system"], ["cpu.mode=user"]]);
    }

    #[tokio::test]
    async fn exporter_samples_until_aborted() {
        let handle = export_process_metrics();
        assert!(!handle.is_finished());
        handle.abort();
    }
}
