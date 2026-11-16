//! A plain hyper server with request spans and metrics.
//!
//! `cargo run --example hyper`, then `curl localhost:3000/`.

use std::convert::Infallible;

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tower::ServiceBuilder;
use tower_telemetry::{HttpMetricsLayer, OtelTraceLayer};

async fn hello(_request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from_static(b"hello"))))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install a `tracing` subscriber with `tracing-opentelemetry` and a
    // `metrics` exporter here; see `examples/axum.rs`.
    let service = ServiceBuilder::new()
        .layer(OtelTraceLayer::new())
        .layer(HttpMetricsLayer::new())
        .service_fn(hello);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let service = TowerToHyperService::new(service.clone());
        tokio::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                eprintln!("connection error: {err}");
            }
        });
    }
}
