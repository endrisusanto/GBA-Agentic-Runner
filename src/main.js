import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import "./styles.css";
import agenticLogo from "./assets/agentic.png";

const TEST_MODES = [
  {
    id: "Cuci SMR",
    name: "Cuci SMR",
    flow: "CTS SMR -> GTS SMR",
    description: "Non-userdebug build approval wash run.",
    needs: "user",
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
    enabled: localStorage.getItem("autoWifiAutoConnect") === "true",
    ssid: localStorage.getItem("autoWifiSsid") || "RTT / IEEE 802.11",
    password: localStorage.getItem("autoWifiPassword") || "1234qwer",
  },
  retryCount: Number(localStorage.getItem("autoRetryCount") || "5"),
  timeoutSecs: Number(localStorage.getItem("autoTestTimeout") || "86400"),
  devices: [],
  selected: new Set(),
  selectedMode: "Cuci SMR",
  running: false,
  logTabs: new Map(),
  activeLogTab: "",
  suiteStatuses: new Map(),
  summaries: new Map(),
  resultDir: "",
  runStartedAt: null,
};

const app = document.querySelector("#app");

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
      <button class="icon-button" id="settingsBtn" title="Settings">⚙</button>
    </header>

    <aside class="devices-pane">
      <div class="pane-head">
        <h2>DEVICES</h2>
        <div class="pane-actions">
          <button class="mini-button" id="unselectBtn">Select</button>
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

      <section class="running-log">
        <div class="log-head">
          <h2>RUNNING LOG</h2>
          <div class="log-tabs" id="logTabs"></div>
          <button id="clearLogBtn">[ Clear Log ]</button>
        </div>
        <pre id="logBox"></pre>
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
        <div class="pane-head compact"><h2>SUITES</h2></div>
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
  </div>
`;

const els = {
  deviceList: document.querySelector("#deviceList"),
  testArea: document.querySelector("#testArea"),
  logBox: document.querySelector("#logBox"),
  logTabs: document.querySelector("#logTabs"),
  runBtn: document.querySelector("#runBtn"),
  cancelBtn: document.querySelector("#cancelBtn"),
  openResultBtn: document.querySelector("#openResultBtn"),
  resultPill: document.querySelector("#resultPill"),
  unselectBtn: document.querySelector("#unselectBtn"),
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
};

els.retryInput.value = state.retryCount;
els.timeoutInput.value = state.timeoutSecs;
els.wifiInput.checked = state.wifi.enabled;

els.refreshBtn.addEventListener("click", refreshDevices);
els.clearLogBtn.addEventListener("click", () => {
  const tab = state.logTabs.get(state.activeLogTab);
  if (tab) tab.lines = [];
  renderLog();
});
els.unselectBtn.addEventListener("click", toggleAllReadyDevices);
els.settingsBtn.addEventListener("click", openSettings);
els.settingsCloseBtn.addEventListener("click", closeSettings);
els.settingsModal.addEventListener("click", (event) => {
  if (event.target === els.settingsModal) closeSettings();
});
els.defaultRootBtn.addEventListener("click", loadDefaultRoot);
els.preflightBtn.addEventListener("click", runPreflight);
els.settingsSaveBtn.addEventListener("click", saveSettings);
els.browseBtn.addEventListener("click", browseRoot);
els.runBtn.addEventListener("click", runSelected);
els.cancelBtn.addEventListener("click", cancelRun);
els.openResultBtn.addEventListener("click", openResult);
els.resultPill.addEventListener("click", openResult);
els.retryInput.addEventListener("change", saveInlineSettings);
els.timeoutInput.addEventListener("change", saveInlineSettings);
els.wifiInput.addEventListener("change", () => {
  state.wifi.enabled = els.wifiInput.checked;
  localStorage.setItem("autoWifiAutoConnect", String(state.wifi.enabled));
});

listen("gba-run-log", (event) => appendLog(String(event.payload || "")));
listen("gba-suite-status", (event) => {
  const payload = event.payload || {};
  state.suiteStatuses.set(`${payload.suite}:${payload.devices || ""}`, payload);
  renderSuiteStatus();
  renderMetrics();
});
listen("gba-summary", (event) => {
  const payload = event.payload || {};
  state.summaries.set(`${payload.suite}:${payload.devices || ""}`, payload);
  renderSuiteStatus();
  renderMetrics();
});
listen("gba-run-finished", (event) => {
  const payload = event.payload || {};
  state.running = false;
  state.resultDir = payload.result_dir || state.resultDir;
  els.runBtn.disabled = false;
  els.cancelBtn.disabled = true;
  els.openResultBtn.disabled = !state.resultDir;
  els.resultPill.disabled = !state.resultDir;
  els.resultPill.textContent = state.resultDir ? "Open" : "None";
  els.statusLine.textContent = payload.exit_code === 0 ? "Completed" : "Finished with issue";
  appendLog(`[runner] Finished exit=${payload.exit_code} result=${state.resultDir || "N/A"}`);
  renderMetrics();
});

init();

async function init() {
  render();
  await reconcileAutoRoot();
  await refreshDevices();
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
    if (!state.autoRoot || state.autoRoot !== root) {
      if (state.autoRoot) appendLog(`[settings] AUTO root updated: ${state.autoRoot} -> ${root}`);
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
    state.devices = await invoke("list_devices");
    const ready = new Set(state.devices.filter((d) => d.state === "device").map((d) => d.serial));
    state.selected = new Set([...state.selected].filter((serial) => ready.has(serial)));
    appendLog(`[adb] Found ${state.devices.length} device(s).`);
  } catch (error) {
    appendLog(`[adb] Refresh failed: ${error}`);
    state.devices = [];
  }
  renderDevices();
  els.deviceFooter.textContent = `${state.devices.length} detected`;
}

function render() {
  renderDevices();
  renderTestArea();
  renderLog();
  renderSuiteStatus();
  renderMetrics();
}

function renderDevices() {
  const readyDevices = state.devices.filter((device) => device.state === "device");
  els.unselectBtn.textContent = state.selected.size === readyDevices.length && readyDevices.length ? "Unselect" : "Select";

  if (!state.devices.length) {
    els.deviceList.innerHTML = `<div class="empty">No ADB devices detected.</div>`;
    return;
  }

  els.deviceList.innerHTML = state.devices.map((device) => {
    const ready = device.state === "device";
    const selected = state.selected.has(device.serial);
    const badge = device.is_userdebug ? "USERDEBUG" : "USER";
    return `
      <button class="device-card ${selected ? "selected" : ""} ${ready ? "" : "disabled"}" data-serial="${escapeHtml(device.serial)}" ${ready ? "" : "disabled"}>
        <div class="device-top">
          <span class="check-dot ${selected ? "checked" : ""}">${selected ? "✓" : ""}</span>
          <div>
            <strong>${escapeHtml(device.model || device.serial)}</strong>
            <p><b>${escapeHtml(device.serial)}</b> <span>${escapeHtml(device.state)}</span></p>
          </div>
          <span class="type-pill ${device.is_userdebug ? "debug" : ""}">${badge}</span>
        </div>
        <div class="device-meta">
          <span><small>ANDROID</small>${escapeHtml(device.android || "-")}</span>
          <span><small>SPL</small>${escapeHtml(device.security_patch || "-")}</span>
          <span><small>IP</small>${escapeHtml(device.ip || "N/A")}</span>
          <span><small>PDA</small>${escapeHtml(device.pda || "-")}</span>
          <span><small>CP</small>${escapeHtml(device.cp || "-")}</span>
          <span><small>CSC</small>${escapeHtml(device.csc || device.sales_code || "-")}</span>
        </div>
      </button>
    `;
  }).join("");

  els.deviceList.querySelectorAll(".device-card").forEach((card) => {
    card.addEventListener("click", () => {
      const serial = card.dataset.serial;
      if (state.selected.has(serial)) state.selected.delete(serial);
      else state.selected.add(serial);
      render();
    });
  });
}

function renderTestArea() {
  els.testArea.innerHTML = TEST_MODES.map((mode) => {
    const selected = state.selectedMode === mode.id;
    const requirement = requirementText(mode);
    return `
      <button class="mode-card ${selected ? "selected" : ""}" data-mode="${escapeHtml(mode.id)}">
        <header>
          <div>
            <h3>${escapeHtml(mode.name)}</h3>
            <p>${escapeHtml(mode.description)}</p>
          </div>
          <span>${escapeHtml(mode.flow)}</span>
        </header>
        <div class="mode-foot">
          <strong>${escapeHtml(requirement)}</strong>
          <span>${selected ? "SELECTED" : "READY"}</span>
        </div>
      </button>
    `;
  }).join("");

  els.testArea.querySelectorAll(".mode-card").forEach((card) => {
    card.addEventListener("click", () => {
      state.selectedMode = card.dataset.mode;
      renderTestArea();
    });
  });
}

function renderLog() {
  renderLogTabs();
  const tab = state.logTabs.get(state.activeLogTab);
  els.logBox.textContent = (tab?.lines || []).slice(-600).join("\n");
  els.logBox.scrollTop = els.logBox.scrollHeight;
}

function renderLogTabs() {
  if (!state.logTabs.size) {
    ensureLogTab("runner:boot", "Runner", "Boot");
  }
  els.logTabs.innerHTML = [...state.logTabs.values()].map((tab) => `
    <button class="log-tab ${tab.key === state.activeLogTab ? "active" : ""}" data-key="${escapeHtml(tab.key)}" title="${escapeHtml(tab.title)}">
      ${escapeHtml(tab.title)}
    </button>
  `).join("");
  els.logTabs.querySelectorAll(".log-tab").forEach((button) => {
    button.addEventListener("click", () => {
      state.activeLogTab = button.dataset.key;
      renderLog();
    });
  });
}

function renderSuiteStatus() {
  const statuses = [...state.suiteStatuses.values()];
  if (!statuses.length) {
    els.suiteList.innerHTML = `<div class="empty">No active suite.</div>`;
    return;
  }
  els.suiteList.innerHTML = statuses.map((status) => {
    const key = `${status.suite}:${status.devices || ""}`;
    const summary = state.summaries.get(key);
    const failed = summary?.failed ?? "-";
    const passed = summary?.passed ?? "-";
    return `
      <div class="suite-card">
        <div>
          <strong>${escapeHtml(status.suite || "-")}</strong>
          <p>${escapeHtml(status.devices || "-")}</p>
        </div>
        <span class="status-${statusClass(status.status)}">${escapeHtml(status.status || "Standby")}</span>
        <small>Pass ${passed} / Fail ${failed}</small>
      </div>
    `;
  }).join("");
}

function renderMetrics() {
  const statuses = [...state.suiteStatuses.values()];
  const summaries = [...state.summaries.values()];
  const active = statuses.filter((s) => !["Completed", "Cancelled", "Failed", "Timeout"].includes(s.status)).length;
  const completed = statuses.filter((s) => ["Completed", "Failed", "Timeout"].includes(s.status)).length;
  const passed = summaries.reduce((sum, s) => sum + Number(s.passed || 0), 0);
  const failed = summaries.reduce((sum, s) => sum + Number(s.failed || 0), 0);
  els.activeMetric.textContent = String(active);
  els.completedMetric.textContent = String(completed);
  els.passedMetric.textContent = String(passed);
  els.failedMetric.textContent = String(failed);
  els.runtimeMetric.textContent = state.runStartedAt ? formatDuration(Math.floor((Date.now() - state.runStartedAt) / 1000)) : "00:00:00";
}

function toggleAllReadyDevices() {
  const ready = state.devices.filter((device) => device.state === "device").map((device) => device.serial);
  if (ready.length && state.selected.size === ready.length) state.selected.clear();
  else state.selected = new Set(ready);
  render();
}

async function runSelected() {
  saveInlineSettings();
  const mode = TEST_MODES.find((item) => item.id === state.selectedMode);
  const selectedDevices = state.devices.filter((device) => state.selected.has(device.serial));
  const userDevices = selectedDevices.filter((device) => !device.is_userdebug).map((device) => device.serial);
  const userdebugDevices = selectedDevices.filter((device) => device.is_userdebug).map((device) => device.serial);
  const validation = validateRun(mode, userDevices, userdebugDevices);
  if (validation) {
    appendLog(`[runner] ${validation}`);
    els.statusLine.textContent = validation;
    return;
  }

  state.running = true;
  state.suiteStatuses.clear();
  state.summaries.clear();
  state.resultDir = "";
  state.runStartedAt = Date.now();
  createRunLogTabs(selectedDevices);
  els.runBtn.disabled = true;
  els.cancelBtn.disabled = false;
  els.openResultBtn.disabled = true;
  els.resultPill.disabled = true;
  els.resultPill.textContent = "None";
  els.statusLine.textContent = "Running";
  render();

  appendLog(`[runner] Starting ${state.selectedMode}: user=${userDevices.join(",") || "-"} userdebug=${userdebugDevices.join(",") || "-"}`);
  try {
    await invoke("run_suite", {
      request: {
            auto_root: state.autoRoot || await invoke("default_auto_root"),
        test_type: state.selectedMode,
        user_devices: userDevices,
        userdebug_devices: userdebugDevices,
        retry_count: state.retryCount,
        wifi_enabled: state.wifi.enabled,
        wifi_ssid: state.wifi.ssid,
        wifi_password: state.wifi.password,
        timeout_secs: state.timeoutSecs,
      },
    });
  } catch (error) {
    state.running = false;
    els.runBtn.disabled = false;
    els.cancelBtn.disabled = true;
    els.statusLine.textContent = "Run failed";
    appendLog(`[runner] Run failed: ${error}`);
  }
}

async function cancelRun() {
  appendLog("[runner] Cancel requested.");
  els.cancelBtn.disabled = true;
  els.statusLine.textContent = "Cancelling";
  try {
    await invoke("cancel_run");
  } catch (error) {
    appendLog(`[runner] Cancel failed: ${error}`);
    els.cancelBtn.disabled = false;
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
  if (mode.needs === "user" && userDevices.length === 0) return `${mode.name} needs at least one non-userdebug device.`;
  if (mode.needs === "userdebug" && userdebugDevices.length === 0) return `${mode.name} needs at least one userdebug device.`;
  if (mode.needs === "both" && (userDevices.length === 0 || userdebugDevices.length === 0)) return "SMR needs non-userdebug and userdebug devices.";
  if (!state.autoRoot) return "AUTO root is empty. Open settings and save root.";
  return "";
}

function appendLog(line) {
  const text = redact(String(line || ""));
  const stamp = new Date().toLocaleTimeString("en-GB", { hour12: false });
  const kind = logKind(text);
  const key = latestLogTabKey(kind) || ensureLogTab(`runner:${Date.now()}`, "Runner", "General");
  const tab = state.logTabs.get(key);
  tab.lines.push(`${stamp} ${text}`);
  renderLog();
}

function createRunLogTabs(devices) {
  const stamp = Date.now();
  const primary = devices[0] || {};
  const deviceTitle = `${primary.serial || "NO_SERIAL"} ${primary.pda || "NO_PDA"} ${primary.model || "NO_MODEL"}`;
  const runnerKey = ensureLogTab(`runner:${stamp}`, "Runner", `${state.selectedMode} ${deviceTitle}`);
  const mode = TEST_MODES.find((item) => item.id === state.selectedMode);
  if (mode?.needs === "user" || mode?.needs === "both") {
    ensureLogTab(`cts:${stamp}`, "CTS", deviceTitle);
    ensureLogTab(`gts:${stamp}`, "GTS", deviceTitle);
  }
  if (mode?.needs === "userdebug" || mode?.needs === "both") {
    const debugDevice = devices.find((device) => device.is_userdebug) || primary;
    const debugTitle = `${debugDevice.serial || "NO_SERIAL"} ${debugDevice.pda || "NO_PDA"} ${debugDevice.model || "NO_MODEL"}`;
    ensureLogTab(`sts:${stamp}`, "STS", debugTitle);
  }
  state.activeLogTab = runnerKey;
}

function ensureLogTab(key, kind, title) {
  if (!state.logTabs.has(key)) {
    state.logTabs.set(key, { key, kind, title: `${kind} | ${title}`, lines: [] });
  }
  if (!state.activeLogTab) state.activeLogTab = key;
  return key;
}

function latestLogTabKey(kind) {
  const normalized = kind.toLowerCase();
  const matches = [...state.logTabs.values()].filter((tab) => tab.kind.toLowerCase() === normalized);
  return matches.length ? matches[matches.length - 1].key : "";
}

function logKind(text) {
  const match = text.match(/^\[([^\]]+)\]/);
  const prefix = match ? match[1].toLowerCase() : "";
  if (prefix === "cts") return "CTS";
  if (prefix === "gts") return "GTS";
  if (prefix === "sts") return "STS";
  return "Runner";
}

function requirementText(mode) {
  if (mode.needs === "both") return "Requires USER + USERDEBUG";
  if (mode.needs === "userdebug") return "Requires USERDEBUG";
  return "Requires USER";
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
