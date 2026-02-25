use std::sync::Mutex;

use bdk_esplora::esplora_client;
use bdk_esplora::EsploraAsyncExt;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::template::Bip86;
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};

use crate::config::Config;
use crate::error::AppError;

const STOP_GAP: usize = 20;
const PARALLEL_REQUESTS: usize = 5;

pub struct AppWallet {
    pub wallet: Mutex<PersistedWallet<Connection>>,
    pub db: Mutex<Connection>,
    esplora: esplora_client::AsyncClient,
}

impl AppWallet {
    pub fn new(config: &Config) -> Result<Self, AppError> {
        let network = config.network.to_bitcoin_network();
        let mnemonic = Mnemonic::parse(&config.mnemonic)
            .map_err(|e| AppError::Wallet(format!("invalid mnemonic: {e}")))?;

        let xpriv = Xpriv::new_master(network, &mnemonic.to_seed(""))
            .map_err(|e| AppError::Wallet(format!("failed to derive master key: {e}")))?;

        let external = Bip86(xpriv, KeychainKind::External);
        let internal = Bip86(xpriv, KeychainKind::Internal);

        let db_path = &config.wallet_db_path;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Wallet(format!("failed to create wallet dir: {e}")))?;
        }

        let mut conn = Connection::open(db_path)
            .map_err(|e| AppError::Wallet(format!("failed to open wallet db: {e}")))?;

        let has_existing_db = db_path.exists()
            && std::fs::metadata(db_path)
                .map(|m| m.len() > 0)
                .unwrap_or(false);

        let wallet = if has_existing_db {
            match Wallet::load().load_wallet(&mut conn) {
                Ok(Some(w)) => w,
                Ok(None) | Err(_) => Wallet::create(external, internal)
                    .network(network)
                    .create_wallet(&mut conn)
                    .map_err(|e| AppError::Wallet(format!("failed to create wallet: {e}")))?,
            }
        } else {
            Wallet::create(external, internal)
                .network(network)
                .create_wallet(&mut conn)
                .map_err(|e| AppError::Wallet(format!("failed to create wallet: {e}")))?
        };

        let esplora_url = format!("{}/api", config.mempool_api_url);
        let esplora = esplora_client::Builder::new(&esplora_url)
            .build_async()
            .map_err(|e| AppError::Wallet(format!("failed to build esplora client: {e}")))?;

        Ok(Self {
            wallet: Mutex::new(wallet),
            db: Mutex::new(conn),
            esplora,
        })
    }

    pub async fn sync(&self) -> Result<(), AppError> {
        let request = {
            let wallet = self
                .wallet
                .lock()
                .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
            wallet.start_full_scan().build()
        };

        let update = self
            .esplora
            .full_scan(request, STOP_GAP, PARALLEL_REQUESTS)
            .await
            .map_err(|e| AppError::Wallet(format!("sync failed: {e}")))?;

        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| AppError::Wallet(format!("failed to apply sync update: {e}")))?;

        let mut db = self
            .db
            .lock()
            .map_err(|e| AppError::Wallet(format!("db lock poisoned: {e}")))?;
        wallet
            .persist(&mut *db)
            .map_err(|e| AppError::Wallet(format!("failed to persist wallet: {e}")))?;

        Ok(())
    }

    pub fn utxo_count(&self) -> Result<usize, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        Ok(wallet.list_unspent().count())
    }

    pub fn balance(&self) -> Result<u64, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        Ok(wallet.balance().total().to_sat())
    }

    /// Returns true if the wallet has a UTXO large enough to cover
    /// the given fee amount.
    pub fn can_cover_fee(&self, fee: bitcoin::Amount) -> Result<bool, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        let has_sufficient = wallet.list_unspent().any(|u| u.txout.value >= fee);
        Ok(has_sufficient)
    }
}
