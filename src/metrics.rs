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

/// The names [`HttpMetricsLayer`] records under. The defaults follow
/// Prometheus naming; set your own to match existing dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricNames {
    /// Counter: requests answered.
    pub requests: &'static str,
    /// Histogram: seconds from the request arriving to the response body
    /// finishing (or the client going away).
    pub duration: &'static str,
    /// Histogram: seconds from the request arriving to the response headers
    /// being ready.
    pub time_to_first_byte: &'static str,
    /// Histogram: declared request body size (`Content-Length`), in bytes.
    pub request_body_size: &'static str,
    /// Histogram: response body bytes sent.
    pub response_body_size: &'static str,
    /// Gauge: requests being served, until their response body finishes.
    pub active_requests: &'static str,
}

impl MetricNames {
    /// `http_server_requests_total`, `http_server_request_duration_seconds`,
    /// `http_server_time_to_first_byte_seconds`,
    /// `http_server_request_body_size_bytes`,
    /// `http_server_response_body_size_bytes` and
    /// `http_server_active_requests`.
    pub const DEFAULT: Self = Self {
        requests: "http_server_requests_total",
        duration: "http_server_request_duration_seconds",
        time_to_first_byte: "http_server_time_to_first_byte_seconds",
        request_body_size: "http_server_request_body_size_bytes",
        response_body_size: "http_server_response_body_size_bytes",
        active_requests: "http_server_active_requests",
    };

    /// The duration histograms, for [`DURATION_BUCKETS`].
    #[must_use]
    pub const fn durations(&self) -> [&'static str; 2] {
        [self.duration, self.time_to_first_byte]
    }

    /// The body size histograms, for [`BODY_SIZE_BUCKETS`].
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

/// Histogram buckets for durations, in seconds: 1 ms to 5 minutes, wide
/// enough for long streamed uploads and downloads.
pub const DURATION_BUCKETS: [f64; 16] = [
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

/// Histogram buckets for body sizes, in bytes: 100 B to 10 GB.
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
