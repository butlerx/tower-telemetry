//! Label values that are cheap to copy.
//!
//! Every request's labels are copied once per metric it records. An owned
//! `SharedString` copies its string each time; a static one copies a
//! pointer, and an `Arc`-backed one bumps a reference count. So known
//! values are static, and computed ones are made `Arc`-backed once.

use std::borrow::Cow;
use std::sync::Arc;

use ::metrics::SharedString;
use http::StatusCode;

/// `value`, allocated once and shared by every copy.
#[inline]
pub(crate) fn shared(value: &str) -> SharedString {
    SharedString::from_shared(Arc::from(value))
}

/// A [`MetricLabels`](super::MetricLabels) value: static ones stay static,
/// owned ones become shared.
#[inline]
pub(crate) fn from_cow(value: Cow<'static, str>) -> SharedString {
    match value {
        Cow::Borrowed(value) => SharedString::const_str(value),
        Cow::Owned(value) => SharedString::from_shared(Arc::from(value)),
    }
}

/// `http.response.status_code` and `error.type`: static for the common
/// statuses, shared for the rest.
#[inline]
pub(crate) fn status(status: StatusCode) -> SharedString {
    let common = match status.as_u16() {
        200 => "200",
        201 => "201",
        202 => "202",
        204 => "204",
        206 => "206",
        301 => "301",
        302 => "302",
        303 => "303",
        304 => "304",
        307 => "307",
        308 => "308",
        400 => "400",
        401 => "401",
        403 => "403",
        404 => "404",
        405 => "405",
        408 => "408",
        409 => "409",
        410 => "410",
        412 => "412",
        413 => "413",
        415 => "415",
        422 => "422",
        429 => "429",
        499 => "499",
        500 => "500",
        501 => "501",
        502 => "502",
        503 => "503",
        504 => "504",
        _ => return shared(status.as_str()),
    };
    SharedString::const_str(common)
}

/// `server.port`: static for the default HTTP ports, shared for the rest.
#[inline]
pub(crate) fn port(port: u16) -> SharedString {
    match port {
        80 => SharedString::const_str("80"),
        443 => SharedString::const_str("443"),
        other => SharedString::from_shared(Arc::from(other.to_string())),
    }
}

/// `url.scheme`: static for `http` and `https`, shared for the rest.
#[inline]
pub(crate) fn scheme(scheme: &str) -> SharedString {
    match scheme {
        "https" => SharedString::const_str("https"),
        "http" => SharedString::const_str("http"),
        other => shared(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_keep_their_codes() {
        for code in [200, 404, 418, 503, 599] {
            let status = StatusCode::from_u16(code).expect("valid status");
            assert_eq!(&*super::status(status), status.as_str());
        }
    }

    #[test]
    fn ports_keep_their_numbers() {
        assert_eq!(&*port(443), "443");
        assert_eq!(&*port(8080), "8080");
    }
}
