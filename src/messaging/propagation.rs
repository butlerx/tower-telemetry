//! Trace context carried in message headers.

use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::TraceContextExt;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Write `span`'s trace context into a message's headers with the global
/// propagator, for consumers to continue the trace.
pub fn inject_context(span: &Span, headers: &mut dyn Injector) {
    let context = span.context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, headers);
    });
}

/// Read the producer's trace context from a message's headers with the
/// global propagator. Empty (no valid span) when the message carries none.
#[must_use]
pub fn extract_context(headers: &dyn Extractor) -> opentelemetry::Context {
    opentelemetry::global::get_text_map_propagator(|propagator| propagator.extract(headers))
}

/// Link `span` to the producer of each message in a batch, as the
/// conventions ask for batch receive and process spans. Contexts without a
/// valid span are skipped.
pub fn link_producers<'h>(span: &Span, headers: impl IntoIterator<Item = &'h dyn Extractor>) {
    for headers in headers {
        let context = extract_context(headers);
        let producer = context.span().span_context().clone();
        if producer.is_valid() {
            span.add_link(producer);
        }
    }
}
