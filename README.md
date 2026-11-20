# tower-telemetry

[![crates.io](https://img.shields.io/crates/v/tower-telemetry.svg)](https://crates.io/crates/tower-telemetry)
[![docs.rs](https://img.shields.io/docsrs/tower-telemetry)](https://docs.rs/tower-telemetry)
[![CI](https://github.com/butlerx/tower-telemetry/actions/workflows/ci.yml/badge.svg)](https://github.com/butlerx/tower-telemetry/actions/workflows/ci.yml)
[![MIT](https://img.shields.io/crates/l/tower-telemetry.svg)](LICENSE)

OpenTelemetry spans and metrics as [Tower](https://docs.rs/tower) layers:

- **HTTP servers**: one server span per request, plus request metrics, for [axum](https://docs.rs/axum) and plain [hyper](https://docs.rs/hyper).
- **HTTP clients**: one client span per outgoing request, with the trace context sent along, plus request metrics.
- **Messaging**: spans for any broker (SQS, Kafka, NATS, ...), trace context carried in message headers, and a layer for consumers.

Spans follow OpenTelemetry's [HTTP](https://opentelemetry.io/docs/specs/semconv/http/http-spans/) and [messaging](https://opentelemetry.io/docs/specs/semconv/messaging/messaging-spans/) conventions and go through [`tracing`](https://docs.rs/tracing). Export them with [`tracing-opentelemetry`](https://docs.rs/tracing-opentelemetry). Metrics go through the [`metrics`](https://docs.rs/metrics) facade. Install any exporter you like.

## Install

```sh
cargo add tower-telemetry                      # HTTP spans and metrics
cargo add tower-telemetry --features axum      # name them by axum's matched route
cargo add tower-telemetry --features messaging # messaging spans
```

Needs Rust 1.88 or later.

## Features

| Feature           | Default | Adds                                                                 |
| ----------------- | ------- | -------------------------------------------------------------------- |
| `trace`           | yes     | `OtelTraceLayer` and `OtelClientTraceLayer`: HTTP server and client spans |
| `metrics`         | yes     | `HttpMetricsLayer`, `HttpClientMetricsLayer`, `install_panic_counter` |
| `axum`            | no      | Names spans and metrics by axum's matched route (`GET /users/{id}`)  |
| `messaging`       | no      | `MessagingSpan`, `inject_context`/`extract_context`, `MessageTraceLayer` |
| `runtime-metrics` | no      | `export_runtime_metrics`: Tokio runtime gauges, sampled every second |

## axum

```rust
use axum::{Router, routing::get};
use tower_telemetry::{HttpMetricsLayer, OtelTraceLayer};

let app: Router = Router::new()
    .route("/users/{id}", get(|| async { "hello" }))
    .layer(HttpMetricsLayer::default())
    .layer(<OtelTraceLayer>::default());
```

Add the layers with `Router::layer` so the matched route is known. See [`examples/axum.rs`](examples/axum.rs) for tracer and Prometheus setup.

## hyper

The layers are plain Tower middleware. Wrap your service with `ServiceBuilder` and serve it with `hyper_util::service::TowerToHyperService`. See [`examples/hyper.rs`](examples/hyper.rs).

## Customising HTTP spans

`OtelTraceLayer<D, F>` takes a `SpanDecorator` (span names and extra fields) and a `RequestFilter` (requests to skip):

```rust
use tower_telemetry::{HealthcheckFilter, OtelTraceLayer, PropagatingSpanDecorator};

// Return `traceparent` to callers and skip health checks.
let layer = OtelTraceLayer::<PropagatingSpanDecorator, HealthcheckFilter>::default();
```

A decorator can also name query parameters whose values the span's `url.query` redacts. Its names are added to the built-in list, never replacing it:

```rust
use tower_telemetry::{OtelTraceLayer, SpanDecorator};

#[derive(Debug, Clone, Copy, Default)]
struct ApiKeyDecorator;

impl SpanDecorator for ApiKeyDecorator {
    const SENSITIVE_QUERY_KEYS: &'static [&'static str] = &["x-api-key"];
}

let layer = OtelTraceLayer::<ApiKeyDecorator>::default();
```

## Request filters

A `RequestFilter` drops requests from tracing before their span is made. It doesn't affect metrics. Implement it on a unit struct:

```rust
use http::Request;
use tower_telemetry::{DefaultSpanDecorator, HealthcheckFilter, OtelTraceLayer, RequestFilter};

#[derive(Debug, Clone, Copy, Default)]
struct SkipProbes;

impl RequestFilter for SkipProbes {
    fn should_ignore<B>(request: &Request<B>) -> bool {
        matches!(request.uri().path(), "/metrics" | "/livez")
            || HealthcheckFilter::should_ignore(request)
    }
}

let layer = OtelTraceLayer::<DefaultSpanDecorator, SkipProbes>::default();
```

`HealthcheckFilter` skips `/health`, `/ready`, `/healthz` and `/healthcheck` (exact paths, with or without a trailing slash), plus well-known health checkers by user agent on any path: AWS ELB, ECS and Route 53, Kubernetes probes, Google Cloud and Azure load balancers, Azure Front Door, Envoy and Consul (`HEALTHCHECK_USER_AGENTS` has the list). Uptime monitors are still traced.

The filter runs on every request, so match on the method, path and headers only.

## Custom span attributes

Set attributes with `OpenTelemetrySpanExt::set_attribute` from `tracing-opentelemetry`. Don't use `span.record("my.field", ..)`. A `tracing` span drops any field it didn't declare when it was created, without warning.

From the request or response, in a `SpanDecorator`:

```rust
use http::{Request, Response};
use tower_telemetry::{OtelTraceLayer, SpanDecorator, mark_server_errors};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

#[derive(Debug, Clone, Copy, Default)]
struct TenantDecorator;

impl SpanDecorator for TenantDecorator {
    fn on_request<B>(request: &Request<B>, span: &Span) {
        if let Some(tenant) = request.headers().get("x-tenant-id").and_then(|v| v.to_str().ok()) {
            span.set_attribute("tenant.id", tenant.to_owned());
        }
    }

    fn on_response<B>(span: &Span, response: &mut Response<B>) {
        mark_server_errors(span, response); // keep 5xx marked as errors
    }
}

let layer = OtelTraceLayer::<TenantDecorator>::default();
```

From a handler, for what only it knows: the span is in the request's extensions.

```rust
async fn handler(Extension(span): Extension<Span>) -> &'static str {
    span.set_attribute("enduser.id", "42");
    "hello"
}
```

Messaging spans work the same way: call `set_attribute` on the `Span` that `MessagingSpan::start` returns.

## HTTP metrics

Metrics use OpenTelemetry's HTTP server names, plus a request counter and time to first byte, which the conventions don't define:

| Default name                     | Type      | Unit    | Prometheus (recommended naming)          | Recorded when          |
| -------------------------------- | --------- | ------- | ---------------------------------------- | ---------------------- |
| `http.server.requests`           | counter   | count   | `http_server_requests_total`             | response headers ready |
| `http.server.time_to_first_byte` | histogram | seconds | `http_server_time_to_first_byte_seconds` | response headers ready |
| `http.server.request.duration`   | histogram | seconds | `http_server_request_duration_seconds`   | response body finished |
| `http.server.request.body.size`  | histogram | bytes   | `http_server_request_body_size_bytes`    | from `Content-Length`  |
| `http.server.response.body.size` | histogram | bytes   | `http_server_response_body_size_bytes`   | bytes actually sent    |
| `http.server.active_requests`    | gauge     | count   | `http_server_active_requests`            | until body finished    |

The layer describes each metric's unit, so install your exporter before building the app. With Prometheus, use `PrometheusBuilder::new().with_recommended_naming(true)` to get the names above; without it the unit and `_total` suffixes are left off. DogStatsD sends the dotted names as they are.

Labels are OpenTelemetry's: `http.request.method`, `url.scheme` (`http` or `https`, `_OTHER` for anything else), `network.protocol.version`, `http.route` (when a route matched), `http.response.status_code`, and `error.type` on `5xx` and failures. `server.address` and `server.port` are opt-in with `with_host_label(true)`. In Prometheus the dots become underscores (`http_request_method`). Add your own from a handler with `MetricLabels` in the response extensions. Keep their values few, since every distinct set is a new time series:

```rust
response
    .extensions_mut()
    .insert(MetricLabels(vec![("tenant", "acme".into())]));
```

Rename metrics to match existing dashboards:

```rust
let layer = HttpMetricsLayer::default().with_names(MetricNames {
    requests: "request_count",
    ..MetricNames::DEFAULT
});
```

This crate records but doesn't export: install any `metrics` exporter once per process. For exporters that take histogram buckets, `DURATION_BUCKETS` and `BODY_SIZE_BUCKETS` cover 1 ms to 5 minutes and 100 B to 10 GB.

## HTTP clients

Wrap the client you send requests with (a `hyper_util` `Client`, or any Tower service):

```rust
use tower::ServiceBuilder;
use tower_telemetry::{HttpClientMetricsLayer, OtelClientTraceLayer};

let client = ServiceBuilder::new()
    .layer(HttpClientMetricsLayer::default())
    .layer(OtelClientTraceLayer)
    .service(client);
```

Each request gets a client span named by its method, a child of the current span. Its trace context goes into the request's headers through the global propagator, so the server continues the trace. `url.full` is recorded without credentials, and with credential query values redacted: the same list as the server span's `url.query`, below. A `4xx` marks the span as an error, as the conventions ask for clients.

To redact more query parameters in `url.full`, add their names to the built-in list:

```rust
use tower_telemetry::OtelClientTraceLayer;

let layer = OtelClientTraceLayer::with_redacted_query_keys(&["x-api-key"]);
```

The metrics are the server's six under `http.client.*` (`MetricNames::CLIENT`), labelled with `http.request.method`, `server.address`, `server.port`, `network.protocol.version`, `http.response.status_code` and `error.type` (on `4xx`, `5xx` and failures).

## Process metrics

```rust
use tower_telemetry::metrics::{export_runtime_metrics, install_panic_counter};

install_panic_counter("process_panics_total"); // counts panics, then runs the previous hook
let sampler = export_runtime_metrics();        // `runtime-metrics`: tokio_runtime_* gauges, every second
```

## Messaging

Producer:

```rust
use std::collections::HashMap;
use tower_telemetry::messaging::{MessagingOperation, MessagingSpan, inject_context};

let mut headers = HashMap::new();
let span = MessagingSpan::new("aws_sqs", MessagingOperation::Send)
    .destination("orders")
    .start();
inject_context(&span, &mut headers); // send `headers` with the message
```

Consumer: implement `Message` for your message type and wrap your handler in `MessageTraceLayer`. Each message gets a `process orders` span that continues the producer's trace:

```rust
use std::collections::HashMap;
use tower::ServiceBuilder;
use tower_telemetry::messaging::{Message, MessageTraceLayer, extract_context};

struct Job {
    queue: String,
    headers: HashMap<String, String>,
}

impl Message for Job {
    fn system(&self) -> &str {
        "aws_sqs"
    }

    fn destination(&self) -> Option<&str> {
        Some(&self.queue)
    }

    fn context(&self) -> opentelemetry::Context {
        extract_context(&self.headers)
    }
}

let consumer = ServiceBuilder::new()
    .layer(MessageTraceLayer)
    .service_fn(|job: Job| async move { Ok::<_, std::io::Error>(()) });
```

A span that covers a batch can link to each message's producer with `link_producers`. Pulling (`receive`) and acknowledging (`settle`) get their own spans from `MessagingSpan`.

With the `metrics` feature too, `MessageTraceLayer` records `messaging.process.duration` and `messaging.client.consumed.messages`. For producers and pull consumers, `MessagingSpan::run` runs the operation inside the span, marks failures, and records `messaging.client.operation.duration` (plus `messaging.client.sent.messages` for `send`):

```rust
let id = MessagingSpan::new("aws_sqs", MessagingOperation::Send)
    .destination("orders")
    .run(|span| {
        let mut headers = HashMap::new();
        inject_context(span, &mut headers);
        client.send_message("orders", body, headers)
    })
    .await?;
```

The [`messaging` docs](https://docs.rs/tower-telemetry/latest/tower_telemetry/messaging/) walk through producing, carrying context in your client's own header type, pull consumers with `receive` and `settle` spans, errors and extra attributes.

## Forwarding headers

Host, client address and scheme are read from `X-Forwarded-Host`, `X-Client-IP`/`X-Forwarded-For` and `X-Forwarded-Proto` when present. Run behind a proxy that sets or strips them.

The server span's `url.query` keeps parameter names and redacts the values of credentials: signed-URL parameters (`X-Amz-Signature`, `X-Amz-Credential`, `X-Amz-Security-Token`, `X-Goog-Signature`, `Signature`, `sig`, `AWSAccessKeyId`) and token names (`access_token`, `refresh_token`, `id_token`, `token`, `api_key`, `client_secret`). Names match without regard to case, and a `SpanDecorator` can add its own. The span and the metrics record `url.scheme` only as `http`, `https` or `_OTHER`, so a client sending other schemes can't add metric series.
