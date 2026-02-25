use bitcoin::consensus::Decodable;
use bitcoin::script::Builder;
use bitcoin::transaction::Version;
use bitcoin::{opcodes, ScriptBuf, Transaction};

use crate::error::AppError;

const TRUC_VERSION: Version = Version(3);
const MAX_TRUC_PARENT_VSIZE: u64 = 10_000;

#[derive(Debug)]
pub struct ValidatedParent {
    pub tx: Transaction,
    pub raw_hex: String,
    pub p2a_vout: u32,
    pub vsize: u64,
}

pub fn p2a_script() -> ScriptBuf {
    Builder::new()
        .push_opcode(opcodes::all::OP_PUSHNUM_1)
        .push_slice([0x4e, 0x73])
        .into_script()
}

pub fn validate_parent_tx(raw_hex: &str) -> Result<ValidatedParent, AppError> {
    let bytes = hex::decode(raw_hex).map_err(|e| AppError::InvalidTx {
        reason: format!("invalid hex: {e}"),
    })?;

    let tx =
        Transaction::consensus_decode(&mut bytes.as_slice()).map_err(|e| AppError::InvalidTx {
            reason: format!("invalid transaction: {e}"),
        })?;

    if tx.version != TRUC_VERSION {
        return Err(AppError::InvalidTx {
            reason: format!("transaction version is {}, expected 3 (TRUC)", tx.version.0),
        });
    }

    let expected_p2a = p2a_script();
    let p2a_vout = tx
        .output
        .iter()
        .position(|o| o.script_pubkey == expected_p2a)
        .ok_or_else(|| AppError::InvalidTx {
            reason: "no P2A output found (expected OP_1 <0x4e73>)".into(),
        })?;

    let vsize = tx.vsize() as u64;
    if vsize > MAX_TRUC_PARENT_VSIZE {
        return Err(AppError::InvalidTx {
            reason: format!(
                "transaction vsize {vsize} exceeds TRUC limit \
                 of {MAX_TRUC_PARENT_VSIZE}"
            ),
        });
    }

    Ok(ValidatedParent {
        tx,
        raw_hex: raw_hex.to_string(),
        p2a_vout: u32::try_from(p2a_vout).map_err(|_| AppError::InvalidTx {
            reason: "too many outputs".into(),
        })?,
        vsize,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use bitcoin::absolute::LockTime;
    use bitcoin::consensus::Encodable;
    use bitcoin::hashes::Hash;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, OutPoint, Sequence, Transaction, TxIn, TxOut};

    use super::*;

    fn dummy_p2wpkh() -> ScriptBuf {
        ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::all_zeros())
    }

    fn make_truc_tx_with_p2a() -> Transaction {
        Transaction {
            version: Version(3),
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(50_000),
                    script_pubkey: dummy_p2wpkh(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: p2a_script(),
                },
            ],
        }
    }

    fn encode_tx(tx: &Transaction) -> String {
        let mut buf = Vec::new();
        tx.consensus_encode(&mut buf).unwrap();
        hex::encode(buf)
    }

    #[test]
    fn valid_truc_with_p2a() {
        let tx = make_truc_tx_with_p2a();
        let hex_str = encode_tx(&tx);
        let parent = validate_parent_tx(&hex_str).unwrap();
        assert_eq!(parent.p2a_vout, 1);
        assert!(parent.vsize > 0);
    }

    #[test]
    fn rejects_non_truc_version() {
        let mut tx = make_truc_tx_with_p2a();
        tx.version = Version(2);
        let hex_str = encode_tx(&tx);
        let err = validate_parent_tx(&hex_str).unwrap_err().to_string();
        assert!(err.contains("version is 2"));
    }

    #[test]
    fn rejects_missing_p2a_output() {
        let mut tx = make_truc_tx_with_p2a();
        tx.output = vec![TxOut {
            value: Amount::from_sat(50_000),
            script_pubkey: dummy_p2wpkh(),
        }];
        let hex_str = encode_tx(&tx);
        let err = validate_parent_tx(&hex_str).unwrap_err().to_string();
        assert!(err.contains("no P2A output"));
    }

    #[test]
    fn rejects_invalid_hex() {
        let err = validate_parent_tx("not_hex!").unwrap_err().to_string();
        assert!(err.contains("invalid hex"));
    }

    #[test]
    fn accepts_nonzero_p2a_value() {
        let mut tx = make_truc_tx_with_p2a();
        tx.output[1].value = Amount::from_sat(240);
        let hex_str = encode_tx(&tx);
        assert!(validate_parent_tx(&hex_str).is_ok());
    }

    #[test]
    fn finds_p2a_at_any_index() {
        let tx = Transaction {
            version: Version(3),
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(50_000),
                    script_pubkey: dummy_p2wpkh(),
                },
                TxOut {
                    value: Amount::from_sat(30_000),
                    script_pubkey: dummy_p2wpkh(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: p2a_script(),
                },
            ],
        };
        let hex_str = encode_tx(&tx);
        let parent = validate_parent_tx(&hex_str).unwrap();
        assert_eq!(parent.p2a_vout, 2);
    }
}
