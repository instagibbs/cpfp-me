use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bitcoin::Amount;

use crate::config::Config;
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
}

pub struct Order {
    pub parent_raw_hex: String,
    pub invoice: Invoice,
    pub total_fee: Amount,
    pub fee_rate: u64,
    pub status: OrderStatus,
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
        total_fee: Amount,
        fee_rate: u64,
    ) -> Self {
        Self {
            parent_raw_hex: parent.raw_hex.clone(),
            total_fee,
            fee_rate,
            invoice,
            status: OrderStatus::AwaitingPayment,
        }
    }
}
