/* global QRCode */

const INVOICE_TTL_SECS = 60;

// Minimal Bitcoin tx parser: extracts version and P2A output index.
// Returns { truc: bool, p2a: bool, p2aVout: number|null } or null on parse failure.
function parseTxChecks(rawHex) {
  try {
    const hex = rawHex.toLowerCase().replace(/\s/g, "");
    if (hex.length < 18 || hex.length % 2 !== 0) return null;

    let pos = 0;

    function readHex(nBytes) {
      if (pos + nBytes * 2 > hex.length) throw new Error("overflow");
      const s = hex.slice(pos, pos + nBytes * 2);
      pos += nBytes * 2;
      return s;
    }

    function readUInt32LE() {
      const b = readHex(4);
      return (
        parseInt(b.slice(0, 2), 16) |
        (parseInt(b.slice(2, 4), 16) << 8) |
        (parseInt(b.slice(4, 6), 16) << 16) |
        (parseInt(b.slice(6, 8), 16) << 24)
      ) >>> 0;
    }

    function readVarInt() {
      const first = parseInt(readHex(1), 16);
      if (first < 0xfd) return first;
      if (first === 0xfd) {
        const b = readHex(2);
        return parseInt(b.slice(2, 4) + b.slice(0, 2), 16);
      }
      if (first === 0xfe) {
        const b = readHex(4);
        return parseInt(
          b.slice(6, 8) + b.slice(4, 6) + b.slice(2, 4) + b.slice(0, 2), 16,
        );
      }
      throw new Error("varint too large");
    }

    const version = readUInt32LE();

    // Detect segwit marker (00 01) and skip it
    const isSegwit = hex.slice(pos, pos + 4) === "0001";
    if (isSegwit) {
      pos += 4;
    }

    // Skip inputs
    const inputCount = readVarInt();
    for (let i = 0; i < inputCount; i++) {
      readHex(32); // txid
      readHex(4);  // vout index
      const scriptLen = readVarInt();
      readHex(scriptLen); // scriptSig
      readHex(4);  // sequence
    }

    // Parse outputs, look for P2A: script = 51 02 4e 73 (OP_1 PUSH2 0x4e73)
    const outputCount = readVarInt();
    let p2aVout = null;
    for (let i = 0; i < outputCount; i++) {
      readHex(8); // value (8 bytes LE)
      const scriptLen = readVarInt();
      const script = readHex(scriptLen);
      if (scriptLen === 4 && script === "51024e73" && p2aVout === null) {
        p2aVout = i;
      }
    }

    // Parse witness stacks (one per input for segwit txs)
    if (isSegwit) {
      for (let i = 0; i < inputCount; i++) {
        const itemCount = readVarInt();
        for (let j = 0; j < itemCount; j++) {
          readHex(readVarInt()); // witness item
        }
      }
    }

    readHex(4); // locktime

    // Full consumption check: any truncation or trailing garbage fails here
    if (pos !== hex.length) return null;

    return { truc: version === 3, p2a: p2aVout !== null, p2aVout };
  } catch (_) {
    return null;
  }
}

let txCheckDebounce = null;

function updateTxChecks() {
  const rawTx = document.getElementById("raw-tx").value.trim();
  const container = document.getElementById("tx-checks");

  if (!rawTx) {
    container.classList.add("hidden");
    document.getElementById("btn-submit").disabled = true;
    return;
  }

  const checks = parseTxChecks(rawTx);
  if (!checks) {
    container.classList.add("hidden");
    document.getElementById("btn-submit").disabled = true;
    return;
  }

  container.classList.remove("hidden");
  document.getElementById("btn-submit").disabled = !(checks.truc && checks.p2a);

  const trucEl = document.getElementById("check-truc");
  trucEl.className = "check-item " + (checks.truc ? "check-pass" : "check-fail");
  trucEl.querySelector(".check-icon").textContent = checks.truc ? "\u2713" : "\u2717";

  const p2aEl = document.getElementById("check-p2a");
  p2aEl.className = "check-item " + (checks.p2a ? "check-pass" : "check-fail");
  p2aEl.querySelector(".check-icon").textContent = checks.p2a ? "\u2713" : "\u2717";
  const p2aDetail = document.getElementById("check-p2a-detail");
  p2aDetail.textContent = checks.p2a ? "(vout #" + checks.p2aVout + ")" : "(not found)";

  const feeEl = document.getElementById("check-fee");
  if (checks.truc && checks.p2a) {
    feeEl.classList.remove("hidden");
  } else {
    feeEl.classList.add("hidden");
  }
}

let currentOrderId = null;
let pollInterval = null;
let countdownInterval = null;
let expiresAt = null;

function showState(name) {
  const states = [
    "input", "invoice", "broadcasting", "success", "error", "expired",
  ];
  for (const s of states) {
    const el = document.getElementById("state-" + s);
    if (el) el.classList.toggle("hidden", s !== name);
  }
}

function showInputError(msg) {
  const el = document.getElementById("input-error");
  el.textContent = msg;
  el.classList.remove("hidden");
}

function clearInputError() {
  const el = document.getElementById("input-error");
  el.textContent = "";
  el.classList.add("hidden");
}

async function submitTx() {
  clearInputError();
  const rawTx = document.getElementById("raw-tx").value.trim();
  if (!rawTx) {
    showInputError("Please paste a raw transaction hex.");
    return;
  }

  const btn = document.getElementById("btn-submit");
  btn.disabled = true;
  btn.textContent = "Submitting...";

  try {
    const resp = await fetch("/api/submit", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ raw_tx: rawTx }),
    });

    const data = await resp.json();
    if (!resp.ok) {
      showInputError(data.error || "Submission failed.");
      return;
    }

    currentOrderId = data.order_id;
    showInvoice(data);
  } catch (err) {
    showInputError("Network error: " + err.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Bump Transaction";
  }
}

function showInvoiceChecks() {
  const rawTx = document.getElementById("raw-tx").value.trim();
  const checks = parseTxChecks(rawTx);
  const container = document.getElementById("invoice-checks");
  if (!checks) {
    container.classList.add("hidden");
    return;
  }
  container.classList.remove("hidden");
  const p2aDetail = document.getElementById("icheck-p2a-detail");
  p2aDetail.textContent = checks.p2a ? "(vout #" + checks.p2aVout + ")" : "";
}

function showInvoice(data) {
  document.getElementById("bolt11-text").textContent = data.bolt11;
  document.getElementById("fee-rate-display").textContent = data.fee_rate;
  document.getElementById("amount-display").textContent =
    data.amount_sat.toLocaleString();

  const qrContainer = document.getElementById("qr-container");
  qrContainer.innerHTML = "";

  if (typeof QRCode !== "undefined") {
    new QRCode(qrContainer, {
      text: "lightning:" + data.bolt11.toUpperCase(),
      width: 220,
      height: 220,
      colorDark: "#000000",
      colorLight: "#ffffff",
      correctLevel: QRCode.CorrectLevel.L,
    });
  } else {
    qrContainer.textContent = "(QR library not loaded)";
  }

  expiresAt = Date.now() + INVOICE_TTL_SECS * 1000;
  updateCountdown();

  showInvoiceChecks();
  showState("invoice");
  startPolling();
  startCountdown();
}

function startPolling() {
  if (pollInterval) clearInterval(pollInterval);
  pollInterval = setInterval(checkStatus, 2000);
}

function stopPolling() {
  if (pollInterval) {
    clearInterval(pollInterval);
    pollInterval = null;
  }
}

function startCountdown() {
  if (countdownInterval) clearInterval(countdownInterval);
  countdownInterval = setInterval(updateCountdown, 1000);
}

function stopCountdown() {
  if (countdownInterval) {
    clearInterval(countdownInterval);
    countdownInterval = null;
  }
}

function updateCountdown() {
  const remaining = Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000));
  const el = document.getElementById("countdown");
  if (el) el.textContent = remaining + "s";

  if (remaining <= 0) {
    stopPolling();
    stopCountdown();
    showState("expired");
  }
}

async function checkStatus() {
  if (!currentOrderId) return;

  try {
    const resp = await fetch("/api/status/" + currentOrderId);
    const data = await resp.json();

    if (data.status === "awaiting_payment") {
      return;
    }

    if (data.status === "broadcast") {
      stopPolling();
      stopCountdown();
      showState("broadcasting");
      setTimeout(() => {
        document.getElementById("mempool-link").href = data.mempool_url;
        document.getElementById("mempool-link").textContent = data.txid;
        loadRecentBumps();
        showState("success");
      }, 1000);
      return;
    }

    if (data.status === "failed") {
      stopPolling();
      stopCountdown();
      document.getElementById("error-message").textContent =
        data.error || "Unknown error.";
      showState("error");
      return;
    }
  } catch (_err) {
    // Network hiccup, keep polling
  }
}

function copyBolt11() {
  const text = document.getElementById("bolt11-text").textContent;
  navigator.clipboard.writeText(text).then(() => {
    const btn = document.querySelector(".copy-btn");
    btn.textContent = "Copied";
    setTimeout(() => { btn.textContent = "Copy"; }, 1500);
  });
}

function resetForm() {
  stopPolling();
  stopCountdown();
  currentOrderId = null;
  expiresAt = null;
  document.getElementById("raw-tx").value = "";
  document.getElementById("tx-checks").classList.add("hidden");
  document.getElementById("invoice-checks").classList.add("hidden");
  document.getElementById("btn-submit").disabled = true;
  clearInputError();
  showState("input");
}

function resubmit() {
  stopPolling();
  stopCountdown();
  currentOrderId = null;
  expiresAt = null;
  showState("input");
  submitTx();
}

async function loadRecentBumps() {
  try {
    const resp = await fetch("/api/recent-bumps");
    const data = await resp.json();
    const bumps = data.bumps || [];
    const section = document.getElementById("recent-bumps");
    const tbody = document.getElementById("recent-bumps-body");
    tbody.innerHTML = "";
    if (bumps.length === 0) {
      section.classList.add("hidden");
      return;
    }
    for (const bump of bumps) {
      const tr = document.createElement("tr");
      const td = document.createElement("td");
      const a = document.createElement("a");
      a.href = bump.url;
      a.target = "_blank";
      a.rel = "noopener";
      a.textContent = bump.txid;
      td.appendChild(a);
      tr.appendChild(td);
      tbody.appendChild(tr);
    }
    section.classList.remove("hidden");
  } catch (_err) {
    // Non-critical; leave section hidden
  }
}

async function loadDemoParent() {
  clearInputError();
  const btn = document.getElementById("btn-demo");
  btn.disabled = true;
  btn.textContent = "Loading...";

  try {
    const resp = await fetch("/api/demo-parent");
    const data = await resp.json();
    if (!resp.ok) {
      showInputError(data.error || "Failed to load demo transaction.");
      return;
    }
    document.getElementById("raw-tx").value = data.raw_tx;
    updateTxChecks();
  } catch (err) {
    showInputError("Network error: " + err.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Try with demo tx";
  }
}

document.addEventListener("DOMContentLoaded", () => {
  loadRecentBumps();

  const textarea = document.getElementById("raw-tx");
  function onTextareaChange() {
    document.getElementById("btn-submit").disabled = true;
    document.getElementById("tx-checks").classList.add("hidden");
    clearTimeout(txCheckDebounce);
    txCheckDebounce = setTimeout(updateTxChecks, 150);
  }

  textarea.addEventListener("input", onTextareaChange);
  textarea.addEventListener("cut", onTextareaChange);
  textarea.addEventListener("paste", onTextareaChange);
});
