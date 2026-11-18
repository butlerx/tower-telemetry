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
/// Leave out scrapes and liveness probes as well as the usual health checks:
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
///         matches!(request.uri().path(), "/metrics" | "/livez")
///             || HealthcheckFilter::should_ignore(request)
///     }
/// }
///
/// let layer = OtelTraceLayer::<DefaultSpanDecorator, SkipProbes>::default();
/// # let _ = layer;
/// # assert!(SkipProbes::should_ignore(&Request::get("/metrics").body(()).unwrap()));
/// # assert!(!SkipProbes::should_ignore(&Request::get("/users").body(()).unwrap()));
/// ```
///
/// Keep it cheap: it runs on every request. Match on the method, path and
/// headers; don't parse bodies or do I/O.
pub trait RequestFilter {
    /// Whether to leave `request` out of tracing: `true` skips it.
    ///
    /// Called once per request, before the span is created, with only the
    /// request's method, URI, version, headers and extensions to go on: the body
    /// hasn't been read. Keep it cheap. Default: `false`, trace every request.
    fn should_ignore<B>(_request: &Request<B>) -> bool {
        false
    }
}

/// A [`RequestFilter`] that keeps every request: the default for
/// [`OtelTraceLayer`](crate::OtelTraceLayer).
///
/// Use it to trace health checks too, or as the base for a filter of your own.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpFilter;

impl RequestFilter for NoOpFilter {}

/// The request paths [`HealthcheckFilter`] leaves out of tracing.
///
/// | Path           | Commonly used for                               |
/// | -------------- | ----------------------------------------------- |
/// | `/health`      | load balancer and container health checks       |
/// | `/ready`       | readiness checks                                |
/// | `/healthz`     | Kubernetes-style liveness and health probes     |
/// | `/healthcheck` | load balancer target group health checks        |
///
/// # Matching
///
/// A request matches when its path is one of these exactly:
///
/// - A single trailing slash is ignored: `/health/` matches.
/// - The query string is ignored: `/health?verbose=1` matches.
/// - Case matters: `/Health` doesn't match.
/// - Prefixes and suffixes don't match: `/api/health`, `/health/db` and
///   `/healthy` are all traced. If your service is mounted under a prefix,
///   its health path won't match.
///
/// Only the path is checked, not axum's matched route, so a request matches
/// whether or not a route handles it.
///
/// # Adding paths
///
/// To skip more paths, write your own [`RequestFilter`] that checks them and
/// then falls back to [`HealthcheckFilter`]:
///
/// ```
/// use http::Request;
/// use tower_telemetry::{HealthcheckFilter, RequestFilter};
///
/// #[derive(Debug, Clone, Copy, Default)]
/// struct ProbeFilter;
///
/// impl RequestFilter for ProbeFilter {
///     fn should_ignore<B>(request: &Request<B>) -> bool {
///         matches!(request.uri().path(), "/livez" | "/readyz" | "/api/health")
///             || HealthcheckFilter::should_ignore(request)
///     }
/// }
/// # let request = |path| Request::get(path).body(()).expect("static request");
/// # assert!(ProbeFilter::should_ignore(&request("/api/health")));
/// # assert!(ProbeFilter::should_ignore(&request("/healthz/?probe=1")));
/// # assert!(!ProbeFilter::should_ignore(&request("/Health")));
/// ```
pub const HEALTHCHECK_PATHS: [&str; 4] = ["/health", "/ready", "/healthz", "/healthcheck"];

/// The user-agent prefixes of well-known health checkers, which
/// [`HealthcheckFilter`] leaves out whatever path they probe:
///
/// | Prefix                                  | Sent by                          |
/// | --------------------------------------- | -------------------------------- |
/// | `ELB-HealthChecker/`                    | AWS Elastic Load Balancing       |
/// | `ECS-HealthChecker`                     | AWS ECS container health checks  |
/// | `Amazon-Route53-Health-Check-Service`   | AWS Route 53 health checks       |
/// | `Amazon Route 53 Health Check Service`  | the same, as some servers log it |
/// | `kube-probe/`                           | Kubernetes liveness/readiness probes |
/// | `GoogleHC`                              | Google Cloud load balancing      |
/// | `Load Balancer Agent`                   | Azure Load Balancer              |
/// | `Edge Health Probe`                     | Azure Front Door                 |
/// | `Envoy/HC`                              | Envoy active health checks       |
/// | `Consul Health Check`                   | Consul                           |
///
/// Uptime monitors (Pingdom, `UptimeRobot`, synthetic tests) are left in:
/// they measure what users see. Skip them with your own [`RequestFilter`].
pub const HEALTHCHECK_USER_AGENTS: [&str; 10] = [
    "ELB-HealthChecker/",
    "ECS-HealthChecker",
    "Amazon-Route53-Health-Check-Service",
    "Amazon Route 53 Health Check Service",
    "kube-probe/",
    "GoogleHC",
    "Load Balancer Agent",
    "Edge Health Probe",
    "Envoy/HC",
    "Consul Health Check",
];

/// Leaves out health checks so they don't drown real traffic: requests to
/// any of [`HEALTHCHECK_PATHS`], and requests from any of
/// [`HEALTHCHECK_USER_AGENTS`] whatever path they probe.
#[derive(Debug, Clone, Copy, Default)]
pub struct HealthcheckFilter;

impl RequestFilter for HealthcheckFilter {
    /// `true` when the path is one of [`HEALTHCHECK_PATHS`] or the user agent
    /// starts with one of [`HEALTHCHECK_USER_AGENTS`].
    fn should_ignore<B>(request: &Request<B>) -> bool {
        is_healthcheck_path(request.uri().path())
            || is_healthcheck_user_agent(request::header(request.headers(), &USER_AGENT))
    }
}

/// Whether `user_agent` starts with one of [`HEALTHCHECK_USER_AGENTS`].
#[inline]
fn is_healthcheck_user_agent(user_agent: &str) -> bool {
    HEALTHCHECK_USER_AGENTS
        .iter()
        .any(|prefix| user_agent.starts_with(prefix))
}

/// Whether `path` is one of [`HEALTHCHECK_PATHS`], with or without a
/// trailing slash.
#[inline]
fn is_healthcheck_path(path: &str) -> bool {
    let path = path
        .strip_suffix('/')
        .filter(|path| !path.is_empty())
        .unwrap_or(path);
    HEALTHCHECK_PATHS.contains(&path)
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
    fn healthcheck_filter_ignores_well_known_health_checkers() {
        for user_agent in [
            "ELB-HealthChecker/2.0",
            "ECS-HealthChecker",
            "Amazon-Route53-Health-Check-Service (ref 0a1b2c3d-4e5f; report http://amzn.to/1vsZADi)",
            "kube-probe/1.31",
            "GoogleHC/1.0",
            "Load Balancer Agent",
            "Edge Health Probe",
            "Envoy/HC",
            "Consul Health Check",
        ] {
            assert!(
                HealthcheckFilter::should_ignore(&request_with_user_agent(user_agent)),
                "{user_agent}"
            );
        }
        for user_agent in [
            "Mozilla/5.0 (compatible; UptimeRobot/2.0)",
            "curl/8.0",
            "kube-proxy",
        ] {
            assert!(
                !HealthcheckFilter::should_ignore(&request_with_user_agent(user_agent)),
                "{user_agent}"
            );
        }
    }

    #[test]
    fn healthcheck_filter_ignores_healthcheck_paths() {
        for path in HEALTHCHECK_PATHS
            .iter()
            .flat_map(|path| [(*path).to_owned(), format!("{path}/")])
        {
            let request = Request::get(path.as_str())
                .body(())
                .expect("static request");
            assert!(HealthcheckFilter::should_ignore(&request), "{path}");
        }
        for path in ["/", "/healthy", "/health/db", "/api/health", "/users"] {
            let request = Request::get(path).body(()).expect("static request");
            assert!(!HealthcheckFilter::should_ignore(&request), "{path}");
        }
    }

    #[test]
    fn noop_filter_keeps_everything() {
        assert!(!NoOpFilter::should_ignore(&request_with_user_agent(
            "ELB-HealthChecker/2.0"
        )));
    }
}
