//! A custom filter drops requests, and a custom decorator's attributes reach
//! the exported span.

use std::convert::Infallible;

use http::{Request, Response};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{Key, Value};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use opentelemetry_semantic_conventions::attribute::{
    HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, URL_FULL, URL_PATH, URL_QUERY, URL_SCHEME,
};
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

    fn on_response<B>(span: &Span, response: &mut Response<B>) {
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
    assert_eq!(attribute(URL_PATH), Some(Value::from("/users")));
    assert_eq!(attribute(HTTP_REQUEST_METHOD), Some(Value::from("GET")));
    assert_eq!(attribute(HTTP_RESPONSE_STATUS_CODE), Some(Value::from(200)));
    assert!(
        matches!(attribute("http.server.time_to_first_byte"), Some(Value::F64(seconds)) if seconds >= 0.0),
        "time to first byte is recorded in seconds"
    );
}

#[tokio::test]
async fn client_spans_propagate_and_mark_4xx_as_errors() {
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use tower_telemetry::OtelClientTraceLayer;

    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let client = ServiceBuilder::new()
        .layer(OtelClientTraceLayer)
        .service_fn(|request: Request<String>| async move {
            assert!(
                request.headers().contains_key("traceparent"),
                "the trace context is sent"
            );
            let mut response = Response::new(String::new());
            *response.status_mut() = http::StatusCode::NOT_FOUND;
            Ok::<_, Infallible>(response)
        });
    client
        .oneshot(
            Request::get("https://user:pw@api.example:8443/users?sig=secret")
                .body(String::new())
                .expect("static request"),
        )
        .await
        .expect("infallible");

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert_eq!(span.name, "GET");
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Client);
    assert!(matches!(
        span.status,
        opentelemetry::trace::Status::Error { .. }
    ));
    let attribute = |key: &'static str| {
        span.attributes
            .iter()
            .find(|kv| kv.key == Key::from_static_str(key))
            .map(|kv| kv.value.clone())
    };
    assert_eq!(
        attribute("url.full"),
        Some(Value::from("https://api.example:8443/users?sig=REDACTED"))
    );
    assert_eq!(
        attribute("server.address"),
        Some(Value::from("api.example"))
    );
    assert_eq!(attribute("server.port"), Some(Value::I64(8443)));
    assert_eq!(attribute(HTTP_RESPONSE_STATUS_CODE), Some(Value::I64(404)));
    assert_eq!(attribute("error.type"), Some(Value::from("404")));
}

#[tokio::test]
async fn server_spans_redact_credentials_and_bound_the_scheme() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let service = ServiceBuilder::new()
        .layer(<OtelTraceLayer>::default())
        .service_fn(|_request: Request<()>| async {
            Ok::<_, Infallible>(Response::new(String::new()))
        });
    for request in [
        Request::get("/files?access_token=secret&page=2").body(()),
        Request::get("x-1://host/a").body(()),
    ] {
        service
            .clone()
            .oneshot(request.expect("static request"))
            .await
            .expect("infallible");
    }

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 2);
    let attribute = |span: usize, key: &'static str| {
        spans[span]
            .attributes
            .iter()
            .find(|attribute| attribute.key == Key::from_static_str(key))
            .map(|attribute| attribute.value.clone())
    };
    assert_eq!(
        attribute(0, URL_QUERY),
        Some(Value::from("access_token=REDACTED&page=2"))
    );
    assert_eq!(attribute(0, URL_SCHEME), Some(Value::from("http")));
    assert_eq!(attribute(1, URL_SCHEME), Some(Value::from("_OTHER")));
    assert_eq!(attribute(1, URL_QUERY), None);
}

#[derive(Debug, Clone, Copy, Default)]
struct ApiKeyDecorator;

impl SpanDecorator for ApiKeyDecorator {
    const SENSITIVE_QUERY_KEYS: &'static [&'static str] = &["X-Api-Key"];
}

#[tokio::test]
async fn server_spans_redact_the_decorators_query_keys() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let service = ServiceBuilder::new()
        .layer(OtelTraceLayer::<ApiKeyDecorator>::default())
        .service_fn(|_request: Request<()>| async {
            Ok::<_, Infallible>(Response::new(String::new()))
        });
    service
        .oneshot(
            Request::get("/files?x-api-key=abc&access_token=def&page=2")
                .body(())
                .expect("static request"),
        )
        .await
        .expect("infallible");

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let query = spans[0]
        .attributes
        .iter()
        .find(|attribute| attribute.key == Key::from_static_str(URL_QUERY))
        .map(|attribute| attribute.value.clone());
    assert_eq!(
        query,
        Some(Value::from(
            "x-api-key=REDACTED&access_token=REDACTED&page=2"
        ))
    );
}

#[tokio::test]
async fn client_spans_redact_the_configured_query_keys() {
    use tower_telemetry::OtelClientTraceLayer;

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let client = ServiceBuilder::new()
        .layer(OtelClientTraceLayer::with_redacted_query_keys(&[
            "x-api-key",
        ]))
        .service_fn(|_request: Request<String>| async {
            Ok::<_, Infallible>(Response::new(String::new()))
        });
    client
        .oneshot(
            Request::get("https://api.example/v1?X-API-KEY=abc&token=def&page=2")
                .body(String::new())
                .expect("static request"),
        )
        .await
        .expect("infallible");

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let url = spans[0]
        .attributes
        .iter()
        .find(|attribute| attribute.key == Key::from_static_str(URL_FULL))
        .map(|attribute| attribute.value.clone());
    assert_eq!(
        url,
        Some(Value::from(
            "https://api.example/v1?X-API-KEY=REDACTED&token=REDACTED&page=2"
        )),
        "configured names extend the built-in list, matched without case"
    );
}
