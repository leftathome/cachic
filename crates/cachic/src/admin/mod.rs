//! The admin surface.
//!
//! Metrics and health probes on a port separate from the data plane (FR-50, FR-53). The
//! separation is a security property, not tidiness: purge and drain arrive here in TASK-19, and
//! they must not be reachable by every client on the LAN. Configuration rejects an admin port
//! that collides with the HTTP or HTTPS port for the same reason.

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};

use crate::telemetry::metrics::Metrics;

/// Whether the process is ready to serve.
///
/// Separate from liveness: a process that is up but whose store has not finished opening must
/// fail readiness so a rolling replacement does not send it traffic (FR-53).
#[derive(Debug, Default)]
pub struct Readiness {
    store_open: AtomicBool,
    listeners_bound: AtomicBool,
    draining: AtomicBool,
}

impl Readiness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_store_open(&self, value: bool) {
        self.store_open.store(value, Ordering::Relaxed);
    }

    pub fn set_listeners_bound(&self, value: bool) {
        self.listeners_bound.store(value, Ordering::Relaxed);
    }

    /// Begin draining. Readiness fails immediately so traffic moves away before shutdown starts.
    pub fn begin_drain(&self) {
        self.draining.store(true, Ordering::Relaxed);
    }

    pub fn is_ready(&self) -> bool {
        !self.draining.load(Ordering::Relaxed)
            && self.store_open.load(Ordering::Relaxed)
            && self.listeners_bound.load(Ordering::Relaxed)
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub struct AdminState {
    pub metrics: Arc<Metrics>,
    pub readiness: Arc<Readiness>,
}

pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

async fn metrics(State(state): State<AdminState>) -> impl IntoResponse {
    match state.metrics.render() {
        Ok(body) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4")],
            body,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to render metrics: {e}\n"),
        )
            .into_response(),
    }
}

/// Liveness: the process is running and can serve a request. Deliberately does not consult the
/// store - a cache with a broken disk should be restarted by an operator who has read the
/// metrics, not killed in a loop by a probe.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn readyz(State(state): State<AdminState>) -> impl IntoResponse {
    if state.readiness.is_ready() {
        (StatusCode::OK, "ready\n")
    } else if state.readiness.is_draining() {
        (StatusCode::SERVICE_UNAVAILABLE, "draining\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting\n")
    }
}

/// A running admin server. Dropping it stops the listener.
pub struct AdminServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl AdminServer {
    pub async fn bind(listen: SocketAddr, state: AdminState) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(listen).await?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let task_shutdown = shutdown.clone();
        let app = router(state);

        tokio::spawn(async move {
            let signal = async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if task_shutdown.load(Ordering::Relaxed) {
                        return;
                    }
                }
            };
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(signal)
                .await;
        });

        Ok(Self { addr, shutdown })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for AdminServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn server() -> (AdminServer, Arc<Readiness>, Arc<Metrics>) {
        let (metrics, _) = Metrics::new().unwrap();
        let metrics = Arc::new(metrics);
        let readiness = Arc::new(Readiness::new());
        let server = AdminServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            AdminState {
                metrics: metrics.clone(),
                readiness: readiness.clone(),
            },
        )
        .await
        .unwrap();
        (server, readiness, metrics)
    }

    async fn get(url: String) -> (u16, String) {
        let r = reqwest::get(url).await.unwrap();
        let status = r.status().as_u16();
        (status, r.text().await.unwrap())
    }

    #[tokio::test]
    async fn healthz_is_up_before_the_store_is() {
        // Liveness must not depend on the store, or a probe restarts the process in a loop while
        // an operator is trying to read why the disk failed.
        let (s, _, _) = server().await;
        let (status, body) = get(format!("{}/healthz", s.base_url())).await;
        assert_eq!(status, 200);
        assert_eq!(body.trim(), "ok");
    }

    #[tokio::test]
    async fn readyz_fails_until_the_store_and_listeners_are_up() {
        let (s, readiness, _) = server().await;
        let (status, body) = get(format!("{}/readyz", s.base_url())).await;
        assert_eq!(status, 503, "ready before the store opened");
        assert_eq!(body.trim(), "starting");

        readiness.set_store_open(true);
        assert_eq!(get(format!("{}/readyz", s.base_url())).await.0, 503);

        readiness.set_listeners_bound(true);
        assert_eq!(get(format!("{}/readyz", s.base_url())).await.0, 200);
    }

    #[tokio::test]
    async fn readyz_fails_immediately_on_drain() {
        // So a rolling replacement moves traffic away before shutdown begins.
        let (s, readiness, _) = server().await;
        readiness.set_store_open(true);
        readiness.set_listeners_bound(true);
        assert_eq!(get(format!("{}/readyz", s.base_url())).await.0, 200);

        readiness.begin_drain();
        let (status, body) = get(format!("{}/readyz", s.base_url())).await;
        assert_eq!(status, 503);
        assert_eq!(body.trim(), "draining");
    }

    #[tokio::test]
    async fn metrics_are_served_in_prometheus_format() {
        let (s, _, metrics) = server().await;
        metrics.requests.with_label_values(&["steam", "HIT"]).inc();
        let (status, body) = get(format!("{}/metrics", s.base_url())).await;
        assert_eq!(status, 200);
        assert!(body.contains("cachic_requests_total"), "{body}");
    }

    #[tokio::test]
    async fn the_admin_port_is_separate_from_any_data_plane_port() {
        // Configuration enforces this; the test documents why it matters. Purge and drain arrive
        // here in TASK-19 and must not be reachable by LAN clients.
        let (s, _, _) = server().await;
        let admin = s.addr().port();
        let config = crate::config::Config::try_parse_from_for_test(&[
            "cachic",
            "--admin-port",
            &admin.to_string(),
            "--http-port",
            &admin.to_string(),
        ]);
        assert!(config.is_err() || config.unwrap().validate().is_err());
    }
}
