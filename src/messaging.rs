//! OpenTelemetry messaging spans, following the [messaging semantic
//! conventions], for any broker: SQS, Kafka, `RabbitMQ`, NATS, Pub/Sub and others.
//!
//! The pieces:
//!
//! - [`MessagingSpan`] builds a span for one operation on a broker.
//! - [`inject_context`] and [`extract_context`] carry the trace from producer
//!   to consumer in the message's headers.
//! - [`MessageTraceLayer`] wraps a consumer `Service` in a `process` span per
//!   message.
//! - [`link_producers`] links a batch span to the producer of each message.
//!
//! None of them depend on a broker client: you call them around your
//! client's own calls.
//!
//! # Which span for which operation
//!
//! | [`MessagingOperation`] | Span kind  | Wrap it around                                   |
//! | ---------------------- | ---------- | ------------------------------------------------ |
//! | `Create`               | `producer` | building one message for a later batch send      |
//! | `Send`                 | `producer` | publishing one or more messages                  |
//! | `Receive`              | `client`   | pulling messages from the broker                 |
//! | `Process`              | `consumer` | handling one message ([`MessageTraceLayer`])     |
//! | `Settle`               | `client`   | acknowledging, deleting or rejecting a message   |
//!
//! Spans are named `"{operation} {destination}"`, e.g. `send orders`.
//!
//! # `messaging.system`
//!
//! Use the conventions' well-known value for the broker, so backends
//! recognise it: `aws_sqs`, `kafka`, `rabbitmq`, `gcp_pubsub`, `servicebus`,
//! `eventhubs`, `eventgrid`, `pulsar`, `rocketmq`, `activemq` or `jms`. For
//! anything else, such as NATS, use its name in lower case.
//!
//! # Producing
//!
//! Start a `send` span, write its context into the message's headers, and
//! run the send inside it. Instrument the future rather than entering the
//! span, so the span follows the send across `.await`s.
//!
//! ```
//! use std::collections::HashMap;
//! use tower_telemetry::messaging::{MessagingOperation, MessagingSpan, inject_context};
//! use tracing::Instrument;
//! # struct Client;
//! # impl Client {
//! #     async fn send_message(&self, _queue: &str, _body: &str, _attributes: HashMap<String, String>) -> Result<String, std::io::Error> { Ok("id".into()) }
//! # }
//!
//! async fn publish(client: &Client, body: &str) -> Result<(), std::io::Error> {
//!     let span = MessagingSpan::new("aws_sqs", MessagingOperation::Send)
//!         .destination("orders")
//!         .operation_name("SendMessage")
//!         .body_size(body.len() as u64)
//!         .start();
//!     let mut attributes = HashMap::new();
//!     inject_context(&span, &mut attributes);
//!     let id = client
//!         .send_message("orders", body, attributes)
//!         .instrument(span.clone())
//!         .await?;
//!     span.record("messaging.message.id", id.as_str());
//!     Ok(())
//! }
//! ```
//!
//! # Carrying context in your client's headers
//!
//! `inject_context` and `extract_context` take OpenTelemetry's [`Injector`]
//! and [`Extractor`]. `HashMap<String, String>` has both; for any other
//! header type, implement them on a newtype. Kafka-style headers, as
//! `(key, bytes)` pairs:
//!
//! ```
//! use opentelemetry::propagation::{Extractor, Injector};
//!
//! struct Headers(Vec<(String, Vec<u8>)>);
//!
//! impl Injector for Headers {
//!     fn set(&mut self, key: &str, value: String) {
//!         self.0.push((key.to_owned(), value.into_bytes()));
//!     }
//! }
//!
//! impl Extractor for Headers {
//!     fn get(&self, key: &str) -> Option<&str> {
//!         self.0
//!             .iter()
//!             .find(|(name, _)| name == key)
//!             .and_then(|(_, value)| std::str::from_utf8(value).ok())
//!     }
//!
//!     fn keys(&self) -> Vec<&str> {
//!         self.0.iter().map(|(name, _)| name.as_str()).collect()
//!     }
//! }
//! ```
//!
//! The global propagator decides which headers are written:
//! `traceparent` and `tracestate` with the W3C propagator. Set it once at
//! startup with `opentelemetry::global::set_text_map_propagator`. SQS allows
//! at most 10 message attributes, so leave room for them.
//!
//! # Consuming, one message at a time
//!
//! Implement [`Message`] for your message type and wrap the handler in
//! [`MessageTraceLayer`]. Each message gets a `process` span that continues
//! the producer's trace, and is marked as an error when the handler fails.
//! The span is current while the handler runs, so its own spans and logs
//! nest under it.
//!
//! ```
//! use std::collections::HashMap;
//! use tower::{Service, ServiceBuilder, ServiceExt};
//! use tower_telemetry::messaging::{Message, MessageTraceLayer, extract_context};
//!
//! struct Job {
//!     id: String,
//!     body: String,
//!     attributes: HashMap<String, String>,
//! }
//!
//! impl Message for Job {
//!     fn system(&self) -> &str {
//!         "aws_sqs"
//!     }
//!
//!     fn destination(&self) -> Option<&str> {
//!         Some("orders")
//!     }
//!
//!     fn message_id(&self) -> Option<&str> {
//!         Some(&self.id)
//!     }
//!
//!     fn body_size(&self) -> Option<u64> {
//!         Some(self.body.len() as u64)
//!     }
//!
//!     fn context(&self) -> opentelemetry::Context {
//!         extract_context(&self.attributes)
//!     }
//! }
//!
//! async fn handle(job: Job) -> Result<(), std::io::Error> {
//!     tracing::info!(body = %job.body, "handling order"); // inside `process orders`
//!     Ok(())
//! }
//!
//! async fn consume(jobs: Vec<Job>) -> Result<(), std::io::Error> {
//!     let mut consumer = ServiceBuilder::new()
//!         .layer(MessageTraceLayer)
//!         .service_fn(handle);
//!     for job in jobs {
//!         consumer.ready().await?.call(job).await?;
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # Consuming a batch you pulled
//!
//! A pull consumer adds a `receive` span around the pull, linked to each
//! message's producer, and a `settle` span around the acknowledgement. The
//! `process` spans still come from [`MessageTraceLayer`], each parented on
//! its own producer.
//!
//! ```
//! # use std::collections::HashMap;
//! use opentelemetry::propagation::Extractor;
//! use tower::{Service, ServiceExt};
//! use tower_telemetry::messaging::{Message, MessagingOperation, MessagingSpan, link_producers};
//! use tracing::Instrument;
//! # struct Job { id: String, attributes: HashMap<String, String> }
//! # impl Message for Job { fn system(&self) -> &str { "aws_sqs" } }
//! # struct Client;
//! # impl Client {
//! #     async fn receive_messages(&self, _queue: &str) -> Result<Vec<Job>, std::io::Error> { Ok(Vec::new()) }
//! #     async fn delete_message(&self, _queue: &str, _id: &str) -> Result<(), std::io::Error> { Ok(()) }
//! # }
//!
//! async fn poll<S>(client: &Client, consumer: &mut S) -> Result<(), std::io::Error>
//! where
//!     S: Service<Job, Response = (), Error = std::io::Error>,
//! {
//!     let receive = MessagingSpan::new("aws_sqs", MessagingOperation::Receive)
//!         .destination("orders")
//!         .operation_name("ReceiveMessage")
//!         .start();
//!     let jobs = client
//!         .receive_messages("orders")
//!         .instrument(receive.clone())
//!         .await?;
//!     // Integers as `i64`: `tracing-opentelemetry` exports `u64` as a string.
//!     receive.record("messaging.batch.message_count", jobs.len() as i64);
//!     link_producers(&receive, jobs.iter().map(|job| &job.attributes as &dyn Extractor));
//!     drop(receive);
//!
//!     for job in jobs {
//!         let id = job.id.clone();
//!         consumer.ready().await?.call(job).await?;
//!         let settle = MessagingSpan::new("aws_sqs", MessagingOperation::Settle)
//!             .destination("orders")
//!             .operation_name("DeleteMessage")
//!             .message_id(&id)
//!             .start();
//!         client.delete_message("orders", &id).instrument(settle).await?;
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # Errors
//!
//! A [`MessagingSpan`] declares `otel.status_code`, `error.type` and
//! `exception.message`, so record a failure on it directly:
//!
//! ```
//! # use tower_telemetry::messaging::{MessagingOperation, MessagingSpan};
//! # let span = MessagingSpan::new("kafka", MessagingOperation::Send).start();
//! # let err = std::io::Error::other("broker unavailable");
//! span.record("otel.status_code", "ERROR");
//! span.record("error.type", "std::io::Error");
//! span.record("exception.message", tracing::field::display(&err));
//! ```
//!
//! # Other attributes
//!
//! Anything [`MessagingSpan`] has no setter for, such as a Kafka partition
//! or message key, goes on with `set_attribute` from `tracing-opentelemetry`.
//! `span.record` only works for fields the span declared when it started, and
//! drops any other without warning.
//!
//! ```
//! # use tower_telemetry::messaging::{MessagingOperation, MessagingSpan};
//! use tracing_opentelemetry::OpenTelemetrySpanExt;
//!
//! let span = MessagingSpan::new("kafka", MessagingOperation::Send)
//!     .destination("orders")
//!     .start();
//! span.set_attribute("messaging.destination.partition.id", "3");
//! span.set_attribute("messaging.kafka.message.key", "customer-42");
//! ```
//!
//! [`Injector`]: opentelemetry::propagation::Injector
//! [`Extractor`]: opentelemetry::propagation::Extractor
//! [messaging semantic conventions]: https://opentelemetry.io/docs/specs/semconv/messaging/messaging-spans/

mod consumer;
mod operation;
mod propagation;
mod span;

pub use consumer::{Message, MessageFuture, MessageTraceLayer, MessageTraceService};
pub use operation::MessagingOperation;
pub use propagation::{extract_context, inject_context, link_producers};
pub use span::MessagingSpan;
