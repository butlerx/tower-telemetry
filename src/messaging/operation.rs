//! [`MessagingOperation`]: the operation a messaging span covers.

use std::fmt::{self, Display};

/// The kind of operation a messaging span covers: its
/// `messaging.operation.type`, which also decides its span kind.
///
/// Pick the one that matches what the client call does, whatever the broker
/// calls it; record the broker's own name with
/// [`MessagingSpan::operation_name`](super::MessagingSpan::operation_name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessagingOperation {
    /// Creating a message to send later, as part of a batch: one span per
    /// message, each linked from the `Send` span that publishes the batch. Kind
    /// `producer`.
    Create,
    /// Sending one or more messages to the broker: `SendMessage`, `produce`,
    /// `publish`. Kind `producer`.
    Send,
    /// Pulling one or more messages from the broker: `ReceiveMessage`, `poll`,
    /// `fetch`. Kind `client`.
    Receive,
    /// Handling one message, whether it was pushed to you or you pulled it.
    /// [`MessageTraceLayer`](super::MessageTraceLayer) makes these. Kind
    /// `consumer`.
    Process,
    /// Telling the broker what became of a message: acknowledging, deleting,
    /// rejecting, or extending its visibility. Kind `client`.
    Settle,
}

impl MessagingOperation {
    /// The `messaging.operation.type` value: `create`, `send`, `receive`,
    /// `process` or `settle`. Also the first word of the span's name.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Send => "send",
            Self::Receive => "receive",
            Self::Process => "process",
            Self::Settle => "settle",
        }
    }

    /// The span kind the conventions give the operation: `producer` for
    /// create and send, `consumer` for process, `client` for receive and
    /// settle.
    #[must_use]
    #[inline]
    pub const fn span_kind(self) -> &'static str {
        match self {
            Self::Create | Self::Send => "producer",
            Self::Process => "consumer",
            Self::Receive | Self::Settle => "client",
        }
    }
}

impl Display for MessagingOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_have_the_conventional_span_kinds() {
        assert_eq!(MessagingOperation::Send.span_kind(), "producer");
        assert_eq!(MessagingOperation::Create.span_kind(), "producer");
        assert_eq!(MessagingOperation::Receive.span_kind(), "client");
        assert_eq!(MessagingOperation::Process.span_kind(), "consumer");
        assert_eq!(MessagingOperation::Settle.span_kind(), "client");
    }
}
