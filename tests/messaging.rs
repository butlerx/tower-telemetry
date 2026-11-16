//! A trace crosses a message: the consumer's `process` span continues the
//! producer's `send` span.

use std::collections::HashMap;
use std::convert::Infallible;

use opentelemetry::trace::{SpanKind, TracerProvider as _};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tower::{ServiceBuilder, ServiceExt};
use tower_telemetry::messaging::{
    Message, MessageTraceLayer, MessagingOperation, MessagingSpan, extract_context, inject_context,
};
use tracing_subscriber::layer::SubscriberExt as _;

struct Job {
    headers: HashMap<String, String>,
}

impl Message for Job {
    fn system(&self) -> &'static str {
        "aws_sqs"
    }

    fn destination(&self) -> Option<&str> {
        Some("orders")
    }

    fn context(&self) -> opentelemetry::Context {
        extract_context(&self.headers)
    }
}

#[tokio::test]
async fn consumer_continues_the_producers_trace() {
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut headers = HashMap::new();
    {
        let span = MessagingSpan::new("aws_sqs", MessagingOperation::Send)
            .destination("orders")
            .start();
        inject_context(&span, &mut headers);
    }
    assert!(headers.contains_key("traceparent"));

    ServiceBuilder::new()
        .layer(MessageTraceLayer)
        .service_fn(|_job: Job| async { Ok::<_, Infallible>(()) })
        .oneshot(Job { headers })
        .await
        .expect("infallible");

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let send = spans
        .iter()
        .find(|span| span.name == "send orders")
        .expect("send span");
    let process = spans
        .iter()
        .find(|span| span.name == "process orders")
        .expect("process span");
    assert_eq!(send.span_kind, SpanKind::Producer);
    assert_eq!(process.span_kind, SpanKind::Consumer);
    assert_eq!(
        process.span_context.trace_id(),
        send.span_context.trace_id()
    );
    assert_eq!(process.parent_span_id, send.span_context.span_id());
}
