//! What the layers read from a request. Behind a load balancer or CDN, the
//! client, host and scheme come from forwarding headers.
//!
//! These headers are taken as given: run behind a proxy that sets or strips
//! them, or the client controls what they say.

use http::header::{CONTENT_LENGTH, HOST};
use http::{HeaderMap, HeaderName, Method, Request};

/// `X-Forwarded-Host`.
pub(crate) const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
#[cfg(feature = "trace")]
/// `X-Forwarded-For`.
pub(crate) const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
#[cfg(feature = "trace")]
/// `X-Forwarded-Proto`.
pub(crate) const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
#[cfg(feature = "trace")]
/// `X-Client-IP`, set by some CDNs to the original client address.
pub(crate) const X_CLIENT_IP: HeaderName = HeaderName::from_static("x-client-ip");

/// The route the request matched, when axum's router has matched one.
#[cfg(feature = "axum")]
pub(crate) fn route<B>(request: &Request<B>) -> Option<&str> {
    request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(axum::extract::MatchedPath::as_str)
}

/// The route the request matched: unknown without the `axum` feature.
#[cfg(not(feature = "axum"))]
pub(crate) fn route<B>(_request: &Request<B>) -> Option<&str> {
    None
}

/// A header's value, trimmed; empty when missing or not visible ASCII.
pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &HeaderName) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map_or("", str::trim)
}

/// `X-Forwarded-Host`, else `Host`.
pub(crate) fn host(headers: &HeaderMap) -> &str {
    match header(headers, &X_FORWARDED_HOST) {
        "" => header(headers, &HOST),
        forwarded => forwarded,
    }
}

/// `X-Client-IP`, else the first `X-Forwarded-For` address.
#[cfg(feature = "trace")]
pub(crate) fn client_ip(headers: &HeaderMap) -> &str {
    match header(headers, &X_CLIENT_IP) {
        "" => header(headers, &X_FORWARDED_FOR)
            .split(',')
            .next()
            .unwrap_or_default()
            .trim(),
        client_ip => client_ip,
    }
}

/// `X-Forwarded-Proto`, else the request's own scheme, else `http`.
#[cfg(feature = "trace")]
pub(crate) fn scheme<B>(request: &Request<B>) -> &str {
    match header(request.headers(), &X_FORWARDED_PROTO) {
        "" => request.uri().scheme_str().unwrap_or("http"),
        forwarded => forwarded,
    }
}

/// The request's method as OpenTelemetry records it: `_OTHER` for methods
/// outside RFC 9110 and PATCH, which would otherwise be unbounded.
pub(crate) fn known_method(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::CONNECT => "CONNECT",
        Method::OPTIONS => "OPTIONS",
        Method::TRACE => "TRACE",
        Method::PATCH => "PATCH",
        _ => "_OTHER",
    }
}

/// The declared body size, from `Content-Length`.
pub(crate) fn content_length(headers: &HeaderMap) -> Option<u64> {
    header(headers, &CONTENT_LENGTH).parse().ok()
}

/// The protocol version as OpenTelemetry names it (`network.protocol.version`).
#[cfg(feature = "trace")]
pub(crate) fn http_version(version: http::Version) -> &'static str {
    match version {
        http::Version::HTTP_09 => "0.9",
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_11 => "1.1",
        http::Version::HTTP_2 => "2",
        http::Version::HTTP_3 => "3",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&HeaderName, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| ((*name).clone(), HeaderValue::from_static(value)))
            .collect()
    }

    #[test]
    fn forwarded_host_wins_over_host() {
        assert_eq!(host(&headers(&[(&HOST, "lb.internal")])), "lb.internal");
        assert_eq!(
            host(&headers(&[
                (&HOST, "lb.internal"),
                (&X_FORWARDED_HOST, "api.example"),
            ])),
            "api.example"
        );
    }

    #[cfg(feature = "trace")]
    #[test]
    fn client_ip_prefers_x_client_ip_then_first_forwarded_for() {
        assert_eq!(
            client_ip(&headers(&[(&X_FORWARDED_FOR, "10.0.0.1, 10.0.0.2")])),
            "10.0.0.1"
        );
        assert_eq!(
            client_ip(&headers(&[
                (&X_FORWARDED_FOR, "10.0.0.1"),
                (&X_CLIENT_IP, "192.0.2.7"),
            ])),
            "192.0.2.7"
        );
        assert_eq!(client_ip(&HeaderMap::new()), "");
    }

    #[cfg(feature = "trace")]
    #[test]
    fn scheme_prefers_forwarded_proto() {
        let mut request = Request::new(());
        assert_eq!(scheme(&request), "http");
        request
            .headers_mut()
            .insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
        assert_eq!(scheme(&request), "https");
    }

    #[test]
    fn content_length_is_parsed() {
        assert_eq!(
            content_length(&headers(&[(&CONTENT_LENGTH, "42")])),
            Some(42)
        );
        assert_eq!(content_length(&HeaderMap::new()), None);
    }

    #[test]
    fn unknown_methods_are_other() {
        assert_eq!(known_method(&Method::GET), "GET");
        assert_eq!(
            known_method(&Method::from_bytes(b"PROPFIND").expect("valid method")),
            "_OTHER"
        );
    }

    #[cfg(feature = "trace")]
    #[test]
    fn known_http_versions_are_named() {
        assert_eq!(http_version(http::Version::HTTP_11), "1.1");
        assert_eq!(http_version(http::Version::HTTP_2), "2");
    }
}
