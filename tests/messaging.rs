//! A trace crosses a message: the consumer's `process` span continues the
//! producer's `send` span.

use std::collections::HashMap;
use std::convert::Infallible;

use opentelemetry::trace::{SpanKind, TracerProvider as _};
use opentelemetry::{Key, Value};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use opentelemetry_semantic_conventions::attribute::{
    MESSAGING_DESTINATION_NAME, MESSAGING_OPERATION_TYPE, MESSAGING_SYSTEM,
};
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
    let attribute = |key: &'static str| {
        process
            .attributes
            .iter()
            .find(|attribute| attribute.key == Key::from_static_str(key))
            .map(|attribute| attribute.value.clone())
    };
    assert_eq!(attribute(MESSAGING_SYSTEM), Some(Value::from("aws_sqs")));
    assert_eq!(
        attribute(MESSAGING_OPERATION_TYPE),
        Some(Value::from("process"))
    );
    assert_eq!(
        attribute(MESSAGING_DESTINATION_NAME),
        Some(Value::from("orders"))
    );
}

#[cfg(feature = "metrics")]
#[tokio::test]
async fn operations_and_processing_record_messaging_metrics() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use tower::{Service, ServiceBuilder, ServiceExt};
    use tower_telemetry::messaging::{
        Message, MessageTraceLayer, MessagingOperation, MessagingSpan,
    };

    struct Job;
    impl Message for Job {
        fn system(&self) -> &'static str {
            "kafka"
        }
        fn destination(&self) -> Option<&str> {
            Some("orders")
        }
    }

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let sent: Result<(), std::io::Error> = MessagingSpan::new("kafka", MessagingOperation::Send)
        .destination("orders")
        .batch_message_count(3)
        .run(|_span| async { Err(std::io::Error::other("broker down")) })
        .await;
    assert!(sent.is_err(), "the error is returned unchanged");

    let mut consumer = ServiceBuilder::new()
        .layer(MessageTraceLayer)
        .service_fn(|_job: Job| async { Ok::<_, std::io::Error>(()) });
    consumer
        .ready()
        .await
        .expect("ready")
        .call(Job)
        .await
        .expect("handled");

    let snapshot = snapshotter.snapshot().into_vec();
    let find = |name: &str| {
        snapshot
            .iter()
            .find(|(key, ..)| key.key().name() == name)
            .unwrap_or_else(|| panic!("{name} was not recorded"))
    };
    let labels = |name: &str| -> Vec<(String, String)> {
        find(name)
            .0
            .key()
            .labels()
            .map(|label| (label.key().to_owned(), label.value().to_owned()))
            .collect()
    };
    let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());

    assert_eq!(
        find("messaging.client.sent.messages").3,
        DebugValue::Counter(3)
    );
    assert_eq!(
        labels("messaging.client.operation.duration"),
        [
            pair("messaging.system", "kafka"),
            pair("messaging.operation.name", "send"),
            pair("messaging.operation.type", "send"),
            pair("messaging.destination.name", "orders"),
            pair("error.type", "_OTHER"),
        ]
    );
    assert_eq!(
        find("messaging.client.consumed.messages").3,
        DebugValue::Counter(1)
    );
    assert_eq!(
        labels("messaging.process.duration"),
        [
            pair("messaging.system", "kafka"),
            pair("messaging.operation.name", "process"),
            pair("messaging.destination.name", "orders"),
        ]
    );
}
