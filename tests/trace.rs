//! A custom filter drops requests, and a custom decorator's attributes reach
//! the exported span.

use std::convert::Infallible;

use http::{Request, Response};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{Key, Value};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tower::{ServiceBuilder, ServiceExt};
use tower_telemetry::{OtelTraceLayer, RequestFilter, SpanDecorator, mark_server_errors};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt as _;

#[derive(Debug, Clone, Copy, Default)]
struct SkipProbes;

impl RequestFilter for SkipProbes {
    fn should_ignore<B>(request: &Request<B>) -> bool {
        request.uri().path() == "/healthz"
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct TenantDecorator;

impl SpanDecorator for TenantDecorator {
    fn on_request<B>(request: &Request<B>, span: &Span) {
        if let Some(tenant) = request
            .headers()
            .get("x-tenant-id")
            .and_then(|value| value.to_str().ok())
        {
            span.set_attribute("tenant.id", tenant.to_owned());
        }
    }

    fn on_response<B>(span: &Span, _context: &opentelemetry::Context, response: &mut Response<B>) {
        span.set_attribute("app.handled", true);
        mark_server_errors(span, response);
    }
}

#[tokio::test]
async fn filters_requests_and_records_custom_attributes() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let service = ServiceBuilder::new()
        .layer(OtelTraceLayer::<TenantDecorator, SkipProbes>::default())
        .service_fn(|_request: Request<()>| async {
            Ok::<_, Infallible>(Response::new(String::new()))
        });
    for request in [
        Request::get("/healthz").body(()),
        Request::get("/users")
            .header("x-tenant-id", "acme")
            .body(()),
    ] {
        service
            .clone()
            .oneshot(request.expect("static request"))
            .await
            .expect("infallible");
    }

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 1, "the probe is not traced");
    let attribute = |key: &'static str| {
        spans[0]
            .attributes
            .iter()
            .find(|attribute| attribute.key == Key::from_static_str(key))
            .map(|attribute| attribute.value.clone())
    };
    assert_eq!(attribute("tenant.id"), Some(Value::from("acme")));
    assert_eq!(attribute("app.handled"), Some(Value::from(true)));
    assert_eq!(attribute("url.path"), Some(Value::from("/users")));
}
