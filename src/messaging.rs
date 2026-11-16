//! OpenTelemetry messaging spans, following the [messaging semantic
//! conventions].
//!
//! Client-agnostic: [`MessagingSpan`] builds a span for any broker (SQS,
//! Kafka, NATS, `RabbitMQ`, ...), and [`inject_context`] / [`extract_context`]
//! carry the trace in message headers through OpenTelemetry's
//! [`Injector`] and [`Extractor`], which you implement for your client's
//! header type (`HashMap<String, String>` already has both).
//! [`MessageTraceLayer`] wraps a consumer service in a `process` span per
//! message.
//!
//! ```
//! use std::collections::HashMap;
//! use tower_telemetry::messaging::{MessagingOperation, MessagingSpan, inject_context};
//!
//! let mut headers = HashMap::new();
//! let span = MessagingSpan::new("aws_sqs", MessagingOperation::Send)
//!     .destination("orders")
//!     .operation_name("SendMessage")
//!     .start();
//! inject_context(&span, &mut headers);
//! // send the message with `headers` as its attributes, inside `span`
//! ```
//!
//! [`Injector`]: opentelemetry::propagation::Injector
//! [`Extractor`]: opentelemetry::propagation::Extractor
//! [messaging semantic conventions]:https://opentelemetry.io/docs/specs/semconv/messaging/messaging-spans/

mod consumer;
mod operation;
mod propagation;
mod span;

pub use consumer::{Message, MessageFuture, MessageTraceLayer, MessageTraceService};
pub use operation::MessagingOperation;
pub use propagation::{extract_context, inject_context, link_producers};
pub use span::MessagingSpan;
