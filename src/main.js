import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import confetti from "canvas-confetti";
import "./styles.css";
import agenticLogo from "./assets/agentic.png";

const TEST_MODES = [
  {
    id: "Laundry SMR",
    name: "Laundry SMR",
    flow: "GTS gtsmr -> CTS filters -> GTS retry / STS retry",
    description: "Laundry SMR with deviceinfo replacement and retry.",
    needs: "both",
    laundry: true,
  },
  {
    id: "Laundry Normal",
    name: "Laundry Normal",
    flow: "GTS property -> CTS/GTS retry",
    description: "Laundry Normal with property deviceinfo replacement.",
    needs: "user",
    laundry: true,
  },
  {
    id: "MR",
    name: "MR",
    flow: "CTS normal -> GTS normal",
    description: "Maintenance Release compatibility sequence.",
    needs: "user",
  },
  {
    id: "SMR",
    name: "SMR",
    flow: "CTS/GTS + STS",
    description: "Security maintenance run with user and userdebug groups.",
    needs: "both",
  },
  {
    id: "SKU",
    name: "SKU",
    flow: "CTS SKU -> GTS Variant",
    description: "SKU build validation sequence.",
    needs: "user",
  },
  {
    id: "STS",
    name: "STS",
    flow: "STS dynamic incremental",
    description: "Security Test Suite for userdebug devices.",
    needs: "userdebug",
  },
];

const state = {
  autoRoot: localStorage.getItem("autoRoot") || "",
  wifi: {
    enabled: localStorage.getItem("autoWifiAutoConnect") !== "false",
    ssid: localStorage.getItem("autoWifiSsid") || "RTT / IEEE 802.11",
    password: localStorage.getItem("autoWifiPassword") || "1234qwer",
  },
  retryCount: Number(localStorage.getItem("autoRetryCount") || "5"),
  timeoutSecs: Number(localStorage.getItem("autoTestTimeout") || "86400"),
  devices: [],
  selected: new Set(),
  selectedMode: "Laundry SMR",
  laundryZipPath: "",
  laundryResults: [],
  lockedLaundryResults: new Map(),
  selectedLaundryResults: new Set(),
  lampStates: new Map(),
  running: false,
  activeRuns: new Set(),
  runDevices: new Map(),
  localBusy: new Map(),
  flows: new Map(),
  logFlows: new Map(),
  activeLogFlow: "",
  activeLogSubtab: "Runner",
  suiteStatuses: new Map(),
  summaries: new Map(),
  resultDir: "",
  runStartedAt: null,
  runTableTab: "preview",
  laundryWarnings: [],
};

const app = document.querySelector("#app");
let confettiInterval = null;

app.innerHTML = `
  <div class="shell">
    <div class="splash" id="splash">
      <img src="${agenticLogo}" alt="GBA Agentic Runner" />
      <strong>GBA Agentic Runner</strong>
    </div>

    <header class="titlebar">
      <div class="brand">
        <img class="brand-mark" src="${agenticLogo}" alt="GBA" />
        <div>
          <h1>GBA Agentic Runner</h1>
          <p>CTS, GTS, and STS automation console</p>
        </div>
      </div>
      <button class="icon-button settings-button" id="settingsBtn" title="Settings" aria-label="Settings">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"></path>
          <path d="M19.4 15a1.8 1.8 0 0 0 .36 1.98l.04.04a2 2 0 0 1-2.82 2.82l-.04-.04A1.8 1.8 0 0 0 15 19.44a1.8 1.8 0 0 0-1 .56 1.8 1.8 0 0 0-.52 1.28V21.4a2 2 0 0 1-4 0v-.08A1.8 1.8 0 0 0 8 19.44a1.8 1.8 0 0 0-1.98.36l-.04.04a2 2 0 0 1-2.82-2.82l.04-.04A1.8 1.8 0 0 0 3.56 15a1.8 1.8 0 0 0-.56-1 1.8 1.8 0 0 0-1.28-.52H1.6a2 2 0 0 1 0-4h.08A1.8 1.8 0 0 0 3.56 8a1.8 1.8 0 0 0-.36-1.98l-.04-.04a2 2 0 0 1 2.82-2.82l.04.04A1.8 1.8 0 0 0 8 3.56a1.8 1.8 0 0 0 1-.56 1.8 1.8 0 0 0 .52-1.28V1.6a2 2 0 0 1 4 0v.08A1.8 1.8 0 0 0 15 3.56a1.8 1.8 0 0 0 1.98-.36l.04-.04a2 2 0 0 1 2.82 2.82l-.04.04A1.8 1.8 0 0 0 19.44 8a1.8 1.8 0 0 0 .56 1 1.8 1.8 0 0 0 1.28.52h.12a2 2 0 0 1 0 4h-.08A1.8 1.8 0 0 0 19.4 15Z"></path>
        </svg>
      </button>
    </header>

    <aside class="devices-pane">
      <div class="pane-head">
        <h2>DEVICES</h2>
        <div class="pane-actions">
          <button class="mini-button warn" id="resetBusyBtn">Reset Busy</button>
          <button class="mini-button" id="unselectBtn">Unselect</button>
          <button class="mini-button" id="refreshBtn">Refresh</button>
        </div>
      </div>
      <div class="device-list" id="deviceList"></div>
      <footer id="deviceFooter">Standby</footer>
    </aside>

    <main class="workspace">
      <div class="toolbar">
        <button class="run-button" id="runBtn">Run Selected</button>
        <button class="ghost-button" id="cancelBtn" disabled>Cancel</button>
        <button class="ghost-button" id="openResultBtn" disabled>Open Result</button>
        <div class="toolbar-spacer"></div>
        <label class="retry">Retry <input id="retryInput" type="number" min="0" max="99" /></label>
        <label class="retry">Timeout <input id="timeoutInput" type="number" min="60" /></label>
        <label class="check"><input id="wifiInput" type="checkbox" /> Wi-Fi</label>
      </div>

      <section class="test-area" id="testArea"></section>

      <section class="flow-map expanded" id="flowMapSection">
        <div class="accordion-header" id="flowMapHeader">
          <div class="accordion-header-left">
            <span class="accordion-icon">▼</span>
            <strong>RUN TABLE</strong>
          </div>
          <div class="flow-map-actions">
            <button class="mini-button" id="clearTableBtn">Clear Table</button>
          </div>
        </div>
        <div class="accordion-content">
          <div id="flowMap"></div>
        </div>
      </section>
      <div class="flow-resizer" id="flowResizer" title="Resize testcase table"></div>

      <section class="running-log expanded" id="runningLogSection">
        <div class="accordion-header" id="runningLogHeader">
          <div class="accordion-header-left">
            <span class="accordion-icon">▼</span>
            <strong>CONSOLE LOG</strong>
          </div>
        </div>
        <div class="accordion-content">
          <div class="log-head">
            <div class="log-tab-stack">
              <div class="log-tabs">
                <div class="log-flow-tabs" id="logFlowTabs"></div>
                <button class="log-clear-button" id="clearLogBtn">Clear Log</button>
              </div>
              <div class="log-subtabs" id="logSubtabs"></div>
            </div>
          </div>
          <pre id="logBox"></pre>
        </div>
      </section>
    </main>

    <aside class="summary-pane">
      <section class="summary">
        <h2>SUMMARIZE</h2>
        <div class="metric-row"><span>Active suites:</span><strong id="activeMetric">0</strong></div>
        <div class="metric-row"><span>Completed:</span><strong id="completedMetric">0</strong></div>
        <div class="metric-row"><span>Passed:</span><strong class="pass" id="passedMetric">0</strong></div>
        <div class="metric-row"><span>Failed:</span><strong class="fail" id="failedMetric">0</strong></div>
        <div class="metric-row"><span>Total runtime:</span><strong id="runtimeMetric">00:00:00</strong></div>
        <div class="metric-row result-row"><span>Result:</span><button class="result-pill" id="resultPill" disabled>None</button></div>
      </section>
      <section class="suite-panel">
        <div class="pane-head compact">
          <h2>SUITES</h2>
          <button class="mini-button" id="clearSuitesBtn">Clear</button>
        </div>
        <div class="suite-list" id="suiteList"></div>
      </section>
      <footer class="status-line" id="statusLine">Standby</footer>
    </aside>

    <div class="modal-backdrop hidden" id="settingsModal">
      <section class="settings-modal" role="dialog" aria-modal="true" aria-labelledby="settingsTitle">
        <header>
          <div>
            <h2 id="settingsTitle">SETTINGS</h2>
            <p>AUTO root path, Wi-Fi preset, and preflight check</p>
          </div>
          <button class="icon-button" id="settingsCloseBtn" title="Close">×</button>
        </header>
        <label class="path-field">
          <span>AUTO Path</span>
          <div class="path-row">
            <input id="autoRootInput" type="text" placeholder="/run/media/endri-pro/BINARY HDD/AUTO" />
            <button id="browseBtn" class="ghost-button">Browse</button>
          </div>
        </label>
        <section class="settings-section">
          <label class="check"><input id="wifiAutoConnectInput" type="checkbox" /> Auto connect Wi-Fi before test</label>
          <label class="path-field compact">
            <span>Preset SSID</span>
            <input id="wifiSsidInput" type="text" autocomplete="off" />
          </label>
          <label class="path-field compact">
            <span>Password</span>
            <input id="wifiPasswordInput" type="password" autocomplete="off" />
          </label>
        </section>
        <div class="settings-actions">
          <button class="ghost-button" id="defaultRootBtn">Default Root</button>
          <button class="ghost-button" id="preflightBtn">Check</button>
          <button class="run-button" id="settingsSaveBtn">Save</button>
        </div>
        <pre class="settings-output" id="settingsOutput"></pre>
      </section>
    </div>

    <div class="modal-backdrop hidden" id="warningsModal">
      <section class="settings-modal warnings-modal" role="dialog" aria-modal="true" aria-labelledby="warningsTitle">
        <header>
          <div>
            <h2 id="warningsTitle" style="color: var(--yellow);">Mismatched Tools Warning</h2>
            <p>Some required tools are mismatched or missing.</p>
          </div>
          <button class="icon-button" id="warningsCloseBtn" title="Close">×</button>
        </header>
        <div class="warnings-content" style="max-height: 300px; overflow-y: auto; padding: 0 16px; margin: 16px 0;">
          <ul id="warningsList" style="list-style: none; padding: 0; margin: 0; font-family: var(--font-mono); font-size: 13px; color: var(--text-muted); line-height: 1.5; display: flex; flex-direction: column; gap: 8px; word-break: break-all; white-space: pre-wrap;"></ul>
        </div>
        <div class="settings-actions">
          <button class="run-button" id="warningsOkBtn">OK</button>
        </div>
      </section>
    </div>
  </div>
`;

const els = {
  deviceList: document.querySelector("#deviceList"),
  testArea: document.querySelector("#testArea"),
  flowMapPanel: document.querySelector(".flow-map"),
  flowMap: document.querySelector("#flowMap"),
  clearTableBtn: document.querySelector("#clearTableBtn"),
  flowResizer: document.querySelector("#flowResizer"),
  logBox: document.querySelector("#logBox"),
  logFlowTabs: document.querySelector("#logFlowTabs"),
  logSubtabs: document.querySelector("#logSubtabs"),
  runBtn: document.querySelector("#runBtn"),
  cancelBtn: document.querySelector("#cancelBtn"),
  openResultBtn: document.querySelector("#openResultBtn"),
  resultPill: document.querySelector("#resultPill"),
  unselectBtn: document.querySelector("#unselectBtn"),
  resetBusyBtn: document.querySelector("#resetBusyBtn"),
  refreshBtn: document.querySelector("#refreshBtn"),
  settingsBtn: document.querySelector("#settingsBtn"),
  settingsModal: document.querySelector("#settingsModal"),
  settingsCloseBtn: document.querySelector("#settingsCloseBtn"),
  defaultRootBtn: document.querySelector("#defaultRootBtn"),
  preflightBtn: document.querySelector("#preflightBtn"),
  settingsSaveBtn: document.querySelector("#settingsSaveBtn"),
  browseBtn: document.querySelector("#browseBtn"),
  autoRootInput: document.querySelector("#autoRootInput"),
  wifiAutoConnectInput: document.querySelector("#wifiAutoConnectInput"),
  wifiSsidInput: document.querySelector("#wifiSsidInput"),
  wifiPasswordInput: document.querySelector("#wifiPasswordInput"),
  settingsOutput: document.querySelector("#settingsOutput"),
  retryInput: document.querySelector("#retryInput"),
  timeoutInput: document.querySelector("#timeoutInput"),
  wifiInput: document.querySelector("#wifiInput"),
  clearLogBtn: document.querySelector("#clearLogBtn"),
  statusLine: document.querySelector("#statusLine"),
  deviceFooter: document.querySelector("#deviceFooter"),
  activeMetric: document.querySelector("#activeMetric"),
  completedMetric: document.querySelector("#completedMetric"),
  passedMetric: document.querySelector("#passedMetric"),
  failedMetric: document.querySelector("#failedMetric"),
  runtimeMetric: document.querySelector("#runtimeMetric"),
  suiteList: document.querySelector("#suiteList"),
  clearSuitesBtn: document.querySelector("#clearSuitesBtn"),
  warningsModal: document.querySelector("#warningsModal"),
  warningsCloseBtn: document.querySelector("#warningsCloseBtn"),
  warningsOkBtn: document.querySelector("#warningsOkBtn"),
  warningsList: document.querySelector("#warningsList"),
};

els.retryInput.value = state.retryCount;
els.timeoutInput.value = state.timeoutSecs;
els.wifiInput.checked = state.wifi.enabled;

els.refreshBtn.addEventListener("click", refreshDevices);
els.resetBusyBtn.addEventListener("click", resetBusyState);
els.clearLogBtn.addEventListener("click", () => {
  clearLogView();
  renderLog();
});
els.clearTableBtn.addEventListener("click", clearInactiveTableCards);
els.clearSuitesBtn.addEventListener("click", clearInactiveSuites);
els.unselectBtn.addEventListener("click", toggleAllReadyDevices);
els.settingsBtn.addEventListener("click", openSettings);
els.settingsCloseBtn.addEventListener("click", closeSettings);
els.settingsModal.addEventListener("click", (event) => {
  if (event.target === els.settingsModal) closeSettings();
});
els.warningsCloseBtn.addEventListener("click", () => els.warningsModal.classList.add("hidden"));
els.warningsOkBtn.addEventListener("click", () => els.warningsModal.classList.add("hidden"));
els.warningsModal.addEventListener("click", (event) => {
  if (event.target === els.warningsModal) els.warningsModal.classList.add("hidden");
});
els.defaultRootBtn.addEventListener("click", loadDefaultRoot);
els.preflightBtn.addEventListener("click", runPreflight);
els.settingsSaveBtn.addEventListener("click", saveSettings);
els.browseBtn.addEventListener("click", browseRoot);
els.runBtn.addEventListener("click", runSelected);
els.cancelBtn.addEventListener("click", cancelRun);
els.openResultBtn.addEventListener("click", openResult);
els.resultPill.addEventListener("click", openResult);
els.flowResizer.addEventListener("pointerdown", startFlowResize);
  const flowMapHeader = document.getElementById("flowMapHeader");
  if (flowMapHeader) {
    flowMapHeader.addEventListener("click", (e) => {
      if (e.target.closest(".flow-map-actions") || e.target.closest("button")) return;
      toggleSection("flowMapSection");
    });
  }
  const runningLogHeader = document.getElementById("runningLogHeader");
  if (runningLogHeader) {
    runningLogHeader.addEventListener("click", () => {
      toggleSection("runningLogSection");
    });
  }
els.retryInput.addEventListener("change", saveInlineSettings);
els.timeoutInput.addEventListener("change", saveInlineSettings);
els.wifiInput.addEventListener("change", () => {
  state.wifi.enabled = els.wifiInput.checked;
  localStorage.setItem("autoWifiAutoConnect", String(state.wifi.enabled));
});
document.addEventListener("keydown", (event) => {
  if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === "b") {
    event.preventDefault();
    resetBusyState();
  }
});

listen("gba-run-log", (event) => appendLog(String(event.payload || "")));
listen("gba-suite-status", (event) => {
  const payload = event.payload || {};
  ensureFlow(payload.run_id, payload.test_type, payload.devices);
  state.suiteStatuses.set(`${payload.run_id || "legacy"}:${payload.suite}:${payload.devices || ""}`, payload);
  renderFlowMap();
  renderSuiteStatus();
  renderMetrics();
});
listen("gba-summary", (event) => {
  const payload = event.payload || {};
  ensureFlow(payload.run_id, payload.test_type, payload.devices);
  state.summaries.set(`${payload.run_id || "legacy"}:${payload.suite}:${payload.devices || ""}`, payload);
  appendRunnerLogForRun(payload.run_id || "legacy", `[runner] Summary ${payload.suite || "-"} ${payload.devices || "-"}: total=${payload.total ?? 0} pass=${payload.passed ?? 0} fail=${payload.failed ?? 0} runtime=${payload.run_time || "N/A"}`);
  renderFlowMap();
  renderSuiteStatus();
  renderMetrics();
});
listen("gba-laundry-result-update", (event) => {
  const payload = event.payload || {};
  if (!payload.id) return;
  const lockedKey = `${payload.run_id || "legacy"}:${payload.id}`;
  if (state.lockedLaundryResults.has(lockedKey)) {
    state.lockedLaundryResults.set(lockedKey, updateLaundryRow(state.lockedLaundryResults.get(lockedKey), payload));
  } else {
    state.laundryResults = state.laundryResults.map((row) => row.id === payload.id ? updateLaundryRow(row, payload) : row);
  }
  renderFlowMap();
});
listen("gba-run-finished", (event) => {
  const payload = event.payload || {};
  if (payload.run_id) {
    state.activeRuns.delete(payload.run_id);
    clearLocalBusy(state.runDevices.get(payload.run_id) || []);
    state.runDevices.delete(payload.run_id);
  }
  state.running = state.activeRuns.size > 0;
  state.resultDir = payload.result_dir || state.resultDir;
  els.runBtn.disabled = false;
  els.cancelBtn.disabled = state.activeRuns.size === 0;
  els.openResultBtn.disabled = !state.resultDir;
  els.resultPill.disabled = !state.resultDir;
  els.resultPill.textContent = state.resultDir ? "Open" : "None";
  els.statusLine.textContent = payload.exit_code === 0 ? "Completed" : "Finished with issue";
  appendRunSummaryToLog(payload.run_id || "legacy");
  appendRunnerLogForRun(payload.run_id || "legacy", `[runner] Finished exit=${payload.exit_code} result=${state.resultDir || "N/A"}`);
  if (Number(payload.exit_code) === 0) startConfettiLoop();
  renderFlowMap();
  renderMetrics();
});

listen("gba-tool-error", (event) => {
  const errorMsg = String(event.payload || "");
  els.warningsList.innerHTML = `<li>${escapeHtml(errorMsg)}</li>`;
  els.warningsModal.classList.remove("hidden");
});

init();

async function init() {
  render();
  try { confetti({ particleCount: 0 }); } catch (_) {}
  await reconcileAutoRoot();
  await refreshDevices();

  // Fade out splash screen
  const splash = document.getElementById("splash");
  if (splash) {
    splash.classList.add("fade-out");
  }
}

async function loadDefaultRoot(overwrite = true) {
  if (state.autoRoot && !overwrite) return;
  try {
    const root = await invoke("default_auto_root");
    state.autoRoot = root;
    localStorage.setItem("autoRoot", root);
    if (els.autoRootInput) els.autoRootInput.value = root;
  } catch (error) {
    appendLog(`[settings] Default root failed: ${error}`);
  }
}

async function reconcileAutoRoot() {
  try {
    const root = await invoke("default_auto_root");
    // ponytail: only initialize if empty to prevent overwriting user's custom saved path
    if (!state.autoRoot) {
      state.autoRoot = root;
      localStorage.setItem("autoRoot", root);
    }
    if (els.autoRootInput) els.autoRootInput.value = state.autoRoot;
  } catch (error) {
    appendLog(`[settings] Root reconcile failed: ${error}`);
  }
}

async function refreshDevices() {
  appendLog("[adb] Refreshing devices...");
  els.deviceFooter.textContent = "Scanning";
  try {
    state.devices = applyLocalBusyOverlay(await invoke("list_devices"));
    const ready = new Set(state.devices.filter((d) => d.state === "device" && !d.busy).map((d) => d.serial));
    state.selected = new Set([...state.selected].filter((serial) => ready.has(serial)));
    appendLog(`[adb] Found ${state.devices.length} device(s).`);
  } catch (error) {
    appendLog(`[adb] Refresh failed: ${error}`);
    state.devices = [];
  }
  renderDevices();
  els.deviceFooter.textContent = `${state.devices.length} detected`;
}

function applyLocalBusyOverlay(devices) {
  return devices.map((device) => {
    const reason = state.localBusy.get(device.serial);
    if (!reason) return device;
    return {
      ...device,
      busy: true,
      busy_reason: device.busy_reason || reason,
    };
  });
}

function render() {
  renderDevices();
  renderTestArea();
  renderFlowMap();
  renderLog();
  renderSuiteStatus();
  renderMetrics();
}

function renderDevices() {
  const readyDevices = state.devices.filter((device) => device.state === "device" && !device.busy);
  els.unselectBtn.textContent = state.selected.size === readyDevices.length && readyDevices.length ? "Unselect" : "Select";

  if (!state.devices.length) {
    els.deviceList.innerHTML = `<div class="empty">No ADB devices detected.</div>`;
    return;
  }

  els.deviceList.innerHTML = state.devices.map((device) => {
    const isBusy = Boolean(device.busy || device.busy_reason);
    const ready = device.state === "device" && !isBusy;
    const selected = state.selected.has(device.serial);
    const badge = isBusy ? "BUSY" : (device.is_userdebug ? "USERDEBUG" : "USER");
    const lampActive = state.lampStates.get(device.serial);
    return `
      <article class="device-card ${selected ? "selected" : ""} ${ready ? "" : "disabled"} ${isBusy ? "busy" : ""}" data-serial="${escapeHtml(device.serial)}" role="button" tabindex="${ready ? "0" : "-1"}">
        <div class="device-top">
          <span class="check-dot ${selected ? "checked" : ""} ${isBusy ? "loading" : ""}">${isBusy ? "" : (selected ? "✓" : "")}</span>
          <div>
            <strong>${escapeHtml(device.model || device.serial)}</strong>
            <p><b>${escapeHtml(device.serial)}</b> <span>${escapeHtml(device.state)}</span></p>
          </div>
          <div class="device-inline-actions">
            <span class="type-pill ${isBusy ? "busy" : (device.is_userdebug ? "debug" : "")}">${badge}</span>
            <button class="icon-mini lamp-button ${lampActive ? "active" : ""}" data-lamp="${escapeHtml(device.serial)}" ${device.state === "device" ? "" : "disabled"} title="Toggle lamp">🌩️</button>
            <button class="icon-mini scrcpy-button" data-scrcpy="${escapeHtml(device.serial)}" ${device.state === "device" ? "" : "disabled"} title="Open scrcpy">📱</button>
          </div>
        </div>
        <div class="device-actions">
          <span class="busy-note">${escapeHtml(isBusy ? device.busy_reason : "")}</span>
        </div>
        <div class="device-meta">
          <span><small>ANDROID</small>${escapeHtml(device.android || "-")}</span>
          <span><small>SPL</small>${escapeHtml(device.security_patch || "-")}</span>
          <span><small>IP</small>${escapeHtml(device.ip || "N/A")}</span>
          <span><small>PDA</small>${escapeHtml(device.pda || "-")}</span>
          <span><small>CP</small>${escapeHtml(device.cp || "-")}</span>
          <span><small>CSC</small>${escapeHtml(device.csc || device.sales_code || "-")}</span>
        </div>
      </article>
    `;
  }).join("");

  els.deviceList.querySelectorAll(".device-card").forEach((card) => {
    card.addEventListener("click", () => {
      if (card.classList.contains("disabled")) return;
      const serial = card.dataset.serial;
      if (state.selected.has(serial)) state.selected.delete(serial);
      else state.selected.add(serial);
      render();
    });
    card.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      card.click();
    });
  });
  els.deviceList.querySelectorAll("[data-lamp]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      await toggleLamp(button.dataset.lamp);
    });
  });
  els.deviceList.querySelectorAll("[data-scrcpy]").forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.stopPropagation();
      await openScrcpy(button.dataset.scrcpy);
    });
  });
}

function renderTestArea() {
  els.testArea.innerHTML = TEST_MODES.map((mode) => {
    const selected = state.selectedMode === mode.id;
    return `
      <button class="mode-card ${selected ? "selected" : ""}" data-mode="${escapeHtml(mode.id)}">
        <strong>${escapeHtml(mode.name)}</strong>
      </button>
    `;
  }).join("");

  els.testArea.querySelectorAll(".mode-card").forEach((card) => {
    card.addEventListener("click", async () => {
      state.selectedMode = card.dataset.mode;
      if (isLaundryMode(state.selectedMode)) {
        await chooseLaundryZip();
      }
      renderTestArea();
    });
  });
}

function renderLog() {
  renderLogTabs();
  const flow = state.logFlows.get(state.activeLogFlow);
  const tab = flow?.tabs?.get(state.activeLogSubtab);
  els.logBox.textContent = (tab?.lines || []).slice(-600).join("\n");
  els.logBox.scrollTop = els.logBox.scrollHeight;
}

function renderLogTabs() {
  if (!state.logFlows.size) {
    createBootLogFlow();
  }
  els.logFlowTabs.innerHTML = [...state.logFlows.values()].map((flow) => `
    <button class="log-tab ${flow.id === state.activeLogFlow ? "active" : ""}" data-flow="${escapeHtml(flow.id)}" title="${escapeHtml(flow.title)}">
      ${escapeHtml(flow.title)}
    </button>
  `).join("");
  els.logFlowTabs.querySelectorAll(".log-tab").forEach((button) => {
    button.addEventListener("click", () => {
      state.activeLogFlow = button.dataset.flow;
      const flow = state.logFlows.get(state.activeLogFlow);
      if (flow && !flow.tabs.has(state.activeLogSubtab)) state.activeLogSubtab = "Runner";
      renderLog();
    });
  });

  const flow = state.logFlows.get(state.activeLogFlow);
  const subtabs = flow ? [...flow.tabs.keys()] : [];
  els.logSubtabs.innerHTML = subtabs.map((name) => `
    <button class="log-subtab ${name === state.activeLogSubtab ? "active" : ""}" data-subtab="${escapeHtml(name)}">${escapeHtml(name)}</button>
  `).join("");
  els.logSubtabs.querySelectorAll(".log-subtab").forEach((button) => {
    button.addEventListener("click", () => {
      state.activeLogSubtab = button.dataset.subtab;
      renderLog();
    });
  });
}

function toggleSection(sectionId) {
  const section = document.getElementById(sectionId);
  if (!section) return;
  const icon = section.querySelector(".accordion-icon");
  
  if (section.classList.contains("expanded")) {
    section.classList.remove("expanded");
    section.classList.add("collapsed");
    if (icon) icon.textContent = "▶";
  } else {
    section.classList.remove("collapsed");
    section.classList.add("expanded");
    if (icon) icon.textContent = "▼";
  }
  updateLayoutResizerState();
}

function updateLayoutResizerState() {
  const flowMapSec = document.getElementById("flowMapSection");
  const runningLogSec = document.getElementById("runningLogSection");
  const resizer = document.getElementById("flowResizer");
  if (!flowMapSec || !runningLogSec || !resizer) return;
  
  const flowExpanded = flowMapSec.classList.contains("expanded");
  const logExpanded = runningLogSec.classList.contains("expanded");
  
  if (flowExpanded && logExpanded) {
    resizer.style.display = "block";
    const savedHeight = state.flowMapHeight || "220px";
    flowMapSec.style.flex = `0 0 ${savedHeight}`;
    flowMapSec.style.flexBasis = savedHeight;
    flowMapSec.style.height = savedHeight;
    runningLogSec.style.flex = "1 1 auto";
    runningLogSec.style.height = "auto";
  } else {
    resizer.style.display = "none";
    if (!flowExpanded) {
      flowMapSec.style.flex = "0 0 auto";
      flowMapSec.style.flexBasis = "auto";
      flowMapSec.style.height = "auto";
    } else {
      flowMapSec.style.flex = "1 1 auto";
      flowMapSec.style.flexBasis = "auto";
      flowMapSec.style.height = "auto";
    }
    if (!logExpanded) {
      runningLogSec.style.flex = "0 0 auto";
      runningLogSec.style.height = "auto";
    } else {
      runningLogSec.style.flex = "1 1 auto";
      runningLogSec.style.height = "auto";
    }
  }
}

function startFlowResize(event) {
  event.preventDefault();
  const startY = event.clientY;
  const startHeight = els.flowMapPanel.getBoundingClientRect().height;
  const workspaceHeight = document.querySelector(".workspace")?.getBoundingClientRect().height || window.innerHeight;
  const minHeight = 96;
  const maxHeight = Math.max(140, workspaceHeight - 260);
  els.flowResizer.setPointerCapture?.(event.pointerId);
  document.body.classList.add("resizing-flow");

  const onMove = (moveEvent) => {
    const next = Math.min(maxHeight, Math.max(minHeight, startHeight + moveEvent.clientY - startY));
    const rounded = Math.round(next);
    els.flowMapPanel.style.height = `${rounded}px`;
    els.flowMapPanel.style.flexBasis = `${rounded}px`;
    state.flowMapHeight = `${rounded}px`;
  };
  const onUp = () => {
    document.body.classList.remove("resizing-flow");
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
  };

  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function renderFlowMap() {
  if (!isLaundryMode(state.selectedMode)) {
    const mode = TEST_MODES.find((item) => item.id === state.selectedMode);
    els.flowMap.innerHTML = `
      <div class="laundry-table-empty">
        <strong>${escapeHtml(mode?.name || "Flow")}</strong>
        <span>${escapeHtml(mode?.flow || "Select devices and run")}</span>
      </div>
    `;
    return;
  }

  const lockedRows = [...state.lockedLaundryResults.values()]
    .filter((row) => isLaundryMode(row.mode))
    .sort((a, b) => Number(state.activeRuns.has(b.runId)) - Number(state.activeRuns.has(a.runId)));
  if (!state.laundryZipPath && !state.laundryResults.length && !lockedRows.length) {
    els.flowMap.innerHTML = `
      <div class="laundry-table-empty">
        <strong>${escapeHtml(state.selectedMode)}</strong>
        <span>Pick laundry zip to preview CTS, GTS, and STS results.</span>
      </div>
    `;
    return;
  }

  // Count elements for tabs
  const runningRows = lockedRows.filter((row) => state.activeRuns.has(row.runId));
  const completedRows = lockedRows.filter((row) => !state.activeRuns.has(row.runId));

  const runningRunIds = [...new Set(runningRows.map((r) => r.runId))];
  const completedRunIds = [...new Set(completedRows.map((r) => r.runId))];

  const runningCount = runningRunIds.length;
  const completedCount = completedRunIds.length;

  if (!state.runTableTab) {
    state.runTableTab = "preview";
  }

  const warningCount = state.laundryWarnings ? state.laundryWarnings.length : 0;

  const tabsHtml = `
    <div class="run-table-tabs">
      <button class="run-tab-btn ${state.runTableTab === 'preview' ? 'active' : ''}" data-tab="preview">
        Preview ${state.laundryZipPath ? '✓' : ''}
      </button>
      <button class="run-tab-btn ${state.runTableTab === 'running' ? 'active' : ''}" data-tab="running">
        Running (${runningCount})
      </button>
      <button class="run-tab-btn ${state.runTableTab === 'completed' ? 'active' : ''}" data-tab="completed">
        Completed (${completedCount})
      </button>
      <button class="run-tab-btn ${state.runTableTab === 'warning' ? 'active' : ''}" data-tab="warning">
        Warning (${warningCount})
      </button>
    </div>
  `;

  let contentHtml = "";
  if (state.runTableTab === "preview") {
    if (state.laundryResults.length) {
      const selectedCount = state.selectedLaundryResults.size;
      contentHtml = renderLaundryTableCard({
        title: `${state.selectedMode} · Picked Zip`,
        subtitle: `${selectedCount}/${state.laundryResults.length} selected · ${fileName(state.laundryZipPath)}`,
        rows: state.laundryResults.map((row) => ({ ...row, locked: false, running: false })),
      });
    } else {
      contentHtml = `<div class="empty">No preview zip loaded. Select a laundry mode action to browse and load a zip.</div>`;
    }
  } else if (state.runTableTab === "running") {
    if (runningCount > 0) {
      contentHtml = `<div class="laundry-table-stack">${renderLaundryTableCards(runningRows)}</div>`;
    } else {
      contentHtml = `<div class="empty">No active running flows.</div>`;
    }
  } else if (state.runTableTab === "completed") {
    if (completedCount > 0) {
      contentHtml = `<div class="laundry-table-stack">${renderLaundryTableCards(completedRows)}</div>`;
    } else {
      contentHtml = `<div class="empty">No completed flows.</div>`;
    }
  } else if (state.runTableTab === "warning") {
    if (warningCount > 0) {
      contentHtml = `
        <div class="warning-container">
          ${state.laundryWarnings.map((warn) => `
            <div class="warning-card">
              <div class="warning-title">
                <span class="warning-icon-alert">⚠️</span>
                <strong>Mismatched Tool Version</strong>
              </div>
              <pre class="warning-desc">${escapeHtml(warn)}</pre>
            </div>
          `).join("")}
        </div>
      `;
    } else {
      contentHtml = `<div class="empty no-warnings">No tool version mismatches detected. Local tools match the laundry zip versions.</div>`;
    }
  }

  els.flowMap.innerHTML = `
    <div class="run-table-container">
      ${tabsHtml}
      <div class="run-table-content-area">
        ${contentHtml}
      </div>
    </div>
  `;

  // Bind tab buttons
  els.flowMap.querySelectorAll(".run-tab-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.runTableTab = btn.dataset.tab;
      renderFlowMap();
    });
  });

  els.flowMap.querySelectorAll(".laundry-result-check").forEach((input) => {
    input.addEventListener("change", () => {
      if (input.checked) state.selectedLaundryResults.add(input.dataset.id);
      else state.selectedLaundryResults.delete(input.dataset.id);
      renderFlowMap();
    });
  });
}

function renderLaundryTableCards(rows) {
  const byRun = new Map();
  rows.forEach((row) => {
    if (!byRun.has(row.runId)) byRun.set(row.runId, []);
    byRun.get(row.runId).push(row);
  });
  return [...byRun.entries()].map(([runId, runRows]) => {
    const flow = state.flows.get(runId) || { mode: runRows[0]?.mode || "Laundry Run", devices: runRows[0]?.devices || "" };
    const deviceModel = getDeviceModelsForFlow(flow);
    const active = state.activeRuns.has(runId);
    return renderLaundryTableCard({
      title: `${flow.mode || "Laundry Run"} Model ${deviceModel}`,
      subtitle: `${active ? "Running" : "Done"} · ${flow.devices || "-"} · ${flow.flow || ""}`,
      rows: runRows,
      locked: true,
      active,
    });
  }).join("");
}

function renderLaundryTableCard({ title, subtitle, rows, locked = false, active = false }) {
  const body = rows.map((row) => {
    const locked = Boolean(row.locked);
    const running = locked && state.activeRuns.has(row.runId);
    const checked = locked || state.selectedLaundryResults.has(row.id);
    const status = laundryRowStatus(row, checked);
    return `
      <tr class="${checked ? "selected" : "skipped"} ${locked ? "locked" : ""} ${running ? "running" : ""}">
        <td class="select-cell">
          <input class="laundry-result-check" type="checkbox" data-id="${escapeHtml(row.id)}" ${checked ? "checked" : ""} ${locked ? "disabled" : ""} />
        </td>
        <td>
          <strong>${escapeHtml(row.testcase || row.suite)}</strong>
          <span>${escapeHtml(row.suite_version || "-")} · ${escapeHtml(row.locked ? `${row.devices || "-"} · ${row.result_dir || "-"}` : row.result_dir || "-")}</span>
        </td>
        <td class="subtest-cell">${escapeHtml(row.subtestcases || "-")}</td>
        <td><span class="status-pill status-${statusClass(status)}">${escapeHtml(status)}</span></td>
        <td class="time-cell">${escapeHtml(row.time || "-")}</td>
        <td class="result-cell">
          <span>Total ${Number(row.total || 0)}</span>
          <b class="pass">Pass ${Number(row.passed || 0)}</b>
          <b class="fail">Fail ${Number(row.failed || 0)}</b>
        </td>
      </tr>
    `;
  }).join("");

  const completedSuites = rows.filter((row) => row.status && ["Test Done", "Cancelled", "Failed", "Timeout", "Completed"].includes(row.status)).length;
  const totalSuites = rows.length;
  const pct = totalSuites > 0 ? (completedSuites / totalSuites) : 0;

  const totalTestcases = rows.reduce((sum, row) => sum + (Number(row.total) || 0), 0);
  const passedTestcases = rows.reduce((sum, row) => sum + (Number(row.passed) || 0), 0);
  const failedTestcases = rows.reduce((sum, row) => sum + (Number(row.failed) || 0), 0);
  const testedTestcases = passedTestcases + failedTestcases;

  const progressHtml = locked ? `
    <div class="flow-suite-progress">
      <svg class="progress-ring" width="14" height="14">
        <circle class="progress-ring__bg" stroke="rgba(255,255,255,0.1)" stroke-width="2" fill="transparent" r="5" cx="7" cy="7"/>
        <circle class="progress-ring__circle" stroke="${pct === 1 ? 'var(--green)' : 'var(--cyan)'}" stroke-width="2" stroke-dasharray="31.4" stroke-dashoffset="${31.4 * (1 - pct)}" fill="transparent" r="5" cx="7" cy="7" stroke-linecap="round" transform="rotate(-90 7 7)"/>
      </svg>
      <span class="flow-suite-count" style="color: ${pct === 1 ? 'var(--green)' : 'var(--cyan)'}">${completedSuites}/${totalSuites} suite (${testedTestcases}/${totalTestcases} testcases)</span>
    </div>
  ` : "";

  return `
    <section class="laundry-table-card ${locked ? "locked" : ""} ${active ? "active" : ""}">
      <header class="laundry-table-head">
        <div class="laundry-table-head-left">
          <strong>${escapeHtml(title)}</strong>
          <span>${escapeHtml(subtitle)}</span>
        </div>
        ${progressHtml}
      </header>
      <div class="laundry-table-wrap">
        <table class="laundry-table">
          <thead>
            <tr>
              <th>Select</th>
              <th>Testcase</th>
              <th>Subtestcases</th>
              <th>Status</th>
              <th>Time</th>
              <th>Results</th>
            </tr>
          </thead>
          <tbody>${body}</tbody>
        </table>
      </div>
    </section>
  `;
}

function laundryRowStatus(row, checked) {
  if (!checked) return "Skipped";
  if (row.status && row.status !== "Ready") return row.status;
  const matches = [...state.suiteStatuses.values()].filter((status) => {
    const flow = state.flows.get(status.run_id || "legacy");
    return flow?.mode === state.selectedMode && status.suite === row.suite;
  });
  if (matches.some((status) => ["Failed", "Timeout", "Cancelled"].includes(status.status))) {
    return matches.find((status) => ["Failed", "Timeout", "Cancelled"].includes(status.status))?.status || "Failed";
  }
  if (matches.some((status) => status.status === "Test Done" || status.status === "Completed")) {
    return "Test Done";
  }
  if (matches.some((status) => ["Starting", "Running", "Copying result", "Waiting device reconnect"].includes(status.status))) {
    return matches.find((item) => ["Starting", "Running", "Copying result", "Waiting device reconnect"].includes(item.status))?.status || "Running";
  }
  return "Ready";
}

function updateLaundryRow(row, payload) {
  return {
    ...row,
    status: payload.status || row.status,
    time: payload.time || row.time,
    total: Number(payload.total ?? row.total ?? 0),
    passed: Number(payload.passed ?? row.passed ?? 0),
    failed: Number(payload.failed ?? row.failed ?? 0),
  };
}

function renderSuiteStatus() {
  const statuses = [...state.suiteStatuses.values()];
  if (!statuses.length) {
    els.suiteList.innerHTML = `<div class="empty">No active suite.</div>`;
    return;
  }
  const byRun = new Map();
  statuses.forEach((status) => {
    const runId = status.run_id || "legacy";
    if (!byRun.has(runId)) byRun.set(runId, []);
    byRun.get(runId).push(status);
  });

  els.suiteList.innerHTML = [...byRun.entries()].map(([runId, runStatuses]) => {
    const flow = state.flows.get(runId) || { mode: runStatuses[0]?.test_type || "Run", flow: "", devices: "" };
    const sortedStatuses = [...runStatuses].sort(compareSuiteStatuses);
    const deviceModel = getDeviceModelsForFlow(flow);
    const completedSuites = sortedStatuses.filter((s) => ["Test Done", "Cancelled", "Failed", "Timeout", "Completed"].includes(s.status)).length;
    const pct = sortedStatuses.length > 0 ? (completedSuites / sortedStatuses.length) : 0;

    let totalTestcases = 0;
    let passedTestcases = 0;
    let failedTestcases = 0;
    sortedStatuses.forEach((status) => {
      const key = `${status.run_id || "legacy"}:${status.suite}:${status.devices || ""}`;
      const summary = state.summaries.get(key);
      if (summary) {
        totalTestcases += Number(summary.total || 0);
        passedTestcases += Number(summary.passed || 0);
        failedTestcases += Number(summary.failed || 0);
      }
    });
    const testedTestcases = passedTestcases + failedTestcases;

    const rows = sortedStatuses.map((status) => {
      const key = `${status.run_id || "legacy"}:${status.suite}:${status.devices || ""}`;
      const summary = state.summaries.get(key);
      const failed = summary?.failed ?? "-";
      const passed = summary?.passed ?? "-";
      return `
        <div class="suite-row">
          <div>
            <strong>${escapeHtml(status.suite || "-")}</strong>
            <p>${escapeHtml(status.devices || "-")}</p>
          </div>
          <div class="suite-state">
            <span class="status-${statusClass(status.status)}">${escapeHtml(status.status || "Standby")}</span>
            <em>${formatDuration(Number(status.elapsed_secs || 0))}</em>
          </div>
          <div class="suite-badges">
            <span class="count-badge pass">${passed}</span>
            <span class="count-badge fail">${failed}</span>
          </div>
        </div>
      `;
    }).join("");
    return `
      <div class="flow-card">
        <header>
          <div class="flow-card-header-content">
            <strong>${escapeHtml(flow.mode)} Model ${escapeHtml(deviceModel)}</strong>
            <p class="flow-flow-text">${escapeHtml(flow.flow)}</p>
            <div class="flow-suite-progress">
              <svg class="progress-ring" width="14" height="14">
                <circle class="progress-ring__bg" stroke="rgba(255,255,255,0.1)" stroke-width="2" fill="transparent" r="5" cx="7" cy="7"/>
                <circle class="progress-ring__circle" stroke="${pct === 1 ? 'var(--green)' : 'var(--cyan)'}" stroke-width="2" stroke-dasharray="31.4" stroke-dashoffset="${31.4 * (1 - pct)}" fill="transparent" r="5" cx="7" cy="7" stroke-linecap="round" transform="rotate(-90 7 7)"/>
              </svg>
              <span class="flow-suite-count" style="color: ${pct === 1 ? 'var(--green)' : 'var(--cyan)'}">${completedSuites}/${sortedStatuses.length} suite (${testedTestcases}/${totalTestcases} testcases)</span>
            </div>
          </div>
        </header>
        <div class="flow-suite-list">${rows}</div>
      </div>
    `;
  }).join("");
}

function compareSuiteStatuses(a, b) {
  const order = { CTS: 0, GTS: 1, STS: 2, ALL: 3 };
  const suiteA = order[String(a.suite || "").toUpperCase()] ?? 99;
  const suiteB = order[String(b.suite || "").toUpperCase()] ?? 99;
  if (suiteA !== suiteB) return suiteA - suiteB;
  return String(a.devices || "").localeCompare(String(b.devices || ""));
}

function renderMetrics() {
  const statuses = [...state.suiteStatuses.values()];
  const summaries = [...state.summaries.values()];
  const active = statuses.filter((s) => !["Test Done", "Cancelled", "Failed", "Timeout"].includes(s.status)).length;
  const completed = statuses.filter((s) => ["Test Done", "Failed", "Timeout"].includes(s.status)).length;
  const passed = summaries.reduce((sum, s) => sum + Number(s.passed || 0), 0);
  const failed = summaries.reduce((sum, s) => sum + Number(s.failed || 0), 0);
  els.activeMetric.textContent = String(active);
  els.completedMetric.textContent = String(completed);
  els.passedMetric.textContent = String(passed);
  els.failedMetric.textContent = String(failed);
  els.runtimeMetric.textContent = state.runStartedAt ? formatDuration(Math.floor((Date.now() - state.runStartedAt) / 1000)) : "00:00:00";
}

function ensureFlow(runId, testType, devices) {
  if (!runId || state.flows.has(runId)) return;
  const mode = TEST_MODES.find((item) => item.id === testType);
  const serials = (devices || "").split(",").filter(Boolean);
  const models = serials.map(serial => {
    const dev = state.devices.find(d => d.serial === serial);
    return dev ? (dev.model || serial) : serial;
  });
  const groupModels = [...new Set(models)].join(", ");

  state.flows.set(runId, {
    mode: testType || "Run",
    flow: mode?.flow || "",
    devices: devices || "",
    model: groupModels || "Unknown",
  });
}

function getDeviceModelsForFlow(flow) {
  if (!flow) return "Unknown";
  if (flow.model) return flow.model;
  if (!flow.devices) return "Unknown";
  const serials = flow.devices.split(",").filter(Boolean);
  const models = serials.map(serial => {
    const dev = state.devices.find(d => d.serial === serial);
    return dev ? (dev.model || serial) : serial;
  });
  const unique = [...new Set(models)];
  return unique.join(", ") || "Unknown";
}

function toggleAllReadyDevices() {
  const ready = state.devices.filter((device) => device.state === "device" && !device.busy).map((device) => device.serial);
  if (ready.length && state.selected.size === ready.length) state.selected.clear();
  else state.selected = new Set(ready);
  render();
}

function shardSelectedDevices(devices, mode) {
  if (mode?.id === "Laundry SMR") return shardLaundrySmrDevices(devices);
  if (mode?.id === "SMR") return shardSmrDevices(devices);
  const groups = new Map();
  devices.forEach((device) => {
    if (mode?.needs === "user" && device.is_userdebug) return;
    if (mode?.needs === "userdebug" && !device.is_userdebug) return;
    const fingerprint = fingerprintKey(device);
    const kind = device.is_userdebug ? "USERDEBUG" : "USER";
    const key = `${kind}::${fingerprint}`;
    if (!groups.has(key)) groups.set(key, { kind, fingerprint, devices: [] });
    groups.get(key).devices.push(device);
  });
  return [...groups.values()];
}

function shardLaundrySmrDevices(devices) {
  const groups = new Map();
  
  let hasCtsOrGts = false;
  let hasSts = false;
  if (state.selectedLaundryResults && state.selectedLaundryResults.size > 0) {
    const selectedRows = state.laundryResults.filter(row => state.selectedLaundryResults.has(row.id));
    hasCtsOrGts = selectedRows.some(row => {
      const t = (row.testcase || "").toUpperCase();
      return t.includes("CTS") || t.includes("GTS") || t.includes("COMPATIBILITY") || t.includes("GOOGLE");
    });
    hasSts = selectedRows.some(row => {
      const t = (row.testcase || "").toUpperCase();
      return t.includes("STS") || t.includes("SECURITY");
    });
  }

  devices.forEach((device) => {
    const family = fingerprintFamilyKey(device);
    const model = modelKey(device);
    const key = `${model}::${family}`;
    if (!groups.has(key)) groups.set(key, { kind: "", fingerprint: family, model, devices: [] });
    groups.get(key).devices.push(device);
  });
  
  return [...groups.values()].filter((group) => {
    const hasUser = group.devices.some((device) => !device.is_userdebug);
    const hasUserdebug = group.devices.some((device) => device.is_userdebug);
    
    let valid = false;
    if (hasCtsOrGts && hasSts) {
      valid = hasUser && hasUserdebug;
    } else if (hasCtsOrGts) {
      valid = hasUser;
    } else if (hasSts) {
      valid = hasUserdebug;
    } else {
      valid = hasUser || hasUserdebug;
    }
    
    if (valid) {
      if (hasUser && hasUserdebug) group.kind = "USER+USERDEBUG";
      else if (hasUser) group.kind = "USER";
      else group.kind = "USERDEBUG";
    }
  });
}

function shardSmrDevices(devices) {
  const groups = new Map();
  devices.forEach((device) => {
    const family = fingerprintFamilyKey(device);
    const model = modelKey(device);
    const key = `${model}::${family}`;
    if (!groups.has(key)) groups.set(key, { kind: "", fingerprint: family, model, devices: [] });
    groups.get(key).devices.push(device);
  });
  return [...groups.values()].map((group) => {
    const hasUser = group.devices.some((device) => !device.is_userdebug);
    const hasUserdebug = group.devices.some((device) => device.is_userdebug);
    if (hasUser && hasUserdebug) {
      group.kind = "USER+USERDEBUG";
    } else if (hasUser) {
      group.kind = "USER";
    } else {
      group.kind = "USERDEBUG";
    }
    return group;
  });
}

function fingerprintKey(device) {
  return (device.fingerprint || `${device.model || "UNKNOWN"}:${device.pda || "UNKNOWN"}:${device.android || "UNKNOWN"}`).trim();
}

function fingerprintFamilyKey(device) {
  const fingerprint = String(device.fingerprint || "").trim();
  const [productPart] = fingerprint.split(":");
  const pieces = productPart.split("/").filter(Boolean);
  if (pieces.length >= 3) return pieces.slice(0, 3).join("/");
  return modelKey(device);
}

function modelKey(device) {
  return String(device.model || "UNKNOWN_MODEL").trim().toUpperCase();
}

function shortFingerprint(value) {
  const text = String(value || "UNKNOWN_FP");
  const parts = text.split("/");
  const tail = parts.length > 1 ? parts.slice(-2).join("/") : text;
  return tail.length > 48 ? `${tail.slice(0, 45)}...` : tail;
}

function isLaundryMode(modeName) {
  return modeName === "Laundry SMR" || modeName === "Laundry Normal";
}

async function chooseLaundryZip() {
  const selected = await open({
    directory: false,
    multiple: false,
    filters: [{ name: "Laundry zip", extensions: ["zip"] }],
  });
  const path = normalizeDialogPath(selected);
  if (path) {
    state.laundryZipPath = path;
    state.laundryResults = [];
    state.selectedLaundryResults = new Set();
    state.laundryWarnings = [];
    appendLog(`[runner] Laundry zip selected: ${path}`);
    renderFlowMap();
    try {
      const rows = await invoke("analyze_laundry_zip", { zipPath: path });
      state.laundryResults = (Array.isArray(rows) ? rows : []).filter((row) => !isCtsVerifierRow(row));
      state.selectedLaundryResults = new Set(state.laundryResults.map((row) => row.id));
      appendLog(`[runner] Laundry zip scanned: ${state.laundryResults.length} result(s).`);
      
      try {
        const autoRoot = state.autoRoot || await invoke("default_auto_root");
        const warnings = await invoke("check_laundry_mismatches", { autoRoot, zipPath: path });
        state.laundryWarnings = Array.isArray(warnings) ? warnings : [];
        if (state.laundryWarnings.length > 0) {
          appendLog(`[runner] Mismatched tools check: Found ${state.laundryWarnings.length} warning(s).`);
          els.warningsList.innerHTML = state.laundryWarnings.map(w => `<li>${escapeHtml(w)}</li>`).join("");
          els.warningsModal.classList.remove("hidden");
        }
      } catch (err) {
        appendLog(`[runner] Tool version mismatch check failed: ${err}`);
      }

      renderFlowMap();
    } catch (error) {
      appendLog(`[runner] Laundry zip scan failed: ${error}`);
      els.statusLine.textContent = "Laundry zip scan failed";
      renderFlowMap();
    }
  }
}

async function runSelected() {
  saveInlineSettings();
  const mode = TEST_MODES.find((item) => item.id === state.selectedMode);
  const runMode = state.selectedMode;
  const runFlow = currentModeFlow();
  const selectedDevices = state.devices.filter((device) => state.selected.has(device.serial));
  const userDevices = selectedDevices.filter((device) => !device.is_userdebug).map((device) => device.serial);
  const userdebugDevices = selectedDevices.filter((device) => device.is_userdebug).map((device) => device.serial);
  const validation = validateRun(mode, userDevices, userdebugDevices);
  if (validation) {
    appendLog(`[runner] ${validation}`);
    els.statusLine.textContent = validation;
    return;
  }
  if (isLaundryMode(runMode) && !state.laundryZipPath) {
    await chooseLaundryZip();
    if (state.laundryZipPath) {
      appendLog("[runner] Laundry zip loaded. Select testcase rows, then click Run Selected again.");
      els.statusLine.textContent = "Select laundry testcase rows";
      return;
    }
    if (!state.laundryZipPath) {
      appendLog("[runner] Laundry flow needs zip file.");
      els.statusLine.textContent = "Laundry zip required";
      return;
    }
  }
  if (isLaundryMode(runMode) && state.laundryResults.length && state.selectedLaundryResults.size === 0) {
    appendLog("[runner] Select at least one laundry result row.");
    els.statusLine.textContent = "Laundry result selection required";
    return;
  }

  const autoRoot = state.autoRoot || await invoke("default_auto_root");
  const groups = shardSelectedDevices(selectedDevices, mode);
  if (!groups.length) {
    appendLog("[runner] No valid device group for selected mode.");
    return;
  }
  const runnableDevices = groups.flatMap((group) => group.devices);
  const laundryZipPathForRun = state.laundryZipPath;
  const selectedLaundryResultsForRun = [...state.selectedLaundryResults];

  state.running = true;
  state.resultDir = "";
  state.runStartedAt = Date.now();
  markLocalBusy(runnableDevices, runMode);
  state.selected.clear();
  els.cancelBtn.disabled = false;
  els.openResultBtn.disabled = true;
  els.resultPill.disabled = true;
  els.resultPill.textContent = "None";
  els.statusLine.textContent = "Running";
  render();

  appendLog(`[runner] ${runMode}: split into ${groups.length} fingerprint shard group(s).`);
  for (const [index, group] of groups.entries()) {
    const groupDevices = group.devices;
    const groupSerials = groupDevices.map((device) => device.serial);
    const groupUserDevices = groupDevices.filter((device) => !device.is_userdebug).map((device) => device.serial);
    const groupUserdebugDevices = groupDevices.filter((device) => device.is_userdebug).map((device) => device.serial);
    const runId = `${runMode.replaceAll(/\W+/g, "_")}_${Date.now()}_${index + 1}`;
    const flowTitle = `${runFlow} | ${group.kind} | ${shortFingerprint(group.fingerprint)}`;

    const groupModels = [...new Set(groupDevices.map(d => d.model || "Unknown"))].join(", ");
    state.activeRuns.add(runId);
    state.runDevices.set(runId, groupSerials);
    state.flows.set(runId, {
      mode: runMode,
      flow: flowTitle,
      devices: groupSerials.join(","),
      model: groupModels || "Unknown",
    });
    if (isLaundryMode(runMode)) lockLaundryRowsForRun(runId, runMode, groupSerials);
    createRunLogFlow(runId, runMode, groupDevices);
    appendLog(`[runner] Starting shard ${index + 1}/${groups.length}: ${group.kind} fingerprint=${shortFingerprint(group.fingerprint)} devices=${groupSerials.join(",")}`);
    renderFlowMap();

    try {
      await invoke("run_suite", {
        request: {
          run_id: runId,
          auto_root: autoRoot,
          test_type: runMode,
          laundry_zip_path: isLaundryMode(runMode) ? laundryZipPathForRun : null,
          selected_laundry_results: isLaundryMode(runMode) ? selectedLaundryResultsForRun : [],
          user_devices: groupUserDevices,
          userdebug_devices: groupUserdebugDevices,
          retry_count: state.retryCount,
          wifi_enabled: state.wifi.enabled,
          wifi_ssid: state.wifi.ssid,
          wifi_password: state.wifi.password,
          timeout_secs: state.timeoutSecs,
        },
      });
    } catch (error) {
      state.activeRuns.delete(runId);
      clearLocalBusy(groupSerials);
      const errorMsg = String(error);
      appendLog(`[runner] Run failed for ${groupSerials.join(",")}: ${errorMsg}`);
      if (errorMsg.includes("not found")) {
        els.warningsList.innerHTML = `<li>${escapeHtml(errorMsg)}</li>`;
        els.warningsModal.classList.remove("hidden");
      }
    }
  }

  state.running = state.activeRuns.size > 0;
  els.cancelBtn.disabled = state.activeRuns.size === 0;
  els.statusLine.textContent = state.running ? "Running" : "Run failed";
  render();
  if (!state.running) await refreshDevices();
}

function markLocalBusy(devices, testType) {
  const stamp = new Date().toLocaleString("en-GB", { hour12: false });
  const serials = new Set(devices.map((device) => device.serial));
  state.devices = state.devices.map((device) => {
    if (!serials.has(device.serial)) return device;
    const reason = `${testType} ${stamp}`;
    state.localBusy.set(device.serial, reason);
    return {
      ...device,
      busy: true,
      busy_reason: reason,
    };
  });
}

function lockLaundryRowsForRun(runId, mode, serials) {
  const selected = new Set(state.selectedLaundryResults);
  state.laundryResults
    .filter((row) => selected.has(row.id))
    .forEach((row) => {
      state.lockedLaundryResults.set(`${runId}:${row.id}`, {
        ...row,
        runId,
        mode,
        devices: serials.join(","),
        locked: true,
        status: "Queued",
      });
    });
  state.laundryResults = [];
  state.selectedLaundryResults = new Set();
  state.laundryZipPath = "";
}

function isCtsVerifierRow(row) {
  const text = [
    row?.testcase,
    row?.result_dir,
    row?.subtestcases,
    row?.id,
  ].join(" ").toUpperCase();
  return text.includes("CTS_VERIFIER") || text.includes("CTSV");
}

function clearLocalBusy(serials) {
  const set = new Set(serials);
  state.devices = state.devices.map((device) => {
    if (!set.has(device.serial)) return device;
    state.localBusy.delete(device.serial);
    return {
      ...device,
      busy: false,
      busy_reason: "",
    };
  });
  renderDevices();
}

async function cancelRun() {
  appendLog("[runner] Cancel requested.");
  els.cancelBtn.disabled = true;
  els.statusLine.textContent = "Cancelling";
  try {
    await invoke("cancel_run");
    await refreshDevices();
  } catch (error) {
    appendLog(`[runner] Cancel failed: ${error}`);
    els.cancelBtn.disabled = false;
  }
}

async function resetBusyState() {
  appendLog("[runner] Reset busy state requested (Ctrl+Shift+B).");
  try {
    await invoke("reset_busy_state", { autoRoot: state.autoRoot || null });
    state.selected.clear();
    state.localBusy.clear();
    state.runDevices.clear();
    appendLog("[runner] busy.json reset.");
    await refreshDevices();
  } catch (error) {
    appendLog(`[runner] Reset busy state failed: ${error}`);
  }
}

async function openResult() {
  if (!state.resultDir) return;
  try {
    await invoke("open_result", { path: state.resultDir });
  } catch (error) {
    appendLog(`[runner] Open result failed: ${error}`);
  }
}

async function toggleLamp(serial) {
  if (!serial) return;
  const brighten = !state.lampStates.get(serial);
  appendLog(`[runner] Lamp ${serial}: ${brighten ? "max brightness" : "low brightness"}`);
  try {
    await invoke("set_device_lamp", { serial, brighten });
    state.lampStates.set(serial, brighten);
    renderDevices();
  } catch (error) {
    appendLog(`[runner] Lamp failed for ${serial}: ${error}`);
  }
}

async function openScrcpy(serial) {
  if (!serial) return;
  appendLog(`[runner] Opening scrcpy for ${serial}`);
  try {
    await invoke("open_scrcpy", { serial });
  } catch (error) {
    appendLog(`[runner] scrcpy failed for ${serial}: ${error}`);
  }
}

function openSettings() {
  els.autoRootInput.value = state.autoRoot;
  els.wifiAutoConnectInput.checked = state.wifi.enabled;
  els.wifiSsidInput.value = state.wifi.ssid;
  els.wifiPasswordInput.value = state.wifi.password;
  els.settingsOutput.textContent = "";
  els.settingsModal.classList.remove("hidden");
}

function closeSettings() {
  els.settingsModal.classList.add("hidden");
}

async function browseRoot() {
  const selected = await open({ directory: true, multiple: false, defaultPath: state.autoRoot || undefined });
  const path = normalizeDialogPath(selected);
  if (path) els.autoRootInput.value = path;
}

async function runPreflight() {
  saveSettings(false);
  els.settingsOutput.textContent = "Checking...\n";
  try {
    const lines = await invoke("preflight", { autoRoot: state.autoRoot || null });
    els.settingsOutput.textContent = lines.join("\n");
  } catch (error) {
    els.settingsOutput.textContent = String(error);
  }
}

function saveSettings(close = true) {
  state.autoRoot = els.autoRootInput.value.trim();
  state.wifi.enabled = els.wifiAutoConnectInput.checked;
  state.wifi.ssid = els.wifiSsidInput.value;
  state.wifi.password = els.wifiPasswordInput.value;
  localStorage.setItem("autoRoot", state.autoRoot);
  localStorage.setItem("autoWifiAutoConnect", String(state.wifi.enabled));
  localStorage.setItem("autoWifiSsid", state.wifi.ssid);
  localStorage.setItem("autoWifiPassword", state.wifi.password);
  els.wifiInput.checked = state.wifi.enabled;
  appendLog("[settings] Saved.");
  if (close) closeSettings();
}

function saveInlineSettings() {
  state.retryCount = Math.max(0, Number(els.retryInput.value || 0));
  state.timeoutSecs = Math.max(60, Number(els.timeoutInput.value || 86400));
  localStorage.setItem("autoRetryCount", String(state.retryCount));
  localStorage.setItem("autoTestTimeout", String(state.timeoutSecs));
}

function validateRun(mode, userDevices, userdebugDevices) {
  if (!mode) return "No test mode selected.";
  const busySelected = state.devices.filter((device) => state.selected.has(device.serial) && device.busy);
  if (busySelected.length) return `Device busy: ${busySelected.map((device) => device.serial).join(", ")}`;
  if (mode.needs === "user" && userDevices.length === 0) return `${mode.name} needs at least one non-userdebug device.`;
  if (mode.needs === "userdebug" && userdebugDevices.length === 0) return `${mode.name} needs at least one userdebug device.`;
  if (mode.id === "Laundry SMR") {
    const laundryValidation = validateLaundrySmrSelection();
    if (laundryValidation) return laundryValidation;
  }
  if (mode.needs === "both" && userDevices.length === 0 && userdebugDevices.length === 0) return `${mode.name} needs at least one runnable device.`;
  if (!state.autoRoot) return "AUTO root is empty. Open settings and save root.";
  return "";
}

function validateLaundrySmrSelection() {
  const selectedDevices = state.devices.filter((device) => state.selected.has(device.serial));
  const hasUser = selectedDevices.some((device) => !device.is_userdebug);
  const hasUserdebug = selectedDevices.some((device) => device.is_userdebug);
  
  if (state.selectedLaundryResults && state.selectedLaundryResults.size > 0) {
    const selectedRows = state.laundryResults.filter(row => state.selectedLaundryResults.has(row.id));
    const hasCtsOrGts = selectedRows.some(row => {
      const t = (row.testcase || "").toUpperCase();
      return t.includes("CTS") || t.includes("GTS") || t.includes("COMPATIBILITY") || t.includes("GOOGLE");
    });
    const hasSts = selectedRows.some(row => {
      const t = (row.testcase || "").toUpperCase();
      return t.includes("STS") || t.includes("SECURITY");
    });
    
    if (hasCtsOrGts && hasSts) {
      if (!hasUser || !hasUserdebug) return "Laundry SMR needs selected USER and USERDEBUG device cards.";
    } else if (hasCtsOrGts) {
      if (!hasUser) return "Laundry SMR needs selected USER device card for CTS/GTS.";
    } else if (hasSts) {
      if (!hasUserdebug) return "Laundry SMR needs selected USERDEBUG device card for STS.";
    } else {
      if (!hasUser && !hasUserdebug) return "Laundry SMR needs at least one device.";
    }
  } else {
    if (!hasUser && !hasUserdebug) return "Laundry SMR needs at least one device.";
  }

  const models = new Set(selectedDevices.map(modelKey));
  if (models.size > 1) return `Laundry SMR devices must use the same model: ${[...models].join(", ")}`;
  const families = new Set(selectedDevices.map(fingerprintFamilyKey));
  if (families.size > 1) return `Laundry SMR devices must use the same fingerprint family: ${[...families].join(", ")}`;
  return "";
}

function appendLog(line) {
  const text = redact(String(line || ""));
  const stamp = new Date().toLocaleTimeString("en-GB", { hour12: false });
  const kind = logKind(text);
  const flow = latestLogFlowForKind(kind) || createBootLogFlow();
  const tab = ensureLogSubtab(flow.id, kind);
  tab.lines.push(`${stamp} ${text}`);
  renderLog();
}

function appendRunnerLogForRun(runId, line) {
  const text = redact(String(line || ""));
  const stamp = new Date().toLocaleTimeString("en-GB", { hour12: false });
  const flow = state.logFlows.get(runId) || createBootLogFlow();
  const tab = ensureLogSubtab(flow.id, "Runner");
  tab.lines.push(`${stamp} ${text}`);
  renderLog();
}

function createRunLogFlow(runId, modeName, devices) {
  const primary = devices[0] || {};
  const serials = devices.map((d) => d.serial).join(",");
  const deviceTitle = `${primary.model || "NO_MODEL"} | ${primary.pda || "NO_PDA"} [${serials}]`;
  const flow = {
    id: runId,
    title: `${modeName} | ${deviceTitle}`,
    tabs: new Map(),
  };
  state.logFlows.set(runId, flow);
  ensureLogSubtab(runId, "Runner");
  const mode = TEST_MODES.find((item) => item.id === modeName);
  if (mode?.needs === "user" || mode?.needs === "both") {
    ensureLogSubtab(runId, "CTS");
    ensureLogSubtab(runId, "GTS");
  }
  if (mode?.needs === "userdebug" || mode?.needs === "both") {
    ensureLogSubtab(runId, "STS");
  }
  state.activeLogFlow = runId;
  state.activeLogSubtab = "Runner";
}

function clearInactiveLogTabs() {
  const keep = new Set(state.activeRuns);
  if (state.activeLogFlow) keep.add(state.activeLogFlow);
  for (const key of [...state.logFlows.keys()]) {
    if (!keep.has(key)) state.logFlows.delete(key);
  }
  if (!state.logFlows.has(state.activeLogFlow)) {
    state.activeLogFlow = [...state.logFlows.keys()][0] || "";
    state.activeLogSubtab = "Runner";
  }
}

function clearLogView() {
  const flow = state.logFlows.get(state.activeLogFlow);
  const tab = flow?.tabs?.get(state.activeLogSubtab);
  if (tab) tab.lines = [];
  clearInactiveLogTabs();
}

function clearInactiveTableCards() {
  state.laundryResults = [];
  state.selectedLaundryResults = new Set();
  state.laundryZipPath = "";
  for (const [key, row] of [...state.lockedLaundryResults.entries()]) {
    if (!state.activeRuns.has(row.runId)) {
      state.lockedLaundryResults.delete(key);
    }
  }
  appendLog("[runner] Cleared inactive table cards.");
  renderFlowMap();
}

function clearInactiveSuites() {
  // Clear completed suite statuses
  for (const [key, status] of [...state.suiteStatuses.entries()]) {
    const runId = status.run_id || "legacy";
    if (!state.activeRuns.has(runId)) {
      state.suiteStatuses.delete(key);
    }
  }

  // Clear matching summaries
  for (const key of [...state.summaries.keys()]) {
    const runId = key.split(":")[0];
    if (!state.activeRuns.has(runId)) {
      state.summaries.delete(key);
    }
  }

  // Clear flows mapping
  for (const runId of [...state.flows.keys()]) {
    if (!state.activeRuns.has(runId)) {
      state.flows.delete(runId);
    }
  }

  appendLog("[runner] Cleared inactive suite cards.");
  renderSuiteStatus();
  renderMetrics();
}

function ensureLogSubtab(flowId, kind) {
  const flow = state.logFlows.get(flowId) || createBootLogFlow();
  if (!flow.tabs.has(kind)) flow.tabs.set(kind, { kind, lines: [] });
  return flow.tabs.get(kind);
}

function latestLogFlowForKind(kind) {
  const flows = [...state.logFlows.values()].filter((flow) => flow.tabs.has(kind));
  return flows[flows.length - 1];
}

function createBootLogFlow() {
  const id = "boot";
  if (!state.logFlows.has(id)) {
    state.logFlows.set(id, { id, title: "RUNNING LOG", tabs: new Map() });
    ensureLogSubtab(id, "Runner");
  }
  if (!state.activeLogFlow) state.activeLogFlow = id;
  return state.logFlows.get(id);
}

function appendRunSummaryToLog(runId) {
  const summaries = [...state.summaries.values()].filter((summary) => (summary.run_id || "legacy") === runId);
  if (!summaries.length) return;
  const total = summaries.reduce((sum, item) => sum + Number(item.total || 0), 0);
  const passed = summaries.reduce((sum, item) => sum + Number(item.passed || 0), 0);
  const failed = summaries.reduce((sum, item) => sum + Number(item.failed || 0), 0);
  appendRunnerLogForRun(runId, `[runner] Flow summary: suites=${summaries.length} total=${total} pass=${passed} fail=${failed}`);
}

function startConfettiLoop() {
  if (confettiInterval) clearInterval(confettiInterval);
  const defaults = { startVelocity: 30, spread: 360, ticks: 60, zIndex: 9999 };
  const burst = () => confetti({
    ...defaults,
    particleCount: 40,
    origin: { x: Math.random(), y: Math.max(0, Math.random() - 0.2) },
  });
  burst();
  confettiInterval = setInterval(burst, 350);
  setTimeout(() => window.addEventListener("mousedown", stopConfettiLoop, { once: true }), 500);
}

function stopConfettiLoop() {
  if (confettiInterval) clearInterval(confettiInterval);
  confettiInterval = null;
  try { confetti.reset(); } catch (_) {}
}

function logKind(text) {
  const match = text.match(/^\[([^\]]+)\]/);
  const prefix = match ? match[1].toLowerCase() : "";
  if (prefix === "cts") return "CTS";
  if (prefix === "gts") return "GTS";
  if (prefix === "sts") return "STS";
  return "Runner";
}

function currentModeFlow() {
  return TEST_MODES.find((mode) => mode.id === state.selectedMode)?.flow || "";
}

function statusClass(value) {
  return String(value || "standby").toLowerCase().replace(/[^a-z0-9]+/g, "-");
}

function formatDuration(total) {
  const h = Math.floor(total / 3600).toString().padStart(2, "0");
  const m = Math.floor((total % 3600) / 60).toString().padStart(2, "0");
  const s = Math.floor(total % 60).toString().padStart(2, "0");
  return `${h}:${m}:${s}`;
}

function normalizeDialogPath(selected) {
  if (!selected) return "";
  if (typeof selected === "string") return selected;
  if (Array.isArray(selected)) return normalizeDialogPath(selected[0]);
  return selected.path || selected.file || selected.toString?.() || "";
}

function fileName(path) {
  return String(path || "").split(/[\\/]/).filter(Boolean).pop() || "-";
}

function redact(text) {
  if (!state.wifi.password) return text;
  return text.split(state.wifi.password).join("********");
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

setInterval(() => {
  if (state.running) renderMetrics();
}, 1000);
