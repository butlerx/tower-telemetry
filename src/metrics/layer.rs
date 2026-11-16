//! [`HttpMetricsLayer`]: request metrics through the [`metrics`](::metrics)
//! facade.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use std::borrow::Cow;

use ::metrics::{Label, SharedString, counter, gauge, histogram};
use bytes::Buf;
use http::{Request, Response};
use http_body::{Body, Frame, SizeHint};
use tower_layer::Layer;
use tower_service::Service;

use super::MetricNames;
use crate::request;

/// Extra labels for a request's metrics, as a response extension. Handlers
/// insert it once they know them, e.g. the tenant a request was for:
///
/// ```
/// use tower_telemetry::metrics::MetricLabels;
///
/// let mut response = http::Response::new(());
/// response
///     .extensions_mut()
///     .insert(MetricLabels(vec![("tenant", "acme".into())]));
/// ```
///
/// Keep the values few: every distinct set is a new time series. The layer
/// takes the extension out of the response once it has read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricLabels(pub Vec<(&'static str, Cow<'static, str>)>);

/// Records request metrics, named by [`MetricNames`]:
///
/// - `requests`, `time_to_first_byte` and `request_body_size` once the
///   response headers are ready;
/// - `duration` and `response_body_size` once the response body finishes, or
///   the client goes away mid-body;
/// - `active_requests` from the request arriving to the body finishing.
///
/// Labelled `method`, `route` (axum's matched route; empty without the
/// `axum` feature or when none matched), `status_code` (`error` when the
/// inner service failed), optionally `host`, and any [`MetricLabels`] the
/// response carries.
///
/// Add it with axum's `Router::layer`, not around the router from outside,
/// so the matched route is known.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpMetricsLayer {
    names: MetricNames,
    host_label: bool,
}

/// The default layer records under [`MetricNames::DEFAULT`], without `host`.
impl HttpMetricsLayer {
    /// Record under `names` instead.
    #[must_use]
    pub const fn with_names(self, names: MetricNames) -> Self {
        Self { names, ..self }
    }

    /// Label metrics with `host`: `X-Forwarded-Host`, else `Host`. Off by
    /// default, since the client picks it: turn it on only behind a proxy
    /// that restricts it to your own domains.
    #[must_use]
    pub const fn with_host_label(self, host_label: bool) -> Self {
        Self { host_label, ..self }
    }
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpMetricsService {
            inner,
            layer: *self,
        }
    }
}

/// The service [`HttpMetricsLayer`] wraps around an inner service.
#[derive(Debug, Clone)]
pub struct HttpMetricsService<S> {
    inner: S,
    layer: HttpMetricsLayer,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for HttpMetricsService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    ResBody: Body,
{
    type Response = Response<MetricsBody<ResBody>>;
    type Error = S::Error;
    type Future = MetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let HttpMetricsLayer { names, host_label } = self.layer;
        let labels = [
            Label::new("method", request::known_method(request.method())),
            Label::new(
                "route",
                request::route(&request).unwrap_or_default().to_owned(),
            ),
        ]
        .into_iter()
        .chain(host_label.then(|| Label::new("host", request::host(request.headers()).to_owned())))
        .collect();
        let recorder = Recorder {
            names,
            labels,
            started: Instant::now(),
            request_body_size: request::content_length(request.headers()),
            response_body_size: 0,
            _active: Active::new(names.active_requests),
        };
        MetricsFuture {
            recorder: Some(recorder),
            inner: self.inner.call(request),
        }
    }
}

/// Counts a request as active until dropped: also when the client goes away
/// and the future or body is dropped unfinished.
struct Active(&'static str);

impl Active {
    fn new(name: &'static str) -> Self {
        gauge!(name).increment(1);
        Self(name)
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        gauge!(self.0).decrement(1);
    }
}

/// A request's labels and timings, carried from the request to the end of
/// its response body. Records `duration` and `response_body_size` on drop.
struct Recorder {
    names: MetricNames,
    labels: Vec<Label>,
    started: Instant,
    request_body_size: Option<u64>,
    response_body_size: u64,
    /// Held only for its `Drop`.
    _active: Active,
}

impl Recorder {
    /// Record what is known once the response headers are ready, and keep
    /// the rest for the end of the body.
    #[expect(
        clippy::cast_precision_loss,
        reason = "histogram samples, not exact values"
    )]
    fn headers_ready(
        mut self,
        status: impl Into<SharedString>,
        extra: Option<MetricLabels>,
    ) -> Self {
        self.labels.extend(
            std::iter::once(Label::new("status_code", status)).chain(
                extra
                    .into_iter()
                    .flat_map(|MetricLabels(extra)| extra)
                    .map(|(key, value)| Label::new(key, value)),
            ),
        );
        counter!(self.names.requests, self.labels.clone()).increment(1);
        histogram!(self.names.time_to_first_byte, self.labels.clone())
            .record(self.started.elapsed().as_secs_f64());
        if let Some(size) = self.request_body_size.filter(|size| *size > 0) {
            histogram!(self.names.request_body_size, self.labels.clone()).record(size as f64);
        }
        self
    }
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
    /// Response future for [`HttpMetricsService`].
    pub struct MetricsFuture<F> {
        #[pin]
        inner: F,
        recorder: Option<Recorder>,
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
        let recorder = this.recorder.take();
        Poll::Ready(match result {
            Ok(mut response) => {
                let extra = response.extensions_mut().remove::<MetricLabels>();
                let status = response.status().as_str().to_owned();
                let recorder = recorder.map(|recorder| recorder.headers_ready(status, extra));
                Ok(response.map(|inner| MetricsBody { inner, recorder }))
            }
            Err(err) => {
                // Recorded, with no body, as the recorder drops here.
                drop(recorder.map(|recorder| recorder.headers_ready("error", None)));
                Err(err)
            }
        })
    }
}

pin_project_lite::pin_project! {
    /// Response body for [`HttpMetricsService`]: counts the bytes sent and
    /// records the request's duration when it finishes or is dropped.
    pub struct MetricsBody<B> {
        #[pin]
        inner: B,
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

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use tower::{ServiceBuilder, ServiceExt, service_fn};

    use super::*;

    #[tokio::test]
    async fn records_requests_with_labels_and_body_size() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _guard = ::metrics::set_default_local_recorder(&recorder);

        let service = ServiceBuilder::new()
            .layer(HttpMetricsLayer::default())
            .service(service_fn(|_request: Request<()>| async {
                let mut response = Response::new(Full::new(Bytes::from_static(b"hello")));
                response
                    .extensions_mut()
                    .insert(MetricLabels(vec![("tenant", "acme".into())]));
                Ok::<_, Infallible>(response)
            }));
        let response = service
            .oneshot(Request::post("/").body(()).expect("static request"))
            .await
            .expect("infallible");
        let body = response.into_body().collect().await.expect("infallible");
        assert_eq!(body.to_bytes(), "hello");

        let snapshot = snapshotter.snapshot().into_vec();
        let find = |name: &str| {
            snapshot
                .iter()
                .find(|(key, ..)| key.key().name() == name)
                .unwrap_or_else(|| panic!("{name} was not recorded"))
        };

        let (key, _, _, value) = find(MetricNames::DEFAULT.requests);
        assert_eq!(value, &DebugValue::Counter(1));
        let labels: Vec<_> = key
            .key()
            .labels()
            .map(|label| (label.key(), label.value()))
            .collect();
        assert_eq!(
            labels,
            [
                ("method", "POST"),
                ("route", ""),
                ("status_code", "200"),
                ("tenant", "acme"),
            ]
        );

        let (_, _, _, value) = find(MetricNames::DEFAULT.response_body_size);
        let DebugValue::Histogram(sizes) = value else {
            panic!("response_body_size is a histogram");
        };
        assert_eq!(sizes.len(), 1);
        assert!((sizes[0].into_inner() - 5.0).abs() < f64::EPSILON);

        let (_, _, _, value) = find(MetricNames::DEFAULT.active_requests);
        assert_eq!(value, &DebugValue::Gauge(0.0.into()));
    }
}
