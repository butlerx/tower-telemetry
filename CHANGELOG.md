# Changelog

## [0.2.0](https://github.com/butlerx/tower-telemetry/compare/v0.1.0...v0.2.0) (2026-11-18)


### ⚠ BREAKING CHANGES

* SpanDecorator::on_response no longer takes the span's context; call span.context() when needed

### Features

* HttpClientMetricsLayer, the OpenTelemetry http.client.* metrics ([1853920](https://github.com/butlerx/tower-telemetry/commit/18539208ffd83b8c80f69171a63360eb26fbcd9a))
* OpenTelemetry messaging metrics and MessagingSpan::run ([#11](https://github.com/butlerx/tower-telemetry/issues/11)) ([66915e4](https://github.com/butlerx/tower-telemetry/commit/66915e41c1e92921d4290ba8633a9f887e38fb57))
* OtelClientTraceLayer, one client span per outgoing request with the trace context sent along ([1853920](https://github.com/butlerx/tower-telemetry/commit/18539208ffd83b8c80f69171a63360eb26fbcd9a))
* SpanDecorator::on_response no longer takes the span's context; call span.context() when needed ([fce2fd1](https://github.com/butlerx/tower-telemetry/commit/fce2fd142e1500527d9acd748f2dee6e6504d723))


### Bug Fixes

* leave HTTP server span status unset on success, and set error.type on 5xx ([fce2fd1](https://github.com/butlerx/tower-telemetry/commit/fce2fd142e1500527d9acd748f2dee6e6504d723))
* server.address is the host without the port; server.port is recorded beside it ([1853920](https://github.com/butlerx/tower-telemetry/commit/18539208ffd83b8c80f69171a63360eb26fbcd9a))


### Performance

* cheaper metric labels: static for known values, shared for computed ones ([fce2fd1](https://github.com/butlerx/tower-telemetry/commit/fce2fd142e1500527d9acd748f2dee6e6504d723))
* inline small hot-path helpers and builder methods ([#13](https://github.com/butlerx/tower-telemetry/issues/13)) ([1fecece](https://github.com/butlerx/tower-telemetry/commit/1fecece3154fa0911beacbc0183a1cfb1f1ee890))

## [0.1.0](https://github.com/butlerx/tower-telemetry/compare/v0.0.1...v0.1.0) (2026-11-17)


### ⚠ BREAKING CHANGES

* OpenTelemetry metric names, labels and units by default ([#7](https://github.com/butlerx/tower-telemetry/issues/7))

### Features

* OpenTelemetry metric names, labels and units by default ([#7](https://github.com/butlerx/tower-telemetry/issues/7)) ([8177ba5](https://github.com/butlerx/tower-telemetry/commit/8177ba593ae67d67cf608dc1edcb1072fd9a1cd4))

## [0.0.1](https://github.com/butlerx/tower-telemetry/compare/v0.0.0...v0.0.1) (2026-11-16)


### Bug Fixes

* export span integers (status code, sizes, port, batch count) as integers, not strings ([66794e4](https://github.com/butlerx/tower-telemetry/commit/66794e45c356f807b24b5f79bb9bb537488beec5))
* requests dropped before a response no longer record duration or response size ([13f2add](https://github.com/butlerx/tower-telemetry/commit/13f2add4040e310b27a6f412da4e558f7ada1d3b))


### Code Improvements

* take attribute names from opentelemetry-semantic-conventions ([66794e4](https://github.com/butlerx/tower-telemetry/commit/66794e45c356f807b24b5f79bb9bb537488beec5))


### Documentation

* document every public item in depth ([13f2add](https://github.com/butlerx/tower-telemetry/commit/13f2add4040e310b27a6f412da4e558f7ada1d3b))
* install, MSRV and full examples in the README ([#2](https://github.com/butlerx/tower-telemetry/issues/2)) ([b834e76](https://github.com/butlerx/tower-telemetry/commit/b834e764a38afd072040397515578f526819734d))
* messaging guide, and HEALTHCHECK_PATHS matching ([#4](https://github.com/butlerx/tower-telemetry/issues/4)) ([87c6e7e](https://github.com/butlerx/tower-telemetry/commit/87c6e7e040228c1d1c262100263324d464315791))

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
