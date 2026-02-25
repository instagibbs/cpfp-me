use std::net::SocketAddr;
use std::path::PathBuf;

use bitcoin::Network;
use serde::Deserialize;

use crate::error::AppError;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub network: NetworkConfig,
    pub listen_addr: SocketAddr,
    pub markup_percent: f64,
    pub utxo_target_count: u32,
    pub mnemonic: String,
    pub wallet_db_path: PathBuf,
    pub phoenixd_url: String,
    pub phoenixd_password: String,
    pub mempool_api_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkConfig {
    Bitcoin,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
}

impl NetworkConfig {
    pub fn to_bitcoin_network(&self) -> Network {
        match self {
            Self::Bitcoin => Network::Bitcoin,
            Self::Testnet | Self::Testnet4 => Network::Testnet,
            Self::Signet => Network::Signet,
            Self::Regtest => Network::Regtest,
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self, AppError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| AppError::Internal(format!("Failed to read config file '{path}': {e}")))?;
        let config: Self = toml::from_str(&contents)
            .map_err(|e| AppError::Internal(format!("Failed to parse config file: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), AppError> {
        if self.markup_percent < 0.0 {
            return Err(AppError::Internal(
                "markup_percent must be non-negative".into(),
            ));
        }
        if self.utxo_target_count == 0 {
            return Err(AppError::Internal(
                "utxo_target_count must be at least 1".into(),
            ));
        }
        if self.mnemonic.split_whitespace().count() != 12
            && self.mnemonic.split_whitespace().count() != 24
        {
            return Err(AppError::Internal("mnemonic must be 12 or 24 words".into()));
        }
        Ok(())
    }

    pub fn mempool_url_for_tx(&self, txid: &str) -> String {
        format!("{}/tx/{txid}", self.mempool_api_url)
    }
}
