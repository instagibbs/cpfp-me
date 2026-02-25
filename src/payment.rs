use serde::Deserialize;

use crate::error::AppError;

pub struct Invoice {
    pub bolt11: String,
    pub payment_hash: String,
    pub amount_sat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentStatus {
    Pending,
    Paid,
}

pub struct PhoenixdClient {
    client: reqwest::Client,
    base_url: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct CreateInvoiceResponse {
    #[serde(rename = "serialized")]
    bolt11: String,
    #[serde(rename = "paymentHash")]
    payment_hash: String,
}

#[derive(Debug, Deserialize)]
struct PaymentResponse {
    #[serde(rename = "isPaid")]
    is_paid: bool,
}

impl PhoenixdClient {
    pub fn new(base_url: String, password: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            password,
        }
    }

    pub async fn create_invoice(
        &self,
        amount_sat: u64,
        description: &str,
        expiry_secs: u64,
    ) -> Result<Invoice, AppError> {
        let url = format!("{}/createinvoice", self.base_url);
        let response: CreateInvoiceResponse = self
            .client
            .post(&url)
            .basic_auth("phoenix", Some(&self.password))
            .form(&[
                ("amountSat", amount_sat.to_string()),
                ("description", description.to_string()),
                ("expirySeconds", expiry_secs.to_string()),
            ])
            .send()
            .await
            .map_err(|e| AppError::Payment(format!("phoenixd request failed: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::Payment(format!("phoenixd returned error: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::Payment(format!("invalid phoenixd response: {e}")))?;

        Ok(Invoice {
            bolt11: response.bolt11,
            payment_hash: response.payment_hash,
            amount_sat,
        })
    }

    pub async fn check_payment(&self, payment_hash: &str) -> Result<PaymentStatus, AppError> {
        let url = format!("{}/payments/incoming/{}", self.base_url, payment_hash);
        let response: PaymentResponse = self
            .client
            .get(&url)
            .basic_auth("phoenix", Some(&self.password))
            .send()
            .await
            .map_err(|e| AppError::Payment(format!("phoenixd request failed: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::Payment(format!("phoenixd returned error: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::Payment(format!("invalid phoenixd response: {e}")))?;

        if response.is_paid {
            Ok(PaymentStatus::Paid)
        } else {
            Ok(PaymentStatus::Pending)
        }
    }
}
