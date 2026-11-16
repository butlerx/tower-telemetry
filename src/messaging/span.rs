//! [`MessagingSpan`]: a messaging span's attributes, and starting it.

use tracing::Span;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::MessagingOperation;

/// A messaging span's attributes. Build one with [`MessagingSpan::new`],
/// set what the client knows, then [`start`](Self::start) it.
///
/// The span is named `"{operation} {destination}"` (`"send orders"`), or by
/// the operation alone without a destination. For destinations with
/// identifiers in their names (temporary queues, per-tenant topics), set
/// [`destination_template`](Self::destination_template): it names the span
/// instead.
#[derive(Debug, Clone)]
#[must_use = "a messaging span does nothing until started"]
pub struct MessagingSpan<'a> {
    /// `messaging.system`.
    pub(super) system: &'a str,
    /// `messaging.operation.type`, and the span kind.
    pub(super) operation: MessagingOperation,
    /// `messaging.operation.name`.
    pub(super) operation_name: Option<&'a str>,
    /// `messaging.destination.name`.
    pub(super) destination: Option<&'a str>,
    /// `messaging.destination.template`; names the span when set.
    pub(super) destination_template: Option<&'a str>,
    /// `messaging.message.id`.
    pub(super) message_id: Option<&'a str>,
    /// `messaging.message.conversation_id`.
    pub(super) conversation_id: Option<&'a str>,
    /// `messaging.message.body.size`.
    pub(super) body_size: Option<u64>,
    /// `messaging.batch.message_count`.
    pub(super) batch_message_count: Option<u64>,
    /// `messaging.consumer.group.name`.
    pub(super) consumer_group: Option<&'a str>,
    /// `server.address`.
    pub(super) server_address: Option<&'a str>,
    /// `server.port`.
    pub(super) server_port: Option<u16>,
    /// The parent context; the current span when unset.
    pub(super) parent: Option<opentelemetry::Context>,
}

impl<'a> MessagingSpan<'a> {
    /// A span for `operation` on `system` (`messaging.system`: `aws_sqs`,
    /// `kafka`, `rabbitmq`, `nats`, ... as the conventions name them).
    pub const fn new(system: &'a str, operation: MessagingOperation) -> Self {
        Self {
            system,
            operation,
            operation_name: None,
            destination: None,
            destination_template: None,
            message_id: None,
            conversation_id: None,
            body_size: None,
            batch_message_count: None,
            consumer_group: None,
            server_address: None,
            server_port: None,
            parent: None,
        }
    }

    /// The client's own name for the operation (`messaging.operation.name`),
    /// e.g. `SendMessage`, `publish`, `ack`. Defaults to the operation type.
    pub fn operation_name(self, name: &'a str) -> Self {
        Self {
            operation_name: Some(name),
            ..self
        }
    }

    /// The queue or topic (`messaging.destination.name`).
    pub fn destination(self, name: &'a str) -> Self {
        Self {
            destination: Some(name),
            ..self
        }
    }

    /// The low-cardinality pattern of the destination
    /// (`messaging.destination.template`), e.g. `/customers/{customerId}`.
    pub fn destination_template(self, template: &'a str) -> Self {
        Self {
            destination_template: Some(template),
            ..self
        }
    }

    /// The message's id (`messaging.message.id`), for single-message spans.
    pub fn message_id(self, id: &'a str) -> Self {
        Self {
            message_id: Some(id),
            ..self
        }
    }

    /// The conversation or correlation id
    /// (`messaging.message.conversation_id`).
    pub fn conversation_id(self, id: &'a str) -> Self {
        Self {
            conversation_id: Some(id),
            ..self
        }
    }

    /// The message body's size in bytes (`messaging.message.body.size`).
    pub fn body_size(self, bytes: u64) -> Self {
        Self {
            body_size: Some(bytes),
            ..self
        }
    }

    /// The number of messages, for batch operations
    /// (`messaging.batch.message_count`).
    pub fn batch_message_count(self, count: u64) -> Self {
        Self {
            batch_message_count: Some(count),
            ..self
        }
    }

    /// The consumer group or subscription (`messaging.consumer.group.name`).
    pub fn consumer_group(self, name: &'a str) -> Self {
        Self {
            consumer_group: Some(name),
            ..self
        }
    }

    /// The broker's address (`server.address`) and port (`server.port`).
    pub fn server(self, address: &'a str, port: Option<u16>) -> Self {
        Self {
            server_address: Some(address),
            server_port: port,
            ..self
        }
    }

    /// Parent the span on `context`, e.g. one from [`extract_context`](super::extract_context).
    /// Without it, the span's parent is the current span.
    pub fn parent(self, context: opentelemetry::Context) -> Self {
        Self {
            parent: Some(context),
            ..self
        }
    }

    /// The span's name: `"{operation} {destination}"`.
    #[must_use]
    pub fn name(&self) -> String {
        match self.destination_template.or(self.destination) {
            Some(destination) => format!("{} {destination}", self.operation),
            None => self.operation.to_string(),
        }
    }

    /// Start the span. Enter it, or instrument the operation's future with
    /// it, for the span to cover the operation.
    pub fn start(self) -> Span {
        let span = tracing::info_span!(
            target: "tower_telemetry::messaging",
            "messaging",
            otel.name = self.name(),
            otel.kind = self.operation.span_kind(),
            otel.status_code = Empty,
            messaging.system = self.system,
            messaging.operation.type = self.operation.as_str(),
            messaging.operation.name = self.operation_name.unwrap_or(self.operation.as_str()),
            messaging.destination.name = self.destination,
            messaging.destination.template = self.destination_template,
            messaging.message.id = self.message_id,
            messaging.message.conversation_id = self.conversation_id,
            messaging.message.body.size = self.body_size,
            messaging.batch.message_count = self.batch_message_count,
            messaging.consumer.group.name = self.consumer_group,
            server.address = self.server_address,
            server.port = self.server_port,
            error.type = Empty,
            exception.message = Empty,
        );
        if let Some(parent) = self.parent
            && let Err(err) = span.set_parent(parent)
        {
            tracing::debug!(error = %err, "could not set the messaging span's parent");
        }
        span
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_named_by_operation_and_destination() {
        let send = MessagingSpan::new("kafka", MessagingOperation::Send).destination("orders");
        assert_eq!(send.name(), "send orders");
        let templated = MessagingSpan::new("rabbitmq", MessagingOperation::Process)
            .destination("reply-8f3a")
            .destination_template("reply-{id}");
        assert_eq!(templated.name(), "process reply-{id}");
        assert_eq!(
            MessagingSpan::new("nats", MessagingOperation::Receive).name(),
            "receive"
        );
    }
}
