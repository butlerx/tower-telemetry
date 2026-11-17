//! [`HttpMetricsLayer`]: request metrics through the [`metrics`](::metrics)
//! facade.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use std::borrow::Cow;

use ::metrics::{
    Gauge, Label, SharedString, Unit, counter, describe_counter, describe_gauge,
    describe_histogram, gauge, histogram,
};
use bytes::Buf;
use http::{Request, Response, StatusCode};
use http_body::{Body, Frame, SizeHint};
use tower_layer::Layer;
use tower_service::Service;

use super::MetricNames;
use crate::attribute::{
    ERROR_TYPE, HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, HTTP_ROUTE,
    NETWORK_PROTOCOL_VERSION, SERVER_ADDRESS, URL_SCHEME,
};
use crate::request;

/// Extra labels for a request's metrics, set by a handler as a response
/// extension.
///
/// Use it for what only the handler knows, such as the customer plan a
/// request was for. [`HttpMetricsLayer`] adds the labels to every metric it
/// records for the request, after its own, then removes the extension from
/// the response.
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
/// Default names ([`MetricNames::DEFAULT`]) are OpenTelemetry's HTTP server
/// metrics, plus a request counter and time to first byte, which the
/// conventions don't define and which follow their style:
///
/// | Name                             | Type      | Unit    | Recorded                                   |
/// | -------------------------------- | --------- | ------- | ------------------------------------------ |
/// | `http.server.requests`           | counter   | count   | when the response headers are ready        |
/// | `http.server.time_to_first_byte` | histogram | seconds | when the response headers are ready        |
/// | `http.server.request.body.size`  | histogram | bytes   | when the headers are ready, from `Content-Length`, if over 0 |
/// | `http.server.request.duration`   | histogram | seconds | when the response body finishes or is dropped |
/// | `http.server.response.body.size` | histogram | bytes   | when the response body finishes or is dropped, counting the bytes sent |
/// | `http.server.active_requests`    | gauge     | count   | up when a request arrives, down when its body finishes |
///
/// Duration and response size cover the whole response, streamed bodies
/// included, because the layer wraps the body in [`MetricsBody`]. If the
/// client goes away mid-response, they're recorded with what was sent so
/// far. If the request is dropped before any response, nothing is recorded,
/// but `active_requests` still comes back down.
///
/// # How exporters name them
///
/// The layer describes each metric's unit to the recorder when it's
/// applied, so install your exporter before building the router or service.
///
/// | Exporter                                       | `http.server.request.duration` becomes  | Label keys            |
/// | ---------------------------------------------- | --------------------------------------- | --------------------- |
/// | Prometheus, `with_recommended_naming(true)`    | `http_server_request_duration_seconds`  | `http_request_method` |
/// | Prometheus, default                            | `http_server_request_duration`          | `http_request_method` |
/// | `DogStatsD`                                    | `http.server.request.duration`          | `http.request.method` |
///
/// For Prometheus, turn on recommended naming: it adds the unit (`_seconds`,
/// `_bytes`) from the description, and `_total` to the counter, giving
/// `http_server_requests_total`. Prometheus replaces the dots with
/// underscores either way.
///
/// # Labels
///
/// Every metric is labelled with OpenTelemetry's HTTP attributes, as the
/// conventions give them for each:
///
/// | Label                       | Value                                                     | On                 |
/// | --------------------------- | --------------------------------------------------------- | ------------------ |
/// | `http.request.method`       | the method; `_OTHER` for non-standard ones                | all                |
/// | `url.scheme`                | `X-Forwarded-Proto`, else the URI, else `http`            | all                |
/// | `network.protocol.version`  | `1.1`, `2`, ...                                           | all but active     |
/// | `http.route`                | axum's matched route (feature `axum`); left off when none | all but active     |
/// | `http.response.status_code` | the response status; left off when the service failed     | all but active     |
/// | `error.type`                | the status code for a `5xx`, `_OTHER` when the service failed; left off otherwise | all but active |
/// | `server.address`            | only with [`with_host_label(true)`](Self::with_host_label) | all               |
/// | yours                       | any [`MetricLabels`] the response carries                 | all but active     |
///
/// The route, never the path, keeps the number of series bounded. Without
/// axum, or behind a layer added from outside the router, there's no route:
/// see the [crate docs](crate#axum) for where to add the layer.
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
///     .try_fold(PrometheusBuilder::new().with_recommended_naming(true), |builder, (name, buckets)| {
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

    /// Label metrics with `server.address`: `X-Forwarded-Host`, else `Host`.
    /// The conventions make it opt-in, and so does this: the client picks
    /// it, so turn it on only behind a proxy that restricts it to your own
    /// domains, or every made-up host is a new time series.
    #[must_use]
    pub const fn with_host_label(self, host_label: bool) -> Self {
        Self { host_label, ..self }
    }
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        describe(&self.names);
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
        let method = Label::new(HTTP_REQUEST_METHOD, request::known_method(request.method()));
        let scheme = Label::new(URL_SCHEME, scheme_label(request::scheme(&request)));
        let host = host_label
            .then(|| Label::new(SERVER_ADDRESS, request::host(request.headers()).to_owned()));
        let active = Active::new(
            names.active_requests,
            [method.clone(), scheme.clone()]
                .into_iter()
                .chain(host.clone())
                .collect::<Vec<_>>(),
        );
        let labels = [
            method,
            scheme,
            Label::new(
                NETWORK_PROTOCOL_VERSION,
                request::http_version(request.version()),
            ),
        ]
        .into_iter()
        .chain(request::route(&request).map(|route| Label::new(HTTP_ROUTE, route.to_owned())))
        .chain(host)
        .collect();
        let pending = Pending {
            names,
            labels,
            started: Instant::now(),
            request_body_size: request::content_length(request.headers()),
            active,
        };
        MetricsFuture {
            pending: Some(pending),
            inner: self.inner.call(request),
        }
    }
}

/// Describe each metric's unit to the installed recorder, once per layer
/// applied. Exporters use it: Prometheus, with recommended naming, turns it
/// into the `_seconds`, `_bytes` and `_total` suffixes.
fn describe(names: &MetricNames) {
    describe_counter!(
        names.requests,
        Unit::Count,
        "HTTP server requests answered."
    );
    describe_histogram!(
        names.duration,
        Unit::Seconds,
        "Duration of HTTP server requests, to the end of the response body."
    );
    describe_histogram!(
        names.time_to_first_byte,
        Unit::Seconds,
        "Time from an HTTP request arriving to its response headers being ready."
    );
    describe_histogram!(
        names.request_body_size,
        Unit::Bytes,
        "Size of HTTP server request bodies, as declared by Content-Length."
    );
    describe_histogram!(
        names.response_body_size,
        Unit::Bytes,
        "Size of HTTP server response bodies, as sent."
    );
    describe_gauge!(
        names.active_requests,
        Unit::Count,
        "HTTP server requests in progress."
    );
}

/// `url.scheme` without allocating for the usual two.
fn scheme_label(scheme: &str) -> SharedString {
    match scheme {
        "https" => SharedString::const_str("https"),
        "http" => SharedString::const_str("http"),
        other => SharedString::from(other.to_owned()),
    }
}

/// How a request ended, as its labels record it.
enum Outcome {
    /// The service answered with this status.
    Answered(StatusCode),
    /// The service failed without answering.
    Failed,
}

impl Outcome {
    /// `http.response.status_code` when there's a response, and `error.type`
    /// when the request failed: the status code for a `5xx`, `_OTHER` when
    /// there's no response, as the conventions ask.
    fn labels(&self) -> impl Iterator<Item = Label> {
        let (status, error) = match self {
            Self::Answered(status) => (
                Some(status.as_str().to_owned()),
                status
                    .is_server_error()
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
struct Active(Gauge);

impl Active {
    /// Count one more request in the gauge `name`, with `labels`.
    fn new(name: &'static str, labels: Vec<Label>) -> Self {
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
struct Pending {
    /// The names to record under.
    names: MetricNames,
    /// The request's labels so far: method, scheme, protocol version, and
    /// route and host when known.
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
    fn headers_ready(self, outcome: &Outcome, extra: Option<MetricLabels>) -> Recorder {
        let Self {
            names,
            labels,
            started,
            request_body_size,
            active,
        } = self;
        let labels: Vec<Label> = labels
            .into_iter()
            .chain(outcome.labels())
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
                ("http.request.method", "POST"),
                ("url.scheme", "http"),
                ("network.protocol.version", "1.1"),
                ("http.response.status_code", "200"),
                ("tenant", "acme"),
            ]
        );

        let (_, unit, _, _) = find(MetricNames::DEFAULT.duration);
        assert_eq!(
            unit,
            &Some(Unit::Seconds),
            "units are described for exporters"
        );
        let (_, unit, _, _) = find(MetricNames::DEFAULT.response_body_size);
        assert_eq!(unit, &Some(Unit::Bytes));

        let (_, _, _, value) = find(MetricNames::DEFAULT.response_body_size);
        let DebugValue::Histogram(sizes) = value else {
            panic!("response_body_size is a histogram");
        };
        assert_eq!(sizes.len(), 1);
        assert!((sizes[0].into_inner() - 5.0).abs() < f64::EPSILON);

        let (key, _, _, value) = find(MetricNames::DEFAULT.active_requests);
        assert_eq!(value, &DebugValue::Gauge(0.0.into()));
        let labels: Vec<_> = key.key().labels().map(Label::key).collect();
        assert_eq!(labels, ["http.request.method", "url.scheme"]);
    }

    /// The outcome labels of `requests` for one request answered by
    /// `service`.
    async fn outcome_labels<S>(service: S) -> Vec<(String, String)>
    where
        S: Service<Request<()>, Response = Response<Full<Bytes>>, Error = std::io::Error>,
    {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _guard = ::metrics::set_default_local_recorder(&recorder);
        let _ = ServiceBuilder::new()
            .layer(HttpMetricsLayer::default())
            .service(service)
            .oneshot(Request::new(()))
            .await;
        snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .find(|(key, ..)| key.key().name() == MetricNames::DEFAULT.requests)
            .expect("requests is recorded")
            .0
            .key()
            .labels()
            .filter(|label| matches!(label.key(), "http.response.status_code" | "error.type"))
            .map(|label| (label.key().to_owned(), label.value().to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn server_errors_and_failures_set_error_type() {
        let answered = |status: u16| {
            service_fn(move |_request: Request<()>| async move {
                let mut response = Response::new(Full::new(Bytes::new()));
                *response.status_mut() = StatusCode::from_u16(status).expect("valid status");
                Ok::<_, std::io::Error>(response)
            })
        };
        let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());

        assert_eq!(
            outcome_labels(answered(404)).await,
            [pair("http.response.status_code", "404")],
            "a 4xx is the client's error, not the server's"
        );
        assert_eq!(
            outcome_labels(answered(503)).await,
            [
                pair("http.response.status_code", "503"),
                pair("error.type", "503"),
            ]
        );
        let failing = service_fn(|_request: Request<()>| async {
            Err::<Response<Full<Bytes>>, _>(std::io::Error::other("upstream gone"))
        });
        assert_eq!(
            outcome_labels(failing).await,
            [pair("error.type", "_OTHER")],
            "no status without a response"
        );
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
