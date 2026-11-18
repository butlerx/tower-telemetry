//! [`HttpClientMetricsLayer`]: metrics for the requests a client sends.

use std::task::{Context, Poll};
use std::time::Instant;

use ::metrics::Label;
use http::{Request, Response};
use http_body::Body;
use tower_layer::Layer;
use tower_service::Service;

use super::MetricNames;
use super::layer::host_labels;
use super::recording::{Active, MetricsBody, MetricsFuture, Pending, Side, describe};
use crate::attribute::{HTTP_REQUEST_METHOD, NETWORK_PROTOCOL_VERSION};
use crate::request;

/// Records HTTP client metrics for each outgoing request through the
/// [`metrics`](::metrics) facade.
///
/// Wrap the client you send requests with: a `hyper_util` legacy `Client`,
/// or any `tower::Service<Request<B>>` that answers with a `Response`.
///
/// ```
/// use tower::ServiceBuilder;
/// use tower_telemetry::metrics::HttpClientMetricsLayer;
///
/// # let client = tower::service_fn(|_request: http::Request<String>| async {
/// #     Ok::<_, std::convert::Infallible>(http::Response::new(String::new()))
/// # });
/// let client = ServiceBuilder::new()
///     .layer(HttpClientMetricsLayer::default())
///     .service(client);
/// # let _ = client;
/// ```
///
/// # Metrics
///
/// The same six as [`HttpMetricsLayer`](crate::HttpMetricsLayer), under the
/// client names ([`MetricNames::CLIENT`]):
///
/// | Name                             | Type      | Unit    | Recorded                                   |
/// | -------------------------------- | --------- | ------- | ------------------------------------------ |
/// | `http.client.requests`           | counter   | count   | when the response headers arrive           |
/// | `http.client.time_to_first_byte` | histogram | seconds | when the response headers arrive           |
/// | `http.client.request.body.size`  | histogram | bytes   | when the headers arrive, from the body's exact size or `Content-Length`, if over 0 |
/// | `http.client.request.duration`   | histogram | seconds | when the response body is read to the end, fails or is dropped |
/// | `http.client.response.body.size` | histogram | bytes   | with the duration, counting the bytes read |
/// | `http.client.active_requests`    | gauge     | count   | up when the request is sent, down when its response body is done |
///
/// Duration covers reading the response body, so a caller that never reads
/// it records when the response is dropped.
///
/// # Labels
///
/// | Label                       | Value                                                     | On             |
/// | --------------------------- | --------------------------------------------------------- | -------------- |
/// | `http.request.method`       | the method; `_OTHER` for non-standard ones                | all            |
/// | `server.address`            | the URI's host, else `Host`                               | all            |
/// | `server.port`               | the URI's port, else the scheme's default                 | all            |
/// | `network.protocol.version`  | the request's HTTP version                                | all but active |
/// | `http.response.status_code` | the response status; left off when the request failed     | all but active |
/// | `error.type`                | the status code for a `4xx` or `5xx`, `_OTHER` when the request failed | all but active |
///
/// The conventions require `server.address` and `server.port` for clients,
/// so they're always on. Keep the hosts a client talks to few: one series
/// per host.
#[derive(Debug, Clone, Copy)]
pub struct HttpClientMetricsLayer {
    /// The names to record under.
    names: MetricNames,
}

impl Default for HttpClientMetricsLayer {
    fn default() -> Self {
        Self {
            names: MetricNames::CLIENT,
        }
    }
}

impl HttpClientMetricsLayer {
    /// Record under `names` instead of [`MetricNames::CLIENT`].
    #[must_use]
    pub const fn with_names(self, names: MetricNames) -> Self {
        Self { names }
    }
}

impl<S> Layer<S> for HttpClientMetricsLayer {
    type Service = HttpClientMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        describe(&self.names, Side::Client);
        HttpClientMetricsService {
            inner,
            names: self.names,
        }
    }
}

/// The service [`HttpClientMetricsLayer`] wraps around a client.
///
/// [`HttpClientMetricsLayer`] builds it. Requests pass through unchanged; the
/// response's body is wrapped in a [`MetricsBody`] that records when it's
/// read to the end. It's `Clone` when the client is.
#[derive(Debug, Clone)]
pub struct HttpClientMetricsService<S> {
    /// The wrapped client.
    inner: S,
    /// The names to record under.
    names: MetricNames,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for HttpClientMetricsService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    ReqBody: Body,
    ResBody: Body,
{
    type Response = Response<MetricsBody<ResBody>>;
    type Error = S::Error;
    type Future = MetricsFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        let names = self.names;
        let method = Label::new(HTTP_REQUEST_METHOD, request::known_method(request.method()));
        let target: Vec<Label> = host_labels(request::target(&request)).collect();
        let active = Active::new(
            names.active_requests,
            std::iter::once(method.clone())
                .chain(target.iter().cloned())
                .collect(),
        );
        let labels = std::iter::once(method)
            .chain(target)
            .chain(std::iter::once(Label::new(
                NETWORK_PROTOCOL_VERSION,
                request::http_version(request.version()),
            )))
            .collect();
        let pending = Pending {
            names,
            side: Side::Client,
            labels,
            started: Instant::now(),
            request_body_size: request
                .body()
                .size_hint()
                .exact()
                .or_else(|| request::content_length(request.headers())),
            active,
        };
        MetricsFuture::new(self.inner.call(request), pending)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::StatusCode;
    use http_body_util::{BodyExt, Full};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use tower::{ServiceBuilder, ServiceExt, service_fn};

    use super::*;

    #[tokio::test]
    async fn records_client_requests_by_target_and_marks_4xx_as_errors() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _guard = ::metrics::set_default_local_recorder(&recorder);

        let client = ServiceBuilder::new()
            .layer(HttpClientMetricsLayer::default())
            .service(service_fn(|_request: Request<Full<Bytes>>| async {
                let mut response = Response::new(Full::new(Bytes::from_static(b"nope")));
                *response.status_mut() = StatusCode::NOT_FOUND;
                Ok::<_, std::io::Error>(response)
            }));
        let response = client
            .oneshot(
                Request::post("https://api.example/v1/users")
                    .body(Full::new(Bytes::from_static(b"{}")))
                    .expect("static request"),
            )
            .await
            .expect("answered");
        response.into_body().collect().await.expect("full body");

        let snapshot = snapshotter.snapshot().into_vec();
        let find = |name: &str| {
            snapshot
                .iter()
                .find(|(key, ..)| key.key().name() == name)
                .unwrap_or_else(|| panic!("{name} was not recorded"))
        };
        let labels = |name: &str| -> Vec<(String, String)> {
            find(name)
                .0
                .key()
                .labels()
                .map(|label| (label.key().to_owned(), label.value().to_owned()))
                .collect()
        };
        let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());

        assert_eq!(
            labels(MetricNames::CLIENT.requests),
            [
                pair("http.request.method", "POST"),
                pair("server.address", "api.example"),
                pair("server.port", "443"),
                pair("network.protocol.version", "1.1"),
                pair("http.response.status_code", "404"),
                pair("error.type", "404"),
            ]
        );
        assert_eq!(
            labels(MetricNames::CLIENT.active_requests),
            [
                pair("http.request.method", "POST"),
                pair("server.address", "api.example"),
                pair("server.port", "443"),
            ]
        );
        let DebugValue::Histogram(sizes) = &find(MetricNames::CLIENT.request_body_size).3 else {
            panic!("request_body_size is a histogram");
        };
        assert_eq!(sizes.len(), 1, "the body's exact size is recorded");
        assert!(matches!(
            find(MetricNames::CLIENT.duration).3,
            DebugValue::Histogram(_)
        ));
    }
}
