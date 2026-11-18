//! HTTP and process metrics through the [`metrics`] facade.
//!
//! - [`HttpMetricsLayer`]: metrics for the requests a server answers.
//! - [`HttpClientMetricsLayer`]: metrics for the requests a client sends.
//! - [`install_panic_counter`] and, with `runtime-metrics`,
//!   [`export_runtime_metrics`]: the process itself.
//!
//! This crate records; it does not export. Install any `metrics` exporter
//! (`metrics-exporter-prometheus`, `metrics-exporter-dogstatsd`, ...) once per
//! process, and use [`DURATION_BUCKETS`] and [`BODY_SIZE_BUCKETS`] for the
//! histograms when it takes buckets.

use opentelemetry_semantic_conventions::metric::{
    HTTP_CLIENT_ACTIVE_REQUESTS, HTTP_CLIENT_REQUEST_BODY_SIZE, HTTP_CLIENT_REQUEST_DURATION,
    HTTP_CLIENT_RESPONSE_BODY_SIZE, HTTP_SERVER_ACTIVE_REQUESTS, HTTP_SERVER_REQUEST_BODY_SIZE,
    HTTP_SERVER_REQUEST_DURATION, HTTP_SERVER_RESPONSE_BODY_SIZE,
};

mod client;
pub(crate) mod label;
mod layer;
mod process;
mod recording;
#[cfg(feature = "runtime-metrics")]
mod runtime;

pub use client::{HttpClientMetricsLayer, HttpClientMetricsService};
pub use layer::{HttpMetricsLayer, HttpMetricsService, MetricLabels};
pub use process::install_panic_counter;
pub use recording::{MetricsBody, MetricsFuture};
#[cfg(feature = "runtime-metrics")]
pub use runtime::export_runtime_metrics;

/// The names [`HttpMetricsLayer`] and [`HttpClientMetricsLayer`] record
/// under, one per metric.
///
/// The defaults are OpenTelemetry's HTTP metric names:
/// [`MetricNames::DEFAULT`] for servers and [`MetricNames::CLIENT`] for
/// clients. The two the conventions don't define, the request counter and
/// time to first byte, follow their style. Exporters adapt them: Prometheus with recommended naming turns
/// `http.server.request.duration` into `http_server_request_duration_seconds`,
/// and `DogStatsD` sends it as is.
///
/// Set your own to keep existing dashboards and alerts working, changing only
/// the names you need:
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
/// Pass them to [`HttpMetricsLayer::with_names`] or
/// [`HttpClientMetricsLayer::with_names`]. The names are
/// `&'static str`, so nothing is allocated per request to look them up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricNames {
    /// Counter of requests answered, incremented once the response headers are
    /// ready. Default: `http.server.requests` (Prometheus:
    /// `http_server_requests_total`). Not in the conventions, which count
    /// requests from the duration histogram; kept as a plain counter for
    /// dashboards and alerts that want one.
    pub requests: &'static str,
    /// Histogram of seconds from the request arriving to the response body
    /// finishing, or the client going away. Covers streamed bodies end to end.
    /// Default: `http.server.request.duration` (Prometheus:
    /// `http_server_request_duration_seconds`).
    pub duration: &'static str,
    /// Histogram of seconds from the request arriving to the response headers
    /// being ready: how long the handler took before it started answering.
    /// Default: `http.server.time_to_first_byte` (Prometheus:
    /// `http_server_time_to_first_byte_seconds`). Not in the conventions; named
    /// in their style.
    pub time_to_first_byte: &'static str,
    /// Histogram of request body sizes in bytes, as declared by `Content-Length`.
    /// Not recorded for requests without one (chunked uploads) or with a size of
    /// 0. Default: `http.server.request.body.size` (Prometheus:
    /// `http_server_request_body_size_bytes`).
    pub request_body_size: &'static str,
    /// Histogram of response body bytes actually sent, counted as the body
    /// streams; short when the client goes away mid-body. Default:
    /// `http.server.response.body.size` (Prometheus:
    /// `http_server_response_body_size_bytes`).
    pub response_body_size: &'static str,
    /// Gauge of requests in progress, from arrival until the response body
    /// finishes or is dropped, labelled by method and scheme (and host when
    /// enabled). Default: `http.server.active_requests` (Prometheus:
    /// `http_server_active_requests`).
    pub active_requests: &'static str,
}

impl MetricNames {
    /// The default names, as a constant for struct update syntax
    /// (`MetricNames { requests: "...", ..MetricNames::DEFAULT }`):
    ///
    /// | Field                | Name                             | Unit    | In the conventions |
    /// | -------------------- | -------------------------------- | ------- | ------------------ |
    /// | `requests`           | `http.server.requests`           | count   | no                 |
    /// | `duration`           | `http.server.request.duration`   | seconds | yes                |
    /// | `time_to_first_byte` | `http.server.time_to_first_byte` | seconds | no                 |
    /// | `request_body_size`  | `http.server.request.body.size`  | bytes   | yes                |
    /// | `response_body_size` | `http.server.response.body.size` | bytes   | yes                |
    /// | `active_requests`    | `http.server.active_requests`    | count   | yes                |
    pub const DEFAULT: Self = Self {
        requests: "http.server.requests",
        duration: HTTP_SERVER_REQUEST_DURATION,
        time_to_first_byte: "http.server.time_to_first_byte",
        request_body_size: HTTP_SERVER_REQUEST_BODY_SIZE,
        response_body_size: HTTP_SERVER_RESPONSE_BODY_SIZE,
        active_requests: HTTP_SERVER_ACTIVE_REQUESTS,
    };

    /// The client defaults, for [`HttpClientMetricsLayer`]: OpenTelemetry's
    /// HTTP client metrics, plus the same two extras in the `http.client`
    /// namespace.
    ///
    /// | Field                | Name                             | Unit    | In the conventions |
    /// | -------------------- | -------------------------------- | ------- | ------------------ |
    /// | `requests`           | `http.client.requests`           | count   | no                 |
    /// | `duration`           | `http.client.request.duration`   | seconds | yes                |
    /// | `time_to_first_byte` | `http.client.time_to_first_byte` | seconds | no                 |
    /// | `request_body_size`  | `http.client.request.body.size`  | bytes   | yes                |
    /// | `response_body_size` | `http.client.response.body.size` | bytes   | yes                |
    /// | `active_requests`    | `http.client.active_requests`    | count   | yes                |
    pub const CLIENT: Self = Self {
        requests: "http.client.requests",
        duration: HTTP_CLIENT_REQUEST_DURATION,
        time_to_first_byte: "http.client.time_to_first_byte",
        request_body_size: HTTP_CLIENT_REQUEST_BODY_SIZE,
        response_body_size: HTTP_CLIENT_RESPONSE_BODY_SIZE,
        active_requests: HTTP_CLIENT_ACTIVE_REQUESTS,
    };

    /// The two duration histograms, `duration` and `time_to_first_byte`: the
    /// names to give [`DURATION_BUCKETS`] when your exporter takes buckets.
    #[must_use]
    #[inline]
    pub const fn durations(&self) -> [&'static str; 2] {
        [self.duration, self.time_to_first_byte]
    }

    /// The two body size histograms, `request_body_size` and
    /// `response_body_size`: the names to give [`BODY_SIZE_BUCKETS`] when your
    /// exporter takes buckets.
    #[must_use]
    #[inline]
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
