//! OpenTelemetry spans and metrics as Tower layers: HTTP for [hyper] and
//! [axum] servers, and messaging for message producers and consumers.
//!
//! - [`OtelTraceLayer`] (feature `trace`): one server span per request,
//!   continuing the caller's trace, with OpenTelemetry's HTTP fields. Spans
//!   go through [`tracing`]; export them with `tracing-opentelemetry`.
//! - [`HttpMetricsLayer`] (feature `metrics`): request count, duration, time
//!   to first byte, body sizes and active requests through the [`metrics`]
//!   facade; install any `metrics` exporter to ship them.
//! - [`messaging`] (feature `messaging`): OpenTelemetry messaging spans for
//!   any broker, trace context in message headers, and
//!   [`messaging::MessageTraceLayer`] for consumers.
//! - [`metrics::export_runtime_metrics`] (feature `runtime-metrics`): Tokio
//!   runtime metrics.
//!
//! With the `axum` feature, spans and metrics are named by the matched route
//! (`GET /users/{id}`), not the path, which carries identifiers.
//!
//! # axum
//!
//! ```
//! # #[cfg(all(feature = "trace", feature = "metrics"))] {
//! use axum::{Router, routing::get};
//! use tower_telemetry::{HttpMetricsLayer, OtelTraceLayer};
//!
//! let app: Router = Router::new()
//!     .route("/users/{id}", get(|| async { "hello" }))
//!     .layer(HttpMetricsLayer::new())
//!     .layer(OtelTraceLayer::new());
//! # }
//! ```
//!
//! # hyper
//!
//! The layers are plain Tower middleware: wrap a service with
//! `tower::ServiceBuilder` and serve it with
//! `hyper_util::service::TowerToHyperService`. See `examples/hyper.rs`.
//!
//! [hyper]: https://docs.rs/hyper
//! [axum]: https://docs.rs/axum
//! [`metrics`]: ::metrics

#[cfg(feature = "messaging")]
pub mod messaging;
#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(any(feature = "trace", feature = "metrics"))]
mod request;
#[cfg(feature = "trace")]
mod trace;

#[cfg(feature = "metrics")]
pub use metrics::{HttpMetricsLayer, MetricLabels, MetricNames};
#[cfg(feature = "trace")]
pub use trace::{
    DefaultSpanDecorator, HealthcheckFilter, NoOpFilter, OtelTraceLayer, OtelTraceService,
    PropagatingSpanDecorator, RequestFilter, ResponseFuture, SpanDecorator, mark_server_errors,
};
