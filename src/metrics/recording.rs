//! What the server and client metrics layers share: recording a request
//! from arrival to the end of its response body.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use ::metrics::{
    Gauge, Label, SharedString, Unit, counter, describe_counter, describe_gauge,
    describe_histogram, gauge, histogram,
};
use bytes::Buf;
use http::{Response, StatusCode};
use http_body::{Body, Frame, SizeHint};

use super::{MetricLabels, MetricNames};
use crate::attribute::{ERROR_TYPE, HTTP_RESPONSE_STATUS_CODE};

/// Which end of the request the metrics are for. The conventions differ
/// in which statuses are errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    /// An incoming request this process answers.
    Server,
    /// An outgoing request this process sends.
    Client,
}

impl Side {
    /// Whether a response with `status` failed: a `5xx` for a server, which
    /// isn't at fault for a client's `4xx`, and any `4xx` or `5xx` for a
    /// client, which didn't get what it asked for.
    #[inline]
    fn is_error(self, status: StatusCode) -> bool {
        match self {
            Self::Server => status.is_server_error(),
            Self::Client => status.is_client_error() || status.is_server_error(),
        }
    }
}

/// Describe each metric's unit to the installed recorder, once per layer
/// applied. Exporters use it: Prometheus, with recommended naming, turns it
/// into the `_seconds`, `_bytes` and `_total` suffixes.
pub(super) fn describe(names: &MetricNames, side: Side) {
    let (requests, duration, time_to_first_byte, request_body_size, response_body_size, active) =
        match side {
            Side::Server => (
                "HTTP server requests answered.",
                "Duration of HTTP server requests, to the end of the response body.",
                "Time from an HTTP request arriving to its response headers being ready.",
                "Size of HTTP server request bodies, as declared by Content-Length.",
                "Size of HTTP server response bodies, as sent.",
                "HTTP server requests in progress.",
            ),
            Side::Client => (
                "HTTP client requests answered.",
                "Duration of HTTP client requests, to the end of the response body.",
                "Time from sending an HTTP request to its response headers arriving.",
                "Size of HTTP client request bodies.",
                "Size of HTTP client response bodies, as received.",
                "HTTP client requests in progress.",
            ),
        };
    describe_counter!(names.requests, Unit::Count, requests);
    describe_histogram!(names.duration, Unit::Seconds, duration);
    describe_histogram!(names.time_to_first_byte, Unit::Seconds, time_to_first_byte);
    describe_histogram!(names.request_body_size, Unit::Bytes, request_body_size);
    describe_histogram!(names.response_body_size, Unit::Bytes, response_body_size);
    describe_gauge!(names.active_requests, Unit::Count, active);
}

/// How a request ended, as its labels record it.
pub(super) enum Outcome {
    /// The service answered with this status.
    Answered(StatusCode),
    /// The service failed without answering.
    Failed,
}

impl Outcome {
    /// `http.response.status_code` when there's a response, and `error.type`
    /// when the request failed: the status code for an error status (see
    /// [`Side::is_error`]), `_OTHER` when there's no response, as the
    /// conventions ask.
    #[inline]
    fn labels(&self, side: Side) -> impl Iterator<Item = Label> {
        let (status, error) = match self {
            Self::Answered(status) => (
                Some(status.as_str().to_owned()),
                side.is_error(*status)
                    .then(|| status.as_str().to_owned().into()),
            ),
            Self::Failed => (None, Some(SharedString::const_str("_OTHER"))),
        };
        status
            .map(|status| Label::new(HTTP_RESPONSE_STATUS_CODE, status))
            .into_iter()
            .chain(error.map(|error| Label::new(ERROR_TYPE, error)))
    }
}

/// Counts a request as active until dropped: also when the client goes away
/// and the future or body is dropped unfinished.
pub(super) struct Active(Gauge);

impl Active {
    /// Count one more request in the gauge `name`, with `labels`.
    pub(super) fn new(name: &'static str, labels: Vec<Label>) -> Self {
        let gauge = gauge!(name, labels);
        gauge.increment(1);
        Self(gauge)
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.decrement(1);
    }
}

/// A request waiting for its response: what [`MetricsFuture`] knows
/// before the response headers are ready. Dropped unanswered, it records
/// nothing; only its [`Active`] guard decrements the gauge.
pub(super) struct Pending {
    /// The names to record under.
    pub(super) names: MetricNames,
    /// Which end of the request this is.
    pub(super) side: Side,
    /// The request's labels so far: everything but the outcome.
    pub(super) labels: Vec<Label>,
    /// When the request arrived or was sent.
    pub(super) started: Instant,
    /// The request body size, when known.
    pub(super) request_body_size: Option<u64>,
    /// Counts the request in `active_requests` until dropped.
    pub(super) active: Active,
}

impl Pending {
    /// Record what is known once the response headers are ready, and hand
    /// the rest to a [`Recorder`] for the end of the body.
    #[expect(
        clippy::cast_precision_loss,
        reason = "histogram samples, not exact values"
    )]
    fn headers_ready(self, outcome: &Outcome, extra: Option<MetricLabels>) -> Recorder {
        let Self {
            names,
            side,
            labels,
            started,
            request_body_size,
            active,
        } = self;
        let labels: Vec<Label> = labels
            .into_iter()
            .chain(outcome.labels(side))
            .chain(
                extra
                    .into_iter()
                    .flat_map(|MetricLabels(extra)| extra)
                    .map(|(key, value)| Label::new(key, value)),
            )
            .collect();
        counter!(names.requests, labels.clone()).increment(1);
        histogram!(names.time_to_first_byte, labels.clone())
            .record(started.elapsed().as_secs_f64());
        if let Some(size) = request_body_size.filter(|size| *size > 0) {
            histogram!(names.request_body_size, labels.clone()).record(size as f64);
        }
        Recorder {
            names,
            labels,
            started,
            response_body_size: 0,
            _active: active,
        }
    }
}

/// An answered request, carried to the end of its response body. Records
/// `duration` and `response_body_size` on drop.
struct Recorder {
    /// The names to record under.
    names: MetricNames,
    /// The request's labels, complete with its outcome and any
    /// [`MetricLabels`].
    labels: Vec<Label>,
    /// When the request arrived.
    started: Instant,
    /// Response body bytes sent so far.
    response_body_size: u64,
    /// Held only for its `Drop`.
    _active: Active,
}

impl Drop for Recorder {
    #[expect(
        clippy::cast_precision_loss,
        reason = "histogram samples, not exact values"
    )]
    fn drop(&mut self) {
        let labels = std::mem::take(&mut self.labels);
        histogram!(self.names.duration, labels.clone())
            .record(self.started.elapsed().as_secs_f64());
        histogram!(self.names.response_body_size, labels).record(self.response_body_size as f64);
    }
}

pin_project_lite::pin_project! {
    /// The future [`HttpMetricsService`](super::HttpMetricsService) and
    /// [`HttpClientMetricsService`](super::HttpClientMetricsService) return:
    /// the inner service's response future, plus the request's labels and
    /// start time.
    ///
    /// When the response is ready, it records the request count, time to first
    /// byte and request body size, and moves the rest of the recording into the
    /// response's [`MetricsBody`]. If the inner service fails, it records the
    /// request with `error.type` `_OTHER` and no status. If it's dropped before
    /// either, it records nothing but still decrements `active_requests`.
    pub struct MetricsFuture<F> {
        // The inner service's response future.
        #[pin]
        inner: F,
        // Taken when the response is ready, and turned into its body's
        // recorder.
        pending: Option<Pending>,
    }
}

impl<F> MetricsFuture<F> {
    /// Record the request `pending` once `inner` answers it.
    pub(super) const fn new(inner: F, pending: Pending) -> Self {
        Self {
            inner,
            pending: Some(pending),
        }
    }
}

impl<F, ResBody, E> Future for MetricsFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
    ResBody: Body,
{
    type Output = Result<Response<MetricsBody<ResBody>>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let result = ready!(this.inner.poll(cx));
        let pending = this.pending.take();
        Poll::Ready(match result {
            Ok(mut response) => {
                let extra = response.extensions_mut().remove::<MetricLabels>();
                let outcome = Outcome::Answered(response.status());
                let recorder = pending.map(|pending| pending.headers_ready(&outcome, extra));
                Ok(response.map(|inner| MetricsBody { inner, recorder }))
            }
            Err(err) => {
                // Recorded, with no body, as the recorder drops here.
                drop(pending.map(|pending| pending.headers_ready(&Outcome::Failed, None)));
                Err(err)
            }
        })
    }
}

pin_project_lite::pin_project! {
    /// The response body the metrics services answer with: the inner body,
    /// counting the bytes that pass through it.
    ///
    /// It records the request's duration and response size when the body
    /// finishes, fails, or is dropped (on a server, the client went away; on
    /// a client, the caller stopped reading), whichever comes first, then
    /// decrements `active_requests`. Frames, trailers, size hints and
    /// end of stream pass through unchanged, so it serves like the body it wraps.
    pub struct MetricsBody<B> {
        // The inner response body.
        #[pin]
        inner: B,
        // Taken, and so recorded, when the body finishes or fails.
        recorder: Option<Recorder>,
    }
}

impl<B: Body> Body for MetricsBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let frame = ready!(this.inner.poll_frame(cx));
        match &frame {
            Some(Ok(frame)) => {
                if let (Some(data), Some(recorder)) = (frame.data_ref(), this.recorder.as_mut()) {
                    recorder.response_body_size += data.remaining() as u64;
                }
            }
            // Finished or failed: record now, not when the body is dropped.
            Some(Err(_)) | None => drop(this.recorder.take()),
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
