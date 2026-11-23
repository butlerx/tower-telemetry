//! What the layers read from a request. Behind a load balancer or CDN, the
//! client, host and scheme come from forwarding headers.
//!
//! These headers are taken as given: run behind a proxy that sets or strips
//! them, or the client controls what they say.

#[cfg(feature = "trace")]
use std::borrow::Cow;

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
#[inline]
pub(crate) fn header<'a>(headers: &'a HeaderMap, name: &HeaderName) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map_or("", str::trim)
}

/// `X-Forwarded-Host`, else `Host`.
#[inline]
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
#[inline]
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
#[inline]
pub(crate) fn default_port(scheme: &str) -> Option<u16> {
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("ws") {
        Some(80)
    } else if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("wss") {
        Some(443)
    } else {
        None
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

/// Query parameters whose values are credentials: signed URLs (AWS, Google
/// and Azure SAS), and the common names for bearer and API tokens. Names
/// match as sent or after percent-decoding, without regard to case.
#[cfg(feature = "trace")]
const SENSITIVE_QUERY_KEYS: [&str; 13] = [
    "AWSAccessKeyId",
    "Signature",
    "sig",
    "X-Goog-Signature",
    "X-Amz-Signature",
    "X-Amz-Credential",
    "X-Amz-Security-Token",
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "api_key",
    "client_secret",
];

/// One hex digit's value, for a percent-escape.
#[cfg(feature = "trace")]
fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// A query parameter's name, decoded as form encoding has it: `+` is a
/// space and `%XX` is that byte. Borrows when there's nothing to decode,
/// which is most names. `None` for an escape that isn't two hex digits, or
/// a result that isn't UTF-8: such a name can't be one of the credentials.
#[cfg(feature = "trace")]
fn decode_name(name: &str) -> Option<Cow<'_, str>> {
    if !name.contains(['%', '+']) {
        return Some(Cow::Borrowed(name));
    }
    let bytes = name.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                let high = hex_value(*bytes.get(index + 1)?)?;
                let low = hex_value(*bytes.get(index + 2)?)?;
                decoded.push(high << 4 | low);
                index += 3;
            }
            _ => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok().map(Cow::Owned)
}

/// Whether a query parameter's name is a credential: one of the built-in
/// names, or one of `extra`, matched as sent or after percent-decoding, and
/// without regard to case. The name as sent is tried first, so a configured
/// name that itself contains an escape, or isn't valid as decoded, still
/// matches.
#[cfg(feature = "trace")]
fn is_sensitive_key(key: &str, extra: &[&str]) -> bool {
    if extra
        .iter()
        .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
    {
        return true;
    }
    decode_name(key).is_some_and(|name| {
        SENSITIVE_QUERY_KEYS
            .iter()
            .chain(extra)
            .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
    })
}

/// Whether one `key=value` pair of a query carries a credential.
#[cfg(feature = "trace")]
fn is_sensitive_pair(pair: &str, extra: &[&str]) -> bool {
    pair.split_once('=')
        .is_some_and(|(key, _)| is_sensitive_key(key, extra))
}

/// `query` with each credential's value replaced by `REDACTED`, its name
/// kept. `extra` adds parameter names to the built-in list. Borrows when
/// there's nothing to redact, which is most requests.
#[cfg(feature = "trace")]
pub(crate) fn redact_query<'a>(query: &'a str, extra: &[&str]) -> Cow<'a, str> {
    if !query.split('&').any(|pair| is_sensitive_pair(pair, extra)) {
        return Cow::Borrowed(query);
    }
    let mut redacted = String::with_capacity(query.len());
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            redacted.push('&');
        }
        match pair.split_once('=') {
            Some((key, _)) if is_sensitive_pair(pair, extra) => {
                redacted.push_str(key);
                redacted.push_str("=REDACTED");
            }
            _ => redacted.push_str(pair),
        }
    }
    Cow::Owned(redacted)
}

/// The request's query for `url.query`, with credentials redacted.
#[cfg(feature = "trace")]
pub(crate) fn query<'a, B>(request: &'a Request<B>, extra: &[&str]) -> Option<Cow<'a, str>> {
    request
        .uri()
        .query()
        .map(|query| redact_query(query, extra))
}

/// An outgoing request's full URL, for `url.full`: without credentials in
/// the authority, and with the query redacted as [`query`] does.
#[cfg(feature = "trace")]
pub(crate) fn full_url<B>(request: &Request<B>, extra: &[&str]) -> String {
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
    let scheme = uri.scheme_str().unwrap_or("http");
    let path = uri.path();
    let query = query(request, extra);
    // One allocation, sized for the URL as given.
    let mut url = String::with_capacity(
        scheme.len()
            + 3
            + authority.len()
            + path.len()
            + query.as_deref().map_or(0, |query| query.len() + 1),
    );
    url.push_str(scheme);
    url.push_str("://");
    url.push_str(authority);
    url.push_str(path);
    if let Some(query) = query {
        url.push('?');
        url.push_str(&query);
    }
    url
}

/// `X-Client-IP`, else the first `X-Forwarded-For` address.
#[cfg(feature = "trace")]
#[inline]
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

/// The request's scheme as it arrived: `X-Forwarded-Proto`, else the
/// request's own scheme, else `http`. Used to infer the port; the value
/// isn't recorded as is, see [`url_scheme`].
#[cfg(any(feature = "trace", feature = "metrics"))]
#[inline]
pub(crate) fn scheme<B>(request: &Request<B>) -> &str {
    match header(request.headers(), &X_FORWARDED_PROTO) {
        "" => request.uri().scheme_str().unwrap_or("http"),
        forwarded => forwarded,
    }
}

/// `url.scheme`: [`scheme`], bounded to `http`, `https`, `ws` or `wss`, and
/// `_OTHER` for anything else. The value can come from the client, and each
/// distinct value would be a new series, so the set is fixed, as
/// [`known_method`] does for methods.
#[cfg(any(feature = "trace", feature = "metrics"))]
#[inline]
pub(crate) fn url_scheme<B>(request: &Request<B>) -> &'static str {
    let scheme = scheme(request);
    if scheme.eq_ignore_ascii_case("https") {
        "https"
    } else if scheme.eq_ignore_ascii_case("http") {
        "http"
    } else if scheme.eq_ignore_ascii_case("wss") {
        "wss"
    } else if scheme.eq_ignore_ascii_case("ws") {
        "ws"
    } else {
        "_OTHER"
    }
}

/// The request's method as OpenTelemetry records it: `_OTHER` for methods
/// outside RFC 9110 and PATCH, which would otherwise be unbounded.
#[inline]
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
#[inline]
pub(crate) fn content_length(headers: &HeaderMap) -> Option<u64> {
    header(headers, &CONTENT_LENGTH).parse().ok()
}

/// The protocol version as OpenTelemetry names it (`network.protocol.version`).
#[cfg(any(feature = "trace", feature = "metrics"))]
#[inline]
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

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn scheme_is_bounded_to_known_schemes_or_other() {
        let forwarded = |proto: &'static str| {
            let mut request = Request::get("/").body(()).expect("static request");
            request
                .headers_mut()
                .insert(X_FORWARDED_PROTO, HeaderValue::from_static(proto));
            request
        };
        assert_eq!(url_scheme(&forwarded("HTTPS")), "https");
        assert_eq!(url_scheme(&forwarded("WSS")), "wss");
        assert_eq!(url_scheme(&forwarded("ws")), "ws");
        assert_eq!(url_scheme(&forwarded("javascript")), "_OTHER");
        assert_eq!(url_scheme(&forwarded("x-1")), "_OTHER");

        let absolute = Request::get("x-1://host/a")
            .body(())
            .expect("static request");
        assert_eq!(url_scheme(&absolute), "_OTHER");
        let secure = Request::get("https://host/a")
            .body(())
            .expect("static request");
        assert_eq!(url_scheme(&secure), "https");
    }

    #[cfg(any(feature = "trace", feature = "metrics"))]
    #[test]
    fn websocket_schemes_keep_their_default_ports() {
        let forwarded = |proto: &'static str| {
            Request::get("/")
                .header(HOST, "api.example")
                .header(X_FORWARDED_PROTO, proto)
                .body(())
                .expect("static request")
        };
        assert_eq!(
            server_address(&forwarded("wss")),
            ("api.example", Some(443))
        );
        assert_eq!(server_address(&forwarded("ws")), ("api.example", Some(80)));
        assert_eq!(
            server_address(&forwarded("WSS")),
            ("api.example", Some(443))
        );
        assert_eq!(server_address(&forwarded("x-1")), ("api.example", None));
    }

    #[cfg(feature = "trace")]
    #[test]
    fn redact_query_keeps_names_and_borrows_when_clean() {
        assert!(matches!(
            redact_query("page=2&sort=name", &[]),
            Cow::Borrowed("page=2&sort=name")
        ));
        assert_eq!(
            redact_query("access_token=abc&page=2&X-Amz-Security-Token=xyz&flag", &[]),
            "access_token=REDACTED&page=2&X-Amz-Security-Token=REDACTED&flag"
        );
        assert_eq!(
            redact_query("x-amz-signature=abc&TOKEN=def", &[]),
            "x-amz-signature=REDACTED&TOKEN=REDACTED"
        );
        assert_eq!(
            redact_query("tokenizer=1&my_token=2", &[]),
            "tokenizer=1&my_token=2"
        );
    }

    #[cfg(feature = "trace")]
    #[test]
    fn extra_keys_extend_the_built_in_list() {
        assert!(matches!(
            redact_query("page=2", &["X-Api-Key"]),
            Cow::Borrowed("page=2")
        ));
        assert_eq!(
            redact_query("x-api-key=abc&access_token=def&page=2", &["X-Api-Key"]),
            "x-api-key=REDACTED&access_token=REDACTED&page=2"
        );
        assert_eq!(
            redact_query("x-api-key=abc", &[]),
            "x-api-key=abc",
            "extras only apply when given"
        );
    }

    #[cfg(feature = "trace")]
    #[test]
    fn encoded_names_are_matched_decoded() {
        assert_eq!(
            redact_query("access%5Ftoken=secret&page=2", &[]),
            "access%5Ftoken=REDACTED&page=2"
        );
        assert_eq!(
            redact_query("ACCESS%5fTOKEN=secret", &[]),
            "ACCESS%5fTOKEN=REDACTED"
        );
        assert_eq!(
            redact_query("%61ccess_token=secret", &[]),
            "%61ccess_token=REDACTED"
        );
        assert_eq!(
            redact_query("x%2Dapi%2Dkey=abc", &["x-api-key"]),
            "x%2Dapi%2Dkey=REDACTED",
            "user-added names are matched decoded too"
        );
        assert_eq!(
            redact_query("x+api+key=abc", &["x api key"]),
            "x+api+key=REDACTED",
            "`+` decodes to a space"
        );
        assert_eq!(
            redact_query("pa%67e=2", &[]),
            "pa%67e=2",
            "a decoded name that isn't a credential is left alone"
        );
    }

    #[cfg(feature = "trace")]
    #[test]
    fn configured_names_match_as_sent_too() {
        assert_eq!(
            redact_query("x%2Dapi%2Dkey=secret", &["x%2Dapi%2Dkey"]),
            "x%2Dapi%2Dkey=REDACTED",
            "a configured name with an escape matches as sent"
        );
        assert_eq!(
            redact_query("x-api%key=secret", &["x-api%key"]),
            "x-api%key=REDACTED",
            "a configured name that isn't a valid escape matches as sent"
        );
    }

    #[cfg(feature = "trace")]
    #[test]
    fn invalid_escapes_do_not_match_or_panic() {
        for name in ["access%zz", "access%5", "%", "%FF", "access%2"] {
            let query = format!("{name}=secret&page=2");
            assert_eq!(
                redact_query(&query, &[]),
                query,
                "{name} is not a credential name"
            );
        }
    }

    #[cfg(feature = "trace")]
    #[test]
    fn server_query_is_redacted() {
        let request = Request::get("/files?access_token=abc&page=2")
            .body(())
            .expect("static request");
        assert_eq!(
            query(&request, &[]).as_deref(),
            Some("access_token=REDACTED&page=2")
        );
        let bare = Request::get("/files").body(()).expect("static request");
        assert_eq!(query(&bare, &[]), None);
    }

    #[cfg(feature = "trace")]
    #[test]
    fn full_url_drops_credentials_and_redacts_signatures() {
        let request =
            Request::get("https://user:pw@bucket.example/key?X-Goog-Signature=abc&part=2")
                .body(())
                .expect("static request");
        assert_eq!(
            full_url(&request, &[]),
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
