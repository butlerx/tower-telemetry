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
#[cfg(any(feature = "trace", feature = "metrics"))]
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

/// Split a `host[:port]` authority, as `Host` and `X-Forwarded-Host` carry
/// it, into the host and port. IPv6 hosts lose their brackets, as
/// `server.address` records them; a port that isn't a number is dropped.
#[cfg(any(feature = "trace", feature = "metrics"))]
pub(crate) fn split_host_port(authority: &str) -> (&str, Option<u16>) {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, rest) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        return (
            host,
            rest.strip_prefix(':').and_then(|port| port.parse().ok()),
        );
    }
    match authority.rsplit_once(':') {
        // A bare IPv6 address has colons but no port.
        Some((host, port)) if !host.contains(':') => (host, port.parse().ok()),
        _ => (authority, None),
    }
}

/// The port a scheme uses when none is given.
#[cfg(any(feature = "trace", feature = "metrics"))]
pub(crate) fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// The address and port a server was reached on: `X-Forwarded-Host`, else
/// `Host`, with the scheme's default port when the header has none.
#[cfg(any(feature = "trace", feature = "metrics"))]
pub(crate) fn server_address<B>(request: &Request<B>) -> (&str, Option<u16>) {
    let (address, port) = split_host_port(host(request.headers()));
    (address, port.or_else(|| default_port(scheme(request))))
}

/// The server an outgoing request goes to: the URI's authority, else the
/// `Host` header, with the scheme's default port when neither gives one.
#[cfg(any(feature = "trace", feature = "metrics"))]
pub(crate) fn target<B>(request: &Request<B>) -> (&str, Option<u16>) {
    let uri = request.uri();
    let (address, port) = split_host_port(
        uri.authority()
            .map_or_else(
                || header(request.headers(), &HOST),
                http::uri::Authority::as_str,
            )
            .rsplit('@')
            .next()
            .unwrap_or_default(),
    );
    (
        address,
        port.or_else(|| default_port(uri.scheme_str().unwrap_or("http"))),
    )
}

/// Query parameters whose values are credentials: AWS and Google signed
/// URLs, and Azure SAS tokens. `url.full` records them as `REDACTED`, as the
/// conventions ask.
#[cfg(feature = "trace")]
const SENSITIVE_QUERY_KEYS: [&str; 4] = ["AWSAccessKeyId", "Signature", "sig", "X-Goog-Signature"];

/// An outgoing request's full URL, for `url.full`: without credentials in
/// the authority, and with signed-URL parameters redacted.
#[cfg(feature = "trace")]
pub(crate) fn full_url<B>(request: &Request<B>) -> String {
    let uri = request.uri();
    let authority = uri
        .authority()
        .map_or_else(
            || header(request.headers(), &HOST),
            http::uri::Authority::as_str,
        )
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let query = uri.query().map(|query| {
        query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((key, _)) if SENSITIVE_QUERY_KEYS.contains(&key) => {
                    std::borrow::Cow::Owned(format!("{key}=REDACTED"))
                }
                _ => std::borrow::Cow::Borrowed(pair),
            })
            .collect::<Vec<_>>()
            .join("&")
    });
    format!(
        "{}://{authority}{}{}{}",
        uri.scheme_str().unwrap_or("http"),
        uri.path(),
        if query.is_some() { "?" } else { "" },
        query.unwrap_or_default(),
    )
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
#[cfg(any(feature = "trace", feature = "metrics"))]
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
#[cfg(any(feature = "trace", feature = "metrics"))]
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

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn scheme_prefers_forwarded_proto() {
        let mut request = Request::new(());
        assert_eq!(scheme(&request), "http");
        request
            .headers_mut()
            .insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
        assert_eq!(scheme(&request), "https");
    }

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn hosts_split_from_ports() {
        assert_eq!(split_host_port("api.example"), ("api.example", None));
        assert_eq!(
            split_host_port("api.example:8080"),
            ("api.example", Some(8080))
        );
        assert_eq!(split_host_port("[::1]:8443"), ("::1", Some(8443)));
        assert_eq!(split_host_port("[::1]"), ("::1", None));
        assert_eq!(split_host_port("::1"), ("::1", None));
        assert_eq!(split_host_port("api.example:http"), ("api.example", None));
    }

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn server_address_defaults_the_port_by_scheme() {
        let request = |host: &'static str, proto: &'static str| {
            Request::get("/")
                .header(HOST, host)
                .header(X_FORWARDED_PROTO, proto)
                .body(())
                .expect("static request")
        };
        assert_eq!(
            server_address(&request("api.example", "https")),
            ("api.example", Some(443))
        );
        assert_eq!(
            server_address(&request("api.example:8080", "http")),
            ("api.example", Some(8080))
        );
    }

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn target_reads_the_uri_without_credentials() {
        let request = |uri: &'static str| Request::get(uri).body(()).expect("static request");
        assert_eq!(
            target(&request("https://api.example/v1")),
            ("api.example", Some(443))
        );
        assert_eq!(
            target(&request("http://user:pw@10.0.0.1:9000/")),
            ("10.0.0.1", Some(9000))
        );
        let relative = Request::get("/v1")
            .header(HOST, "api.example")
            .body(())
            .expect("static request");
        assert_eq!(target(&relative), ("api.example", Some(80)));
    }

    #[cfg(feature = "trace")]
    #[test]
    fn full_url_drops_credentials_and_redacts_signatures() {
        let request =
            Request::get("https://user:pw@bucket.example/key?X-Goog-Signature=abc&part=2")
                .body(())
                .expect("static request");
        assert_eq!(
            full_url(&request),
            "https://bucket.example/key?X-Goog-Signature=REDACTED&part=2"
        );
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

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn known_http_versions_are_named() {
        assert_eq!(http_version(http::Version::HTTP_11), "1.1");
        assert_eq!(http_version(http::Version::HTTP_2), "2");
    }
}
