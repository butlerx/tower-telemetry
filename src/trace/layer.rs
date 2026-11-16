//! [`OtelTraceLayer`]: the Tower layer, its service and response future.

use std::fmt::{self, Display};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use http::header::{CONTENT_TYPE, USER_AGENT};
use http::{Request, Response};
use http_body::Body;
use opentelemetry_http::HeaderExtractor;
use tower_layer::Layer;
use tower_service::Service;
use tracing::Span;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::{DefaultSpanDecorator, NoOpFilter, RequestFilter, SpanDecorator};
use crate::request;

/// Starts an OpenTelemetry server span for each request, continuing the
/// caller's trace, and records the response or error on it.
///
/// # Type parameters
///
/// | Parameter | Default                  | Decides                                   |
/// | --------- | ------------------------ | ----------------------------------------- |
/// | `D`       | [`DefaultSpanDecorator`] | the span's name, and attributes to add ([`SpanDecorator`]) |
/// | `F`       | [`NoOpFilter`]           | requests to leave out ([`RequestFilter`]) |
///
/// Both are types, not values, so the layer is zero-sized and free to copy.
/// The crate provides [`PropagatingSpanDecorator`](crate::PropagatingSpanDecorator),
/// which also returns `traceparent` to callers, and
/// [`HealthcheckFilter`](crate::HealthcheckFilter).
///
/// ```
/// use tower_telemetry::{HealthcheckFilter, OtelTraceLayer, PropagatingSpanDecorator};
///
/// // Default decorator, every request traced. The angle brackets matter:
/// // `OtelTraceLayer::default()` alone doesn't compile, because Rust doesn't
/// // fill in a type's default parameters in an expression.
/// let layer = <OtelTraceLayer>::default();
///
/// // Return `traceparent` to callers and skip health checks.
/// let layer = OtelTraceLayer::<PropagatingSpanDecorator, HealthcheckFilter>::default();
/// # let _ = layer;
/// ```
///
/// # The span
///
/// The span's parent is, in order:
///
/// 1. a [`Span`] already in the request's extensions, put there by earlier
///    middleware;
/// 2. the trace context in the request's headers, read with OpenTelemetry's
///    global propagator (set one with
///    `opentelemetry::global::set_text_map_propagator`);
/// 3. none: the request starts a new trace.
///
/// It's named by the decorator: `"{method} {route}"` by default, or the
/// method alone without a matched route. It carries OpenTelemetry's HTTP
/// server attributes:
///
/// | Attribute                                        | From                                     |
/// | ------------------------------------------------ | ---------------------------------------- |
/// | `http.request.method`                            | the method; `_OTHER` for non-standard ones |
/// | `http.route`                                     | axum's matched route (feature `axum`)    |
/// | `url.path`, `url.query`                          | the URI                                  |
/// | `url.scheme`                                     | `X-Forwarded-Proto`, else the URI, else `http` |
/// | `server.address`                                 | `X-Forwarded-Host`, else `Host`          |
/// | `client.address`                                 | `X-Client-IP`, else the first `X-Forwarded-For` |
/// | `network.protocol.version`                       | the HTTP version                         |
/// | `user_agent.original`                            | `User-Agent`                             |
/// | `http.request.header.content-type`               | the request's `Content-Type`             |
/// | `http.request.body.size`                         | `Content-Length`                         |
/// | `http.response.status_code`                      | the response                             |
/// | `http.response.header.content-type`              | the response's `Content-Type`            |
/// | `http.response.body.size`                        | the body's exact size, else `Content-Length` |
/// | `http.server.time_to_first_byte`                 | seconds until the response was ready     |
/// | `otel.status_code`                               | `ERROR` for `5xx` (default decorator) or a failed service |
/// | `error.type`, `exception.message`                | when the inner service returns an error  |
///
/// Forwarding headers are trusted as sent: run behind a proxy that sets or
/// strips them.
///
/// # Using the span
///
/// The span is current while the inner service is called and polled, so
/// spans and events the handler creates nest under it. It's also put in the
/// request's extensions: a handler can take it (in axum, with
/// `Extension<Span>`) to add attributes with
/// `OpenTelemetrySpanExt::set_attribute`, or to parent work it spawns. See
/// [`SpanDecorator`] for adding attributes from every request.
///
/// # Setup
///
/// Spans go through [`tracing`]. They reach an OpenTelemetry backend only
/// when the process has a subscriber with `tracing_opentelemetry`'s layer;
/// without one, the layer costs little and records nothing. The
/// [crate docs](crate#axum) have a full setup for axum and hyper.
///
/// The inner service's error type must implement `Display`, to record
/// failures; axum's and `service_fn`'s `Infallible` does.
pub struct OtelTraceLayer<D = DefaultSpanDecorator, F = NoOpFilter> {
    /// The decorator and filter types; `fn() -> _` keeps the layer `Send`,
    /// `Sync` and `Copy` whatever they are.
    marker: PhantomData<fn() -> (D, F)>,
}

/// Write `<OtelTraceLayer>::default()` for the default types: in expression
/// position, Rust doesn't fill in a type's default parameters.
impl<D, F> Default for OtelTraceLayer<D, F> {
    fn default() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

impl<D, F> Clone for OtelTraceLayer<D, F> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D, F> Copy for OtelTraceLayer<D, F> {}

impl<D, F> fmt::Debug for OtelTraceLayer<D, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtelTraceLayer")
            .field("decorator", &std::any::type_name::<D>())
            .field("filter", &std::any::type_name::<F>())
            .finish()
    }
}

impl<S, D, F> Layer<S> for OtelTraceLayer<D, F> {
    type Service = OtelTraceService<S, D, F>;

    fn layer(&self, inner: S) -> Self::Service {
        OtelTraceService {
            inner,
            marker: PhantomData,
        }
    }
}

/// The service [`OtelTraceLayer`] wraps around an inner service.
///
/// You don't build it yourself: [`OtelTraceLayer`] does, when it's added to a
/// router or `ServiceBuilder`. For each request, it creates the span (unless
/// the filter `F` skips it), puts it in the request's extensions, and calls
/// the inner service inside it. Requests and responses pass through
/// otherwise unchanged. It's `Clone` when the inner service is.
pub struct OtelTraceService<S, D = DefaultSpanDecorator, F = NoOpFilter> {
    /// The wrapped service.
    inner: S,
    /// The decorator and filter types.
    marker: PhantomData<fn() -> (D, F)>,
}

impl<S: Clone, D, F> Clone for OtelTraceService<S, D, F> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            marker: PhantomData,
        }
    }
}

impl<S: fmt::Debug, D, F> fmt::Debug for OtelTraceService<S, D, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtelTraceService")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl<S, ReqBody, ResBody, D, F> Service<Request<ReqBody>> for OtelTraceService<S, D, F>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Error: Display,
    ResBody: Body,
    D: SpanDecorator,
    F: RequestFilter,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future, D>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<ReqBody>) -> Self::Future {
        let span = if F::should_ignore(&request) {
            Span::none()
        } else {
            make_span::<_, D>(&request)
        };
        request.extensions_mut().insert(span.clone());
        let inner = span.in_scope(|| self.inner.call(request));
        ResponseFuture {
            inner,
            span,
            started: Instant::now(),
            decorator: PhantomData,
        }
    }
}

pin_project_lite::pin_project! {
    /// The future [`OtelTraceService`] returns: the inner service's response
    /// future, polled inside the request's span.
    ///
    /// When the response is ready, it records the status code, response content
    /// type and body size, and the time to first byte, then hands the response
    /// to the decorator's [`on_response`](SpanDecorator::on_response). When the
    /// inner service fails, it marks the span as an error with the error's
    /// `Display` text. The span ends once the future and every clone of it (in
    /// the request's extensions, say) are dropped.
    pub struct ResponseFuture<Fut, D> {
        // The inner service's response future.
        #[pin]
        inner: Fut,
        // The request's span; disabled for filtered-out requests.
        span: Span,
        // When the request arrived, for time to first byte.
        started: Instant,
        // The decorator that records the response.
        decorator: PhantomData<fn() -> D>,
    }
}

impl<Fut, ResBody, E, D> Future for ResponseFuture<Fut, D>
where
    Fut: Future<Output = Result<Response<ResBody>, E>>,
    ResBody: Body,
    E: Display,
    D: SpanDecorator,
{
    type Output = Fut::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.span.enter();
        let result = ready!(this.inner.poll(cx));
        let span = &*this.span;
        match result {
            Ok(mut response) => {
                span.record("http.response.status_code", response.status().as_u16());
                let content_type = request::header(response.headers(), &CONTENT_TYPE);
                if !content_type.is_empty() {
                    span.record("http.response.header.content-type", content_type);
                }
                if let Some(size) = response
                    .body()
                    .size_hint()
                    .exact()
                    .or_else(|| request::content_length(response.headers()))
                {
                    span.record("http.response.body.size", size);
                }
                span.record(
                    "http.server.time_to_first_byte",
                    this.started.elapsed().as_secs_f64(),
                );
                D::on_response(span, &span.context(), &mut response);
                Poll::Ready(Ok(response))
            }
            Err(err) => {
                span.record("otel.status_code", "ERROR");
                span.record("error.type", "_OTHER");
                span.record("exception.message", tracing::field::display(&err));
                Poll::Ready(Err(err))
            }
        }
    }
}

/// Start the request's span with OpenTelemetry's HTTP server fields,
/// parented on a span in the request's extensions, else on the caller's
/// trace context in its headers.
fn make_span<B, D: SpanDecorator>(request: &Request<B>) -> Span {
    let headers = request.headers();
    let span = tracing::info_span!(
        target: "tower_telemetry",
        "HTTP request",
        otel.name = &*D::span_name(request),
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = request::known_method(request.method()),
        http.route = request::route(request),
        url.path = request.uri().path(),
        url.query = request.uri().query(),
        url.scheme = request::scheme(request),
        network.protocol.name = "http",
        network.protocol.version = request::http_version(request.version()),
        server.address = request::host(headers),
        client.address = request::client_ip(headers),
        user_agent.original = request::header(headers, &USER_AGENT),
        "http.request.header.content-type" = request::header(headers, &CONTENT_TYPE),
        http.request.body.size = request::content_length(headers),
        http.response.status_code = Empty,
        "http.response.header.content-type" = Empty,
        http.response.body.size = Empty,
        http.server.time_to_first_byte = Empty,
        error.type = Empty,
        exception.message = Empty,
    );

    // A parent span set by earlier middleware wins over the caller's headers.
    let parent = request.extensions().get::<Span>().map_or_else(
        || {
            opentelemetry::global::get_text_map_propagator(|propagator| {
                propagator.extract(&HeaderExtractor(headers))
            })
        },
        OpenTelemetrySpanExt::context,
    );
    if let Err(err) = span.set_parent(parent) {
        tracing::debug!(error = %err, "could not set the request span's parent");
    }

    D::on_request(request, &span);
    span
}
