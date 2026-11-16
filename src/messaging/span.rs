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
    /// A span for `operation` on `system`, with no other attributes yet.
    ///
    /// `system` is `messaging.system`: use the conventions' value for the broker
    /// (`aws_sqs`, `kafka`, `rabbitmq`, `gcp_pubsub`, ...; see the
    /// [module docs](super#messagingsystem)). Set what else the client knows with
    /// the other methods, then call [`start`](Self::start).
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

    /// The client's own name for the operation (`messaging.operation.name`):
    /// `SendMessage`, `publish`, `ack`, `DeleteMessage`. Without it, the
    /// operation type (`send`, `settle`, ...) is used.
    pub fn operation_name(self, name: &'a str) -> Self {
        Self {
            operation_name: Some(name),
            ..self
        }
    }

    /// The queue, topic or subject (`messaging.destination.name`), e.g. `orders`.
    /// Names the span: `send orders`. For a destination with identifiers in its
    /// name, also set [`destination_template`](Self::destination_template).
    pub fn destination(self, name: &'a str) -> Self {
        Self {
            destination: Some(name),
            ..self
        }
    }

    /// The destination's low-cardinality pattern
    /// (`messaging.destination.template`), e.g. `replies.{client}` for
    /// `replies.4f2a`. When set, it names the span instead of the destination,
    /// so temporary or per-tenant queues don't make every span name unique.
    pub fn destination_template(self, template: &'a str) -> Self {
        Self {
            destination_template: Some(template),
            ..self
        }
    }

    /// The id the broker gave the message (`messaging.message.id`). Only for
    /// spans about one message; for a `send` span, the id is often only known
    /// after sending: record it then with
    /// `span.record("messaging.message.id", id)`.
    pub fn message_id(self, id: &'a str) -> Self {
        Self {
            message_id: Some(id),
            ..self
        }
    }

    /// The id that ties related messages together
    /// (`messaging.message.conversation_id`), such as a correlation id for
    /// request and reply.
    pub fn conversation_id(self, id: &'a str) -> Self {
        Self {
            conversation_id: Some(id),
            ..self
        }
    }

    /// The message body's size in bytes (`messaging.message.body.size`), for
    /// spans about one message.
    pub fn body_size(self, bytes: u64) -> Self {
        Self {
            body_size: Some(bytes),
            ..self
        }
    }

    /// The number of messages, for spans covering more than one
    /// (`messaging.batch.message_count`). For a `receive` span, the count is only
    /// known after the pull: record it then with
    /// `span.record("messaging.batch.message_count", count)`.
    pub fn batch_message_count(self, count: u64) -> Self {
        Self {
            batch_message_count: Some(count),
            ..self
        }
    }

    /// The consumer group, subscription or queue group reading the message
    /// (`messaging.consumer.group.name`). For `receive`, `process` and `settle`
    /// spans.
    pub fn consumer_group(self, name: &'a str) -> Self {
        Self {
            consumer_group: Some(name),
            ..self
        }
    }

    /// The broker's address (`server.address`), such as a hostname or queue URL
    /// host, and port (`server.port`) when it isn't the protocol's default.
    pub fn server(self, address: &'a str, port: Option<u16>) -> Self {
        Self {
            server_address: Some(address),
            server_port: port,
            ..self
        }
    }

    /// Make the span a child of `context`, rather than of the current span.
    ///
    /// Use it with [`extract_context`](super::extract_context) to continue a
    /// producer's trace by hand. A context with no valid span (a message without
    /// trace headers) makes the span the root of a new trace.
    pub fn parent(self, context: opentelemetry::Context) -> Self {
        Self {
            parent: Some(context),
            ..self
        }
    }

    /// The name [`start`](Self::start) gives the span: `"{operation}
    /// {destination}"`, with the template in place of the destination when set,
    /// or the operation alone without either.
    #[must_use]
    pub fn name(&self) -> String {
        match self.destination_template.or(self.destination) {
            Some(destination) => format!("{} {destination}", self.operation),
            None => self.operation.to_string(),
        }
    }

    /// Create the span, with every attribute set so far.
    ///
    /// The span starts now and ends when the last clone of the returned [`Span`]
    /// is dropped. Instrument the operation's future with it
    /// (`future.instrument(span)`), or enter it around synchronous work, so
    /// the operation's own spans and logs nest under it. It declares
    /// `otel.status_code`, `error.type`, `exception.message`,
    /// `messaging.message.id` and `messaging.batch.message_count`, so those can
    /// be recorded later with `span.record`.
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
