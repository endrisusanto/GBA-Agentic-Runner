import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
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
    enabled: false,
    ssid: localStorage.getItem("autoWifiSsid") || "RTT / IEEE 802.11",
    password: localStorage.getItem("autoWifiPassword") || "1234qwer",
  },
  retryCount: Number(localStorage.getItem("autoRetryCount") || "5"),
  timeoutSecs: Number(localStorage.getItem("autoTestTimeout") || "86400"),
  devices: [],
  selected: new Set(),
  selectedMode: "Laundry SMR",
  laundryZipPath: "",
  laundrySources: [],
  laundryResults: [],
  lockedLaundryResults: new Map(),
  selectedLaundryResults: new Set(),
  lampStates: new Map(),
  running: false,
  activeRuns: new Set(),
  runDevices: new Map(),
  localBusy: new Map(),
  completedDevices: new Set(),
  flows: new Map(),
  logFlows: new Map(),
  activeLogFlow: "",
  activeLogSubtab: "AI Worker",
  suiteStatuses: new Map(),
  summaries: new Map(),
  resultDirs: new Map(),
  resultDir: "",
  runStartedAt: null,
  runElapsedSecs: 0,
  runTableTab: "preview",
  laundryWarnings: [],
  preflightLines: [],
  manualSelection: false,
};

const app = document.querySelector("#app");

app.innerHTML = `
  <div class="shell">
    <header class="titlebar">
      <div class="brand">
        <img class="brand-mark" src="${agenticLogo}" alt="GBA" />
        <div>
          <h1>GBA Agentic AI Worker</h1>
          <p>AI Worker</p>
        </div>
      </div>
      <div class="titlebar-actions">
        <div class="overall-progress" id="overallProgress" title="Overall run progress" aria-label="Overall run progress">
          <svg viewBox="0 0 36 36" aria-hidden="true">
            <circle class="overall-progress__bg" cx="18" cy="18" r="15"></circle>
            <circle class="overall-progress__value" id="overallProgressCircle" cx="18" cy="18" r="15"></circle>
          </svg>
          <span id="overallProgressText">0%</span>
        </div>
        <button class="icon-button settings-button" id="settingsBtn" title="Settings" aria-label="Settings">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"></path>
          <path d="M19.4 15a1.8 1.8 0 0 0 .36 1.98l.04.04a2 2 0 0 1-2.82 2.82l-.04-.04A1.8 1.8 0 0 0 15 19.44a1.8 1.8 0 0 0-1 .56 1.8 1.8 0 0 0-.52 1.28V21.4a2 2 0 0 1-4 0v-.08A1.8 1.8 0 0 0 8 19.44a1.8 1.8 0 0 0-1.98.36l-.04.04a2 2 0 0 1-2.82-2.82l.04-.04A1.8 1.8 0 0 0 3.56 15a1.8 1.8 0 0 0-.56-1 1.8 1.8 0 0 0-1.28-.52H1.6a2 2 0 0 1 0-4h.08A1.8 1.8 0 0 0 3.56 8a1.8 1.8 0 0 0-.36-1.98l-.04-.04a2 2 0 0 1 2.82-2.82l.04.04A1.8 1.8 0 0 0 8 3.56a1.8 1.8 0 0 0 1-.56 1.8 1.8 0 0 0 .52-1.28V1.6a2 2 0 0 1 4 0v.08A1.8 1.8 0 0 0 15 3.56a1.8 1.8 0 0 0 1.98-.36l.04-.04a2 2 0 0 1 2.82 2.82l-.04.04A1.8 1.8 0 0 0 19.44 8a1.8 1.8 0 0 0 .56 1 1.8 1.8 0 0 0 1.28.52h.12a2 2 0 0 1 0 4h-.08A1.8 1.8 0 0 0 19.4 15Z"></path>
        </svg>
        </button>
      </div>
    </header>

    <aside class="devices-pane">
      <div class="pane-head">
        <h2>DEVICES</h2>
        <div class="pane-actions">
          <button class="mini-button warn" id="resetBusyBtn">Reset Busy</button>
          <button class="mini-button" id="unselectBtn">Select All</button>
          <button class="mini-button" id="refreshBtn">Refresh</button>
        </div>
      </div>
      <div class="device-list" id="deviceList"></div>
      <footer id="deviceFooter">Standby</footer>
    </aside>

    <main class="workspace">
      <div class="toolbar">
        <button class="run-button" id="runBtn">Run Selected</button>
        <button class="ghost-button emergency-button" id="cancelBtn" disabled>Emergency Stop</button>
        <button class="ghost-button" id="openResultBtn" disabled>Open Result</button>
        <button class="ghost-button" id="preflightToolbarBtn">Preflight</button>
        <div class="toolbar-spacer"></div>
        <span class="toolbar-pill" id="modePill">Laundry SMR</span>
        <span class="toolbar-pill" id="selectedPill">0 selected</span>
        <span class="toolbar-pill" id="preflightPill">Preflight -</span>
        <span class="elapsed-pill" id="elapsedPill">00:00:00</span>
      </div>

      <section class="selected-strip" id="selectedStrip"></section>
      <section class="preflight-panel" id="preflightPanel"></section>
      <section class="run-options">
        <label class="retry">Retry <input id="retryInput" type="number" min="0" max="99" /></label>
        <label class="retry">Timeout <input id="timeoutInput" type="number" min="60" /></label>
      </section>

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
        <h2>RUN</h2>
        <div class="current-run" id="currentRun"></div>
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
            <p>AUTO root path and preflight check</p>
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
            <h2 id="warningsTitle">Info</h2>
            <p id="warningsText">Details</p>
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
  preflightToolbarBtn: document.querySelector("#preflightToolbarBtn"),
  preflightPanel: document.querySelector("#preflightPanel"),
  selectedStrip: document.querySelector("#selectedStrip"),
  modePill: document.querySelector("#modePill"),
  selectedPill: document.querySelector("#selectedPill"),
  preflightPill: document.querySelector("#preflightPill"),
  elapsedPill: document.querySelector("#elapsedPill"),
  resultPill: document.querySelector("#resultPill"),
  unselectBtn: document.querySelector("#unselectBtn"),
  resetBusyBtn: document.querySelector("#resetBusyBtn"),
  refreshBtn: document.querySelector("#refreshBtn"),
  overallProgress: document.querySelector("#overallProgress"),
  overallProgressCircle: document.querySelector("#overallProgressCircle"),
  overallProgressText: document.querySelector("#overallProgressText"),
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
  clearLogBtn: document.querySelector("#clearLogBtn"),
  statusLine: document.querySelector("#statusLine"),
  deviceFooter: document.querySelector("#deviceFooter"),
  activeMetric: document.querySelector("#activeMetric"),
  completedMetric: document.querySelector("#completedMetric"),
  passedMetric: document.querySelector("#passedMetric"),
  failedMetric: document.querySelector("#failedMetric"),
  runtimeMetric: document.querySelector("#runtimeMetric"),
  currentRun: document.querySelector("#currentRun"),
  suiteList: document.querySelector("#suiteList"),
  clearSuitesBtn: document.querySelector("#clearSuitesBtn"),
  warningsModal: document.querySelector("#warningsModal"),
  warningsCloseBtn: document.querySelector("#warningsCloseBtn"),
  warningsOkBtn: document.querySelector("#warningsOkBtn"),
  warningsList: document.querySelector("#warningsList"),
  warningsTitle: document.querySelector("#warningsTitle"),
  warningsText: document.querySelector("#warningsText"),
};

els.retryInput.value = state.retryCount;
els.timeoutInput.value = state.timeoutSecs;

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
els.preflightToolbarBtn.addEventListener("click", () => runPreflight(false));
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
document.addEventListener("keydown", (event) => {
  if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === "b") {
    event.preventDefault();
    resetBusyState();
  }
});

listen("gba-run-started", async (event) => {
  const payload = event.payload || {};
  const runId = payload.run_id;
  const runMode = payload.test_type;
  const serials = [...(payload.selected_devices || []), ...(payload.userdebug_devices || [])];
  
  if (!state.activeRuns.has(runId)) {
    state.activeRuns.add(runId);
    state.runDevices.set(runId, serials);
    serials.forEach((serial) => state.completedDevices.delete(serial));
        state.flows.set(runId, {
      mode: runMode,
      flow: runMode,
      devices: serials.join(","),
      model: payload.model || "Unknown",
    });
    
    // Add missing createRunLogFlow for external runs
    if (!state.logFlows.has(runId)) {
      const deviceObjects = serials.map(serial => state.devices.find(d => d.serial === serial) || { serial });
      createRunLogFlow(runId, runMode, deviceObjects);
    }
    
    // Add to selected devices so they are checked (selected)
    serials.forEach((serial) => {
      state.selected.add(serial);
    });
  }
  
  state.running = true;
  els.cancelBtn.disabled = false;
  els.statusLine.textContent = "Running";
  
  await refreshDevices();
});

listen("gba-run-log", (event) => {
  const payload = event.payload;
  if (payload && typeof payload === 'object' && payload.line !== undefined) {
    appendLog(payload.line, payload.run_id);
  } else {
    appendLog(String(payload || ""));
  }
});
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
  appendRunnerLogForRun(payload.run_id || "legacy", `[AI Worker] Summary ${payload.suite || "-"} ${payload.devices || "-"}: total=${payload.total ?? 0} pass=${payload.passed ?? 0} fail=${payload.failed ?? 0} runtime=${payload.run_time || "N/A"}`);
  renderFlowMap();
  renderSuiteStatus();
  renderMetrics();
});
listen("gba-laundry-result-update", (event) => {
  const payload = event.payload || {};
  if (!payload.id) return;
  const lockedKey = [...state.lockedLaundryResults.entries()]
    .find(([key, row]) => key.startsWith(`${payload.run_id || "legacy"}:`) && (row.originalId || row.id) === payload.id)?.[0];
  if (lockedKey) {
    state.lockedLaundryResults.set(lockedKey, updateLaundryRow(state.lockedLaundryResults.get(lockedKey), payload));
  } else {
    state.laundryResults = state.laundryResults.map((row) =>
      (row.originalId || row.id) === payload.id ? updateLaundryRow(row, payload) : row);
  }
  renderFlowMap();
});
listen("gba-run-finished", (event) => {
  const payload = event.payload || {};
  if (payload.run_id) {
    if (payload.result_dir) state.resultDirs.set(payload.run_id, payload.result_dir);
    const finishedSerials = state.runDevices.get(payload.run_id) || [];
    finishedSerials.forEach((serial) => {
      state.selected.delete(serial);
      state.completedDevices.add(serial);
    });
    state.activeRuns.delete(payload.run_id);
    clearLocalBusy(finishedSerials);
    state.runDevices.delete(payload.run_id);
  }
  state.running = state.activeRuns.size > 0;
  if (payload.run_id === state.activeLogFlow || !state.resultDir) {
    state.resultDir = payload.result_dir || state.resultDir;
  }
  els.runBtn.disabled = false;
  els.cancelBtn.disabled = state.activeRuns.size === 0;
  els.openResultBtn.disabled = !state.resultDir;
  els.resultPill.disabled = !state.resultDir;
  els.resultPill.textContent = state.resultDir ? "Open" : "None";
  els.statusLine.textContent = payload.exit_code === 0 ? "Completed" : "Finished with issue";
  appendRunSummaryToLog(payload.run_id || "legacy");
  appendRunnerLogForRun(payload.run_id || "legacy", `[AI Worker] Finished exit=${payload.exit_code} result=${state.resultDir || "N/A"}`);
  if (Number(payload.exit_code) !== 0) showInfoModal("Run Finished With Issue", `Exit code ${payload.exit_code}`, [`Result: ${state.resultDir || "N/A"}`], "error");
  renderFlowMap();
  renderMetrics();
});

listen("gba-tool-error", (event) => {
  showInfoModal("Tool Error", "AI Worker reported an error.", [String(event.payload || "")], "error");
});

init();

async function init() {
  render();
  await reconcileAutoRoot();
  await refreshDevices();
  await runPreflight(false);
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
    showInfoModal("ADB Refresh Failed", "Device scan failed.", [String(error)], "error");
    state.devices = [];
  }
  if (!state.selected.size) selectLaundryModelFromResults();
  renderDevices();
  els.deviceFooter.textContent = `${state.devices.length} detected`;
  
  // Dynamically update log flow titles if they have NO_MODEL
  state.logFlows.forEach((flow, runId) => {
    if (flow.title.includes("NO_MODEL") || flow.title.includes("NO_PDA")) {
      const runDevices = state.runDevices.get(runId) || [];
      if (runDevices.length > 0) {
        const deviceObjects = runDevices.map(serial => state.devices.find(d => d.serial === serial) || { serial });
        const primary = deviceObjects[0] || {};
        if (primary.model) {
          const serialsStr = runDevices.join(",");
          const deviceTitle = `${primary.model || "NO_MODEL"} | ${primary.pda || "NO_PDA"} [${serialsStr}]`;
          flow.title = `${flow.mode || "Run"} | ${deviceTitle}`;
        }
      }
    }
  });
  if (state.activeLogFlow) renderLogTabs();
  renderSuiteStatus();
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
  renderSelectedStrip();
  renderFlowMap();
  renderLog();
  renderSuiteStatus();
  renderMetrics();
  renderPreflight();
}

function renderDevices() {
  const readyDevices = state.devices.filter((device) => device.state === "device" && !device.busy);
  const allReadySelected = readyDevices.length > 0 && readyDevices.every((device) => state.selected.has(device.serial));
  els.unselectBtn.textContent = allReadySelected ? "Deselect All" : "Select All";

  if (!state.devices.length) {
    els.deviceList.innerHTML = `<div class="empty">No ADB devices detected.</div>`;
    renderSelectedStrip();
    return;
  }

  const devicesByModel = new Map();
  [...state.devices]
    .sort((a, b) => {
      const status = (device) => state.completedDevices.has(device.serial) ? 2 : Number(Boolean(device.busy || device.busy_reason));
      return status(a) - status(b) || modelKey(a).localeCompare(modelKey(b)) || String(a.serial).localeCompare(String(b.serial));
    })
    .forEach((device) => {
      const model = device.model || "UNKNOWN_MODEL";
      if (!devicesByModel.has(model)) devicesByModel.set(model, []);
      devicesByModel.get(model).push(device);
    });

  els.deviceList.innerHTML = [...devicesByModel.entries()].map(([model, devices], groupIndex) => `
    <section class="model-group model-group-${groupIndex % 5}">
      <header class="model-group-header">
        <strong>${escapeHtml(model)}</strong>
        <span>${devices.length} device${devices.length === 1 ? "" : "s"}</span>
      </header>
      ${devices.map((device) => {
    const isBusy = Boolean(device.busy || device.busy_reason);
    const ready = device.state === "device" && !isBusy;
    const selected = state.selected.has(device.serial);
    const badge = isBusy ? "BUSY" : (device.is_userdebug ? "USERDEBUG" : "USER");
    const lampActive = state.lampStates.get(device.serial);
    const completed = state.completedDevices.has(device.serial) && !isBusy;
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
      ${completed ? '<div class="device-completed-badge">TEST COMPLETED</div>' : ""}
    `;
      }).join("")}
    </section>
  `).join("");

  els.deviceList.querySelectorAll(".device-card").forEach((card) => {
    card.addEventListener("click", () => {
      if (card.classList.contains("disabled")) return;
      const serial = card.dataset.serial;
      if (state.selected.has(serial)) state.selected.delete(serial);
      else state.selected.add(serial);
      state.manualSelection = true;
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
  renderSelectedStrip();
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
      renderFlowMap();
    });
  });
}

function renderPreflight() {
  if (!state.preflightLines.length) {
    els.preflightPanel.innerHTML = `<div class="preflight-empty">Preflight has not run.</div>`;
    els.preflightPill.textContent = "Preflight -";
    return;
  }
  const groups = preflightGroups();
  const missing = groups.reduce((sum, group) => sum + group.bad.length, 0);
  els.preflightPill.textContent = missing ? `Preflight ${missing}` : "Preflight OK";
  els.preflightPill.className = `toolbar-pill ${missing ? "bad" : "ok"}`;
  els.preflightPanel.innerHTML = `
    <div class="preflight-head">
      <strong class="${missing ? "fail" : "pass"}">${missing ? `${missing} issue(s)` : "Ready"}</strong>
      <span>${escapeHtml(state.autoRoot || "-")}</span>
    </div>
    <div class="preflight-list">
      ${groups.map((group) => `
        <button class="${group.bad.length ? "bad" : "ok"}" title="${escapeHtml([...group.bad, ...group.ok].join("\n"))}">
          <b>${escapeHtml(group.name)}</b>
          <span>${group.bad.length ? `${group.bad.length} issue` : `${group.ok.length} OK`}</span>
        </button>
      `).join("")}
    </div>
  `;
}

function renderSelectedStrip() {
  const selectedDevices = state.devices.filter((device) => state.selected.has(device.serial));
  const models = [...new Set(selectedDevices.map(modelKey))];
  const kinds = [
    selectedDevices.some((device) => !device.is_userdebug) ? "USER" : "",
    selectedDevices.some((device) => device.is_userdebug) ? "USERDEBUG" : "",
  ].filter(Boolean).join("+") || "-";
  const selectedText = `${selectedDevices.length} selected`;
  els.modePill.textContent = state.selectedMode;
  els.selectedPill.textContent = selectedText;
  els.selectedStrip.innerHTML = `
    <span><b>Selected</b> ${selectedText}</span>
    <span><b>Model</b> ${escapeHtml(models.join(", ") || "-")}</span>
    <span><b>Type</b> ${escapeHtml(kinds)}</span>
    <span><b>Zip</b> ${escapeHtml(state.laundrySources.map((source) => fileName(source.path)).join(", ") || "-")}</span>
  `;
}

function renderLog() {
  renderLogTabs();
  const flow = state.logFlows.get(state.activeLogFlow);
  const tab = flow?.tabs?.get(state.activeLogSubtab);
  els.logBox.textContent = (tab?.lines || []).slice(-600).join("\n");
}

function renderLogTabs() {
  if (!state.logFlows.size) {
    createBootLogFlow();
  }
  els.logFlowTabs.innerHTML = [...state.logFlows.values()].reverse().map((flow) => `
    <span class="log-tab-wrap ${flow.id === state.activeLogFlow ? "active" : ""}">
      <button class="log-tab" data-flow="${escapeHtml(flow.id)}" title="${escapeHtml(flow.title)}">${escapeHtml(flow.title)}</button>
      <button class="log-tab-close" data-close-log="${escapeHtml(flow.id)}" ${flow.id === "boot" ? "disabled" : ""} title="${state.activeRuns.has(flow.id) ? "Emergency stop and close" : "Close"}">×</button>
    </span>
  `).join("");
  els.logFlowTabs.querySelectorAll(".log-tab").forEach((button) => {
    button.addEventListener("click", () => {
      state.activeLogFlow = button.dataset.flow;
      state.resultDir = state.resultDirs.get(state.activeLogFlow) || "";
      const flow = state.logFlows.get(state.activeLogFlow);
      if (flow && !flow.tabs.has(state.activeLogSubtab)) state.activeLogSubtab = "AI Worker";
      renderLog();
    });
  });
  els.logFlowTabs.querySelectorAll("[data-close-log]").forEach((button) => {
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      closeLogFlow(button.dataset.closeLog);
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
  const previousScrollTop = els.flowMap.querySelector(".run-table-content-area")?.scrollTop || 0;
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
  if (!state.laundrySources.length && !state.laundryResults.length && !lockedRows.length) {
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
  if (!["preview", "running", "completed"].includes(state.runTableTab)) {
    state.runTableTab = "preview";
  }

  const tabsHtml = `
    <div class="run-table-tabs">
      <button class="run-tab-btn ${state.runTableTab === 'preview' ? 'active' : ''}" data-tab="preview">
        Preview ${state.laundrySources.length ? `✓ ${state.laundrySources.length}` : ''}
      </button>
      <button class="run-tab-btn ${state.runTableTab === 'running' ? 'active' : ''}" data-tab="running">
        Running (${runningCount})
      </button>
      <button class="run-tab-btn ${state.runTableTab === 'completed' ? 'active' : ''}" data-tab="completed">
        Completed (${completedCount})
      </button>
    </div>
  `;

  let contentHtml = "";
  if (state.runTableTab === "preview") {
    if (state.laundryResults.length) {
      contentHtml = `<div class="laundry-table-stack">${state.laundrySources.map((source) => {
        const rows = source.rows.map((row) => ({ ...row, locked: false, running: false }));
        const sourceSelected = rows.filter((row) => state.selectedLaundryResults.has(row.id)).length;
        const model = source.models.join(", ") || "Model auto-detect";
        return renderLaundryTableCard({
          title: `${state.selectedMode} · ${model}`,
          subtitle: `${sourceSelected}/${rows.length} selected · ${fileName(source.path)}`,
          rows,
        });
      }).join("")}</div>`;
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
  }

  els.flowMap.innerHTML = `
    <div class="run-table-container">
      ${tabsHtml}
      <div class="run-table-content-area">
        ${contentHtml}
      </div>
    </div>
  `;
  els.flowMap.querySelector(".run-table-content-area").scrollTop = previousScrollTop;

  // Bind tab buttons
  els.flowMap.querySelectorAll(".run-tab-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.runTableTab = btn.dataset.tab;
      if (state.runTableTab === "preview" && !state.manualSelection) selectLaundryModelFromResults();
      render();
    });
  });

  els.flowMap.querySelectorAll(".laundry-result-check").forEach((input) => {
    input.addEventListener("change", () => {
      if (input.checked) state.selectedLaundryResults.add(input.dataset.id);
      else state.selectedLaundryResults.delete(input.dataset.id);
      selectLaundryModelFromResults();
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
  let progressStatuses = [];
  if (statuses.length > 0) {
    const activeStatuses = statuses.filter((s) => state.activeRuns.has(s.run_id));
    if (activeStatuses.length > 0) {
      progressStatuses = activeStatuses;
    } else {
      const runIds = [];
      statuses.forEach((s) => {
        const id = s.run_id || "legacy";
        if (!runIds.includes(id)) {
          runIds.push(id);
        }
      });
      const latestRunId = runIds[runIds.length - 1];
      progressStatuses = statuses.filter((s) => (s.run_id || "legacy") === latestRunId);
    }
  }
  renderOverallProgress(progressStatuses);
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

  els.suiteList.innerHTML = [...byRun.entries()].reverse().map(([runId, runStatuses]) => {
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
        <div class="suite-row status-row-${statusClass(status.status)}">
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
    const activeRun = state.activeRuns.has(runId);
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
      <button class="flow-close" data-close-suite="${escapeHtml(runId)}" title="${activeRun ? "Emergency stop and close" : "Close"}">×</button>
        </header>
        <div class="flow-suite-list">${rows}</div>
      </div>
    `;
  }).join("");

  els.suiteList.querySelectorAll("[data-close-suite]").forEach((button) => {
      button.addEventListener("click", () => closeSuiteRun(button.dataset.closeSuite));
  });
}

function renderOverallProgress(statuses) {
  const doneStatuses = new Set(["Test Done", "Cancelled", "Failed", "Timeout", "Completed"]);
  const total = statuses.length;
  const done = statuses.filter((status) => doneStatuses.has(status.status)).length;
  const pct = total ? Math.round((done / total) * 100) : 0;
  const circumference = 2 * Math.PI * 15;
  els.overallProgressCircle.style.strokeDasharray = circumference;
  els.overallProgressCircle.style.strokeDashoffset = circumference * (1 - pct / 100);
  els.overallProgressCircle.style.stroke = total && done === total ? "var(--green)" : "var(--cyan)";
  els.overallProgressText.textContent = `${pct}%`;
  els.overallProgress.title = total ? `Overall run progress: ${done}/${total} suites` : "Overall run progress: no suites";
  els.overallProgress.setAttribute("aria-label", els.overallProgress.title);
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
  if (state.running && state.runStartedAt) state.runElapsedSecs = Math.floor((Date.now() - state.runStartedAt) / 1000);
  const elapsed = formatDuration(state.runElapsedSecs || 0);
  els.runtimeMetric.textContent = elapsed;
  els.elapsedPill.textContent = elapsed;
  if (state.running) els.statusLine.textContent = `Running ${elapsed}`;
  renderCurrentRun(statuses, elapsed);
}

function renderCurrentRun(statuses, elapsed) {
  const active = statuses.find((status) => !["Test Done", "Cancelled", "Failed", "Timeout", "Completed"].includes(status.status)) || statuses[statuses.length - 1];
  const flow = active ? state.flows.get(active.run_id || "legacy") : null;
  els.currentRun.innerHTML = `
    <div><span>Mode</span><b>${escapeHtml(flow?.mode || state.selectedMode || "-")}</b></div>
    <div><span>Model</span><b>${escapeHtml(getDeviceModelsForFlow(flow) || "-")}</b></div>
    <div><span>Devices</span><b>${escapeHtml(flow?.devices || "-")}</b></div>
    <div><span>Suite</span><b>${escapeHtml(active?.suite || "-")}</b></div>
    <div><span>Status</span><b class="status-${statusClass(active?.status)}">${escapeHtml(active?.status || "Standby")}</b></div>
    <div><span>Elapsed</span><b>${escapeHtml(elapsed)}</b></div>
  `;
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
  if (flow.model && flow.model !== "Unknown") return flow.model;
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
  const readyDevices = state.devices.filter((device) => device.state === "device" && !device.busy);
  const allReadySelected = readyDevices.length > 0 && readyDevices.every((device) => state.selected.has(device.serial));
  if (allReadySelected) {
    state.selected.clear();
    state.manualSelection = true;
  } else {
    state.selected = new Set(readyDevices.map((device) => device.serial));
    state.manualSelection = true;
  }
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
      const t = ((row.testcase || "") + " " + (row.suite || "")).toUpperCase();
      return t.includes("CTS") || t.includes("GTS") || t.includes("COMPATIBILITY") || t.includes("GOOGLE");
    });
    hasSts = selectedRows.some(row => {
      const t = ((row.testcase || "") + " " + (row.suite || "")).toUpperCase();
      return t.includes("STS") || t.includes("SECURITY");
    });
  }

  devices.forEach((device) => {
    const model = modelKey(device);
    const key = model;
    if (!groups.has(key)) {
      // ponytail: group by model so user+userdebug of same model run together
      groups.set(key, { kind: "", fingerprint: fingerprintFamilyKey(device), model, devices: [] });
    }
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
      // ponytail: no row type detected — accept any device combo
      valid = hasUser || hasUserdebug;
    }

    if (valid) {
      if (hasUser && hasUserdebug) group.kind = "USER+USERDEBUG";
      else if (hasUser) group.kind = "USER";
      else group.kind = "USERDEBUG";
    }
    return valid;
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
    multiple: true,
    filters: [{ name: "Laundry zip", extensions: ["zip"] }],
  });
  const paths = (Array.isArray(selected) ? selected : [selected])
    .map(normalizeDialogPath)
    .filter(Boolean);
  if (!paths.length) return;

  state.laundrySources = [];
  state.laundryResults = [];
  state.selectedLaundryResults = new Set();
  state.laundryWarnings = [];
  state.manualSelection = false;

  for (const [index, path] of paths.entries()) {
    try {
      const analyzed = await invoke("analyze_laundry_zip", { zipPath: path });
      const rows = (Array.isArray(analyzed) ? analyzed : []).filter((row) => !isCtsVerifierRow(row));
      const sourceId = `zip-${index + 1}`;
      const sourceRows = (Array.isArray(rows) ? rows : []).map((row) => ({
        ...row,
        id: `${sourceId}:${row.id}`,
        originalId: row.id,
        sourceId,
        sourcePath: path,
      }));
      const source = { id: sourceId, path, rows: sourceRows };
      source.models = laundryPreviewModelsForSource(source, state.devices);
      state.laundrySources.push(source);
      state.laundryResults.push(...sourceRows);
      sourceRows.forEach((row) => state.selectedLaundryResults.add(row.id));
      appendLog(`[runner] Laundry zip scanned: ${fileName(path)} (${sourceRows.length} result(s), model ${source.models.join(", ") || "auto"}).`);
      try {
        const autoRoot = state.autoRoot || await invoke("default_auto_root");
        const warnings = await invoke("check_laundry_mismatches", { autoRoot, zipPath: path });
        state.laundryWarnings.push(...(Array.isArray(warnings) ? warnings : []));
      } catch (error) {
        appendLog(`[runner] Tool version mismatch check failed (${fileName(path)}): ${error}`);
      }
    } catch (error) {
      appendLog(`[runner] Laundry zip scan failed (${fileName(path)}): ${error}`);
      showInfoModal("Laundry Zip Scan Failed", `Cannot read ${fileName(path)}.`, [String(error)], "error");
    }
  }

  state.laundryZipPath = state.laundrySources[0]?.path || "";
  if (!state.laundrySources.length) return;
  if (state.laundryWarnings.length) {
    renderPreflight();
    showInfoModal("Mismatched Tools Warning", "Some required tools are mismatched or missing.", state.laundryWarnings, "warning");
  }
  selectLaundryModelFromResults();
  renderFlowMap();
}

async function runSelected() {
  saveInlineSettings();
  const mode = TEST_MODES.find((item) => item.id === state.selectedMode);
  const runMode = state.selectedMode;
  const runFlow = currentModeFlow();
  if (isLaundryMode(runMode) && !state.laundrySources.length) {
    await chooseLaundryZip();
    if (state.laundrySources.length) {
      appendLog("[runner] Laundry zip loaded. Select testcase rows, then click Run Selected again.");
      els.statusLine.textContent = "Select laundry testcase rows";
      return;
    }
    if (!state.laundrySources.length) {
      appendLog("[runner] Laundry flow needs zip file.");
      els.statusLine.textContent = "Laundry zip required";
      showInfoModal("Laundry Zip Required", "Pick a laundry result zip before running.", ["No zip selected."], "error");
      return;
    }
  }
  let selectedDevices = state.devices.filter((device) => state.selected.has(device.serial));
  let userDevices = selectedDevices.filter((device) => !device.is_userdebug).map((device) => device.serial);
  let userdebugDevices = selectedDevices.filter((device) => device.is_userdebug).map((device) => device.serial);
  const validation = validateRun(mode, userDevices, userdebugDevices);
  if (validation) {
    appendLog(`[runner] ${validation}`);
    els.statusLine.textContent = validation;
    showInfoModal("Run Blocked", "Fix this before running.", [validation], "error");
    return;
  }
  if (isLaundryMode(runMode) && state.laundryResults.length && state.selectedLaundryResults.size === 0) {
    appendLog("[runner] Select at least one laundry result row.");
    els.statusLine.textContent = "Laundry result selection required";
    showInfoModal("No Laundry Rows Selected", "Select at least one row from the run table.", ["All rows are currently unchecked."], "error");
    return;
  }

  const autoRoot = state.autoRoot || await invoke("default_auto_root");
  const preflight = await runPreflight(false);
  const preflightIssues = preflight.filter((line) => !isPreflightOk(line));
  if (preflightIssues.length) {
    showInfoModal("Preflight Failed", "Required commands or folders are missing.", preflightIssues, "error");
    return;
  }
  const groups = shardSelectedDevices(selectedDevices, mode);
  if (!groups.length) {
    appendLog("[runner] No valid device group for selected mode.");
    showInfoModal("No Runnable Device Group", "Selected devices do not match this mode.", ["Use Select to pick a same-model ready group."], "error");
    return;
  }
  const runPlans = groups.map((group) => {
    if (!isLaundryMode(runMode)) return { source: null, selectedUiIds: [], selectedIds: [] };
    const model = modelKey(group.devices[0]);
    const source = laundrySourceForModel(model);
    if (!source) return { error: `Tidak ada laundry ZIP yang cocok untuk model ${model}.` };
    const selectedRows = state.laundryResults.filter((row) =>
      row.sourceId === source.id && state.selectedLaundryResults.has(row.id));
    if (!selectedRows.length) return { error: `Tidak ada testcase terpilih dari ZIP untuk model ${model}.` };
    return {
      source,
      selectedUiIds: selectedRows.map((row) => row.id),
      selectedIds: selectedRows.map((row) => row.originalId || row.id),
    };
  });
  const planError = runPlans.find((plan) => plan.error)?.error;
  if (planError) {
    showInfoModal("Laundry ZIP Model Mismatch", "Each selected model needs its matching laundry ZIP.", [planError], "error");
    return;
  }
  const runnableDevices = groups.flatMap((group) => group.devices);

  state.running = true;
  state.resultDir = "";
  state.runStartedAt = Date.now();
  state.runElapsedSecs = 0;
  state.runTableTab = "running";
  markLocalBusy(runnableDevices, runMode);
  state.selected.clear();
  state.manualSelection = false;
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
    const plan = runPlans[index];
    if (isLaundryMode(runMode)) lockLaundryRowsForRun(runId, runMode, groupSerials, plan.selectedUiIds);
    createRunLogFlow(runId, runMode, groupDevices);
    appendLog(`[runner] Starting shard ${index + 1}/${groups.length}: ${group.kind} fingerprint=${shortFingerprint(group.fingerprint)} devices=${groupSerials.join(",")}`);
    renderFlowMap();

    try {
      await invoke("run_suite", {
        request: {
          run_id: runId,
          auto_root: autoRoot,
          test_type: runMode,
          laundry_zip_path: isLaundryMode(runMode) ? plan.source.path : null,
          selected_laundry_results: isLaundryMode(runMode) ? plan.selectedIds : [],
          user_devices: groupUserDevices,
          userdebug_devices: groupUserdebugDevices,
          retry_count: state.retryCount,
          wifi_enabled: false,
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
      showInfoModal("Run Failed", `Devices: ${groupSerials.join(",")}`, [errorMsg], "error");
    }
  }

  if (isLaundryMode(runMode)) {
    state.laundryResults = [];
    state.selectedLaundryResults = new Set();
    state.laundrySources = [];
    state.laundryZipPath = "";
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

function lockLaundryRowsForRun(runId, mode, serials, selectedIds = [...state.selectedLaundryResults]) {
  const selected = new Set(selectedIds);
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

async function emergencyStop(reason = "Emergency stop", runId = null) {
  appendLog(`[runner] ${reason}: stopping ${runId ? `run ${runId}` : "all test processes"}.`);
  els.cancelBtn.disabled = true;
  els.statusLine.textContent = "Emergency stop";
  try {
    await invoke("cancel_run", { runId });
    const serials = runId
      ? (state.runDevices.get(runId) || [])
      : [...state.runDevices.values()].flat();
    if (runId) {
      state.activeRuns.delete(runId);
      state.runDevices.delete(runId);
    } else {
      await invoke("reset_busy_state", { autoRoot: state.autoRoot || null });
      state.activeRuns.clear();
      state.runDevices.clear();
      state.localBusy.clear();
      state.completedDevices.clear();
      state.selected.clear();
    }
    clearLocalBusy(serials);
    state.running = state.activeRuns.size > 0;
    els.cancelBtn.disabled = state.activeRuns.size === 0;
    await refreshDevices();
  } catch (error) {
    appendLog(`[runner] Emergency stop failed: ${error}`);
    showInfoModal("Cancel Failed", "AI Worker could not cancel the active process.", [String(error)], "error");
    els.cancelBtn.disabled = state.activeRuns.size === 0;
  }
}

async function cancelRun() {
  await emergencyStop("Emergency stop requested");
}

async function resetBusyState() {
  appendLog("[runner] Reset busy state requested (Ctrl+Shift+B).");
  try {
    await invoke("reset_busy_state", { autoRoot: state.autoRoot || null });
    state.selected.clear();
    state.localBusy.clear();
    state.completedDevices.clear();
    state.runDevices.clear();
    appendLog("[runner] busy.json reset.");
    await refreshDevices();
  } catch (error) {
    appendLog(`[runner] Reset busy state failed: ${error}`);
    showInfoModal("Reset Busy Failed", "Cannot clear busy state.", [String(error)], "error");
  }
}

async function openResult() {
  const resultDir = state.resultDirs.get(state.activeLogFlow) || state.resultDir;
  if (!resultDir) return;
  try {
    await invoke("open_result", { path: resultDir });
  } catch (error) {
    appendLog(`[runner] Open result failed: ${error}`);
    showInfoModal("Open Result Failed", "Cannot open result folder.", [String(error)], "error");
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
    showInfoModal("Lamp Failed", `Device: ${serial}`, [String(error)], "error");
  }
}

async function openScrcpy(serial) {
  if (!serial) return;
  appendLog(`[runner] Opening scrcpy for ${serial}`);
  try {
    await invoke("open_scrcpy", { serial });
  } catch (error) {
    appendLog(`[runner] scrcpy failed for ${serial}: ${error}`);
    showInfoModal("Scrcpy Failed", `Device: ${serial}`, [String(error)], "error");
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

async function runPreflight(updateSettings = true) {
  if (updateSettings) saveSettings(false);
  if (els.settingsOutput) els.settingsOutput.textContent = "Checking...\n";
  try {
    const lines = await invoke("preflight", { autoRoot: state.autoRoot || null });
    state.preflightLines = Array.isArray(lines) ? lines : [];
    if (els.settingsOutput) els.settingsOutput.textContent = state.preflightLines.join("\n");
    renderPreflight();
    return state.preflightLines;
  } catch (error) {
    state.preflightLines = [String(error)];
    if (els.settingsOutput) els.settingsOutput.textContent = String(error);
    renderPreflight();
    showInfoModal("Preflight Failed", "Cannot run preflight.", [String(error)], "error");
    return state.preflightLines;
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
  if (isLaundryMode(mode.id)) {
    const modelValidation = validateLaundryModelSelection();
    if (modelValidation) return modelValidation;
  }
  if (mode.needs === "both" && userDevices.length === 0 && userdebugDevices.length === 0) return `${mode.name} needs at least one runnable device.`;
  if (!state.autoRoot) return "AUTO root is empty. Open settings and save root.";
  return "";
}

function selectLaundryModelFromResults() {
  if (!isLaundryMode(state.selectedMode) || !state.devices.length) return false;
  const readyDevices = state.devices.filter((device) => device.state === "device" && !device.busy);
  const selectedRows = state.laundryResults.filter((row) => !state.selectedLaundryResults.size || state.selectedLaundryResults.has(row.id));
  if (!selectedRows.length) return false;
  const needs = laundryNeeds(selectedRows);
  const hintedModels = [...new Set([
    ...state.laundrySources.flatMap((source) => source.models),
    ...laundryPreviewModels(selectedRows, readyDevices),
  ])];
  const runnableModels = hintedModels.filter((model) => {
    const sameModel = readyDevices.filter((device) => modelKey(device) === model);
    return (!needs.user || sameModel.some((device) => !device.is_userdebug)) &&
      (!needs.userdebug || sameModel.some((device) => device.is_userdebug));
  });
  if (runnableModels.length) {
    const serials = readyDevices
      .filter((device) => runnableModels.includes(modelKey(device)))
      .map((device) => device.serial);
    state.selected = new Set(serials);
    appendLog(`[runner] Auto-selected ${serials.length} device(s) for model(s): ${runnableModels.join(", ")}.`);
    return true;
  }
  return false;
}

function laundryPreviewHint(rows) {
  return `${state.laundrySources.map((source) => source.path).join(" ")} ${rows.map((row) => [
    row.id,
    row.model,
    row.device_model,
    row.deviceModel,
    row.testcase,
    row.result_dir,
  ].filter(Boolean).join(" ")).join(" ")}`.toUpperCase();
}

function laundryPreviewModelsForSource(source, devices = state.devices) {
  const hint = `${source.path} ${source.rows.map((row) => [
    row.id,
    row.originalId,
    row.model,
    row.device_model,
    row.deviceModel,
    row.testcase,
    row.result_dir,
  ].filter(Boolean).join(" ")).join(" ")}`.toUpperCase();
  return [...new Set(devices.map(modelKey))]
    .filter((model) => model !== "UNKNOWN_MODEL" && modelMatchesPreview(model, hint));
}

function laundrySourceForModel(model) {
  const matches = state.laundrySources.filter((source) => source.models.includes(model));
  if (matches.length === 1) return matches[0];
  if (state.laundrySources.length === 1) return state.laundrySources[0];
  return null;
}

function modelMatchesPreview(model, hint) {
  const normalized = modelKey({ model });
  const tokens = [normalized, normalized.replace(/^SM-/, "")];
  return tokens.some((token) => token.length > 3 && hint.includes(token));
}

function laundryPreviewModels(rows, devices = state.devices) {
  const hint = laundryPreviewHint(rows);
  return [...new Set(devices.map(modelKey))]
    .filter((model) => model !== "UNKNOWN_MODEL" && modelMatchesPreview(model, hint));
}

function validateLaundryModelSelection() {
  if (!state.laundryResults.length || !state.selected.size) return "";
  const rows = state.laundryResults.filter((row) => !state.selectedLaundryResults.size || state.selectedLaundryResults.has(row.id));
  const previewModels = laundryPreviewModels(rows);
  if (!previewModels.length) return "";
  const selectedModels = [...new Set(
    state.devices.filter((device) => state.selected.has(device.serial)).map(modelKey),
  )];
  const mismatch = selectedModels.filter((model) => !previewModels.includes(model));
  return mismatch.length
    ? `Selected model ${mismatch.join(", ")} does not match Preview model ${previewModels.join(", ")}.`
    : "";
}

function selectReadyModel(model) {
  const serials = state.devices
    .filter((device) => device.state === "device" && !device.busy && modelKey(device) === model)
    .map((device) => device.serial);
  if (!serials.length) return false;
  state.selected = new Set(serials);
  appendLog(`[runner] Auto-selected ${serials.length} ${model} device(s).`);
  return true;
}

function laundryNeeds(rows) {
  const text = rows.map((row) => `${row.testcase || ""} ${row.suite || ""}`).join(" ").toUpperCase();
  const user = text.includes("CTS") || text.includes("GTS") || text.includes("COMPATIBILITY") || text.includes("GOOGLE");
  const userdebug = text.includes("STS") || text.includes("SECURITY");
  return { user, userdebug };
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

  // Multiple models are valid: runSelected shards them and assigns each ZIP per model.
  return "";
}

function appendLog(line, runId = null) {
  const text = redact(String(line || "")).replaceAll("[runner]", "[AI Worker]");
  const stamp = new Date().toLocaleTimeString("en-GB", { hour12: false });
  const kind = logKind(text);
  
  let flow;
  if (runId && state.logFlows.has(runId)) {
    flow = state.logFlows.get(runId);
  } else {
    flow = latestLogFlowForKind(kind) || createBootLogFlow();
  }
  
  if (!flow.tabs.has(kind)) flow.tabs.set(kind, { kind, lines: [] });
  const tab = flow.tabs.get(kind);
  tab.lines.push(`${stamp} ${text}`);
  renderLog();
}

function appendRunnerLogForRun(runId, line) {
  const text = redact(String(line || "")).replaceAll("[runner]", "[AI Worker]");
  const stamp = new Date().toLocaleTimeString("en-GB", { hour12: false });
  const flow = state.logFlows.get(runId) || createBootLogFlow();
  const tab = ensureLogSubtab(flow.id, "AI Worker");
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
  ensureLogSubtab(runId, "AI Worker");
  const mode = TEST_MODES.find((item) => item.id === modeName);
  if (mode?.needs === "user" || mode?.needs === "both") {
    ensureLogSubtab(runId, "CTS");
    ensureLogSubtab(runId, "GTS");
  }
  if (mode?.needs === "userdebug" || mode?.needs === "both") {
    ensureLogSubtab(runId, "STS");
  }
  state.activeLogFlow = runId;
  state.activeLogSubtab = "AI Worker";
}

function clearInactiveLogTabs() {
  const keep = new Set(state.activeRuns);
  if (state.activeLogFlow) keep.add(state.activeLogFlow);
  for (const key of [...state.logFlows.keys()]) {
    if (!keep.has(key)) state.logFlows.delete(key);
  }
  if (!state.logFlows.has(state.activeLogFlow)) {
    state.activeLogFlow = [...state.logFlows.keys()][0] || "";
    state.activeLogSubtab = "AI Worker";
  }
}

async function closeLogFlow(flowId) {
  if (!flowId || flowId === "boot") return;
  if (state.activeRuns.has(flowId)) await emergencyStop(`Closing AI Worker tab ${flowId}`);
  state.logFlows.delete(flowId);
  if (state.activeLogFlow === flowId) {
    state.activeLogFlow = [...state.logFlows.keys()][0] || "";
    state.activeLogSubtab = "AI Worker";
  }
  renderLog();
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
  state.laundrySources = [];
  state.laundryZipPath = "";
  state.laundryWarnings = [];
  for (const [key, row] of [...state.lockedLaundryResults.entries()]) {
    if (!state.activeRuns.has(row.runId)) {
      state.lockedLaundryResults.delete(key);
    }
  }
  appendLog("[runner] Cleared inactive table cards.");
  renderPreflight();
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

async function closeSuiteRun(runId) {
  if (!runId) return;
  if (state.activeRuns.has(runId)) await emergencyStop(`Closing suite ${runId}`, runId);
  for (const [key, status] of [...state.suiteStatuses.entries()]) {
    if ((status.run_id || "legacy") === runId) state.suiteStatuses.delete(key);
  }
  for (const key of [...state.summaries.keys()]) {
    if (key.split(":")[0] === runId) state.summaries.delete(key);
  }
  state.flows.delete(runId);
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
    ensureLogSubtab(id, "AI Worker");
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
  appendRunnerLogForRun(runId, `[AI Worker] Flow summary: suites=${summaries.length} total=${total} pass=${passed} fail=${failed}`);
}

function logKind(text) {
  const match = text.match(/^\[([^\]]+)\]/);
  const prefix = match ? match[1].toLowerCase() : "";
  if (prefix === "cts") return "CTS";
  if (prefix === "gts") return "GTS";
  if (prefix === "sts") return "STS";
  return "AI Worker";
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

function showInfoModal(title, message, lines = [], tone = "info") {
  els.warningsModal.dataset.tone = tone;
  els.warningsTitle.textContent = title;
  els.warningsText.textContent = message;
  els.warningsList.innerHTML = lines.map((line) => `<li>${escapeHtml(line)}</li>`).join("");
  els.warningsModal.classList.remove("hidden");
}

function preflightGroups() {
  const groups = [
    { name: "Commands", ok: [], bad: [] },
    { name: "Suites", ok: [], bad: [] },
    { name: "Tools", ok: [], bad: [] },
    { name: "Results", ok: [], bad: [] },
    { name: "Warnings", ok: [], bad: [] },
  ];
  const bucket = (line) => {
    if (/command:/.test(line)) return groups[0];
    if (/Results/.test(line)) return groups[3];
    if (/tools\//.test(line)) return groups[2];
    if (/CTS|GTS|STS/.test(line)) return groups[1];
    return groups[1];
  };
  state.preflightLines.forEach((line) => {
    if (/^Root:/.test(line)) return;
    const group = bucket(line);
    (isPreflightOk(line) ? group.ok : group.bad).push(line);
  });
  state.laundryWarnings.forEach((line) => groups[4].bad.push(line));
  return groups.filter((group) => group.ok.length || group.bad.length || group.name === "Warnings");
}

function isPreflightOk(line) {
  return /^Root:|^OK /.test(String(line || ""));
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
