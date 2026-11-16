//! [`MessagingOperation`]: the operation a messaging span covers.

use std::fmt::{self, Display};

/// What a messaging span covers (`messaging.operation.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessagingOperation {
    /// Creating a message to send later, e.g. adding it to a batch.
    Create,
    /// Sending one or more messages to a broker.
    Send,
    /// Pulling one or more messages from a broker.
    Receive,
    /// Handling a message, whether pushed or pulled.
    Process,
    /// Acknowledging, rejecting or otherwise settling a message.
    Settle,
}

impl MessagingOperation {
    /// The `messaging.operation.type` value.
    #[must_use]
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
