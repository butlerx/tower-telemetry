//! [`HttpMetricsLayer`]: request metrics through the [`metrics`](::metrics)
//! facade.

use std::borrow::Cow;
use std::task::{Context, Poll};
use std::time::Instant;

use ::metrics::{Label, SharedString};
use http::{Request, Response};
use http_body::Body;
use tower_layer::Layer;
use tower_service::Service;

use super::MetricNames;
use super::recording::{Active, MetricsBody, MetricsFuture, Pending, Side, describe};
use crate::attribute::{
    HTTP_REQUEST_METHOD, HTTP_ROUTE, NETWORK_PROTOCOL_VERSION, SERVER_ADDRESS, SERVER_PORT,
    URL_SCHEME,
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
/// | `server.address`            | the host, without the port; only with [`with_host_label(true)`](Self::with_host_label) | all |
/// | `server.port`               | the port, else the scheme's default; only with [`with_host_label(true)`](Self::with_host_label) | all |
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

    /// Label metrics with `server.address` and `server.port`: the host and
    /// port of `X-Forwarded-Host`, else `Host`, with the scheme's default
    /// port (80 or 443) when it has none. The conventions make both opt-in,
    /// and so does this: the client picks them, so turn it on only behind a
    /// proxy that restricts them to your own domains, or every made-up host
    /// is a new time series.
    #[must_use]
    pub const fn with_host_label(self, host_label: bool) -> Self {
        Self { host_label, ..self }
    }
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        describe(&self.names, Side::Server);
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
            .then(|| host_labels(request::server_address(&request)))
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
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
            side: Side::Server,
            labels,
            started: Instant::now(),
            request_body_size: request::content_length(request.headers()),
            active,
        };
        MetricsFuture::new(self.inner.call(request), pending)
    }
}

/// `server.address` and, when known, `server.port`.
pub(super) fn host_labels((address, port): (&str, Option<u16>)) -> impl Iterator<Item = Label> {
    std::iter::once(Label::new(SERVER_ADDRESS, address.to_owned()))
        .chain(port.map(|port| Label::new(SERVER_PORT, port.to_string())))
}

/// `url.scheme` without allocating for the usual two.
fn scheme_label(scheme: &str) -> SharedString {
    match scheme {
        "https" => SharedString::const_str("https"),
        "http" => SharedString::const_str("http"),
        other => SharedString::from(other.to_owned()),
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

    use ::metrics::Unit;
    use http::StatusCode;

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
