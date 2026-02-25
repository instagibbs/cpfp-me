use std::time::Duration;

use crate::state::{AppState, OrderStatus};

/// How long to keep pending (unpaid) orders before pruning.
const PENDING_TTL: Duration = Duration::from_secs(120);

/// How long to keep completed/failed orders before pruning.
const FINISHED_TTL: Duration = Duration::from_secs(300);

/// How often to run the cleanup sweep.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Spawns a background task that periodically removes stale orders
/// and releases their UTXO reservations.
pub fn spawn_cleanup_task(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            prune_stale_orders(&state);
        }
    });
}

fn prune_stale_orders(state: &AppState) {
    let Ok(mut orders) = state.orders.lock() else {
        return;
    };

    let before = orders.len();

    orders.retain(|_id, order| {
        let age = order.created_at.elapsed();
        let should_remove = match order.status {
            OrderStatus::AwaitingPayment => age > PENDING_TTL,
            OrderStatus::Broadcast { .. } | OrderStatus::Failed { .. } => age > FINISHED_TTL,
            OrderStatus::Paid => false,
        };

        if should_remove && order.status == OrderStatus::AwaitingPayment {
            state.wallet.release_reservation(&order.reserved_utxo);
        }

        !should_remove
    });

    let pruned = before - orders.len();
    if pruned > 0 {
        tracing::info!(pruned, remaining = orders.len(), "pruned stale orders");
    }
}
