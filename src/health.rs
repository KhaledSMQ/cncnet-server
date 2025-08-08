use crate::metrics::Metrics;
use crate::shutdown::ShutdownReceiver;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::info;

pub fn start_health_server(
    port: u16,
    metrics: Arc<Metrics>,
    mut shutdown: ShutdownReceiver,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = TcpListener::bind(addr).await.unwrap();

        info!("Health/Metrics server listening on port {}", port);

        loop {
            tokio::select! {
                Ok((stream, _)) = listener.accept() => {
                    let io = TokioIo::new(stream);
                    let metrics_clone = metrics.clone();

                    tokio::spawn(async move {
                        let service = service_fn(move |req| {
                            let metrics = metrics_clone.clone();
                            async move {
                                Ok::<_, Infallible>(handle_request(req, metrics).await)
                            }
                        });

                        if let Err(e) = http1::Builder::new()
                            .serve_connection(io, service)
                            .await
                        {
                            tracing::debug!("Health server connection error: {}", e);
                        }
                    });
                }
                _ = shutdown.recv() => {
                    info!("Health server shutting down");
                    break;
                }
            }
        }
    })
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    metrics: Arc<Metrics>,
) -> Response<Full<Bytes>> {
    let response = match req.uri().path() {
        "/health" => Response::new(Full::new(Bytes::from("OK"))),
        "/ready" => Response::new(Full::new(Bytes::from("READY"))),
        "/metrics" => Response::new(Full::new(Bytes::from(metrics.export_prometheus()))),
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap(),
    };

    response
}