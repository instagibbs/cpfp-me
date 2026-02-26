use std::sync::Mutex;

use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::Network;
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::Encodable;
use bitcoin::transaction::Version;
use bitcoin::{Amount, Sequence, Transaction, TxIn, TxOut};

use crate::error::AppError;
use crate::validate::p2a_script;

pub struct DemoWallet {
    wallet: Mutex<Wallet>,
}

impl DemoWallet {
    /// Creates a demo wallet from the same mnemonic as the main wallet
    /// but using BIP86 account 1 instead of account 0.
    pub fn new(mnemonic_str: &str, network: Network) -> Result<Self, AppError> {
        let mnemonic = Mnemonic::parse(mnemonic_str)
            .map_err(|e| AppError::Internal(format!("invalid mnemonic: {e}")))?;
        let xpriv = Xpriv::new_master(network, &mnemonic.to_seed(""))
            .map_err(|e| AppError::Internal(format!("failed to derive master key: {e}")))?;

        // Account 1 descriptors (main wallet uses account 0 via Bip86 template)
        let coin = match network {
            Network::Bitcoin => 0,
            _ => 1,
        };
        let external = format!("tr({xpriv}/86'/{coin}'/1'/0/*)");
        let internal = format!("tr({xpriv}/86'/{coin}'/1'/1/*)");

        let mut wallet = Wallet::create(external, internal)
            .network(network)
            .create_wallet_no_persist()
            .map_err(|e| AppError::Internal(format!("failed to create demo wallet: {e}")))?;

        // Reveal the first address so sync_with_revealed_spks has something to scan
        wallet.reveal_next_address(KeychainKind::External);

        Ok(Self {
            wallet: Mutex::new(wallet),
        })
    }

    /// Syncs the demo wallet via Esplora.
    pub async fn sync(
        &self,
        esplora: &bdk_esplora::esplora_client::AsyncClient,
    ) -> Result<(), AppError> {
        use bdk_esplora::EsploraAsyncExt;

        let request = {
            let wallet = self
                .wallet
                .lock()
                .map_err(|e| AppError::Internal(format!("demo wallet lock poisoned: {e}")))?;
            wallet.start_sync_with_revealed_spks().build()
        };

        let update = esplora
            .sync(request, 5)
            .await
            .map_err(|e| AppError::Internal(format!("demo wallet sync failed: {e}")))?;

        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("demo wallet lock poisoned: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| AppError::Internal(format!("failed to apply demo wallet update: {e}")))?;

        Ok(())
    }

    pub fn deposit_address(&self) -> Result<String, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("demo wallet lock poisoned: {e}")))?;
        // Peek at index 0 — always the same address for the demo wallet
        let addr = wallet.peek_address(KeychainKind::External, 0).address;
        Ok(addr.to_string())
    }

    pub fn balance(&self) -> Result<u64, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("demo wallet lock poisoned: {e}")))?;
        Ok(wallet.balance().total().to_sat())
    }

    /// Builds a TRUC v3 parent transaction from the demo wallet's UTXO.
    ///
    /// - Output 0: full value back to the demo wallet's own address
    /// - Output 1: 0-value P2A anchor (for bumping)
    /// - Fee: 0 sats
    ///
    /// Every call with the same unspent UTXO produces the same tx,
    /// so multiple users just race to bump the same parent.
    pub fn build_parent(&self) -> Result<DemoParent, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("demo wallet lock poisoned: {e}")))?;

        // Only spend confirmed UTXOs — spending unconfirmed would make
        // the parent a grandchild of an unconfirmed tx, violating TRUC 1p1c.
        let utxo = wallet
            .list_unspent()
            .find(|u| {
                matches!(
                    u.chain_position,
                    bdk_wallet::chain::ChainPosition::Confirmed { .. }
                )
            })
            .ok_or_else(|| {
                AppError::Internal(
                    "demo wallet has no confirmed UTXOs — previous bump pending confirmation"
                        .into(),
                )
            })?;

        // Always return to index 0 so the tx is deterministic
        let return_addr = wallet.peek_address(KeychainKind::External, 0).address;

        let parent = Transaction {
            version: Version(3),
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: utxo.outpoint,
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![
                TxOut {
                    value: utxo.txout.value,
                    script_pubkey: return_addr.script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: p2a_script(),
                },
            ],
        };

        let mut psbt = bitcoin::Psbt::from_unsigned_tx(parent)
            .map_err(|e| AppError::Internal(format!("failed to create PSBT: {e}")))?;

        psbt.inputs[0].witness_utxo = Some(utxo.txout.clone());

        #[expect(deprecated)]
        wallet
            .sign(&mut psbt, bdk_wallet::SignOptions::default())
            .map_err(|e| AppError::Internal(format!("failed to sign demo parent: {e}")))?;

        let signed_tx = psbt
            .extract_tx()
            .map_err(|e| AppError::Internal(format!("failed to extract demo parent: {e}")))?;

        let mut buf = Vec::new();
        signed_tx
            .consensus_encode(&mut buf)
            .map_err(|e| AppError::Internal(format!("failed to encode demo parent: {e}")))?;

        Ok(DemoParent {
            hex: hex::encode(buf),
            txid: signed_tx.compute_txid().to_string(),
            value_sats: utxo.txout.value.to_sat(),
        })
    }
}

pub struct DemoParent {
    pub hex: String,
    pub txid: String,
    pub value_sats: u64,
}
