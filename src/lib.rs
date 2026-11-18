//! OpenTelemetry spans and metrics as Tower layers: HTTP for [hyper] and
//! [axum] servers, and messaging for message producers and consumers.
//!
//! - [`OtelTraceLayer`] (feature `trace`): one server span per request,
//!   continuing the caller's trace, with OpenTelemetry's HTTP fields.
//!   [`OtelClientTraceLayer`] does the same for requests a client sends, and
//!   sends the trace context along. Spans
//!   go through [`tracing`]; export them with `tracing-opentelemetry`.
//! - [`HttpMetricsLayer`] (feature `metrics`): request count, duration, time
//!   to first byte, body sizes and active requests through the [`metrics`]
//!   facade; install any `metrics` exporter to ship them.
//!   [`HttpClientMetricsLayer`] records the same for requests a client sends.
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
//! Turn on the `axum` feature, so spans and metrics are named by the route
//! a request matched rather than its path:
//!
//! ```toml
//! tower-telemetry = { version = "0.0", features = ["axum"] }
//! ```
//!
//! A full server: a tracer that continues callers' traces, a Prometheus
//! exporter for the metrics, and both layers.
//!
//! ```
//! # #[cfg(all(feature = "trace", feature = "metrics"))] {
//! use axum::{Extension, Router, extract::Path, routing::get};
//! use metrics_exporter_prometheus::PrometheusBuilder;
//! use opentelemetry::trace::TracerProvider as _;
//! use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider};
//! use tower_telemetry::{HealthcheckFilter, HttpMetricsLayer, OtelTraceLayer, PropagatingSpanDecorator};
//! use tracing::Span;
//! use tracing_opentelemetry::OpenTelemetrySpanExt;
//! use tracing_subscriber::layer::SubscriberExt as _;
//! use tracing_subscriber::util::SubscriberInitExt as _;
//!
//! async fn user(Path(id): Path<u64>, Extension(span): Extension<Span>) -> String {
//!     span.set_attribute("enduser.id", id.to_string()); // the request's span
//!     format!("user {id}")
//! }
//!
//! async fn serve() -> Result<(), Box<dyn std::error::Error>> {
//!     // Read callers' `traceparent`; without a propagator, every request
//!     // starts a new trace.
//!     opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
//!     // Export spans: add an exporter (e.g. `opentelemetry-otlp`) here.
//!     let provider = SdkTracerProvider::builder().build();
//!     tracing_subscriber::registry()
//!         .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("my-service")))
//!         .init();
//!     // Recommended naming adds the unit and `_total` suffixes to the dotted
//!     // OpenTelemetry names: `http_server_request_duration_seconds`.
//!     let prometheus = PrometheusBuilder::new()
//!         .with_recommended_naming(true)
//!         .install_recorder()?;
//!
//!     let app = Router::new()
//!         .route("/users/{id}", get(user))
//!         .layer(HttpMetricsLayer::default())
//!         .layer(OtelTraceLayer::<PropagatingSpanDecorator, HealthcheckFilter>::default())
//!         // Added after the layers, so scrapes are neither traced nor counted.
//!         .route("/metrics", get(move || async move { prometheus.render() }));
//!
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
//!     axum::serve(listener, app).await?;
//!     Ok(())
//! }
//! # }
//! ```
//!
//! Where the layers go matters:
//!
//! - **Use `Router::layer`.** axum runs those layers after routing, so they
//!   see the matched route. Wrapping the whole `Router` from outside (with
//!   `ServiceBuilder`, or `axum::serve(listener, layer.layer(app))`) runs
//!   before routing: every span is named by method alone and the `route`
//!   label is empty.
//! - **A layer covers only the routes added before it.** Add routes you
//!   don't want traced, such as `/metrics`, after the layers.
//! - **The last layer added runs first.** With the order above, the trace
//!   layer wraps the metrics layer, so anything metrics logs is inside the
//!   request span.
//! - **Unmatched requests** (404s from the fallback) are named by method
//!   alone, with an empty `route`.
//!
//! See `examples/axum.rs` for the same server, runnable.
//!
//! # hyper
//!
//! The layers are plain Tower middleware, so they wrap any
//! `Service<Request<B>>`. Build the stack with `tower::ServiceBuilder` and
//! hand it to hyper with `hyper_util::service::TowerToHyperService`:
//!
//! ```
//! # #[cfg(all(feature = "trace", feature = "metrics"))] {
//! use std::convert::Infallible;
//!
//! use bytes::Bytes;
//! use http::{Request, Response};
//! use http_body_util::Full;
//! use hyper::body::Incoming;
//! use hyper::server::conn::http1;
//! use hyper_util::rt::TokioIo;
//! use hyper_util::service::TowerToHyperService;
//! use tower::ServiceBuilder;
//! use tower_telemetry::{HttpMetricsLayer, OtelTraceLayer};
//!
//! async fn hello(_request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
//!     Ok(Response::new(Full::new(Bytes::from_static(b"hello"))))
//! }
//!
//! async fn serve() -> Result<(), Box<dyn std::error::Error>> {
//!     let service = ServiceBuilder::new()
//!         .layer(<OtelTraceLayer>::default())
//!         .layer(HttpMetricsLayer::default())
//!         .service_fn(hello);
//!
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
//!     loop {
//!         let (stream, _) = listener.accept().await?;
//!         let service = TowerToHyperService::new(service.clone());
//!         tokio::spawn(async move {
//!             let connection = http1::Builder::new().serve_connection(TokioIo::new(stream), service);
//!             if let Err(err) = connection.await {
//!                 tracing::warn!(error = %err, "connection failed");
//!             }
//!         });
//!     }
//! }
//! # }
//! ```
//!
//! In `ServiceBuilder`, the first layer added runs first, so here the trace
//! layer wraps the metrics layer. Set up the propagator and tracer as in the
//! axum example.
//!
//! Without axum there's no matched route: spans are named by method alone
//! and the `route` label is empty. If you route requests yourself, name
//! spans by your route with a [`SpanDecorator`] whose
//! [`span_name`](SpanDecorator::span_name) reads it, for example from a
//! request extension your router sets. Never name them by the raw path,
//! which holds identifiers and makes every name unique.
//!
//! The metrics layer wraps response bodies in
//! [`MetricsBody`](metrics::MetricsBody), which is still an
//! `http_body::Body`, to count the bytes sent and time the whole response.
//!
//! [hyper]: https://docs.rs/hyper
//! [axum]: https://docs.rs/axum
//! [`metrics`]: ::metrics

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(any(feature = "trace", feature = "messaging", feature = "metrics"))]
mod attribute;
#[cfg(feature = "messaging")]
pub mod messaging;
#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(any(feature = "trace", feature = "metrics"))]
mod request;
#[cfg(feature = "trace")]
mod trace;

#[cfg(feature = "metrics")]
pub use metrics::{HttpClientMetricsLayer, HttpMetricsLayer, MetricLabels, MetricNames};
#[cfg(feature = "trace")]
pub use trace::{
    ClientResponseFuture, DefaultSpanDecorator, HEALTHCHECK_PATHS, HEALTHCHECK_USER_AGENTS,
    HealthcheckFilter, NoOpFilter, OtelClientTraceLayer, OtelClientTraceService, OtelTraceLayer,
    OtelTraceService, PropagatingSpanDecorator, RequestFilter, ResponseFuture, SpanDecorator,
    mark_server_errors,
};
