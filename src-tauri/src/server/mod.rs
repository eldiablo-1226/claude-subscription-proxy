pub mod auth;
pub mod anthropic;
pub mod claude;
pub mod openai;
pub mod state;
pub mod translate;

use std::{collections::VecDeque, net::SocketAddr, sync::Arc};

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use tokio::{net::TcpListener, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};

use crate::{config::Config, keys::KeyStore};

pub use state::RequestLogEntry;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerHandle {
    pub bind: String,
    pub port: u16,
    #[serde(skip)]
    pub cancel: CancellationToken,
}

pub fn router(state: state::HttpState) -> Router {
    Router::new()
        .route("/v1/models", get(openai::models))
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/messages", post(anthropic::messages))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}

pub async fn start(
    app: tauri::AppHandle,
    config: Config,
    keys: Arc<Mutex<KeyStore>>,
    logs: Arc<Mutex<VecDeque<RequestLogEntry>>>,
) -> Result<ServerHandle, String> {
    let address = format!("{}:{}", config.bind_address, config.port);
    let listener = TcpListener::bind(&address)
        .await
        .map_err(|err| format!("failed to bind {address}: {err}"))?;
    let local_addr: SocketAddr = listener.local_addr().map_err(|err| err.to_string())?;
    let cancel = CancellationToken::new();
    let http_state = state::HttpState {
        config: Arc::new(Mutex::new(config.clone())),
        keys,
        semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrency)),
        logs,
        app: Some(app),
    };
    let service = router(http_state);
    let shutdown = cancel.clone();

    tauri::async_runtime::spawn(async move {
        if let Err(error) = axum::serve(listener, service)
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
            })
            .await
        {
            tracing::error!(%error, "proxy server stopped with error");
        }
    });

    Ok(ServerHandle {
        bind: config.bind_address,
        port: local_addr.port(),
        cancel,
    })
}
