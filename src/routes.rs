use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use bitcoin::Amount;
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;

use crate::broadcast;
use crate::child;
use crate::error::AppError;
use crate::fees;
use crate::payment::PaymentStatus;
use crate::state::{AppState, Order, OrderStatus};
use crate::validate;
use crate::wallet::RESERVATION_TTL;

pub fn router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/api/submit", post(handle_submit))
        .route("/api/status/{order_id}", get(handle_status))
        .route("/api/admin/info", get(handle_admin_info));

    router = router
        .route("/api/demo-parent", get(handle_demo_parent))
        .route("/api/recent-bumps", get(handle_recent_bumps));

    if state.config.testing {
        tracing::warn!("testing mode enabled: /api/admin/fakepay is available");
        router = router.route("/api/admin/fakepay/{order_id}", post(handle_fakepay));
    }

    router
        .fallback_service(ServeDir::new("static"))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct SubmitRequest {
    raw_tx: String,
}

#[derive(Debug, Serialize)]
struct SubmitResponse {
    order_id: String,
    bolt11: String,
    amount_sat: u64,
    fee_rate: u64,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    bolt11: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount_sat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fee_rate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    txid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mempool_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn handle_submit(
    State(state): State<AppState>,
    Json(req): Json<SubmitRequest>,
) -> Result<Json<SubmitResponse>, AppError> {
    let parent = validate::validate_parent_tx(&req.raw_tx)?;

    // Verify the parent pays zero fee — required for ephemeral dust (P2A)
    // relay policy. Fetch each input's UTXO value from Esplora and compute
    // fee = sum(inputs) - sum(outputs).
    let fee = fees::fetch_parent_fee(
        &state.http_client,
        &state.config.mempool_api_url,
        &parent.tx,
    )
    .await?;
    if fee != Amount::ZERO {
        return Err(AppError::InvalidTx {
            reason: format!(
                "transaction fee is {} sat — must be zero; \
                 ephemeral dust (P2A) requires a zero-fee parent",
                fee.to_sat()
            ),
        });
    }

    // Probe whether the parent is actually bumpable by submitting a
    // trial package with a 0-fee child. Rejects parents with spent
    // inputs or other non-fee policy violations before taking payment.
    let trial_child = child::build_trial_child(&parent)?;
    broadcast::validate_parent_broadcastable(
        &state.http_client,
        &state.config.mempool_api_url,
        &parent.raw_hex,
        &trial_child,
    )
    .await?;

    let fee_rate = fees::fetch_fee_rate(&state.http_client, &state.config.mempool_api_url).await?;

    // Estimate child vsize: ~110 vB is typical for a 1-in-1-out
    // taproot child + the P2A input
    let estimated_child_vsize = 110;
    let fee_breakdown = fees::calculate_fees(
        parent.vsize,
        estimated_child_vsize,
        fee_rate,
        state.config.markup_percent,
    );

    // Reserve a UTXO before taking payment.
    let reserved_utxo = match state.wallet.reserve_utxo_for_fee(fee_breakdown.mining_fee) {
        Ok(outpoint) => outpoint,
        Err(e) => {
            // No single UTXO is large enough — trigger background
            // consolidation so the next attempt has a bigger UTXO.
            trigger_consolidation(state.clone());
            return Err(e);
        }
    };

    // Preflight: verify the wallet can actually fund this child tx
    // before taking payment. Zero mutation — no addresses revealed,
    // no PSBT built.
    if let Err(e) = preflight_wallet_check(&state, fee_breakdown.mining_fee) {
        state.wallet.release_reservation(&reserved_utxo);
        return Err(e);
    }

    let description = format!("cpfp.me: bump tx {}", parent.tx.compute_txid());
    // Invoice expiry matches UTXO reservation TTL so the invoice
    // becomes unpayable before the reservation is released.
    let invoice = match state
        .payment
        .create_invoice(
            fee_breakdown.invoice_amount.to_sat(),
            &description,
            RESERVATION_TTL.as_secs(),
        )
        .await
    {
        Ok(inv) => inv,
        Err(e) => {
            state.wallet.release_reservation(&reserved_utxo);
            return Err(e);
        }
    };

    let order_id = uuid::Uuid::new_v4().to_string();
    let parent_txid = parent.tx.compute_txid();

    tracing::info!(
        order_id = %order_id,
        parent_txid = %parent_txid,
        fee_rate,
        mining_fee_sats = fee_breakdown.mining_fee.to_sat(),
        invoice_sats = fee_breakdown.invoice_amount.to_sat(),
        reserved_utxo = %reserved_utxo,
        "order created"
    );

    let response = SubmitResponse {
        order_id: order_id.clone(),
        bolt11: invoice.bolt11.clone(),
        amount_sat: invoice.amount_sat,
        fee_rate,
    };

    let order = Order::new(
        &parent,
        invoice,
        fee_breakdown.mining_fee,
        fee_rate,
        reserved_utxo,
    );

    let mut orders = lock_orders(&state)?;
    orders.insert(order_id, order);

    Ok(Json(response))
}

async fn handle_status(
    State(state): State<AppState>,
    Path(order_id): Path<String>,
) -> Result<Json<StatusResponse>, AppError> {
    let current_status = {
        let orders = lock_orders(&state)?;
        let order = orders
            .get(&order_id)
            .ok_or_else(|| AppError::NotFound(order_id.clone()))?;
        order.status.clone()
    };

    match current_status {
        OrderStatus::AwaitingPayment => handle_awaiting_payment(&state, &order_id).await,
        OrderStatus::Paid => handle_paid(&state, &order_id).await,
        OrderStatus::Broadcast { ref txid } => Ok(Json(StatusResponse {
            status: "broadcast".into(),
            txid: Some(txid.clone()),
            mempool_url: Some(state.config.mempool_url_for_tx(txid)),
            bolt11: None,
            amount_sat: None,
            fee_rate: None,
            error: None,
        })),
        OrderStatus::Failed { ref reason } => Ok(Json(StatusResponse {
            status: "failed".into(),
            error: Some(reason.clone()),
            bolt11: None,
            amount_sat: None,
            fee_rate: None,
            txid: None,
            mempool_url: None,
        })),
    }
}

async fn handle_awaiting_payment(
    state: &AppState,
    order_id: &str,
) -> Result<Json<StatusResponse>, AppError> {
    let payment_hash = {
        let orders = lock_orders(state)?;
        let order = orders
            .get(order_id)
            .ok_or_else(|| AppError::NotFound(order_id.to_string()))?;
        order.invoice.payment_hash.clone()
    };

    let payment_status = state.payment.check_payment(&payment_hash).await?;

    if payment_status == PaymentStatus::Paid {
        tracing::info!(order_id, "payment received");
        let reserved_utxo = {
            let mut orders = lock_orders(state)?;
            let order = orders
                .get_mut(order_id)
                .ok_or_else(|| AppError::NotFound(order_id.to_string()))?;
            order.status = OrderStatus::Paid;
            order.reserved_utxo
        };
        state.wallet.consume_reservation(&reserved_utxo);
        return handle_paid(state, order_id).await;
    }

    // Still waiting
    let orders = lock_orders(state)?;
    let order = orders
        .get(order_id)
        .ok_or_else(|| AppError::NotFound(order_id.to_string()))?;

    Ok(Json(StatusResponse {
        status: "awaiting_payment".into(),
        bolt11: Some(order.invoice.bolt11.clone()),
        amount_sat: Some(order.invoice.amount_sat),
        fee_rate: Some(order.fee_rate),
        txid: None,
        mempool_url: None,
        error: None,
    }))
}

async fn handle_paid(state: &AppState, order_id: &str) -> Result<Json<StatusResponse>, AppError> {
    let (parent_hex, mining_fee) = get_order_details(state, order_id)?;
    let parent = validate::validate_parent_tx(&parent_hex)?;

    // Wallet was synced at startup — skip re-sync here to avoid
    // blocking on slow Esplora responses after user already paid.
    // The wallet's UTXO state is maintained by BDK as we build txs.

    let built_child = match build_child(state, &parent, mining_fee) {
        Ok(child) => child,
        Err(e) => {
            let reason = e.to_string();
            tracing::error!(
                order_id,
                error = %reason,
                "failed to build child tx after payment"
            );
            set_order_status(
                state,
                order_id,
                OrderStatus::Failed {
                    reason: reason.clone(),
                },
            )?;
            return Ok(Json(StatusResponse {
                status: "failed".into(),
                error: Some(reason),
                bolt11: None,
                amount_sat: None,
                fee_rate: None,
                txid: None,
                mempool_url: None,
            }));
        }
    };
    let parent_txid = parent.tx.compute_txid().to_string();

    let result = broadcast::submit_package(
        &state.http_client,
        &state.config.mempool_api_url,
        &parent.raw_hex,
        &built_child.hex,
    )
    .await;

    match result {
        Ok(_) => {
            tracing::info!(
                parent_txid = %parent_txid,
                parent_hex = %parent.raw_hex,
                child_txid = %built_child.tx.compute_txid(),
                child_hex = %built_child.hex,
                fee = %mining_fee,
                "package broadcast successful"
            );
            set_order_status(
                state,
                order_id,
                OrderStatus::Broadcast {
                    txid: parent_txid.clone(),
                },
            )?;
            state.record_bump(parent_txid.clone());
            Ok(Json(StatusResponse {
                status: "broadcast".into(),
                txid: Some(parent_txid.clone()),
                mempool_url: Some(state.config.mempool_url_for_tx(&parent_txid)),
                bolt11: None,
                amount_sat: None,
                fee_rate: None,
                error: None,
            }))
        }
        Err(e) => {
            let reason = e.to_string();
            tracing::error!(
                parent_txid = %parent_txid,
                parent_hex = %parent.raw_hex,
                child_txid = %built_child.tx.compute_txid(),
                child_hex = %built_child.hex,
                error = %reason,
                "package broadcast failed"
            );
            // Re-sync wallet to recover the unbroadcast UTXO
            if let Err(sync_err) = state.wallet.sync().await {
                tracing::error!(error = %sync_err, "failed to re-sync wallet after broadcast failure");
            }
            set_order_status(
                state,
                order_id,
                OrderStatus::Failed {
                    reason: reason.clone(),
                },
            )?;
            Ok(Json(StatusResponse {
                status: "failed".into(),
                error: Some(reason),
                bolt11: None,
                amount_sat: None,
                fee_rate: None,
                txid: None,
                mempool_url: None,
            }))
        }
    }
}

fn get_order_details(state: &AppState, order_id: &str) -> Result<(String, Amount), AppError> {
    let orders = lock_orders(state)?;
    let order = orders
        .get(order_id)
        .ok_or_else(|| AppError::NotFound(order_id.to_string()))?;
    Ok((order.parent_raw_hex.clone(), order.mining_fee))
}

fn build_child(
    state: &AppState,
    parent: &validate::ValidatedParent,
    mining_fee: Amount,
) -> Result<child::BuiltChild, AppError> {
    let mut wallet = state
        .wallet
        .wallet
        .lock()
        .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
    let utxo_target = state.config.utxo_target_count;
    child::build_child_tx(&mut wallet, parent, mining_fee, utxo_target)
}

fn preflight_wallet_check(
    state: &AppState,
    mining_fee: Amount,
) -> Result<(), AppError> {
    let wallet = state
        .wallet
        .wallet
        .lock()
        .map_err(|e| AppError::Wallet(format!("wallet lock poisoned: {e}")))?;
    let utxo_target = state.config.utxo_target_count;

    if let Err(e) = child::preflight_check_wallet(&wallet, mining_fee, utxo_target)
    {
        let balance = wallet.balance().total().to_sat();
        let confirmed: Vec<u64> = wallet
            .list_unspent()
            .filter(|u| {
                matches!(
                    u.chain_position,
                    bdk_wallet::chain::ChainPosition::Confirmed { .. }
                )
            })
            .map(|u| u.txout.value.to_sat())
            .collect();
        tracing::error!(
            error = %e,
            balance_sats = balance,
            confirmed_utxo_count = confirmed.len(),
            confirmed_utxo_values_sats = ?confirmed,
            mining_fee_sats = mining_fee.to_sat(),
            "preflight wallet check failed"
        );
        return Err(e);
    }
    Ok(())
}

/// Test-only: simulate payment received for an order.
/// Consumes the UTXO reservation and triggers child tx build + broadcast.
async fn handle_fakepay(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> Result<Json<StatusResponse>, AppError> {
    check_admin_auth(&headers, &state)?;
    let reserved_utxo = {
        let mut orders = lock_orders(&state)?;
        let order = orders
            .get_mut(&order_id)
            .ok_or_else(|| AppError::NotFound(order_id.clone()))?;
        if order.status != OrderStatus::AwaitingPayment {
            return Err(AppError::Internal("order is not awaiting payment".into()));
        }
        order.status = OrderStatus::Paid;
        order.reserved_utxo
    };
    state.wallet.consume_reservation(&reserved_utxo);
    tracing::info!(order_id = %order_id, "FAKEPAY: simulated payment");
    handle_paid(&state, &order_id).await
}

fn check_admin_auth(headers: &HeaderMap, state: &AppState) -> Result<(), AppError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if token != state.config.admin_token {
        return Err(AppError::NotFound("not found".into()));
    }
    Ok(())
}

async fn handle_admin_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    check_admin_auth(&headers, &state)?;
    let address = state.wallet.next_address()?;
    let balance = state.wallet.balance()?;
    let utxo_count = state.wallet.utxo_count()?;
    Ok(Json(serde_json::json!({
        "deposit_address": address,
        "balance_sats": balance,
        "utxo_count": utxo_count,
    })))
}

async fn handle_demo_parent(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let esplora_url = format!("{}/api", state.config.mempool_api_url);
    let esplora = bdk_esplora::esplora_client::Builder::new(&esplora_url)
        .build_async()
        .map_err(|e| AppError::Internal(format!("failed to build esplora client: {e}")))?;
    state.demo_wallet.sync(&esplora).await?;

    let parent = state.demo_wallet.build_parent()?;

    tracing::info!(
        txid = %parent.txid,
        value_sats = parent.value_sats,
        "built demo parent"
    );

    Ok(Json(serde_json::json!({
        "raw_tx": parent.hex,
        "txid": parent.txid,
        "value_sats": parent.value_sats,
    })))
}

async fn handle_recent_bumps(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let bumps = state.get_recent_bumps();
    let links: Vec<_> = bumps
        .iter()
        .map(|txid| {
            serde_json::json!({
                "txid": txid,
                "url": state.config.mempool_url_for_tx(txid),
            })
        })
        .collect();
    Json(serde_json::json!({ "bumps": links }))
}

fn set_order_status(state: &AppState, order_id: &str, status: OrderStatus) -> Result<(), AppError> {
    let mut orders = lock_orders(state)?;
    if let Some(order) = orders.get_mut(order_id) {
        order.status = status;
    }
    Ok(())
}

fn lock_orders(
    state: &AppState,
) -> Result<std::sync::MutexGuard<'_, std::collections::HashMap<String, Order>>, AppError> {
    state
        .orders
        .lock()
        .map_err(|e| AppError::Internal(format!("orders lock poisoned: {e}")))
}

/// Spawns a background task to consolidate wallet UTXOs into one.
/// Fires and forgets — errors are logged, not returned.
fn trigger_consolidation(state: AppState) {
    tokio::spawn(async move {
        let tx_hex = match state.wallet.build_consolidation_tx() {
            Ok(Some(hex)) => hex,
            Ok(None) => {
                tracing::info!("consolidation skipped: fewer than 2 UTXOs");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to build consolidation tx");
                return;
            }
        };

        let url = format!("{}/api/tx", state.config.mempool_api_url);
        match state.http_client.post(&url).body(tx_hex).send().await {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    tracing::info!(txid = %body, "consolidation tx broadcast");
                } else {
                    tracing::error!(
                        http_status = %status,
                        response = %body,
                        "consolidation broadcast failed"
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "consolidation broadcast request failed");
            }
        }
    });
}
