use crate::error::AppError;

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

    if !status.is_success() {
        return Err(AppError::Broadcast(format!(
            "mempool.space returned {status}: {text}"
        )));
    }

    Ok(text)
}
