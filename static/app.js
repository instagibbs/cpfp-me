/* global QRCode */

let currentOrderId = null;
let pollInterval = null;

function showState(name) {
  const states = [
    "input", "invoice", "broadcasting", "success", "error",
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
  document.getElementById("fee-rate-display").textContent =
    data.fee_rate;
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

  showState("invoice");
  startPolling();
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
      showState("broadcasting");
      setTimeout(() => {
        document.getElementById("mempool-link").href =
          data.mempool_url;
        document.getElementById("mempool-link").textContent =
          data.mempool_url;
        showState("success");
      }, 1000);
      return;
    }

    if (data.status === "failed") {
      stopPolling();
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
  currentOrderId = null;
  document.getElementById("raw-tx").value = "";
  clearInputError();
  showState("input");
}
