//! HTTP and process metrics through the [`metrics`] facade.
//!
//! This crate records; it does not export. Install any `metrics` exporter
//! (`metrics-exporter-prometheus`, `metrics-exporter-dogstatsd`, ...) once per
//! process, and use [`DURATION_BUCKETS`] and [`BODY_SIZE_BUCKETS`] for the
//! histograms when it takes buckets.

mod layer;
mod process;
#[cfg(feature = "runtime-metrics")]
mod runtime;

pub use layer::{HttpMetricsLayer, HttpMetricsService, MetricLabels, MetricsBody, MetricsFuture};
pub use process::install_panic_counter;
#[cfg(feature = "runtime-metrics")]
pub use runtime::export_runtime_metrics;

/// The names [`HttpMetricsLayer`] records under, one per metric.
///
/// The defaults ([`MetricNames::DEFAULT`]) follow Prometheus naming: a
/// `http_server_` prefix, and the unit as a suffix. Set your own to keep
/// existing dashboards and alerts working, changing only the names you need:
///
/// ```
/// use tower_telemetry::MetricNames;
///
/// let names = MetricNames {
///     requests: "request_count",
///     ..MetricNames::DEFAULT
/// };
/// ```
///
/// Pass them to [`HttpMetricsLayer::with_names`]. The names are
/// `&'static str`, so nothing is allocated per request to look them up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricNames {
    /// Counter of requests answered, incremented once the response headers are
    /// ready. Default: `http_server_requests_total`.
    pub requests: &'static str,
    /// Histogram of seconds from the request arriving to the response body
    /// finishing, or the client going away. Covers streamed bodies end to end.
    /// Default: `http_server_request_duration_seconds`.
    pub duration: &'static str,
    /// Histogram of seconds from the request arriving to the response headers
    /// being ready: how long the handler took before it started answering.
    /// Default: `http_server_time_to_first_byte_seconds`.
    pub time_to_first_byte: &'static str,
    /// Histogram of request body sizes in bytes, as declared by `Content-Length`.
    /// Not recorded for requests without one (chunked uploads) or with a size of
    /// 0. Default: `http_server_request_body_size_bytes`.
    pub request_body_size: &'static str,
    /// Histogram of response body bytes actually sent, counted as the body
    /// streams; short when the client goes away mid-body. Default:
    /// `http_server_response_body_size_bytes`.
    pub response_body_size: &'static str,
    /// Gauge of requests in progress, from arrival until the response body
    /// finishes or is dropped. Unlabelled. Default: `http_server_active_requests`.
    pub active_requests: &'static str,
}

impl MetricNames {
    /// The default names, as a constant for struct update syntax
    /// (`MetricNames { requests: "...", ..MetricNames::DEFAULT }`):
    ///
    /// | Field                | Name                                     |
    /// | -------------------- | ---------------------------------------- |
    /// | `requests`           | `http_server_requests_total`             |
    /// | `duration`           | `http_server_request_duration_seconds`   |
    /// | `time_to_first_byte` | `http_server_time_to_first_byte_seconds` |
    /// | `request_body_size`  | `http_server_request_body_size_bytes`    |
    /// | `response_body_size` | `http_server_response_body_size_bytes`   |
    /// | `active_requests`    | `http_server_active_requests`            |
    pub const DEFAULT: Self = Self {
        requests: "http_server_requests_total",
        duration: "http_server_request_duration_seconds",
        time_to_first_byte: "http_server_time_to_first_byte_seconds",
        request_body_size: "http_server_request_body_size_bytes",
        response_body_size: "http_server_response_body_size_bytes",
        active_requests: "http_server_active_requests",
    };

    /// The two duration histograms, `duration` and `time_to_first_byte`: the
    /// names to give [`DURATION_BUCKETS`] when your exporter takes buckets.
    #[must_use]
    pub const fn durations(&self) -> [&'static str; 2] {
        [self.duration, self.time_to_first_byte]
    }

    /// The two body size histograms, `request_body_size` and
    /// `response_body_size`: the names to give [`BODY_SIZE_BUCKETS`] when your
    /// exporter takes buckets.
    #[must_use]
    pub const fn body_sizes(&self) -> [&'static str; 2] {
        [self.request_body_size, self.response_body_size]
    }
}

impl Default for MetricNames {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Histogram bucket bounds for durations, in seconds: 1 ms to 5 minutes.
///
/// Fine-grained below a second for API latency, and wide above it for
/// long-running uploads and downloads. Use them for
/// [`MetricNames::durations`] with exporters that take buckets, such as
/// `metrics-exporter-prometheus` (see [`HttpMetricsLayer`] for an example).
/// Exporters that send histograms as distributions, such as `DogStatsD`, don't
/// need them.
pub const DURATION_BUCKETS: [f64; 16] = [
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

/// Histogram bucket bounds for body sizes, in bytes: 100 B to 10 GB, one per
/// power of ten.
///
/// Use them for [`MetricNames::body_sizes`] with exporters that take
/// buckets.
pub const BODY_SIZE_BUCKETS: [f64; 9] = [
    100.0,
    1_000.0,
    10_000.0,
    100_000.0,
    1_000_000.0,
    10_000_000.0,
    100_000_000.0,
    1_000_000_000.0,
    10_000_000_000.0,
];
