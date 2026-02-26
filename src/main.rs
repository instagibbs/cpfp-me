use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cpfp_me::config;
use cpfp_me::demo::DemoWallet;
use cpfp_me::payment::PhoenixdClient;
use cpfp_me::routes;
use cpfp_me::state::AppState;
use cpfp_me::wallet::AppWallet;

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
    let network = config.network.to_bitcoin_network();

    tracing::info!(
        network = ?config.network,
        addr = %listen_addr,
        markup = config.markup_percent,
        "starting cpfp.me"
    );

    let app_wallet = AppWallet::new(&config)?;

    let balance = app_wallet.balance()?;
    let utxo_count = app_wallet.utxo_count()?;
    tracing::info!(
        balance_sats = balance,
        utxos = utxo_count,
        "wallet loaded from db"
    );

    let payment = PhoenixdClient::new(
        config.phoenixd_url.clone(),
        config.phoenixd_password.clone(),
    );

    let demo_wallet = DemoWallet::new(&config.mnemonic, network)?;
    let demo_addr = demo_wallet.deposit_address()?;
    tracing::info!(address = %demo_addr, "demo wallet ready (account 1)");

    let state = AppState {
        config: Arc::new(config),
        http_client: reqwest::Client::new(),
        wallet: Arc::new(app_wallet),
        payment: Arc::new(payment),
        orders: Arc::new(Mutex::new(HashMap::new())),
        demo_wallet: Arc::new(demo_wallet),
    };

    cpfp_me::cleanup::spawn_cleanup_task(state.clone());

    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    tracing::info!("listening on {listen_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
