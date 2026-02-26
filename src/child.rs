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

/// Builds a minimal 0-fee trial child that only spends the P2A output.
/// Used to probe whether the parent is valid before taking payment.
pub fn build_trial_child(parent: &ValidatedParent) -> Result<String, AppError> {
    let parent_txid = parent.tx.compute_txid();
    let p2a_outpoint = OutPoint::new(parent_txid, parent.p2a_vout);

    // OP_RETURN with 32 bytes of padding to meet min tx size
    let op_return_script =
        bitcoin::ScriptBuf::from_bytes([&[0x6a, 0x20], [0x00; 32].as_slice()].concat());

    let tx = Transaction {
        version: bitcoin::transaction::Version(3),
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: p2a_outpoint,
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: bitcoin::Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: op_return_script,
        }],
    };

    let mut buf = Vec::new();
    tx.consensus_encode(&mut buf)
        .map_err(|e| AppError::Internal(format!("failed to encode trial child: {e}")))?;

    Ok(hex::encode(buf))
}

/// Builds a CPFP child transaction that spends the parent's P2A
/// output and wallet UTXOs to pay the required fee.
pub fn build_child_tx(
    wallet: &mut Wallet,
    parent: &ValidatedParent,
    mining_fee: Amount,
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

    // P2A empty witness: 1 WU for the 0-items count byte
    let satisfaction_weight = Weight::from_wu(1);

    let change_addr = wallet.reveal_next_address(KeychainKind::Internal).address;

    let mut builder = wallet.build_tx();
    builder
        .version(3)
        .drain_wallet()
        .add_foreign_utxo(p2a_outpoint, psbt_input, satisfaction_weight)
        .map_err(|e| AppError::Wallet(format!("failed to add P2A utxo: {e}")))?
        .drain_to(change_addr.script_pubkey())
        .fee_absolute(mining_fee);

    let mut psbt = builder
        .finish()
        .map_err(|e| AppError::Wallet(format!("failed to build child tx: {e}")))?;

    #[expect(deprecated)]
    wallet
        .sign(&mut psbt, bdk_wallet::SignOptions::default())
        .map_err(|e| AppError::Wallet(format!("failed to sign child tx: {e}")))?;

    let mut final_tx = psbt
        .extract_tx()
        .map_err(|e| AppError::Wallet(format!("failed to extract signed tx: {e}")))?;

    // Clear P2A input witness (must be empty stack)
    let p2a_input_idx = final_tx
        .input
        .iter()
        .position(|inp| inp.previous_output == p2a_outpoint)
        .ok_or_else(|| AppError::Internal("P2A input not found in built transaction".into()))?;
    final_tx.input[p2a_input_idx].witness = bitcoin::Witness::default();

    let vsize = final_tx.vsize() as u64;
    if vsize > MAX_TRUC_CHILD_VSIZE {
        return Err(AppError::Internal(format!(
            "child tx vsize {vsize} exceeds TRUC child limit \
             of {MAX_TRUC_CHILD_VSIZE}"
        )));
    }

    // Apply unconfirmed tx so wallet tracks the spent UTXOs
    wallet.apply_unconfirmed_txs([(final_tx.clone(), 0)]);

    let mut buf = Vec::new();
    final_tx
        .consensus_encode(&mut buf)
        .map_err(|e| AppError::Internal(format!("failed to encode child tx: {e}")))?;

    Ok(BuiltChild {
        tx: final_tx,
        hex: hex::encode(buf),
    })
}
