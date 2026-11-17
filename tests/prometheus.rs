//! The default metric names come out of Prometheus, with recommended naming,
//! as the docs say: OpenTelemetry's dotted names with units and `_total`.

use std::convert::Infallible;

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use metrics_exporter_prometheus::PrometheusBuilder;
use tower::{ServiceBuilder, ServiceExt};
use tower_telemetry::HttpMetricsLayer;

#[tokio::test]
async fn prometheus_names_follow_recommended_naming() {
    let recorder = PrometheusBuilder::new()
        .with_recommended_naming(true)
        .build_recorder();
    let handle = recorder.handle();
    let _guard = metrics::set_default_local_recorder(&recorder);

    let service = ServiceBuilder::new()
        .layer(HttpMetricsLayer::default())
        .service_fn(|_request: Request<()>| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"hello"))))
        });
    let response = service
        .oneshot(Request::get("/").body(()).expect("static request"))
        .await
        .expect("infallible");
    response.into_body().collect().await.expect("infallible");

    let rendered = handle.render();
    for series in [
        r#"http_server_requests_total{http_request_method="GET",url_scheme="http",network_protocol_version="1.1",http_response_status_code="200"} 1"#,
        "http_server_request_duration_seconds_count",
        "http_server_time_to_first_byte_seconds_count",
        "http_server_response_body_size_bytes_count",
        r#"http_server_active_requests{http_request_method="GET",url_scheme="http"} 0"#,
    ] {
        assert!(
            rendered.contains(series),
            "missing {series} in:\n{rendered}"
        );
    }
}
