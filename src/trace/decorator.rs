//! [`SpanDecorator`]: how a service names and enriches its request spans.

use std::borrow::Cow;

use http::{Request, Response};
use opentelemetry_http::HeaderInjector;
use tracing::Span;

use crate::request;

/// How a service names and enriches its request spans. Every method has a
/// default, so a decorator overrides only what it changes.
pub trait SpanDecorator {
    /// The span's name (`otel.name`): `"{method} {route}"` when a route
    /// matched, else `"{method}"`, which allocates nothing. Never the raw
    /// path, which carries identifiers and would make every span name unique.
    fn span_name<B>(request: &Request<B>) -> Cow<'static, str> {
        let method = request::known_method(request.method());
        request::route(request).map_or(Cow::Borrowed(method), |route| {
            Cow::Owned(format!("{method} {route}"))
        })
    }

    /// Record request fields beyond the common ones. `span` is disabled
    /// (recording does nothing) for filtered-out requests.
    fn on_request<B>(_request: &Request<B>, _span: &Span) {}

    /// Record response fields, or change the response. `context` is the
    /// span's OpenTelemetry context. The default marks `5xx` responses as
    /// errors, as OpenTelemetry's HTTP conventions do for servers.
    fn on_response<B>(span: &Span, _context: &opentelemetry::Context, response: &mut Response<B>) {
        mark_server_errors(span, response);
    }
}

/// Record `otel.status_code`: `ERROR` for a `5xx` response, else `OK`.
pub fn mark_server_errors<B>(span: &Span, response: &Response<B>) {
    span.record(
        "otel.status_code",
        if response.status().is_server_error() {
            "ERROR"
        } else {
            "OK"
        },
    );
}

/// Names spans by method and route and marks `5xx` responses as errors.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultSpanDecorator;

impl SpanDecorator for DefaultSpanDecorator {}

/// [`DefaultSpanDecorator`], and returns the trace context to the client in
/// the response headers (`traceparent`, with the W3C propagator), so a caller
/// can look up the trace of its request.
#[derive(Debug, Clone, Copy, Default)]
pub struct PropagatingSpanDecorator;

impl SpanDecorator for PropagatingSpanDecorator {
    fn on_response<B>(span: &Span, context: &opentelemetry::Context, response: &mut Response<B>) {
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(context, &mut HeaderInjector(response.headers_mut()));
        });
        mark_server_errors(span, response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_requests_are_named_by_method_only() {
        let request = Request::post("/acme/repo/pkg.tgz")
            .body(())
            .expect("static request");
        assert_eq!(DefaultSpanDecorator::span_name(&request), "POST");
    }
}
