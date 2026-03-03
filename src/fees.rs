use std::collections::HashMap;

use bitcoin::{Amount, Transaction};
use serde::Deserialize;

use crate::error::AppError;

/// mempool.space format: /api/v1/fees/recommended
#[derive(Debug, Deserialize)]
struct MempoolFees {
    #[serde(rename = "fastestFee")]
    fastest_fee: u64,
}

/// Computes the fee paid by a transaction by fetching each input's
/// previous output value from Esplora (`GET /api/tx/{txid}`).
///
/// Esplora returns output values in satoshis as integers.
pub async fn fetch_parent_fee(
    client: &reqwest::Client,
    mempool_url: &str,
    tx: &Transaction,
) -> Result<Amount, AppError> {
    let mut input_sum = Amount::ZERO;

    for input in &tx.input {
        let txid = input.previous_output.txid;
        let vout = input.previous_output.vout as usize;

        let url = format!("{mempool_url}/api/tx/{txid}");
        let resp = client.get(&url).send().await.map_err(|e| {
            AppError::Internal(format!("failed to fetch input tx {txid}: {e}"))
        })?;

        if !resp.status().is_success() {
            return Err(AppError::InvalidTx {
                reason: format!(
                    "could not look up input {txid}:{vout} (HTTP {})",
                    resp.status()
                ),
            });
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            AppError::Internal(format!("invalid response for tx {txid}: {e}"))
        })?;

        let value_sat = json["vout"][vout]["value"].as_u64().ok_or_else(|| {
            AppError::InvalidTx {
                reason: format!("missing output value for {txid}:{vout}"),
            }
        })?;

        input_sum = input_sum
            .checked_add(Amount::from_sat(value_sat))
            .ok_or_else(|| AppError::InvalidTx {
                reason: "input value overflow".into(),
            })?;
    }

    let output_sum: Amount = tx.output.iter().map(|o| o.value).sum();

    input_sum.checked_sub(output_sum).ok_or_else(|| AppError::InvalidTx {
        reason: "outputs exceed inputs".into(),
    })
}

/// Fetches the current fastest fee rate. Tries mempool.space format
/// first, then falls back to Esplora /api/fee-estimates format.
/// Returns fee rate in sat/vB.
pub async fn fetch_fee_rate(client: &reqwest::Client, mempool_url: &str) -> Result<u64, AppError> {
    // Try mempool.space format
    let url = format!("{mempool_url}/api/v1/fees/recommended");
    if let Ok(resp) = client.get(&url).send().await {
        if resp.status().is_success() {
            if let Ok(fees) = resp.json::<MempoolFees>().await {
                if fees.fastest_fee > 0 {
                    return Ok(fees.fastest_fee);
                }
            }
        }
    }

    // Fall back to Esplora format: /api/fee-estimates → {"1": 3.5, "2": 2.1, ...}
    let url = format!("{mempool_url}/api/fee-estimates");
    let estimates: HashMap<String, f64> = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::FeeEstimation(format!("request failed: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::FeeEstimation(format!("bad response: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::FeeEstimation(format!("invalid json: {e}")))?;

    // Key "1" = next block target
    let rate = estimates
        .get("1")
        .or_else(|| estimates.get("2"))
        .copied()
        .ok_or_else(|| AppError::FeeEstimation("no fee estimates available".into()))?;

    if rate <= 0.0 {
        return Err(AppError::FeeEstimation("fee rate is 0".into()));
    }

    // Esplora returns float sat/vB, round up
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(rate.ceil() as u64)
}

pub struct FeeBreakdown {
    /// Fee the child tx pays to the miner.
    pub mining_fee: Amount,
    /// Total amount to charge the user (mining fee + our profit).
    pub invoice_amount: Amount,
}

/// Calculates the mining fee and invoice amount for a CPFP bump.
///
/// The mining fee covers both parent + child at the target fee rate.
/// The invoice amount adds the admin markup on top — the difference
/// stays in our wallet as profit.
pub fn calculate_fees(
    parent_vsize: u64,
    child_vsize: u64,
    fee_rate_sat_per_vb: u64,
    markup_percent: f64,
) -> FeeBreakdown {
    let mining_fee = fee_rate_sat_per_vb * (parent_vsize + child_vsize);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let markup_bps = (markup_percent * 10.0) as u64;
    let invoice_amount = (mining_fee * (1000 + markup_bps)).div_ceil(1000);
    FeeBreakdown {
        mining_fee: Amount::from_sat(mining_fee),
        invoice_amount: Amount::from_sat(invoice_amount),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_markup() {
        let f = calculate_fees(200, 100, 10, 0.0);
        assert_eq!(f.mining_fee, Amount::from_sat(3000));
        assert_eq!(f.invoice_amount, Amount::from_sat(3000));
    }

    #[test]
    fn ten_percent_markup() {
        let f = calculate_fees(200, 100, 10, 10.0);
        assert_eq!(f.mining_fee, Amount::from_sat(3000));
        assert_eq!(f.invoice_amount, Amount::from_sat(3300));
    }

    #[test]
    fn rounds_up() {
        let f = calculate_fees(201, 100, 7, 3.0);
        assert_eq!(f.mining_fee, Amount::from_sat(2107));
        assert_eq!(f.invoice_amount, Amount::from_sat(2171));
    }

    #[test]
    fn large_parent() {
        let f = calculate_fees(5000, 150, 50, 5.0);
        assert_eq!(f.mining_fee, Amount::from_sat(257_500));
        assert_eq!(f.invoice_amount, Amount::from_sat(270_375));
    }

    #[test]
    fn minimum_sizes() {
        let f = calculate_fees(1, 1, 1, 0.0);
        assert_eq!(f.mining_fee, Amount::from_sat(2));
        assert_eq!(f.invoice_amount, Amount::from_sat(2));
    }
}
