use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Invalid transaction: {reason}")]
    InvalidTx { reason: String },

    #[error("Fee estimation failed: {0}")]
    FeeEstimation(String),

    #[error("Wallet error: {0}")]
    Wallet(String),

    #[error("Payment error: {0}")]
    Payment(String),

    #[error("Broadcast failed: {0}")]
    Broadcast(String),

    #[error("Service at capacity: {0}")]
    AtCapacity(String),

    #[error("Order not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::InvalidTx { .. } => (StatusCode::BAD_REQUEST, self.to_string()),
            Self::AtCapacity(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            Self::FeeEstimation(_) => {
                tracing::error!("{self}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Fee estimation unavailable".into(),
                )
            }
            Self::Payment(_) => {
                tracing::error!("{self}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Payment service unavailable".into(),
                )
            }
            Self::Broadcast(_) => {
                tracing::error!("{self}");
                (StatusCode::BAD_GATEWAY, "Broadcast failed".into())
            }
            Self::Wallet(_) | Self::Internal(_) => {
                tracing::error!("{self}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".into(),
                )
            }
        };

        let body = axum::Json(json!({ "error": message }));
        (status, body).into_response()
    }
}
