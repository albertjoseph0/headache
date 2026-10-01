use std::sync::Arc;

use headache::{api, game::Game, jev::GatewayJudge, store::Store};
use tracing_subscriber::EnvFilter;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "headache=info,tower_http=info".into()),
        )
        .init();

    let port: u16 = env_or("PORT", "8080").parse()?;
    let database = env_or("DATABASE_PATH", "headache.db");
    let static_dir = env_or("STATIC_DIR", "web");
    let api_key = std::env::var("AI_GATEWAY_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    if api_key.is_none() {
        tracing::warn!(
            "AI_GATEWAY_API_KEY is not set: Jev calls will fail and answers will be counted unjudged"
        );
    }

    let judge = GatewayJudge::new(
        env_or("AI_GATEWAY_URL", "https://ai-gateway.vercel.sh/v1/evaluate"),
        api_key,
        env_or("JEV_MODEL", "typesafe-ai/jev"),
    )?;
    let store = Store::open(&database)?;
    let game = Arc::new(Game::new(Arc::new(store), Arc::new(judge)));

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, database, static_dir, "diving");
    axum::serve(listener, api::router(game, static_dir))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Ctrl-C locally, SIGTERM from a container host.
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}
