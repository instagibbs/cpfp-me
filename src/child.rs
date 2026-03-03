use bdk_wallet::{KeychainKind, LocalOutput, Wallet};
use bitcoin::absolute::LockTime;
use bitcoin::consensus::Encodable;
use bitcoin::psbt::Input as PsbtInput;
use bitcoin::transaction::Version;
use bitcoin::{Amount, OutPoint, Sequence, Transaction, TxIn, TxOut, Weight};

use crate::error::AppError;
use crate::validate::{p2a_script, ValidatedParent};

const MAX_TRUC_CHILD_VSIZE: u64 = 1_000;

pub struct BuiltChild {
    pub tx: Transaction,
    pub hex: String,
}

/// Builds a minimal 0-fee trial child that only spends the P2A output.
/// Used to probe whether the parent is valid before taking payment.
/// This tx is not meant to be broadcast — it's just for testing
/// the package against mempool policy.
pub fn build_trial_child(parent: &ValidatedParent) -> Result<String, AppError> {
    let parent_txid = parent.tx.compute_txid();
    let p2a_outpoint = OutPoint::new(parent_txid, parent.p2a_vout);

    // OP_RETURN with 32 bytes of padding to meet min tx size
    let op_return_script =
        bitcoin::ScriptBuf::from_bytes([&[0x6a, 0x20], [0x00; 32].as_slice()].concat());

    let tx = Transaction {
        version: Version(3),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: p2a_outpoint,
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
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

/// Selects wallet UTXOs for the child transaction.
///
/// Always picks one UTXO large enough to cover the fee.
/// If above target count, adds one extra small UTXO to consolidate.
pub(crate) fn select_wallet_utxos(
    wallet: &Wallet,
    total_fee: Amount,
    utxo_target: u32,
) -> Result<Vec<OutPoint>, AppError> {
    // Only spend confirmed outputs — unconfirmed change from previous
    // child txs would violate TRUC's 1-unconfirmed-descendant rule.
    let mut utxos: Vec<LocalOutput> = wallet
        .list_unspent()
        .filter(|u| {
            matches!(
                u.chain_position,
                bdk_wallet::chain::ChainPosition::Confirmed { .. }
            )
        })
        .collect();
    #[expect(clippy::cast_possible_truncation)]
    let utxo_count = utxos.len() as u32;

    utxos.sort_by_key(|u| u.txout.value);

    let utxo_values: Vec<u64> =
        utxos.iter().map(|u| u.txout.value.to_sat()).collect();
    tracing::debug!(
        confirmed_utxo_count = utxo_count,
        confirmed_utxo_values_sats = ?utxo_values,
        required_fee_sats = total_fee.to_sat(),
        "selecting wallet UTXOs"
    );

    let Some(primary) =
        utxos.iter().find(|u| u.txout.value >= total_fee)
    else {
        tracing::warn!(
            confirmed_utxo_count = utxo_count,
            confirmed_utxo_values_sats = ?utxo_values,
            required_fee_sats = total_fee.to_sat(),
            "no UTXO large enough to cover fee"
        );
        return Err(AppError::Wallet(
            "no UTXO large enough to cover the fee".into(),
        ));
    };

    tracing::debug!(
        primary_outpoint = %primary.outpoint,
        primary_value_sats = primary.txout.value.to_sat(),
        "selected primary UTXO"
    );

    let mut selected = vec![primary.outpoint];

    // Above target: fold in one extra small UTXO to consolidate
    if utxo_count > utxo_target {
        if let Some(extra) =
            utxos.iter().find(|u| u.outpoint != primary.outpoint)
        {
            tracing::debug!(
                extra_outpoint = %extra.outpoint,
                extra_value_sats = extra.txout.value.to_sat(),
                "adding consolidation UTXO"
            );
            selected.push(extra.outpoint);
        }
    }

    Ok(selected)
}

/// P2TR dust threshold — minimum change output value that won't be
/// rejected as dust. Used by the preflight check to ensure the wallet
/// can produce a valid change output after paying the fee.
const P2TR_DUST_SATS: u64 = 330;

/// Checks whether the wallet can fund a child tx at the given fee
/// without building a PSBT or revealing addresses. Returns an error
/// if UTXO selection fails or if the selected inputs can't cover the
/// fee plus a dust-safe change output.
pub(crate) fn preflight_check_wallet(
    wallet: &Wallet,
    total_fee: Amount,
    utxo_target: u32,
) -> Result<(), AppError> {
    let selected = select_wallet_utxos(wallet, total_fee, utxo_target)?;

    let input_total: Amount = wallet
        .list_unspent()
        .filter(|u| selected.contains(&u.outpoint))
        .map(|u| u.txout.value)
        .sum();

    let dust = Amount::from_sat(P2TR_DUST_SATS);
    let needed = total_fee + dust;
    if input_total < needed {
        return Err(AppError::Wallet(format!(
            "selected UTXOs total {} sats but need {} sats \
             (fee {} + dust {})",
            input_total.to_sat(),
            needed.to_sat(),
            total_fee.to_sat(),
            dust.to_sat(),
        )));
    }

    Ok(())
}

/// Builds a CPFP child transaction that spends the parent's P2A
/// output and wallet UTXOs to pay the required fee.
///
/// UTXO management happens opportunistically during each bump:
/// - Above target: adds an extra wallet input to consolidate
/// - Below target: adds an extra change output to split
pub fn build_child_tx(
    wallet: &mut Wallet,
    parent: &ValidatedParent,
    total_fee: Amount,
    utxo_target: u32,
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

    // P2A has an empty witness stack (1 byte for the 0-items count)
    let satisfaction_weight = Weight::from_witness_data_size(1);

    // Gather all wallet state before build_tx() borrows wallet mutably
    #[expect(clippy::cast_possible_truncation)]
    let utxo_count = wallet.list_unspent().count() as u32;
    let selected = select_wallet_utxos(wallet, total_fee, utxo_target)?;

    let input_total: Amount = wallet
        .list_unspent()
        .filter(|u| selected.contains(&u.outpoint))
        .map(|u| u.txout.value)
        .sum();

    let change_addr = wallet.reveal_next_address(KeychainKind::Internal).address;

    // Pre-compute split output if needed (below target UTXO count)
    let split_output = if utxo_count < utxo_target {
        let split_addr = wallet.reveal_next_address(KeychainKind::Internal).address;
        let change_after_fee = input_total.checked_sub(total_fee).unwrap_or(Amount::ZERO);
        let split_amount = change_after_fee / 2;
        // Only split if resulting UTXOs stay above dust
        (split_amount > Amount::from_sat(1000)).then_some((split_addr, split_amount))
    } else {
        None
    };

    // Now build the transaction — wallet is mutably borrowed from here
    let mut builder = wallet.build_tx();
    builder
        .version(3)
        .add_foreign_utxo(p2a_outpoint, psbt_input, satisfaction_weight)
        .map_err(|e| AppError::Wallet(format!("failed to add P2A utxo: {e}")))?;

    for outpoint in &selected {
        builder
            .add_utxo(*outpoint)
            .map_err(|e| AppError::Wallet(format!("failed to add wallet utxo: {e}")))?;
    }

    builder
        .manually_selected_only()
        .fee_absolute(total_fee)
        .drain_to(change_addr.script_pubkey());

    if let Some((addr, amount)) = split_output {
        builder.add_recipient(addr.script_pubkey(), amount);
    }

    let mut psbt = builder
        .finish()
        .map_err(|e| AppError::Wallet(format!("failed to build child tx: {e}")))?;

    // try_finalize is needed so extract_tx includes the witness.
    // finalized will be false because the P2A input can't be
    // finalized by BDK (it's a foreign anyone-can-spend input),
    // but our wallet input IS signed and finalized.
    #[expect(deprecated)]
    let _finalized = wallet
        .sign(
            &mut psbt,
            bdk_wallet::SignOptions {
                try_finalize: true,
                ..bdk_wallet::SignOptions::default()
            },
        )
        .map_err(|e| AppError::Wallet(format!("failed to sign child tx: {e}")))?;

    let p2a_input_idx = psbt
        .unsigned_tx
        .input
        .iter()
        .position(|inp| inp.previous_output == p2a_outpoint)
        .ok_or_else(|| AppError::Internal("P2A input not found in built transaction".into()))?;

    let mut final_tx = psbt
        .extract_tx()
        .map_err(|e| AppError::Wallet(format!("failed to extract signed tx: {e}")))?;
    // P2A spending requires an empty witness stack
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
