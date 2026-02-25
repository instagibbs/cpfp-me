use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::consensus::Encodable;
use bitcoin::psbt::Input as PsbtInput;
use bitcoin::{Amount, OutPoint, Transaction, TxOut, Weight};

use crate::error::AppError;
use crate::validate::{p2a_script, ValidatedParent};

const MAX_TRUC_CHILD_VSIZE: u64 = 1_000;

pub struct BuiltChild {
    pub tx: Transaction,
    pub hex: String,
}

/// Builds a CPFP child transaction that spends the parent's P2A
/// output and one of our wallet UTXOs to pay the required fee.
pub fn build_child_tx(
    wallet: &mut Wallet,
    parent: &ValidatedParent,
    total_fee: Amount,
) -> Result<BuiltChild, AppError> {
    let parent_txid = parent.tx.compute_txid();
    let p2a_outpoint = OutPoint::new(parent_txid, parent.p2a_vout);
    let p2a_txout = TxOut {
        value: parent.tx.output[parent.p2a_vout as usize].value,
        script_pubkey: p2a_script(),
    };

    let psbt_input = PsbtInput {
        witness_utxo: Some(p2a_txout),
        non_witness_utxo: Some(parent.tx.clone()),
        ..PsbtInput::default()
    };

    // P2A spending requires an empty witness (1 WU for the count byte)
    let satisfaction_weight = Weight::from_witness_data_size(1);

    // Pick one wallet UTXO to fund the fee (keeps child small)
    let our_utxo = wallet
        .list_unspent()
        .find(|u| u.txout.value >= total_fee)
        .ok_or_else(|| AppError::Wallet("no UTXO large enough to cover the fee".into()))?;

    // Change goes back to our wallet
    let change_addr = wallet.reveal_next_address(KeychainKind::Internal).address;

    let mut builder = wallet.build_tx();
    builder
        .version(3)
        .add_foreign_utxo(p2a_outpoint, psbt_input, satisfaction_weight)
        .map_err(|e| AppError::Wallet(format!("failed to add P2A utxo: {e}")))?
        .add_utxo(our_utxo.outpoint)
        .map_err(|e| AppError::Wallet(format!("failed to add wallet utxo: {e}")))?
        .manually_selected_only()
        .fee_absolute(total_fee)
        .drain_to(change_addr.script_pubkey());

    let mut psbt = builder
        .finish()
        .map_err(|e| AppError::Wallet(format!("failed to build child tx: {e}")))?;

    #[expect(deprecated)]
    wallet
        .sign(&mut psbt, bdk_wallet::SignOptions::default())
        .map_err(|e| AppError::Wallet(format!("failed to sign child tx: {e}")))?;

    // Find the P2A input and clear its witness (anyone-can-spend)
    let p2a_input_idx = psbt
        .unsigned_tx
        .input
        .iter()
        .position(|inp| inp.previous_output == p2a_outpoint)
        .ok_or_else(|| AppError::Internal("P2A input not found in built transaction".into()))?;

    let mut final_tx = psbt
        .extract_tx()
        .map_err(|e| AppError::Wallet(format!("failed to extract signed tx: {e}")))?;
    final_tx.input[p2a_input_idx].witness = bitcoin::Witness::default();

    let vsize = final_tx.vsize() as u64;
    if vsize > MAX_TRUC_CHILD_VSIZE {
        return Err(AppError::Internal(format!(
            "child tx vsize {vsize} exceeds TRUC child limit \
             of {MAX_TRUC_CHILD_VSIZE}"
        )));
    }

    let mut buf = Vec::new();
    final_tx
        .consensus_encode(&mut buf)
        .map_err(|e| AppError::Internal(format!("failed to encode child tx: {e}")))?;

    Ok(BuiltChild {
        tx: final_tx,
        hex: hex::encode(buf),
    })
}
