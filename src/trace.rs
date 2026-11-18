//! [`OtelTraceLayer`]: one OpenTelemetry server span per request, and
//! [`OtelClientTraceLayer`]: one client span per outgoing request.
//!
//! Generic over a [`SpanDecorator`], which names and enriches spans, and a
//! [`RequestFilter`], which picks the requests to leave out. Both are types,
//! not values: their methods take no `self`, so the layer stays zero-sized.

mod client;
mod decorator;
mod filter;
mod layer;

pub use client::{ClientResponseFuture, OtelClientTraceLayer, OtelClientTraceService};
pub use decorator::{
    DefaultSpanDecorator, PropagatingSpanDecorator, SpanDecorator, mark_server_errors,
};
pub use filter::{
    HEALTHCHECK_PATHS, HEALTHCHECK_USER_AGENTS, HealthcheckFilter, NoOpFilter, RequestFilter,
};
pub use layer::{OtelTraceLayer, OtelTraceService, ResponseFuture};
