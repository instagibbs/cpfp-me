use crate::error::AppError;

/// Fee-related error substrings from Bitcoin Core. If a package fails
/// with ONLY these errors, the parent is valid but just needs fees.
const FEE_ERRORS: &[&str] = &[
    "min relay fee not met",
    "mempool min fee not met",
    "insufficient fee",
    "too-long-mempool-chain",
];

/// Broadcasts a parent + child transaction package via mempool.space.
///
/// Uses the `POST /api/txs/package` endpoint which wraps Bitcoin
/// Core's `submitpackage` RPC.
pub async fn submit_package(
    client: &reqwest::Client,
    mempool_url: &str,
    parent_hex: &str,
    child_hex: &str,
) -> Result<String, AppError> {
    let url = format!("{mempool_url}/api/txs/package");
    let body = serde_json::json!([parent_hex, child_hex]);

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Broadcast(format!("request failed: {e}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| AppError::Broadcast(format!("failed to read response: {e}")))?;

    tracing::info!(
        http_status = %status,
        response = %text,
        "submitpackage response"
    );

    if !status.is_success() {
        return Err(AppError::Broadcast(format!(
            "mempool.space returned {status}: {text}"
        )));
    }

    // submitpackage returns 200 even on failure — check package_msg
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        let msg = json["package_msg"].as_str().unwrap_or("");
        if msg != "success" {
            return Err(AppError::Broadcast(format!("submitpackage failed: {text}")));
        }
    }

    Ok(text)
}

/// Submits a trial package with a 0-fee child (P2A-only spend) to
/// check whether the parent is valid and bumpable. The package will
/// be rejected for fee reasons (which is expected). Any non-fee
/// rejection means the parent is invalid.
pub async fn validate_parent_broadcastable(
    client: &reqwest::Client,
    mempool_url: &str,
    parent_hex: &str,
    trial_child_hex: &str,
) -> Result<(), AppError> {
    let url = format!("{mempool_url}/api/txs/package");
    let body = serde_json::json!([parent_hex, trial_child_hex]);

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Broadcast(format!("request failed: {e}")))?;

    let text = response
        .text()
        .await
        .map_err(|e| AppError::Broadcast(format!("failed to read response: {e}")))?;

    tracing::debug!(response = %text, "trial package response");

    // If the package succeeded (unlikely with 0-fee, but possible at
    // feerate 0), the parent is definitely valid.
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| AppError::InvalidTx {
        reason: format!("unexpected response from trial broadcast: {e}"),
    })?;

    let msg = json["package_msg"].as_str().unwrap_or("");
    if msg == "success" {
        return Ok(());
    }

    // Package failed — check if ALL tx errors are fee-related.
    // If any error is non-fee, the parent is unbumpable.
    if let Some(results) = json["tx-results"].as_object() {
        for (_txid, result) in results {
            if let Some(error) = result["error"].as_str() {
                let is_fee_error = FEE_ERRORS.iter().any(|fe| error.contains(fe));
                if !is_fee_error {
                    return Err(AppError::InvalidTx {
                        reason: format!("parent transaction is not bumpable: {error}"),
                    });
                }
            }
        }
    }

    Ok(())
}
