use std::sync::Mutex;

use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::Network;
use bdk_wallet::keys::bip39::Mnemonic;
use bdk_wallet::template::Bip86;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::Encodable;
use bitcoin::transaction::Version;
use bitcoin::{Amount, Sequence, Transaction, TxIn, TxOut};

use crate::error::AppError;
use crate::validate::p2a_script;

pub struct TestWallet {
    wallet: Mutex<Wallet>,
}

impl TestWallet {
    pub fn new(mnemonic_str: &str, network: Network) -> Result<Self, AppError> {
        let mnemonic = Mnemonic::parse(mnemonic_str)
            .map_err(|e| AppError::Internal(format!("invalid test mnemonic: {e}")))?;
        let xpriv = Xpriv::new_master(network, &mnemonic.to_seed(""))
            .map_err(|e| AppError::Internal(format!("failed to derive test master key: {e}")))?;

        let external = Bip86(xpriv, KeychainKind::External);
        let internal = Bip86(xpriv, KeychainKind::Internal);

        let wallet = Wallet::create(external, internal)
            .network(network)
            .create_wallet_no_persist()
            .map_err(|e| AppError::Internal(format!("failed to create test wallet: {e}")))?;

        Ok(Self {
            wallet: Mutex::new(wallet),
        })
    }

    /// Returns info about the test wallet: address and current UTXO.
    pub fn info(&self) -> Result<TestWalletInfo, AppError> {
        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("test wallet lock poisoned: {e}")))?;
        let addr = wallet
            .reveal_next_address(KeychainKind::External)
            .address
            .to_string();
        Ok(TestWalletInfo { address: addr })
    }

    /// Syncs the test wallet using the same Esplora client.
    pub async fn sync(
        &self,
        esplora: &bdk_esplora::esplora_client::AsyncClient,
    ) -> Result<(), AppError> {
        use bdk_esplora::EsploraAsyncExt;

        let request = {
            let wallet = self
                .wallet
                .lock()
                .map_err(|e| AppError::Internal(format!("test wallet lock poisoned: {e}")))?;
            wallet.start_full_scan().build()
        };

        let update = esplora
            .full_scan(request, 20, 5)
            .await
            .map_err(|e| AppError::Internal(format!("test wallet sync failed: {e}")))?;

        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("test wallet lock poisoned: {e}")))?;
        wallet
            .apply_update(update)
            .map_err(|e| AppError::Internal(format!("failed to apply test wallet update: {e}")))?;

        Ok(())
    }

    /// Builds a TRUC v3 parent transaction from the test wallet's UTXO.
    ///
    /// Output 0: value back to the test wallet's own address (keyed, safe)
    /// Output 1: 0-value P2A anchor (for bumping)
    /// Fee: 0 sats
    ///
    /// After the parent + child package is mined, the test wallet's
    /// UTXO is recycled at the same value.
    pub fn build_test_parent(&self) -> Result<TestParent, AppError> {
        let mut wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("test wallet lock poisoned: {e}")))?;

        let utxo = wallet
            .list_unspent()
            .next()
            .ok_or_else(|| AppError::Internal("test wallet has no UTXOs — fund it first".into()))?;

        let return_addr = wallet.reveal_next_address(KeychainKind::External).address;

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

        // Sign it
        let mut psbt = bitcoin::Psbt::from_unsigned_tx(parent.clone())
            .map_err(|e| AppError::Internal(format!("failed to create PSBT: {e}")))?;

        // Set witness UTXO for the signer
        psbt.inputs[0].witness_utxo = Some(utxo.txout.clone());

        #[expect(deprecated)]
        wallet
            .sign(&mut psbt, bdk_wallet::SignOptions::default())
            .map_err(|e| AppError::Internal(format!("failed to sign test parent: {e}")))?;

        let signed_tx = psbt
            .extract_tx()
            .map_err(|e| AppError::Internal(format!("failed to extract test parent: {e}")))?;

        let mut buf = Vec::new();
        signed_tx
            .consensus_encode(&mut buf)
            .map_err(|e| AppError::Internal(format!("failed to encode test parent: {e}")))?;

        let txid = signed_tx.compute_txid().to_string();

        Ok(TestParent {
            hex: hex::encode(buf),
            txid,
            value_sats: utxo.txout.value.to_sat(),
        })
    }

    pub fn balance(&self) -> Result<u64, AppError> {
        let wallet = self
            .wallet
            .lock()
            .map_err(|e| AppError::Internal(format!("test wallet lock poisoned: {e}")))?;
        Ok(wallet.balance().total().to_sat())
    }
}

pub struct TestWalletInfo {
    pub address: String,
}

pub struct TestParent {
    pub hex: String,
    pub txid: String,
    pub value_sats: u64,
}
