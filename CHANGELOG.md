# Changelog

## 0.0.0 (2026-11-16)

First release.

### ⚠ BREAKING CHANGES

* construct layers with `Default` instead of an empty `new()` ([3b81c03](https://github.com/butlerx/tower-telemetry/commit/3b81c03b3d4ac821d0807ec5cbb691acff288301))

### Features

* OpenTelemetry HTTP and messaging spans and HTTP metrics as Tower layers ([5eb09f3](https://github.com/butlerx/tower-telemetry/commit/5eb09f380e0c3b80d9b1e7e7542c84de7e64bb39))
  * `OtelTraceLayer`: one server span per request, generic over a `SpanDecorator` and `RequestFilter`, continuing the caller's trace
  * `HttpMetricsLayer`: request count, duration, time to first byte, body sizes and active requests through the `metrics` facade
  * `messaging`: `MessagingSpan`, header context propagation and `MessageTraceLayer` for consumers, per the messaging semantic conventions
  * `runtime-metrics`: Tokio runtime gauges sampled once a second
  * `axum`: name spans and metrics by the matched route
* HealthcheckFilter also skips /health, /ready, /healthz and /healthcheck ([e7501cd](https://github.com/butlerx/tower-telemetry/commit/e7501cdb3c5f09211fc6895dc06439a12f53a4b4))
* HealthcheckFilter skips well-known health checker user agents ([063e51a](https://github.com/butlerx/tower-telemetry/commit/063e51ae71be2403076ee3a6795e4d0b08a7153d))

### Documentation

* request filters and custom span attributes ([2a4092a](https://github.com/butlerx/tower-telemetry/commit/2a4092a02b851562c7b3b0d30db5b2848c5a55ed))
* document every item and show feature requirements on docs.rs ([e10f953](https://github.com/butlerx/tower-telemetry/commit/e10f953b32aeb29ca093a52ffd09e6e8dec16ed9))
