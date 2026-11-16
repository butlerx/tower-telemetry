//! [`MessageTraceLayer`]: a `process` span around each message a consumer
//! handles.

use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use opentelemetry::trace::TraceContextExt;
use tower_layer::Layer;
use tower_service::Service;
use tracing::Span;

use super::{MessagingOperation, MessagingSpan};

/// A consumed message, as [`MessageTraceLayer`] needs to trace it.
///
/// Implement it for your client's message type. Only
/// [`system`](Self::system) is required, but implement
/// [`context`](Self::context) too, or every message starts a new trace
/// instead of continuing its producer's. See the
/// [module docs](super#consuming-one-message-at-a-time) for a full example.
pub trait Message {
    /// `messaging.system`, e.g. `aws_sqs` or `kafka`.
    fn system(&self) -> &str;

    /// The queue or topic it came from (`messaging.destination.name`).
    /// Names the span: `process {destination}`.
    fn destination(&self) -> Option<&str> {
        None
    }

    /// Its broker-assigned id (`messaging.message.id`).
    fn message_id(&self) -> Option<&str> {
        None
    }

    /// Its body size, in bytes (`messaging.message.body.size`).
    fn body_size(&self) -> Option<u64> {
        None
    }

    /// The consumer group or subscription it was read by
    /// (`messaging.consumer.group.name`).
    fn consumer_group(&self) -> Option<&str> {
        None
    }

    /// The producer's trace context, usually [`extract_context`](super::extract_context) over the
    /// message's headers. Default: none, so processing starts a new trace.
    fn context(&self) -> opentelemetry::Context {
        opentelemetry::Context::new()
    }
}

/// Wraps a consumer — a `Service` that handles one message — in a `process`
/// span per message, parented on the producer's trace context, and marks
/// the span as an error when the service fails.
///
/// ```
/// use tower::ServiceBuilder;
/// use tower_telemetry::messaging::{Message, MessageTraceLayer};
///
/// struct Job { queue: String }
///
/// impl Message for Job {
///     fn system(&self) -> &str { "aws_sqs" }
///     fn destination(&self) -> Option<&str> { Some(&self.queue) }
/// }
///
/// let consumer = ServiceBuilder::new()
///     .layer(MessageTraceLayer)
///     .service_fn(|job: Job| async move { Ok::<_, std::io::Error>(()) });
/// # let _ = consumer;
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct MessageTraceLayer;

impl<S> Layer<S> for MessageTraceLayer {
    type Service = MessageTraceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MessageTraceService { inner }
    }
}

/// The service [`MessageTraceLayer`] wraps around a consumer.
#[derive(Debug, Clone)]
pub struct MessageTraceService<S> {
    /// The wrapped consumer.
    inner: S,
}

impl<S, M> Service<M> for MessageTraceService<S>
where
    S: Service<M>,
    S::Error: Display,
    M: Message,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = MessageFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, message: M) -> Self::Future {
        let parent =
            Some(message.context()).filter(|parent| parent.span().span_context().is_valid());
        let span = MessagingSpan {
            destination: message.destination(),
            message_id: message.message_id(),
            body_size: message.body_size(),
            consumer_group: message.consumer_group(),
            parent,
            ..MessagingSpan::new(message.system(), MessagingOperation::Process)
        }
        .start();
        let inner = span.in_scope(|| self.inner.call(message));
        MessageFuture { inner, span }
    }
}

pin_project_lite::pin_project! {
    /// Response future for [`MessageTraceService`].
    pub struct MessageFuture<F> {
        // The consumer's future.
        #[pin]
        inner: F,
        // The message's `process` span.
        span: Span,
    }
}

impl<F, T, E> Future for MessageFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Display,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.span.enter();
        let result = ready!(this.inner.poll(cx));
        if let Err(err) = &result {
            this.span.record("otel.status_code", "ERROR");
            this.span.record("error.type", "_OTHER");
            this.span
                .record("exception.message", tracing::field::display(err));
        }
        Poll::Ready(result)
    }
}
