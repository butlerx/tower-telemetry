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

/// Starts a server span for each request the filter keeps, continuing the
/// caller's trace (or a parent [`Span`] in the request's extensions), and
/// records the response or error on it once the response is ready.
///
/// The span is put in the request's extensions, so later middleware and
/// handlers can parent their own spans on it. Span fields follow
/// OpenTelemetry's HTTP server conventions.
///
/// ```
/// use tower_telemetry::{HealthcheckFilter, OtelTraceLayer, PropagatingSpanDecorator};
///
/// let default = OtelTraceLayer::new();
/// let custom = OtelTraceLayer::<PropagatingSpanDecorator, HealthcheckFilter>::with_types();
/// # let _ = (default, custom);
/// ```
pub struct OtelTraceLayer<D = DefaultSpanDecorator, F = NoOpFilter> {
    marker: PhantomData<fn() -> (D, F)>,
}

impl OtelTraceLayer {
    /// A layer with the [`DefaultSpanDecorator`] that traces every request.
    /// Pick other types with `OtelTraceLayer::<D, F>::with_types()`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

impl<D, F> OtelTraceLayer<D, F> {
    /// A layer with the decorator `D` and filter `F`.
    #[must_use]
    pub const fn with_types() -> Self {
        Self {
            marker: PhantomData,
        }
    }
}

impl<D, F> Default for OtelTraceLayer<D, F> {
    fn default() -> Self {
        Self::with_types()
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
pub struct OtelTraceService<S, D = DefaultSpanDecorator, F = NoOpFilter> {
    inner: S,
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
    /// Response future for [`OtelTraceService`]: polls the inner future
    /// inside the request span and records its outcome on the span.
    pub struct ResponseFuture<Fut, D> {
        #[pin]
        inner: Fut,
        span: Span,
        started: Instant,
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
