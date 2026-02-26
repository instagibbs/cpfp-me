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
