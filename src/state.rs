use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bitcoin::{Amount, OutPoint};

use crate::config::Config;
use crate::demo::DemoWallet;
use crate::payment::{Invoice, PhoenixdClient};
use crate::validate::ValidatedParent;
use crate::wallet::AppWallet;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub http_client: reqwest::Client,
    pub wallet: Arc<AppWallet>,
    pub payment: Arc<PhoenixdClient>,
    pub orders: Arc<Mutex<HashMap<String, Order>>>,
    pub demo_wallet: Arc<DemoWallet>,
}

pub struct Order {
    pub parent_raw_hex: String,
    pub invoice: Invoice,
    pub mining_fee: Amount,
    pub fee_rate: u64,
    pub reserved_utxo: OutPoint,
    pub status: OrderStatus,
    pub created_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderStatus {
    AwaitingPayment,
    Paid,
    Broadcast { txid: String },
    Failed { reason: String },
}

impl Order {
    pub fn new(
        parent: &ValidatedParent,
        invoice: Invoice,
        mining_fee: Amount,
        fee_rate: u64,
        reserved_utxo: OutPoint,
    ) -> Self {
        Self {
            parent_raw_hex: parent.raw_hex.clone(),
            mining_fee,
            fee_rate,
            invoice,
            reserved_utxo,
            status: OrderStatus::AwaitingPayment,
            created_at: Instant::now(),
        }
    }
}
