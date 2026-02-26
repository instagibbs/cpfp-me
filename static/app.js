/* global QRCode */

const INVOICE_TTL_SECS = 60;

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
        document.getElementById("mempool-link").textContent = data.mempool_url;
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
  } catch (err) {
    showInputError("Network error: " + err.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Try with demo tx";
  }
}
