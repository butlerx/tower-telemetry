//! [`RequestFilter`]: which requests to leave out of tracing.

use http::Request;
use http::header::USER_AGENT;

use crate::request;

/// Which requests to leave out of tracing.
///
/// Implement it on a unit struct and pass it as the layer's second type
/// parameter. It decides from the request alone, before the span exists, so
/// it costs nothing for the requests it drops: they get a disabled span
/// ([`Span::none`](tracing::Span::none)), and nothing is recorded or exported.
/// It filters traces only; [`HttpMetricsLayer`](crate::HttpMetricsLayer)
/// still counts the request.
///
/// Leave out probes by path, and AWS health checks by user agent:
///
/// ```
/// use http::Request;
/// use tower_telemetry::{DefaultSpanDecorator, HealthcheckFilter, OtelTraceLayer, RequestFilter};
///
/// #[derive(Debug, Clone, Copy, Default)]
/// struct SkipProbes;
///
/// impl RequestFilter for SkipProbes {
///     fn should_ignore<B>(request: &Request<B>) -> bool {
///         matches!(request.uri().path(), "/healthz" | "/readyz")
///             || HealthcheckFilter::should_ignore(request)
///     }
/// }
///
/// let layer = OtelTraceLayer::<DefaultSpanDecorator, SkipProbes>::default();
/// # let _ = layer;
/// # assert!(SkipProbes::should_ignore(&Request::get("/healthz").body(()).unwrap()));
/// # assert!(!SkipProbes::should_ignore(&Request::get("/users").body(()).unwrap()));
/// ```
///
/// Keep it cheap: it runs on every request. Match on the method, path and
/// headers; don't parse bodies or do I/O.
pub trait RequestFilter {
    /// `true` to leave the request out. Default: trace every request.
    fn should_ignore<B>(_request: &Request<B>) -> bool {
        false
    }
}

/// Traces every request.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpFilter;

impl RequestFilter for NoOpFilter {}

/// Leaves out AWS load balancer and ECS health checks, by user agent, so they
/// don't drown real traffic.
#[derive(Debug, Clone, Copy, Default)]
pub struct HealthcheckFilter;

impl RequestFilter for HealthcheckFilter {
    fn should_ignore<B>(request: &Request<B>) -> bool {
        let user_agent = request::header(request.headers(), &USER_AGENT);
        user_agent.starts_with("ELB-HealthChecker/") || user_agent.starts_with("ECS-HealthChecker")
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn request_with_user_agent(user_agent: &'static str) -> Request<()> {
        let mut request = Request::new(());
        request
            .headers_mut()
            .insert(USER_AGENT, HeaderValue::from_static(user_agent));
        request
    }

    #[test]
    fn healthcheck_filter_ignores_load_balancer_and_ecs_checks() {
        assert!(HealthcheckFilter::should_ignore(&request_with_user_agent(
            "ELB-HealthChecker/2.0"
        )));
        assert!(HealthcheckFilter::should_ignore(&request_with_user_agent(
            "ECS-HealthChecker/1.0"
        )));
        assert!(!HealthcheckFilter::should_ignore(&request_with_user_agent(
            "curl/8.0"
        )));
        assert!(!HealthcheckFilter::should_ignore(&Request::new(())));
    }

    #[test]
    fn noop_filter_keeps_everything() {
        assert!(!NoOpFilter::should_ignore(&request_with_user_agent(
            "ELB-HealthChecker/2.0"
        )));
    }
}
