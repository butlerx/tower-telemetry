//! An axum server with request spans and Prometheus metrics.
//!
//! `cargo run --example axum --all-features`, then
//! `curl localhost:3000/users/42` and `curl localhost:3000/metrics`.

use axum::{Router, extract::Path, routing::get};
use metrics_exporter_prometheus::PrometheusBuilder;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tower_telemetry::metrics::{BODY_SIZE_BUCKETS, DURATION_BUCKETS, install_panic_counter};
use tower_telemetry::{
    HealthcheckFilter, HttpMetricsLayer, MetricNames, OtelTraceLayer, PropagatingSpanDecorator,
};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Traces: add an exporter (e.g. `opentelemetry-otlp`) to ship them.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let provider = SdkTracerProvider::builder().build();
    tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("example")))
        .init();

    // Metrics: any `metrics` exporter works; this one is scraped.
    let mut prometheus = PrometheusBuilder::new().with_recommended_naming(true);
    let names = MetricNames::DEFAULT;
    for name in names.durations() {
        prometheus = prometheus.set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(name.to_owned()),
            &DURATION_BUCKETS,
        )?;
    }
    for name in names.body_sizes() {
        prometheus = prometheus.set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(name.to_owned()),
            &BODY_SIZE_BUCKETS,
        )?;
    }
    let handle = prometheus.install_recorder()?;
    install_panic_counter("process_panics_total");

    let app = Router::new()
        .route(
            "/users/{id}",
            get(|Path(id): Path<u64>| async move { format!("user {id}") }),
        )
        .layer(HttpMetricsLayer::default())
        .layer(OtelTraceLayer::<PropagatingSpanDecorator, HealthcheckFilter>::default())
        // Outside the layers: scrapes are neither traced nor counted.
        .route("/metrics", get(move || async move { handle.render() }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
