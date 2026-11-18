//! One request or message through each layer, with a real metrics recorder
//! and tracer installed, to measure the per-request cost of the layers.

use std::convert::Infallible;
use std::hint::black_box;

use axum::{Router, routing::get};
use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tower::{Service, ServiceBuilder, ServiceExt, service_fn};
use tower_telemetry::messaging::{Message, MessageTraceLayer};
use tower_telemetry::{
    HttpClientMetricsLayer, HttpMetricsLayer, OtelClientTraceLayer, OtelTraceLayer,
};
use tracing_subscriber::layer::SubscriberExt as _;

/// A message for the consumer benchmark.
struct Job;

impl Message for Job {
    fn system(&self) -> &'static str {
        "kafka"
    }

    fn destination(&self) -> Option<&str> {
        Some("orders")
    }
}

/// Install a recorder and tracer, then time each layer.
fn layers(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_recommended_naming(true)
        .build_recorder();
    metrics::set_global_recorder(recorder).expect("first recorder");
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(InMemorySpanExporter::default())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("bench")));
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let server = |app: Router| {
        move || {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        Request::get("/users/42")
                            .body(axum::body::Body::empty())
                            .expect("request"),
                    )
                    .await
                    .expect("infallible");
                black_box(response.into_body().collect().await.expect("body"));
            }
        }
    };
    let route = || Router::new().route("/users/{id}", get(|| async { "hello" }));

    c.bench_function("server: no layers", |b| {
        b.to_async(&runtime).iter(server(route()));
    });
    c.bench_function("server: HttpMetricsLayer", |b| {
        b.to_async(&runtime)
            .iter(server(route().layer(HttpMetricsLayer::default())));
    });
    c.bench_function("server: OtelTraceLayer", |b| {
        b.to_async(&runtime)
            .iter(server(route().layer(<OtelTraceLayer>::default())));
    });

    let client = ServiceBuilder::new()
        .layer(HttpClientMetricsLayer::default())
        .layer(OtelClientTraceLayer)
        .service(service_fn(|_request: Request<Full<Bytes>>| async {
            let mut response = Response::new(Full::new(Bytes::from_static(b"ok")));
            *response.status_mut() = StatusCode::OK;
            Ok::<_, Infallible>(response)
        }));
    c.bench_function("client: metrics and trace", |b| {
        b.to_async(&runtime).iter(|| {
            let client = client.clone();
            async move {
                let response = client
                    .oneshot(
                        Request::get("https://api.example/users?page=2")
                            .body(Full::new(Bytes::new()))
                            .expect("request"),
                    )
                    .await
                    .expect("infallible");
                black_box(response.into_body().collect().await.expect("body"));
            }
        });
    });

    let consumer = ServiceBuilder::new()
        .layer(MessageTraceLayer)
        .service(service_fn(|_job: Job| async {
            Ok::<_, std::io::Error>(())
        }));
    c.bench_function("messaging: process", |b| {
        b.to_async(&runtime).iter(|| {
            let mut consumer = consumer.clone();
            async move {
                consumer
                    .ready()
                    .await
                    .expect("ready")
                    .call(Job)
                    .await
                    .expect("handled");
            }
        });
    });
}

criterion_group!(benches, layers);
criterion_main!(benches);
