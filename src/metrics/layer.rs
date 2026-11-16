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

/// Extra labels for a request's metrics, set by a handler as a response
/// extension.
///
/// Use it for what only the handler knows, such as the customer plan a
/// request was for. [`HttpMetricsLayer`] adds the labels to every metric it
/// records for the request, after `method`, `route` and `status_code`, then
/// removes the extension from the response.
///
/// ```
/// use tower_telemetry::MetricLabels;
///
/// let mut response = http::Response::new(());
/// response
///     .extensions_mut()
///     .insert(MetricLabels(vec![("plan", "enterprise".into())]));
/// ```
///
/// In axum, return `Extension(MetricLabels(...))` as part of the response; see
/// [`HttpMetricsLayer`].
///
/// Keep values few, since every distinct set is a new time series, and give
/// every response of a route the same keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricLabels(
    /// The labels, as `(key, value)` pairs. Keys are static; values are
    /// borrowed when they're static too, and owned when computed.
    pub Vec<(&'static str, Cow<'static, str>)>,
);

/// Records HTTP server metrics for each request through the [`metrics`]
/// facade.
///
/// This layer records; it doesn't export. Install one `metrics` exporter per
/// process (`metrics-exporter-prometheus`, `metrics-exporter-dogstatsd`, ...)
/// or the metrics go nowhere.
///
/// ```
/// use tower_telemetry::HttpMetricsLayer;
///
/// let layer = HttpMetricsLayer::default();
/// ```
///
/// # Metrics
///
/// Default names ([`MetricNames::DEFAULT`]):
///
/// | Name                                     | Type      | Unit    | Recorded                                  |
/// | ---------------------------------------- | --------- | ------- | ----------------------------------------- |
/// | `http_server_requests_total`             | counter   | requests | when the response headers are ready      |
/// | `http_server_time_to_first_byte_seconds` | histogram | seconds | when the response headers are ready       |
/// | `http_server_request_body_size_bytes`    | histogram | bytes   | when the headers are ready, from `Content-Length`, if over 0 |
/// | `http_server_request_duration_seconds`   | histogram | seconds | when the response body finishes or is dropped |
/// | `http_server_response_body_size_bytes`   | histogram | bytes   | when the response body finishes or is dropped, counting the bytes sent |
/// | `http_server_active_requests`            | gauge     | requests | up when a request arrives, down when its body finishes |
///
/// Duration and response size cover the whole response, streamed bodies
/// included, because the layer wraps the body in [`MetricsBody`]. If the
/// client goes away mid-response, they're recorded with what was sent so
/// far. If the request is dropped before any response, nothing is recorded,
/// but `active_requests` still comes back down.
///
/// # Labels
///
/// Every metric but `active_requests`, which has none, is labelled:
///
/// | Label         | Value                                                         |
/// | ------------- | ------------------------------------------------------------- |
/// | `method`      | the method; `_OTHER` for non-standard ones                    |
/// | `route`       | axum's matched route (feature `axum`), else empty             |
/// | `status_code` | the response status, or `error` when the inner service failed |
/// | `host`        | only with [`with_host_label(true)`](Self::with_host_label)    |
/// | yours         | any [`MetricLabels`] the response carries                     |
///
/// The route, never the path, keeps the number of series bounded. Without
/// axum, or behind a layer added from outside the router, `route` is empty
/// for every request: see the [crate docs](crate#axum) for where to add it.
///
/// # Adding labels from a handler
///
/// Put [`MetricLabels`] in the response's extensions. In axum, return it as
/// an `Extension`:
///
/// ```
/// use std::borrow::Cow;
/// use axum::Extension;
/// use tower_telemetry::MetricLabels;
///
/// async fn upload() -> (Extension<MetricLabels>, &'static str) {
///     let plan: Cow<'static, str> = "enterprise".into(); // looked up per request
///     (Extension(MetricLabels(vec![("plan", plan)])), "stored")
/// }
/// ```
///
/// Responses without it don't have those labels, so give every response of
/// a route the same keys. Keep the values few, since each distinct set is a
/// new time series: a plan or region, not a user or tenant id.
///
/// # Renaming metrics
///
/// Match an existing dashboard by setting [`MetricNames`]; unset names keep
/// their defaults:
///
/// ```
/// use tower_telemetry::{HttpMetricsLayer, MetricNames};
///
/// let layer = HttpMetricsLayer::default().with_names(MetricNames {
///     requests: "request_count",
///     duration: "request_time_taken",
///     ..MetricNames::DEFAULT
/// });
/// ```
///
/// # Histogram buckets
///
/// Exporters that take buckets need them for the histograms. The crate's
/// cover 1 ms to 5 minutes and 100 B to 10 GB. With Prometheus:
///
/// ```
/// use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
/// use tower_telemetry::MetricNames;
/// use tower_telemetry::metrics::{BODY_SIZE_BUCKETS, DURATION_BUCKETS};
///
/// # fn main() -> Result<(), metrics_exporter_prometheus::BuildError> {
/// let names = MetricNames::DEFAULT;
/// let builder = names
///     .durations()
///     .into_iter()
///     .map(|name| (name, &DURATION_BUCKETS[..]))
///     .chain(names.body_sizes().into_iter().map(|name| (name, &BODY_SIZE_BUCKETS[..])))
///     .try_fold(PrometheusBuilder::new(), |builder, (name, buckets)| {
///         builder.set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
///     })?;
/// // builder.install_recorder()?;
/// # let _ = builder;
/// # Ok(())
/// # }
/// ```
///
/// # Where to add it
///
/// In axum, with `Router::layer`, so the matched route is known. In hyper,
/// anywhere in a `tower::ServiceBuilder`. Its response type is
/// `Response<MetricsBody<B>>`, which axum and hyper both serve as they would
/// `Response<B>`.
///
/// [`metrics`]: ::metrics
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpMetricsLayer {
    /// The names to record under.
    names: MetricNames,
    /// Whether to label metrics with the request's host.
    host_label: bool,
}

impl HttpMetricsLayer {
    /// Record under `names` instead of [`MetricNames::DEFAULT`], e.g. to match
    /// existing dashboards. See [`MetricNames`] for an example.
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
///
/// You don't build it yourself: [`HttpMetricsLayer`] does, when it's added to
/// a router or `ServiceBuilder`. It passes requests through unchanged, and
/// answers with the inner service's response, its body wrapped in a
/// [`MetricsBody`]. It's `Clone` when the inner service is, as axum and hyper
/// need.
#[derive(Debug, Clone)]
pub struct HttpMetricsService<S> {
    /// The wrapped service.
    inner: S,
    /// The layer's settings, copied into each request's recorder.
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
        let pending = Pending {
            names,
            labels,
            started: Instant::now(),
            request_body_size: request::content_length(request.headers()),
            active: Active::new(names.active_requests),
        };
        MetricsFuture {
            pending: Some(pending),
            inner: self.inner.call(request),
        }
    }
}

/// Counts a request as active until dropped: also when the client goes away
/// and the future or body is dropped unfinished.
struct Active(&'static str);

impl Active {
    /// Count one more request in the gauge `name`.
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

/// A request waiting for its response: what [`MetricsFuture`] knows
/// before the response headers are ready. Dropped unanswered, it records
/// nothing; only its [`Active`] guard decrements the gauge.
struct Pending {
    /// The names to record under.
    names: MetricNames,
    /// The request's labels so far: `method`, `route` and maybe `host`.
    labels: Vec<Label>,
    /// When the request arrived.
    started: Instant,
    /// The declared request body size, from `Content-Length`.
    request_body_size: Option<u64>,
    /// Counts the request in `active_requests` until dropped.
    active: Active,
}

impl Pending {
    /// Record what is known once the response headers are ready, and hand
    /// the rest to a [`Recorder`] for the end of the body.
    #[expect(
        clippy::cast_precision_loss,
        reason = "histogram samples, not exact values"
    )]
    fn headers_ready(
        self,
        status: impl Into<SharedString>,
        extra: Option<MetricLabels>,
    ) -> Recorder {
        let Self {
            names,
            labels,
            started,
            request_body_size,
            active,
        } = self;
        let labels: Vec<Label> = labels
            .into_iter()
            .chain(std::iter::once(Label::new("status_code", status)))
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
    /// The request's labels, complete with `status_code` and any
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
    /// The future [`HttpMetricsService`] returns: the inner service's response
    /// future, plus the request's labels and start time.
    ///
    /// When the response is ready, it records the request count, time to first
    /// byte and request body size, and moves the rest of the recording into the
    /// response's [`MetricsBody`]. If the inner service fails, it records the
    /// request with `status_code` `error`. If it's dropped before either, it
    /// records nothing but still decrements `active_requests`.
    pub struct MetricsFuture<F> {
        // The inner service's response future.
        #[pin]
        inner: F,
        // Taken when the response is ready, and turned into its body's
        // recorder.
        pending: Option<Pending>,
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
                let status = response.status().as_str().to_owned();
                let recorder = pending.map(|pending| pending.headers_ready(status, extra));
                Ok(response.map(|inner| MetricsBody { inner, recorder }))
            }
            Err(err) => {
                // Recorded, with no body, as the recorder drops here.
                drop(pending.map(|pending| pending.headers_ready("error", None)));
                Err(err)
            }
        })
    }
}

pin_project_lite::pin_project! {
    /// The response body [`HttpMetricsService`] answers with: the inner body,
    /// counting the bytes it sends.
    ///
    /// It records the request's duration and response size when the body
    /// finishes, fails, or is dropped (the client went away), whichever comes
    /// first, then decrements `active_requests`. Frames, trailers, size hints and
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

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::pending;

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

    #[tokio::test]
    async fn requests_dropped_before_a_response_record_only_the_gauge() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _guard = ::metrics::set_default_local_recorder(&recorder);

        let mut service = ServiceBuilder::new()
            .layer(HttpMetricsLayer::default())
            .service(service_fn(|_request: Request<()>| async {
                pending::<Result<Response<Full<Bytes>>, Infallible>>().await
            }));
        let future = service
            .ready()
            .await
            .expect("infallible")
            .call(Request::new(()));
        drop(future);

        let snapshot = snapshotter.snapshot().into_vec();
        let names: Vec<_> = snapshot
            .iter()
            .map(|(key, ..)| key.key().name().to_owned())
            .collect();
        assert_eq!(names, [MetricNames::DEFAULT.active_requests]);
        assert_eq!(snapshot[0].3, DebugValue::Gauge(0.0.into()));
    }
}
