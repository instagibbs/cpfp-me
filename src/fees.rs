use std::collections::HashMap;

use bitcoin::Amount;
use serde::Deserialize;

use crate::error::AppError;

/// mempool.space format: /api/v1/fees/recommended
#[derive(Debug, Deserialize)]
struct MempoolFees {
    #[serde(rename = "fastestFee")]
    fastest_fee: u64,
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
    Ok(rate.ceil() as u64)
}

/// Calculates the total fee needed for CPFP, including markup.
///
/// The child must pay enough fee to cover both parent (0-fee) and
/// child at the target fee rate, plus admin markup.
pub fn calculate_total_fee(
    parent_vsize: u64,
    child_vsize: u64,
    fee_rate_sat_per_vb: u64,
    markup_percent: f64,
) -> Amount {
    let base_sats = fee_rate_sat_per_vb * (parent_vsize + child_vsize);
    // Integer math: markup in basis points to avoid float precision
    // e.g. 10.0% -> numerator=1100, denominator=1000
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let markup_bps = (markup_percent * 10.0) as u64;
    let total = (base_sats * (1000 + markup_bps)).div_ceil(1000);
    Amount::from_sat(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_markup() {
        let fee = calculate_total_fee(200, 100, 10, 0.0);
        assert_eq!(fee, Amount::from_sat(3000));
    }

    #[test]
    fn ten_percent_markup() {
        let fee = calculate_total_fee(200, 100, 10, 10.0);
        // 3000 * 1100 / 1000 = 3300
        assert_eq!(fee, Amount::from_sat(3300));
    }

    #[test]
    fn rounds_up() {
        // (2107 * 1030 + 999) / 1000 = 2171209 / 1000 = 2171
        let fee = calculate_total_fee(201, 100, 7, 3.0);
        assert_eq!(fee, Amount::from_sat(2171));
    }

    #[test]
    fn large_parent() {
        let fee = calculate_total_fee(5000, 150, 50, 5.0);
        // 5150 * 50 = 257500, * 1.05 = 270375
        assert_eq!(fee, Amount::from_sat(270_375));
    }

    #[test]
    fn minimum_sizes() {
        let fee = calculate_total_fee(1, 1, 1, 0.0);
        assert_eq!(fee, Amount::from_sat(2));
    }
}
