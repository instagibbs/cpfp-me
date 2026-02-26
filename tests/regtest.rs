//! Integration tests for CPFP child transaction construction on regtest.
//!
//! Requires a bitcoind v28+ binary. Set BITCOIND_EXE env var or have
//! `bitcoind` in PATH. Tests are skipped if no suitable binary is found.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::single_match_else,
    clippy::doc_markdown,
    clippy::print_stderr
)]

use std::path::PathBuf;

use bitcoin::absolute::LockTime;
use bitcoin::consensus::Encodable;
use bitcoin::transaction::Version;
use bitcoin::{Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut};
use bitcoind::bitcoincore_rpc::RpcApi;
use bitcoind::BitcoinD;

use cpfp_me::child::build_child_tx;
use cpfp_me::validate::{p2a_script, validate_parent_tx};

fn start_bitcoind() -> Option<BitcoinD> {
    let exe = std::env::var("BITCOIND_EXE").ok().or_else(|| {
        let out = std::process::Command::new("which")
            .arg("bitcoind")
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).ok())?
            .map(|s| s.trim().to_string())
    })?;
    let mut conf = bitcoind::Conf::default();
    conf.args.push("-txindex=1");
    BitcoinD::with_conf(exe, &conf).ok()
}

fn encode_tx_hex(tx: &Transaction) -> String {
    let mut buf = Vec::new();
    tx.consensus_encode(&mut buf).unwrap();
    hex::encode(buf)
}

/// Syncs a BDK wallet by applying all blocks from the regtest node.
fn sync_wallet(wallet: &mut bdk_wallet::Wallet, rpc: &bitcoind::bitcoincore_rpc::Client) {
    let tip_height = rpc.get_block_count().unwrap();

    for h in 0..=tip_height {
        let hash = rpc.get_block_hash(h).unwrap();
        let block = rpc.get_block(&hash).unwrap();

        let connected_to = if h == 0 {
            bdk_wallet::chain::BlockId { height: 0, hash }
        } else {
            bdk_wallet::chain::BlockId {
                height: (h - 1) as u32,
                hash: rpc.get_block_hash(h - 1).unwrap(),
            }
        };

        wallet
            .apply_block_connected_to(&block, h as u32, connected_to)
            .unwrap();
    }
}

/// Creates a TRUC v3 parent transaction with a P2A output by spending
/// from bitcoind's default wallet.
fn create_truc_parent(rpc: &bitcoind::bitcoincore_rpc::Client) -> Transaction {
    let unspent = rpc.list_unspent(Some(100), None, None, None, None).unwrap();
    let utxo = unspent.first().expect("need a mature UTXO");

    let funding_outpoint = OutPoint::new(utxo.txid, utxo.vout);

    // 0-fee parent: all input value goes to payment output + 0-sat P2A
    let payment_addr = rpc.get_new_address(None, None).unwrap().assume_checked();

    let parent = Transaction {
        version: Version(3),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: funding_outpoint,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: bitcoin::Witness::new(),
        }],
        output: vec![
            TxOut {
                value: utxo.amount,
                script_pubkey: payment_addr.script_pubkey(),
            },
            TxOut {
                value: Amount::ZERO,
                script_pubkey: p2a_script(),
            },
        ],
    };

    // Sign via bitcoind's wallet
    let parent_hex = encode_tx_hex(&parent);
    let signed = rpc
        .sign_raw_transaction_with_wallet(parent_hex, None, None)
        .unwrap();
    assert!(signed.complete, "bitcoind failed to sign parent tx");

    bitcoin::consensus::deserialize(&signed.hex).unwrap()
}

fn create_test_wallet() -> bdk_wallet::Wallet {
    let mnemonic = bdk_wallet::keys::bip39::Mnemonic::parse(
        "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about",
    )
    .unwrap();

    let xpriv = bitcoin::bip32::Xpriv::new_master(Network::Regtest, &mnemonic.to_seed("")).unwrap();

    let external = bdk_wallet::template::Bip86(xpriv, bdk_wallet::KeychainKind::External);
    let internal = bdk_wallet::template::Bip86(xpriv, bdk_wallet::KeychainKind::Internal);

    bdk_wallet::Wallet::create(external, internal)
        .network(Network::Regtest)
        .create_wallet_no_persist()
        .unwrap()
}

#[test]
fn child_tx_has_correct_structure() {
    let Some(bitcoind) = start_bitcoind() else {
        eprintln!("SKIP: bitcoind not found, set BITCOIND_EXE");
        return;
    };
    let rpc = &bitcoind.client;

    // Fund our BDK hot wallet
    let mut wallet = create_test_wallet();
    let addr = wallet.reveal_next_address(bdk_wallet::KeychainKind::External);
    rpc.generate_to_address(101, &addr.address).unwrap();
    sync_wallet(&mut wallet, rpc);

    let balance = wallet.balance().total();
    assert!(balance > Amount::ZERO, "wallet should be funded");

    // Create a TRUC parent from bitcoind's wallet
    // First, fund bitcoind's wallet too
    let btc_addr = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(101, &btc_addr).unwrap();
    sync_wallet(&mut wallet, rpc);

    let parent_tx = create_truc_parent(rpc);
    let parent_hex = encode_tx_hex(&parent_tx);
    let parent = validate_parent_tx(&parent_hex).unwrap();

    assert_eq!(parent.tx.version, Version(3));
    assert_eq!(parent.p2a_vout, 1);

    // Build the CPFP child
    let total_fee = Amount::from_sat(500);
    let child = build_child_tx(&mut wallet, &parent, total_fee, 10).unwrap();

    // -- Verify child structure --
    assert_eq!(child.tx.version, Version(3), "child must be TRUC v3");
    assert!(
        child.tx.vsize() <= 1000,
        "child vsize {} must be <= 1000",
        child.tx.vsize()
    );

    // Must spend the P2A output
    let parent_txid = parent.tx.compute_txid();
    let p2a_outpoint = OutPoint::new(parent_txid, parent.p2a_vout);
    let spends_p2a = child
        .tx
        .input
        .iter()
        .any(|inp| inp.previous_output == p2a_outpoint);
    assert!(spends_p2a, "child must spend the P2A output");

    // P2A input must have an empty witness
    let p2a_input = child
        .tx
        .input
        .iter()
        .find(|inp| inp.previous_output == p2a_outpoint)
        .unwrap();
    assert!(
        p2a_input.witness.is_empty(),
        "P2A input witness must be empty stack"
    );

    // Hex round-trips correctly
    let decoded: Transaction =
        bitcoin::consensus::deserialize(&hex::decode(&child.hex).unwrap()).unwrap();
    assert_eq!(decoded.compute_txid(), child.tx.compute_txid());
}

#[test]
fn package_accepted_by_bitcoind() {
    let Some(bitcoind) = start_bitcoind() else {
        eprintln!("SKIP: bitcoind not found, set BITCOIND_EXE");
        return;
    };
    let rpc = &bitcoind.client;

    // Fund our wallet
    let mut wallet = create_test_wallet();
    let addr = wallet.reveal_next_address(bdk_wallet::KeychainKind::External);
    rpc.generate_to_address(101, &addr.address).unwrap();

    // Fund bitcoind's wallet for the parent
    let btc_addr = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(101, &btc_addr).unwrap();
    sync_wallet(&mut wallet, rpc);

    // Build parent + child
    let parent_tx = create_truc_parent(rpc);
    let parent_hex = encode_tx_hex(&parent_tx);
    let parent = validate_parent_tx(&parent_hex).unwrap();

    let total_fee = Amount::from_sat(1000);
    let child = build_child_tx(&mut wallet, &parent, total_fee, 10).unwrap();

    // Submit as package via submitpackage RPC
    let result: serde_json::Value = rpc
        .call(
            "submitpackage",
            &[serde_json::json!([parent_hex, child.hex])],
        )
        .unwrap();

    let package_msg = result["package_msg"].as_str().unwrap_or("");
    assert_eq!(
        package_msg, "success",
        "package should be accepted by bitcoind: {result}"
    );

    // Mine a block and verify both confirm
    let mining_addr = rpc.get_new_address(None, None).unwrap().assume_checked();
    rpc.generate_to_address(1, &mining_addr).unwrap();

    let parent_txid = parent.tx.compute_txid();
    let child_txid = child.tx.compute_txid();

    // Use getrawtransaction with verbose=true to check confirmations.
    // bitcoincore-rpc's typed API doesn't know the "anchor" script type
    // in v28+ so we use the raw JSON RPC call.
    for (label, txid) in [("parent", parent_txid), ("child", child_txid)] {
        let info: serde_json::Value = rpc
            .call(
                "getrawtransaction",
                &[serde_json::json!(txid.to_string()), serde_json::json!(true)],
            )
            .unwrap();
        let confirmations = info["confirmations"].as_i64().unwrap_or(0);
        assert!(confirmations > 0, "{label} should be confirmed: {info}");
    }
}

/// Simulates a server restart: create wallet, build child, mine it,
/// reload wallet from SQLite, sync, build second child spending the
/// change output. This reproduces the mainnet bug where the wallet
/// can't finalize its own change outputs after reload.
#[test]
fn signing_works_after_wallet_reload() {
    let Some(bitcoind) = start_bitcoind() else {
        eprintln!("SKIP: bitcoind not found, set BITCOIND_EXE");
        return;
    };
    let rpc = &bitcoind.client;

    let mnemonic = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";
    let xpriv = bitcoin::bip32::Xpriv::new_master(
        Network::Regtest,
        &bdk_wallet::keys::bip39::Mnemonic::parse(mnemonic)
            .unwrap()
            .to_seed(""),
    )
    .unwrap();
    let external = bdk_wallet::template::Bip86(xpriv, bdk_wallet::KeychainKind::External);
    let internal = bdk_wallet::template::Bip86(xpriv, bdk_wallet::KeychainKind::Internal);

    let db_path = PathBuf::from("/tmp/cpfp-me-test-wallet.sqlite");
    let _ = std::fs::remove_file(&db_path);

    // --- Session 1: create wallet, fund, build first child ---
    {
        let mut conn = bdk_wallet::rusqlite::Connection::open(&db_path).unwrap();
        let mut wallet = bdk_wallet::Wallet::create(external.clone(), internal.clone())
            .network(Network::Regtest)
            .create_wallet(&mut conn)
            .unwrap();

        // Mine blocks to mature coinbase, then fund our wallet with 1 block
        let btc_addr = rpc.get_new_address(None, None).unwrap().assume_checked();
        rpc.generate_to_address(100, &btc_addr).unwrap();
        let addr = wallet.reveal_next_address(bdk_wallet::KeychainKind::External);
        rpc.generate_to_address(1, &addr.address).unwrap();
        rpc.generate_to_address(100, &btc_addr).unwrap();
        sync_wallet(&mut wallet, rpc);

        assert!(
            wallet.balance().total() > Amount::ZERO,
            "wallet should be funded"
        );

        // Build first child
        let parent_tx = create_truc_parent(rpc);
        let parent_hex = encode_tx_hex(&parent_tx);
        let parent = validate_parent_tx(&parent_hex).unwrap();
        let child1 = build_child_tx(&mut wallet, &parent, Amount::from_sat(500), 10).unwrap();

        // Submit package and mine
        let result: serde_json::Value = rpc
            .call(
                "submitpackage",
                &[serde_json::json!([parent_hex, child1.hex])],
            )
            .unwrap();
        assert_eq!(
            result["package_msg"].as_str().unwrap_or(""),
            "success",
            "first package should succeed: {result}"
        );
        rpc.generate_to_address(1, &btc_addr).unwrap();

        // Persist wallet state
        wallet.persist(&mut conn).unwrap();
    }

    // --- Session 2: reload wallet from DB, build second child ---
    {
        let mut conn = bdk_wallet::rusqlite::Connection::open(&db_path).unwrap();
        let mut wallet = bdk_wallet::Wallet::load()
            .descriptor(bdk_wallet::KeychainKind::External, Some(external.clone()))
            .descriptor(bdk_wallet::KeychainKind::Internal, Some(internal.clone()))
            .extract_keys()
            .load_wallet(&mut conn)
            .unwrap()
            .expect("wallet should exist in db");

        // Sync to discover the confirmed child's change outputs
        sync_wallet(&mut wallet, rpc);

        let utxo_count = wallet.list_unspent().count();
        let balance = wallet.balance().total();
        eprintln!("Session 2: {utxo_count} UTXOs, {balance} balance");
        assert!(
            balance > Amount::ZERO,
            "wallet should have balance after reload"
        );

        // Build second child spending change outputs from session 1
        let parent_tx2 = create_truc_parent(rpc);
        let parent_hex2 = encode_tx_hex(&parent_tx2);
        let parent2 = validate_parent_tx(&parent_hex2).unwrap();
        let child2 = build_child_tx(&mut wallet, &parent2, Amount::from_sat(500), 10).unwrap();

        // Verify child2 has valid witnesses on wallet inputs
        let p2a_outpoint = OutPoint::new(parent_tx2.compute_txid(), parent2.p2a_vout);
        for (i, input) in child2.tx.input.iter().enumerate() {
            let is_p2a = input.previous_output == p2a_outpoint;
            if !is_p2a {
                assert!(
                    !input.witness.is_empty(),
                    "wallet input {i} ({}) should have witness data after reload",
                    input.previous_output
                );
            }
        }

        // Submit and verify it's accepted
        let result: serde_json::Value = rpc
            .call(
                "submitpackage",
                &[serde_json::json!([parent_hex2, child2.hex])],
            )
            .unwrap();
        assert_eq!(
            result["package_msg"].as_str().unwrap_or(""),
            "success",
            "second package (after reload) should succeed: {result}"
        );
    }

    let _ = std::fs::remove_file(&db_path);
}
