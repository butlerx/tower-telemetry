//! The attribute names the spans use.
//!
//! OpenTelemetry's come from `opentelemetry-semantic-conventions`, so a typo
//! fails to compile and a renamed attribute shows up when the crate is
//! bumped. The others are `tracing-opentelemetry`'s special fields, header
//! attributes (the conventions give a prefix plus the header's name), and
//! [`TIME_TO_FIRST_BYTE`], the one attribute this crate adds.

#[cfg(feature = "trace")]
pub(crate) use opentelemetry_semantic_conventions::attribute::{
    CLIENT_ADDRESS, HTTP_REQUEST_BODY_SIZE, HTTP_REQUEST_METHOD, HTTP_RESPONSE_BODY_SIZE,
    HTTP_RESPONSE_STATUS_CODE, HTTP_ROUTE, NETWORK_PROTOCOL_NAME, NETWORK_PROTOCOL_VERSION,
    URL_PATH, URL_QUERY, URL_SCHEME, USER_AGENT_ORIGINAL,
};
pub(crate) use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, EXCEPTION_MESSAGE, SERVER_ADDRESS,
};
#[cfg(feature = "messaging")]
pub(crate) use opentelemetry_semantic_conventions::attribute::{
    MESSAGING_BATCH_MESSAGE_COUNT, MESSAGING_CONSUMER_GROUP_NAME, MESSAGING_DESTINATION_NAME,
    MESSAGING_DESTINATION_TEMPLATE, MESSAGING_MESSAGE_BODY_SIZE, MESSAGING_MESSAGE_CONVERSATION_ID,
    MESSAGING_MESSAGE_ID, MESSAGING_OPERATION_NAME, MESSAGING_OPERATION_TYPE, MESSAGING_SYSTEM,
    SERVER_PORT,
};

/// The span's name, as `tracing-opentelemetry` reads it.
pub(crate) const OTEL_NAME: &str = "otel.name";
/// The span's kind (`server`, `producer`, ...), as `tracing-opentelemetry`
/// reads it.
pub(crate) const OTEL_KIND: &str = "otel.kind";
/// The span's status (`OK` or `ERROR`), as `tracing-opentelemetry` reads it.
pub(crate) const OTEL_STATUS_CODE: &str = "otel.status_code";

/// The request's `Content-Type`: the conventions' `http.request.header`
/// prefix with the header's lower-case name.
#[cfg(feature = "trace")]
pub(crate) const HTTP_REQUEST_HEADER_CONTENT_TYPE: &str = "http.request.header.content-type";
/// The response's `Content-Type`: the conventions' `http.response.header`
/// prefix with the header's lower-case name.
#[cfg(feature = "trace")]
pub(crate) const HTTP_RESPONSE_HEADER_CONTENT_TYPE: &str = "http.response.header.content-type";

/// Seconds from the request arriving to the response headers being ready.
///
/// The conventions have no such attribute, so this one follows their style
/// as closely as it can: in the `http.server` namespace, snake case, and in
/// seconds as a float, like their durations. It pairs with the
/// `http_server_time_to_first_byte_seconds` metric.
#[cfg(feature = "trace")]
pub(crate) const TIME_TO_FIRST_BYTE: &str = "http.server.time_to_first_byte";

/// `value` as the `i64` OpenTelemetry integers are, saturating.
///
/// Record integers on spans through this: `tracing-opentelemetry` exports
/// `i64` as an integer, but has no conversion for `u64`, which it falls back
/// to exporting as a string (`"200"` rather than `200`).
pub(crate) fn int(value: impl Into<u64>) -> i64 {
    i64::try_from(value.into()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn integers_saturate_into_i64() {
        assert_eq!(super::int(200_u16), 200);
        assert_eq!(super::int(u64::MAX), i64::MAX);
    }

    #[cfg(feature = "trace")]
    #[test]
    fn header_attributes_use_the_conventions_prefixes() {
        use opentelemetry_semantic_conventions::attribute::{
            HTTP_REQUEST_HEADER, HTTP_RESPONSE_HEADER,
        };

        assert_eq!(
            super::HTTP_REQUEST_HEADER_CONTENT_TYPE,
            format!("{HTTP_REQUEST_HEADER}.content-type")
        );
        assert_eq!(
            super::HTTP_RESPONSE_HEADER_CONTENT_TYPE,
            format!("{HTTP_RESPONSE_HEADER}.content-type")
        );
    }
}
