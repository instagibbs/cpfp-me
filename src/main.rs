mod broadcast;
mod child;
mod config;
mod error;
mod fees;
mod payment;
mod routes;
mod state;
mod validate;
mod wallet;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::payment::PhoenixdClient;
use crate::state::AppState;
use crate::wallet::AppWallet;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cpfp_me=info".into()),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());

    let config = config::Config::load(&config_path)?;
    let listen_addr = config.listen_addr;

    tracing::info!(
        network = ?config.network,
        addr = %listen_addr,
        markup = config.markup_percent,
        "starting cpfp.me"
    );

    let app_wallet = AppWallet::new(&config)?;

    tracing::info!("syncing wallet...");
    app_wallet.sync().await?;
    let balance = app_wallet.balance()?;
    let utxo_count = app_wallet.utxo_count()?;
    tracing::info!(balance_sats = balance, utxos = utxo_count, "wallet synced");

    let payment = PhoenixdClient::new(
        config.phoenixd_url.clone(),
        config.phoenixd_password.clone(),
    );

    let state = AppState {
        config: Arc::new(config),
        http_client: reqwest::Client::new(),
        wallet: Arc::new(app_wallet),
        payment: Arc::new(payment),
        orders: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    tracing::info!("listening on {listen_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
