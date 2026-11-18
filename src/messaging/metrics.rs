//! OpenTelemetry's messaging metrics, recorded alongside the spans when the
//! `metrics` feature is on too. Without it, [`Recorder`] records nothing.

#[cfg(feature = "metrics")]
pub(super) use enabled::Recorder;

#[cfg(not(feature = "metrics"))]
pub(super) use disabled::Recorder;

/// The recorder, with the `metrics` feature on.
#[cfg(feature = "metrics")]
mod enabled {
    use std::sync::Once;
    use std::time::Instant;

    use ::metrics::{
        Label, SharedString, Unit, counter, describe_counter, describe_histogram, histogram,
    };
    use opentelemetry_semantic_conventions::metric::{
        MESSAGING_CLIENT_CONSUMED_MESSAGES, MESSAGING_CLIENT_OPERATION_DURATION,
        MESSAGING_CLIENT_SENT_MESSAGES, MESSAGING_PROCESS_DURATION,
    };

    use crate::attribute::{
        ERROR_TYPE, MESSAGING_CONSUMER_GROUP_NAME, MESSAGING_DESTINATION_NAME,
        MESSAGING_OPERATION_NAME, MESSAGING_OPERATION_TYPE, MESSAGING_SYSTEM, SERVER_ADDRESS,
        SERVER_PORT,
    };
    use crate::messaging::{MessagingOperation, MessagingSpan};
    use crate::metrics::label;

    /// `messaging.system`: static for the conventions' well-known values,
    /// shared for the rest.
    #[inline]
    fn system(system: &str) -> SharedString {
        const KNOWN: [&str; 11] = [
            "aws_sqs",
            "kafka",
            "rabbitmq",
            "gcp_pubsub",
            "servicebus",
            "eventhubs",
            "eventgrid",
            "pulsar",
            "rocketmq",
            "activemq",
            "jms",
        ];
        KNOWN
            .into_iter()
            .find(|known| *known == system)
            .map_or_else(|| label::shared(system), SharedString::const_str)
    }

    /// Describes the metrics' units to the recorder, once per process.
    static DESCRIBE: Once = Once::new();

    /// Describe each metric's unit, so exporters can name them:
    /// `messaging_process_duration_seconds` in Prometheus with recommended
    /// naming.
    fn describe() {
        DESCRIBE.call_once(|| {
            describe_histogram!(
                MESSAGING_CLIENT_OPERATION_DURATION,
                Unit::Seconds,
                "Duration of messaging operations: send, receive, settle and create."
            );
            describe_histogram!(
                MESSAGING_PROCESS_DURATION,
                Unit::Seconds,
                "Duration of processing one message."
            );
            describe_counter!(
                MESSAGING_CLIENT_SENT_MESSAGES,
                Unit::Count,
                "Messages a producer attempted to send to the broker."
            );
            describe_counter!(
                MESSAGING_CLIENT_CONSUMED_MESSAGES,
                Unit::Count,
                "Messages delivered to the application."
            );
        });
    }

    /// One operation's metrics, from its start to [`finish`](Self::finish).
    pub(in crate::messaging) struct Recorder {
        /// The duration histogram to record into.
        duration: &'static str,
        /// The counter to add to, and how much, if any.
        count: Option<(&'static str, u64)>,
        /// The operation's labels, without `error.type`.
        labels: Vec<Label>,
        /// When the operation started.
        started: Instant,
    }

    impl Recorder {
        /// Start timing `span`'s operation.
        pub(in crate::messaging) fn start(span: &MessagingSpan<'_>) -> Self {
            describe();
            let operation = span.operation;
            let (duration, count) = match operation {
                MessagingOperation::Process => (
                    MESSAGING_PROCESS_DURATION,
                    Some((MESSAGING_CLIENT_CONSUMED_MESSAGES, 1)),
                ),
                MessagingOperation::Send => (
                    MESSAGING_CLIENT_OPERATION_DURATION,
                    Some((
                        MESSAGING_CLIENT_SENT_MESSAGES,
                        span.batch_message_count.unwrap_or(1),
                    )),
                ),
                MessagingOperation::Create
                | MessagingOperation::Receive
                | MessagingOperation::Settle => (MESSAGING_CLIENT_OPERATION_DURATION, None),
            };
            let labels = [
                Some(Label::new(MESSAGING_SYSTEM, system(span.system))),
                Some(Label::new(
                    MESSAGING_OPERATION_NAME,
                    span.operation_name.map_or_else(
                        || SharedString::const_str(operation.as_str()),
                        label::shared,
                    ),
                )),
                // `process` has no operation type label in the conventions.
                (operation != MessagingOperation::Process)
                    .then(|| Label::new(MESSAGING_OPERATION_TYPE, operation.as_str())),
                span.destination_template
                    .or(span.destination)
                    .map(|name| Label::new(MESSAGING_DESTINATION_NAME, label::shared(name))),
                span.consumer_group
                    .map(|name| Label::new(MESSAGING_CONSUMER_GROUP_NAME, label::shared(name))),
                span.server_address
                    .map(|address| Label::new(SERVER_ADDRESS, label::shared(address))),
                span.server_port
                    .map(|port| Label::new(SERVER_PORT, label::port(port))),
            ]
            .into_iter()
            .flatten()
            .collect();
            Self {
                duration,
                count,
                labels,
                started: Instant::now(),
            }
        }

        /// Record the operation's duration and count, with `error.type`
        /// `_OTHER` if it failed.
        pub(in crate::messaging) fn finish(self, failed: bool) {
            let Self {
                duration,
                count,
                labels,
                started,
            } = self;
            let labels: Vec<Label> = labels
                .into_iter()
                .chain(failed.then(|| Label::new(ERROR_TYPE, "_OTHER")))
                .collect();
            histogram!(duration, labels.clone()).record(started.elapsed().as_secs_f64());
            if let Some((name, count)) = count {
                counter!(name, labels).increment(count);
            }
        }
    }
}

/// A recorder that records nothing, with the `metrics` feature off.
#[cfg(not(feature = "metrics"))]
mod disabled {
    use crate::messaging::MessagingSpan;

    /// Records nothing: the `metrics` feature is off.
    pub(in crate::messaging) struct Recorder;

    impl Recorder {
        /// Nothing to start.
        pub(in crate::messaging) const fn start(_span: &MessagingSpan<'_>) -> Self {
            Self
        }

        /// Nothing to record.
        pub(in crate::messaging) fn finish(self, _failed: bool) {
            let Self = self;
        }
    }
}
