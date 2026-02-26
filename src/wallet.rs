use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bdk_esplora::esplora_client;
use bdk_esplora::EsploraAsyncExt;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::template::Bip86;
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};
use bitcoin::{Amount, OutPoint};

use crate::config::Config;
use crate::error::AppError;

const PARALLEL_REQUESTS: usize = 5;
pub const RESERVATION_TTL: Duration = Duration::from_secs(60);

pub struct AppWallet {
    pub wallet: Mutex<PersistedWallet<Connection>>,
    pub db: Mutex<Connection>,
    esplora: esplora_client::AsyncClient,
    reservations: Mutex<HashMap<OutPoint, Instant>>,
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
            match Wallet::load()
                .descriptor(KeychainKind::External, Some(external.clone()))
                .descriptor(KeychainKind::Internal, Some(internal.clone()))
                .extract_keys()
                .load_wallet(&mut conn)
            {
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
            reservations: Mutex::new(HashMap::new()),
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
            .full_scan(request, 20, PARALLEL_REQUESTS)
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

    pub fn next_address(&self) -> Result<String, AppError> {
        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        let info = wallet.reveal_next_address(KeychainKind::External);
        Ok(info.address.to_string())
    }

    /// Tries to reserve a UTXO large enough to cover `fee`.
    ///
    /// Returns the reserved outpoint, or an error if no unreserved
    /// UTXO is available. Reservations expire after 60 seconds.
    pub fn reserve_utxo_for_fee(&self, fee: Amount) -> Result<OutPoint, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|e| AppError::Wallet(format!("reservations lock poisoned: {e}")))?;

        // Expire stale reservations
        let now = Instant::now();
        reservations.retain(|_, expires_at| *expires_at > now);

        // Find unreserved UTXO large enough
        let utxo = wallet
            .list_unspent()
            .find(|u| u.txout.value >= fee && !reservations.contains_key(&u.outpoint));

        match utxo {
            Some(u) => {
                let outpoint = u.outpoint;
                reservations.insert(outpoint, now + RESERVATION_TTL);
                tracing::debug!(
                    outpoint = %outpoint,
                    ttl_secs = RESERVATION_TTL.as_secs(),
                    "reserved UTXO"
                );
                Ok(outpoint)
            }
            None => Err(AppError::AtCapacity(
                "no wallet UTXOs available to fund this bump, try again later".into(),
            )),
        }
    }

    /// Releases a reservation (e.g. after payment timeout).
    pub fn release_reservation(&self, outpoint: &OutPoint) {
        if let Ok(mut reservations) = self.reservations.lock() {
            if reservations.remove(outpoint).is_some() {
                tracing::debug!(outpoint = %outpoint, "released UTXO reservation");
            }
        }
    }

    /// Consumes a reservation after successful payment.
    /// The UTXO will be spent in the child tx, so no need to hold
    /// the reservation anymore.
    pub fn consume_reservation(&self, outpoint: &OutPoint) {
        if let Ok(mut reservations) = self.reservations.lock() {
            reservations.remove(outpoint);
        }
    }

    /// Builds a self-spend transaction that consolidates unreserved
    /// wallet UTXOs into a single output.
    ///
    /// Returns the signed transaction hex, or None if there are fewer
    /// than 2 unreserved UTXOs.
    pub fn build_consolidation_tx(&self) -> Result<Option<String>, AppError> {
        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
        let reservations = self
            .reservations
            .lock()
            .map_err(|e| AppError::Wallet(format!("reservations lock poisoned: {e}")))?;

        let now = Instant::now();
        let unreserved: Vec<_> = wallet
            .list_unspent()
            .filter(|u| reservations.get(&u.outpoint).is_none_or(|exp| *exp <= now))
            .collect();

        if unreserved.len() < 2 {
            return Ok(None);
        }

        let utxo_count = unreserved.len();
        let addr = wallet.reveal_next_address(KeychainKind::Internal).address;

        let mut builder = wallet.build_tx();
        for utxo in &unreserved {
            builder
                .add_utxo(utxo.outpoint)
                .map_err(|e| AppError::Wallet(format!("failed to add utxo: {e}")))?;
        }
        builder
            .manually_selected_only()
            .drain_to(addr.script_pubkey());

        let mut psbt = builder
            .finish()
            .map_err(|e| AppError::Wallet(format!("failed to build consolidation tx: {e}")))?;

        #[expect(deprecated)]
        wallet
            .sign(&mut psbt, bdk_wallet::SignOptions::default())
            .map_err(|e| AppError::Wallet(format!("failed to sign consolidation tx: {e}")))?;

        let tx = psbt
            .extract_tx()
            .map_err(|e| AppError::Wallet(format!("failed to extract consolidation tx: {e}")))?;

        let mut buf = Vec::new();
        bitcoin::consensus::Encodable::consensus_encode(&tx, &mut buf)
            .map_err(|e| AppError::Wallet(format!("failed to encode consolidation tx: {e}")))?;

        let mut db = self
            .db
            .lock()
            .map_err(|e| AppError::Wallet(format!("db lock poisoned: {e}")))?;
        wallet
            .persist(&mut *db)
            .map_err(|e| AppError::Wallet(format!("failed to persist wallet: {e}")))?;

        tracing::info!(
            utxos_merged = utxo_count,
            txid = %tx.compute_txid(),
            "built consolidation tx"
        );

        Ok(Some(hex::encode(buf)))
    }
}
