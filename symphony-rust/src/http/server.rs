use std::future::{pending, Future};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio::sync::mpsc;

use crate::error::SymphonyError;
use crate::orchestrator::OrchestratorMsg;

use super::{create_router, StateProvider};

pub struct HttpServer {
    port: u16,
}

impl HttpServer {
    pub fn new(port: u16) -> Self {
        Self { port }
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
        let app: Router = create_router(state_provider, msg_tx);
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
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
