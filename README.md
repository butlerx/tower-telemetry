# tower-telemetry

OpenTelemetry spans and metrics as [Tower](https://docs.rs/tower) layers:

- **HTTP**: one server span per request, plus request metrics, for [axum](https://docs.rs/axum) and plain [hyper](https://docs.rs/hyper).
- **Messaging**: spans for any broker (SQS, Kafka, NATS, ...), trace context carried in message headers, and a layer for consumers.

Spans follow OpenTelemetry's [HTTP](https://opentelemetry.io/docs/specs/semconv/http/http-spans/) and [messaging](https://opentelemetry.io/docs/specs/semconv/messaging/messaging-spans/) conventions and go through [`tracing`](https://docs.rs/tracing). Export them with [`tracing-opentelemetry`](https://docs.rs/tracing-opentelemetry). Metrics go through the [`metrics`](https://docs.rs/metrics) facade. Install any exporter you like.

## Features

| Feature           | Default | Adds                                                                 |
| ----------------- | ------- | -------------------------------------------------------------------- |
| `trace`           | yes     | `OtelTraceLayer`: HTTP server spans                                  |
| `metrics`         | yes     | `HttpMetricsLayer`, `install_panic_counter`                          |
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

// Return `traceparent` to callers and skip AWS load balancer health checks.
let layer = OtelTraceLayer::<PropagatingSpanDecorator, HealthcheckFilter>::default();
```

## HTTP metrics

| Default name                             | Type      | Recorded when            |
| ---------------------------------------- | --------- | ------------------------ |
| `http_server_requests_total`             | counter   | response headers ready   |
| `http_server_time_to_first_byte_seconds` | histogram | response headers ready   |
| `http_server_request_duration_seconds`   | histogram | response body finished   |
| `http_server_request_body_size_bytes`    | histogram | from `Content-Length`    |
| `http_server_response_body_size_bytes`   | histogram | bytes actually sent      |
| `http_server_active_requests`            | gauge     | until body finished      |

Labels: `method`, `route`, `status_code`, `host` (opt-in with `with_host_label(true)`), plus any `MetricLabels` a handler puts in the response extensions. Rename metrics with `HttpMetricsLayer::default().with_names(MetricNames { .. })`. `DURATION_BUCKETS` and `BODY_SIZE_BUCKETS` are there for exporters that take histogram buckets.

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

Consumer: implement `Message` for your message type and wrap your handler in `MessageTraceLayer`. Each message gets a `process orders` span that continues the producer's trace. For batches, `link_producers` links a span to each message's producer.

## Forwarding headers

Host, client address and scheme are read from `X-Forwarded-Host`, `X-Client-IP`/`X-Forwarded-For` and `X-Forwarded-Proto` when present. Run behind a proxy that sets or strips them.
