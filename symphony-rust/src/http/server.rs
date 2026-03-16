use std::future::{pending, Future};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use tokio::sync::mpsc;

use crate::error::SymphonyError;
use crate::orchestrator::OrchestratorMsg;

use super::{create_router, StateProvider};

pub struct HttpServer {
    host: String,
    port: u16,
}

impl HttpServer {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }

    pub async fn start(
        self,
        state_provider: Arc<StateProvider>,
        msg_tx: mpsc::Sender<OrchestratorMsg>,
    ) -> Result<(), SymphonyError> {
        self.start_with_shutdown(state_provider, msg_tx, pending::<()>())
            .await
    }

    pub async fn start_with_shutdown<F>(
        self,
        state_provider: Arc<StateProvider>,
        msg_tx: mpsc::Sender<OrchestratorMsg>,
        shutdown: F,
    ) -> Result<(), SymphonyError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self { host, port } = self;
        let app: Router = create_router(state_provider, msg_tx);
        let ip_addr = host.parse::<IpAddr>().map_err(|error| {
            SymphonyError::Http(format!("invalid server host '{host}': {error}"))
        })?;
        let addr = SocketAddr::new(ip_addr, port);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|error| SymphonyError::Http(error.to_string()))?;
        tracing::info!("HTTP server listening on {}", addr);
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|error| SymphonyError::Http(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::HttpServer;
    use crate::error::SymphonyError;
    use crate::http::StateProvider;

    #[tokio::test]
    async fn invalid_host_returns_http_error() {
        let server = HttpServer::new("not-an-ip".into(), 0);
        let state_provider = Arc::new(StateProvider::new());
        let (msg_tx, _) = mpsc::channel(1);

        let error = server
            .start_with_shutdown(state_provider, msg_tx, async {})
            .await
            .unwrap_err();

        assert!(
            matches!(error, SymphonyError::Http(message) if message.contains("invalid server host 'not-an-ip'"))
        );
    }
}
