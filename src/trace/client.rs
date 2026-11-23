//! [`OtelClientTraceLayer`]: one OpenTelemetry client span per outgoing
//! request, with the trace context sent along.

use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use http::header::USER_AGENT;
use http::{Request, Response};
use http_body::Body;
use opentelemetry_http::HeaderInjector;
use tower_layer::Layer;
use tower_service::Service;
use tracing::Span;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::attribute::{
    ERROR_TYPE, EXCEPTION_MESSAGE, HTTP_REQUEST_BODY_SIZE, HTTP_REQUEST_METHOD,
    HTTP_RESPONSE_BODY_SIZE, HTTP_RESPONSE_STATUS_CODE, NETWORK_PROTOCOL_VERSION, OTEL_KIND,
    OTEL_NAME, OTEL_STATUS_CODE, SERVER_ADDRESS, SERVER_PORT, URL_FULL, USER_AGENT_ORIGINAL, int,
};
use crate::request;

/// Starts an OpenTelemetry client span for each request a client sends, and
/// writes the span's trace context into the request's headers so the server
/// continues the trace.
///
/// Wrap the client you send requests with: a `hyper_util` legacy `Client`,
/// or any `tower::Service<Request<B>>` that answers with a `Response`.
///
/// ```
/// use tower::ServiceBuilder;
/// use tower_telemetry::OtelClientTraceLayer;
///
/// # let client = tower::service_fn(|_request: http::Request<String>| async {
/// #     Ok::<_, std::convert::Infallible>(http::Response::new(String::new()))
/// # });
/// let client = ServiceBuilder::new()
///     .layer(OtelClientTraceLayer)
///     .service(client);
/// # let _ = client;
/// ```
///
/// # The span
///
/// Its parent is the current `tracing` span: the server span of the request
/// being handled, say. It's named by the method (`GET`), or `HTTP` for
/// non-standard ones, as the conventions ask, and carries their HTTP client
/// attributes:
///
/// | Attribute                         | From                                              |
/// | --------------------------------- | ------------------------------------------------- |
/// | `http.request.method`             | the method; `_OTHER` for non-standard ones        |
/// | `server.address`, `server.port`   | the URI, else `Host`; the port defaults by scheme |
/// | `url.full`                        | the URI, without credentials and with credential query values redacted (see [`OtelTraceLayer`](crate::OtelTraceLayer)) |
/// | `user_agent.original`             | `User-Agent`                                      |
/// | `http.request.body.size`          | the body's exact size, else `Content-Length`      |
/// | `http.response.status_code`       | the response                                      |
/// | `network.protocol.version`        | the response's HTTP version                       |
/// | `http.response.body.size`         | the body's exact size, else `Content-Length`      |
/// | `otel.status_code`, `error.type`  | `ERROR` and the status for a `4xx` or `5xx`; `_OTHER` when the request fails |
/// | `exception.message`               | the client's error                                |
///
/// For a client, a `4xx` is an error too: the request didn't get what it
/// asked for. The span ends when the response headers arrive.
///
/// # Propagation
///
/// The span's context goes into the request's headers through the global
/// propagator: `traceparent` and `tracestate` with the W3C one. Set it once
/// with `opentelemetry::global::set_text_map_propagator`; without one,
/// nothing is sent.
///
/// The client's error type must implement `Display`, to record failures.
#[derive(Debug, Clone, Copy, Default)]
pub struct OtelClientTraceLayer;

impl OtelClientTraceLayer {
    /// The same layer, with `keys` added to the query parameter names whose
    /// values `url.full` redacts. The built-in names always apply; `keys`
    /// only extends them. Names match as sent or after percent-decoding,
    /// without regard to case.
    ///
    /// ```
    /// use tower::ServiceBuilder;
    /// use tower_telemetry::OtelClientTraceLayer;
    ///
    /// # let client = tower::service_fn(|_request: http::Request<String>| async {
    /// #     Ok::<_, std::convert::Infallible>(http::Response::new(String::new()))
    /// # });
    /// // `?x-api-key=...` is recorded as `x-api-key=REDACTED` in `url.full`.
    /// let client = ServiceBuilder::new()
    ///     .layer(OtelClientTraceLayer::with_redacted_query_keys(&["x-api-key"]))
    ///     .service(client);
    /// # let _ = client;
    /// ```
    #[must_use]
    pub const fn with_redacted_query_keys(
        keys: &'static [&'static str],
    ) -> RedactingClientTraceLayer {
        RedactingClientTraceLayer { keys }
    }
}

impl<S> Layer<S> for OtelClientTraceLayer {
    type Service = OtelClientTraceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OtelClientTraceService { inner, keys: &[] }
    }
}

/// [`OtelClientTraceLayer`] with extra query parameter names to redact,
/// from [`OtelClientTraceLayer::with_redacted_query_keys`].
#[derive(Debug, Clone, Copy)]
pub struct RedactingClientTraceLayer {
    /// The extra names, on top of the built-in list.
    keys: &'static [&'static str],
}

impl<S> Layer<S> for RedactingClientTraceLayer {
    type Service = OtelClientTraceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        OtelClientTraceService {
            inner,
            keys: self.keys,
        }
    }
}

/// The service [`OtelClientTraceLayer`] wraps around a client.
///
/// [`OtelClientTraceLayer`] builds it. It starts each request's span, adds
/// the trace context to its headers, and sends it inside the span. It's
/// `Clone` when the client is.
#[derive(Debug, Clone)]
pub struct OtelClientTraceService<S> {
    /// The wrapped client.
    inner: S,
    /// Extra query parameter names to redact in `url.full`.
    keys: &'static [&'static str],
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for OtelClientTraceService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Error: Display,
    ReqBody: Body,
    ResBody: Body,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ClientResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<ReqBody>) -> Self::Future {
        let span = make_span(&request, self.keys);
        let context = span.context();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut HeaderInjector(request.headers_mut()));
        });
        let inner = span.in_scope(|| self.inner.call(request));
        ClientResponseFuture { inner, span }
    }
}

pin_project_lite::pin_project! {
    /// The future [`OtelClientTraceService`] returns: the client's response
    /// future, polled inside the request's span.
    ///
    /// When the response arrives, it records the status, protocol version and
    /// body size, and marks a `4xx` or `5xx` as an error. When the client
    /// fails, it marks the span as an error with the error's `Display` text.
    pub struct ClientResponseFuture<F> {
        // The client's response future.
        #[pin]
        inner: F,
        // The request's span.
        span: Span,
    }
}

impl<F, ResBody, E> Future for ClientResponseFuture<F>
where
    F: Future<Output = Result<Response<ResBody>, E>>,
    ResBody: Body,
    E: Display,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.span.enter();
        let result = ready!(this.inner.poll(cx));
        let span = &*this.span;
        match &result {
            Ok(response) => {
                let status = response.status();
                span.record(HTTP_RESPONSE_STATUS_CODE, int(status.as_u16()));
                span.record(
                    NETWORK_PROTOCOL_VERSION,
                    request::http_version(response.version()),
                );
                if let Some(size) = response
                    .body()
                    .size_hint()
                    .exact()
                    .or_else(|| request::content_length(response.headers()))
                {
                    span.record(HTTP_RESPONSE_BODY_SIZE, int(size));
                }
                if status.is_client_error() || status.is_server_error() {
                    span.record(OTEL_STATUS_CODE, "ERROR");
                    span.record(ERROR_TYPE, status.as_str());
                }
            }
            Err(err) => {
                span.record(OTEL_STATUS_CODE, "ERROR");
                span.record(ERROR_TYPE, "_OTHER");
                span.record(EXCEPTION_MESSAGE, tracing::field::display(err));
            }
        }
        Poll::Ready(result)
    }
}

/// Start an outgoing request's span with OpenTelemetry's HTTP client fields,
/// as a child of the current span.
fn make_span<B: Body>(request: &Request<B>, keys: &[&str]) -> Span {
    let method = request::known_method(request.method());
    let (address, port) = request::target(request);
    tracing::info_span!(
        target: "tower_telemetry",
        "HTTP client request",
        { OTEL_NAME } = if method == "_OTHER" { "HTTP" } else { method },
        { OTEL_KIND } = "client",
        { OTEL_STATUS_CODE } = Empty,
        { HTTP_REQUEST_METHOD } = method,
        { SERVER_ADDRESS } = address,
        { SERVER_PORT } = port.map(i64::from),
        { URL_FULL } = request::full_url(request, keys),
        { USER_AGENT_ORIGINAL } = request::header(request.headers(), &USER_AGENT),
        { HTTP_REQUEST_BODY_SIZE } = request
            .body()
            .size_hint()
            .exact()
            .or_else(|| request::content_length(request.headers()))
            .map(int),
        { HTTP_RESPONSE_STATUS_CODE } = Empty,
        { NETWORK_PROTOCOL_VERSION } = Empty,
        { HTTP_RESPONSE_BODY_SIZE } = Empty,
        { ERROR_TYPE } = Empty,
        { EXCEPTION_MESSAGE } = Empty,
    )
}
