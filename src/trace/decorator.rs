//! [`SpanDecorator`]: how a service names and enriches its request spans.

use std::borrow::Cow;

use http::{Request, Response};
use opentelemetry_http::HeaderInjector;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::attribute::{ERROR_TYPE, OTEL_STATUS_CODE};
use crate::request;

/// How a service names and enriches its request spans. Every method has a
/// default, so a decorator overrides only what it changes.
///
/// Implement it on a unit struct and pass it as the layer's first type
/// parameter.
///
/// # Custom attributes
///
/// Add attributes with [`OpenTelemetrySpanExt::set_attribute`]. Don't use
/// `span.record`: a `tracing` span only records fields declared when it was
/// created, and silently drops any others.
///
/// Attributes from the request go in [`on_request`](Self::on_request), and
/// attributes from the response in [`on_response`](Self::on_response). Call
/// [`mark_server_errors`] there to keep marking `5xx` responses as errors.
///
/// ```
/// use http::{Request, Response};
/// use tower_telemetry::{OtelTraceLayer, SpanDecorator, mark_server_errors};
/// use tracing::Span;
/// use tracing_opentelemetry::OpenTelemetrySpanExt;
///
/// #[derive(Debug, Clone, Copy, Default)]
/// struct TenantDecorator;
///
/// impl SpanDecorator for TenantDecorator {
///     fn on_request<B>(request: &Request<B>, span: &Span) {
///         if let Some(tenant) = request
///             .headers()
///             .get("x-tenant-id")
///             .and_then(|value| value.to_str().ok())
///         {
///             span.set_attribute("tenant.id", tenant.to_owned());
///         }
///     }
///
///     fn on_response<B>(span: &Span, response: &mut Response<B>) {
///         if let Some(cache) = response
///             .headers()
///             .get("x-cache")
///             .and_then(|value| value.to_str().ok())
///         {
///             span.set_attribute("http.response.header.x-cache", cache.to_owned());
///         }
///         mark_server_errors(span, response);
///     }
/// }
///
/// let layer = OtelTraceLayer::<TenantDecorator>::default();
/// # let _ = layer;
/// ```
///
/// # Attributes only a handler knows
///
/// The layer puts the request's span in the request's extensions, so a
/// handler can add what it learns itself, such as the user it authenticated.
/// In axum:
///
/// ```
/// # #[cfg(feature = "axum")] {
/// use axum::Extension;
/// use tracing::Span;
/// use tracing_opentelemetry::OpenTelemetrySpanExt;
///
/// async fn handler(Extension(span): Extension<Span>) -> &'static str {
///     span.set_attribute("enduser.id", "42");
///     "hello"
/// }
/// # }
/// ```
///
/// # Span names
///
/// Override [`span_name`](Self::span_name) for a different name. Keep it low
/// cardinality: a route or operation, never an identifier.
///
/// Attributes are about traces. For metric labels, see
/// [`MetricLabels`](crate::MetricLabels).
///
/// [`OpenTelemetrySpanExt::set_attribute`]: tracing_opentelemetry::OpenTelemetrySpanExt::set_attribute
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

    /// Add attributes from the request, once its span exists.
    ///
    /// Called once per request, before the inner service. Add attributes with
    /// `OpenTelemetrySpanExt::set_attribute`: `span.record` only reaches fields
    /// the span declared, and drops others without warning. For filtered-out
    /// requests, `span` is disabled and anything set on it is dropped. Default:
    /// nothing.
    fn on_request<B>(_request: &Request<B>, _span: &Span) {}

    /// Record response fields, or change the response. The default marks
    /// `5xx` responses as errors, as OpenTelemetry's HTTP conventions do for
    /// servers.
    ///
    /// For the span's OpenTelemetry context, call `span.context()` from
    /// `OpenTelemetrySpanExt`. It isn't passed in, so decorators that don't
    /// need it don't pay to look it up on every request.
    fn on_response<B>(span: &Span, response: &mut Response<B>) {
        mark_server_errors(span, response);
    }
}

/// Mark `span` as an error (`otel.status_code` `ERROR`, and `error.type` the
/// status code) for a `5xx` response, and leave its status unset otherwise.
///
/// OpenTelemetry's HTTP conventions mark server spans as errors only for
/// `5xx`: a `4xx` is the client's mistake, not the server's. They also ask
/// instrumentation not to set `OK`, leaving that to the application, so a
/// successful response leaves the status unset. The default
/// [`SpanDecorator::on_response`] calls it; call it from your own
/// `on_response` to keep that behaviour.
#[inline]
pub fn mark_server_errors<B>(span: &Span, response: &Response<B>) {
    let status = response.status();
    if status.is_server_error() {
        span.record(OTEL_STATUS_CODE, "ERROR");
        span.record(ERROR_TYPE, status.as_str());
    }
}

/// The [`SpanDecorator`] [`OtelTraceLayer`](crate::OtelTraceLayer) uses by
/// default: every method left at its default.
///
/// It names spans `"{method} {route}"`, or the method alone without a matched
/// route, adds nothing beyond the layer's own attributes, and marks `5xx`
/// responses as errors. It doesn't touch the response.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultSpanDecorator;

impl SpanDecorator for DefaultSpanDecorator {}

/// [`DefaultSpanDecorator`], and returns the trace context to the client in
/// the response headers (`traceparent`, with the W3C propagator), so a caller
/// can look up the trace of its request.
#[derive(Debug, Clone, Copy, Default)]
pub struct PropagatingSpanDecorator;

impl SpanDecorator for PropagatingSpanDecorator {
    /// Write the span's trace context into the response headers with the
    /// global propagator (`traceparent` and `tracestate` with the W3C one), then
    /// mark `5xx` responses as errors like the default. Callers can then log or
    /// show the trace id of their request.
    fn on_response<B>(span: &Span, response: &mut Response<B>) {
        let context = span.context();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut HeaderInjector(response.headers_mut()));
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
