#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tauri::{AppHandle, Emitter, Manager, State};
use tempfile::TempDir;
use walkdir::WalkDir;
use zip::ZipArchive;
use axum::{
    extract::{State as AxumState, Json, Query},
    routing::{get, post},
    Router,
};
use tokio::net::TcpListener;

const DEVICE_RECONNECT_TIMEOUT_SECS: u64 = 300;
// ponytail: serialize GTS tradefed to protect the shared ADB server; shard/process parallelism can be added if ADB isolation is available.
static GTS_RUN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static GHIDRA_PREP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
// ponytail: serialize STS because Tradefed GhidraPreparer shares /tmp/tradefed_ghidra; isolate temp dirs if parallel STS is needed.
static STS_RUN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Debug, Clone, Serialize)]
struct DeviceInfo {
    serial: String,
    state: String,
    is_userdebug: bool,
    fingerprint: String,
    security_patch: String,
    android: String,
    sdk: String,
    sales_code: String,
    model: String,
    pda: String,
    cp: String,
    csc: String,
    ip: String,
    busy: bool,
    busy_reason: String,
    run_id: Option<String>,
    result_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RunSuiteRequest {
    run_id: Option<String>,
    auto_root: String,
    test_type: String,
    laundry_zip_path: Option<String>,
    #[serde(default)]
    selected_laundry_results: Vec<String>,
    user_devices: Vec<String>,
    userdebug_devices: Vec<String>,
    retry_count: u32,
    wifi_enabled: bool,
    wifi_ssid: String,
    wifi_password: String,
    timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiRunRequest {
    #[serde(flatten)]
    suite_request: RunSuiteRequest,
    webhook_url: Option<String>,
}

#[derive(Clone)]
struct AppState {
    app_handle: AppHandle,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiPreflightRequest {
    auto_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiResetBusyStateRequest {
    auto_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiSetDeviceLampRequest {
    serial: String,
    brighten: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiAnalyzeLaundryZipRequest {
    zip_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiCheckLaundryMismatchesRequest {
    auto_root: String,
    zip_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct SuiteStatus {
    run_id: String,
    test_type: String,
    suite: String,
    status: String,
    devices: String,
    elapsed_secs: u64,
    log_file: String,
}

#[derive(Debug, Clone, Serialize)]
struct SuiteSummary {
    run_id: String,
    test_type: String,
    suite: String,
    devices: String,
    run_time: String,
    modules: String,
    total: u64,
    passed: u64,
    failed: u64,
}

#[derive(Debug, Clone, Serialize)]
struct RunFinished {
    run_id: String,
    test_type: String,
    exit_code: i32,
    result_dir: String,
    summary: Option<SuiteSummary>,
    zip_file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LaundryResultInfo {
    id: String,
    suite: String,
    testcase: String,
    subtestcases: String,
    status: String,
    time: String,
    total: u64,
    passed: u64,
    failed: u64,
    suite_version: String,
    result_dir: String,
}

#[derive(Debug, Clone, Serialize)]
struct LaundryResultUpdate {
    run_id: String,
    test_type: String,
    id: String,
    suite: String,
    status: String,
    time: String,
    total: u64,
    passed: u64,
    failed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BusyRegistry {
    devices: HashMap<String, BusyDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BusyDevice {
    serial: String,
    #[serde(default)]
    is_userdebug: bool,
    test_type: String,
    model: String,
    pda: String,
    run_id: String,
    started_at: String,
    result_dir: Option<String>,
    current_suite: Option<String>,
    #[serde(default)]
    suite_statuses: HashMap<String, SuiteRuntimeStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SuiteRuntimeStatus {
    status: String,
    elapsed_secs: u64,
    #[serde(default)]
    zip_file: Option<String>,
}

#[derive(Default)]
struct RunState {
    active: Mutex<ActiveRun>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LogFlowOption {
    id: String,
    title: String,
    run_id: String,
    result_dir: Option<String>,
}

#[derive(Default)]
struct ActiveRun {
    running: bool,
    pids: Vec<(String, u32)>,
    root: Option<PathBuf>,
    busy_serials: Vec<String>,
    log_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct SuiteOutcome {
    exit_code: i32,
    elapsed_secs: u64,
}

#[tauri::command]
fn default_auto_root() -> Result<String, String> {
    // ponytail: prefer the local system's AUTO directory if present
    let preferred = PathBuf::from("/run/media/endri-pro/BINARY_HDD/AUTO");
    if preferred.is_dir() {
        return Ok(preferred.display().to_string());
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest
        .parent()
        .unwrap_or(manifest.as_path())
        .display()
        .to_string())
}

#[tauri::command]
fn preflight(auto_root: Option<String>) -> Result<Vec<String>, String> {
    let root = resolve_auto_root(auto_root)?;
    let mut lines = vec![format!("Root: {}", root.display())];
    match prepare_ghidra_for_sts() {
        Ok(message) => lines.push(format!("OK Ghidra: {}", message.trim())),
        Err(error) => lines.push(format!("MISS Ghidra: {error}")),
    }
    lines.push(check_command("adb"));
    lines.push(check_command("java"));
    lines.push(check_dir("CTS", root.join("CTS")));
    lines.push(check_dir("GTS", root.join("GTS")));
    lines.push(check_dir("STS", root.join("STS")));
    lines.push(check_dir("Results", root.join("Results")));

    let gts_versions = available_suite_versions(&root, "GTS", "android-gts");
    if gts_versions.is_empty() {
        lines.push("MISS GTS/*/android-gts".to_string());
    }
    for version in gts_versions {
        let gts_root = root.join("GTS").join(&version).join("android-gts");
        lines.push(check_dir(&format!("GTS/{version}/android-gts"), &gts_root));
        lines.push(check_file(
            &format!("GTS/{version}/tools/gts-tradefed"),
            gts_root.join("tools/gts-tradefed"),
        ));
    }
    if let Ok(gts_root) = resolve_gts_root(&root, "") {
        let version = gts_root
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("default");
        lines.push(check_file(
            &format!("GTS/{version}/subplans/gtsmr.xml"),
            gts_root.join("subplans/gtsmr.xml"),
        ));
    }

    let cts_versions = available_suite_versions(&root, "CTS", "android-cts");
    if cts_versions.is_empty() {
        lines.push("MISS CTS/*/android-cts".to_string());
    }
    for version in cts_versions {
        let cts_root = root.join("CTS").join(&version).join("android-cts");
        lines.push(check_dir(&format!("CTS/{version}/android-cts"), &cts_root));
        lines.push(check_file(
            &format!("CTS/{version}/tools/cts-tradefed"),
            cts_root.join("tools/cts-tradefed"),
        ));
    }

    Ok(lines)
}

#[tauri::command]
fn list_devices() -> Result<Vec<DeviceInfo>, String> {
    let root = resolve_auto_root(None)?;
    let busy = read_busy_registry(&root);
    let output = run_output(Command::new(adb_path()).args(["devices", "-l"]))?;
    let mut devices = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("List of devices") || trimmed.starts_with('*')
        {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let serial = parts[0].to_string();
        let state = parts[1].to_string();
        let props = if state == "device" {
            device_props(&serial).unwrap_or_default()
        } else {
            HashMap::new()
        };
        let fingerprint = prop(&props, "ro.build.fingerprint");
        let ip = if state == "device" {
            device_ip(&serial).unwrap_or_default()
        } else {
            String::new()
        };

        let build_type = prop(&props, "ro.build.type").to_lowercase();
        let is_userdebug = fingerprint.to_lowercase().contains("userdebug") || build_type.contains("userdebug");

        let busy_entry = busy.devices.get(&serial);
        devices.push(DeviceInfo {
            serial: serial.clone(),
            state,
            is_userdebug,
            fingerprint,
            security_patch: prop(&props, "ro.build.version.security_patch"),
            android: prop(&props, "ro.build.version.release"),
            sdk: prop(&props, "ro.product.build.version.sdk"),
            sales_code: prop(&props, "ro.csc.sales_code"),
            model: first_non_empty(&[
                prop(&props, "ro.product.model"),
                token_value(trimmed, "model"),
            ]),
            pda: prop(&props, "ro.build.PDA"),
            cp: first_non_empty(&[prop(&props, "ril.sw_ver"), prop(&props, "gsm.version.baseband")]),
            csc: prop(&props, "ril.official_cscver"),
            ip,
            busy: busy_entry.is_some(),
            busy_reason: busy_entry
                .map(|entry| format!("{} {}", entry.test_type, entry.started_at))
                .unwrap_or_default(),
            run_id: busy_entry.map(|entry| entry.run_id.clone()),
            result_dir: busy_entry.and_then(|entry| entry.result_dir.clone()),
        });
    }

    Ok(devices)
}

#[tauri::command]
fn generate_ro_xml(serial: String, output_dir: String) -> Result<String, String> {
    generate_ro_xml_file(&serial, Path::new(&output_dir))
}

#[tauri::command]
fn analyze_laundry_zip(zip_path: String) -> Result<Vec<LaundryResultInfo>, String> {
    let zip_path = PathBuf::from(zip_path);
    if !zip_path.is_file() {
        return Err(format!("Laundry zip not found: {}", zip_path.display()));
    }

    let temp = tempfile::Builder::new()
        .prefix("gba-laundry-preview-")
        .tempdir()
        .map_err(|err| format!("Cannot create laundry preview dir: {err}"))?;
    extract_zip_safe(&zip_path, temp.path())?;
    extract_nested_zips(temp.path())?;

    let mut rows = scan_laundry_result_infos(temp.path())?;
    rows.sort_by(|a, b| {
        a.suite
            .cmp(&b.suite)
            .then(a.suite_version.cmp(&b.suite_version))
            .then(a.result_dir.cmp(&b.result_dir))
    });
    Ok(rows)
}

#[tauri::command]
fn check_laundry_mismatches(auto_root: String, zip_path: String) -> Result<Vec<String>, String> {
    let root = resolve_auto_root(Some(auto_root))?;
    let zip_path = PathBuf::from(zip_path);
    if !zip_path.is_file() {
        return Err(format!("Laundry zip not found: {}", zip_path.display()));
    }

    let temp = tempfile::Builder::new()
        .prefix("gba-laundry-mismatch-check-")
        .tempdir()
        .map_err(|err| format!("Cannot create temp dir: {err}"))?;
    extract_zip_safe(&zip_path, temp.path())?;
    extract_nested_zips(temp.path())?;

    let (cts_results, gts_results, sts_results) = scan_laundry_results(temp.path());
    let mut warnings = Vec::new();

    let suites = vec![
        ("CTS", &cts_results),
        ("GTS", &gts_results),
        ("STS", &sts_results),
    ];

    for (suite, result_dirs) in suites {
        for dir in result_dirs {
            let xml_path = dir.join("test_result.xml");
            if !xml_path.is_file() {
                continue;
            }
            let Some((name, version, build)) = get_suite_info_from_xml(&xml_path) else {
                continue;
            };

            // Try to resolve the suite root
            let suite_root = if suite == "CTS" || suite == "GTS" {
                if let Some(v) = suite_version_from_result(dir) {
                    if suite == "CTS" {
                        resolve_cts_root(&root, &v).ok()
                    } else {
                        resolve_gts_root(&root, &v).ok()
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // If we could not resolve it or it resolved but doesn't exist, we fall back to generic suite folder
            let suite_root = suite_root.unwrap_or_else(|| root.join(suite));

            let version_txt = suite_root.join("tools/version.txt");
            let local_version = if version_txt.is_file() {
                fs::read_to_string(&version_txt)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            if !local_version.is_empty() && !build.is_empty() && build != local_version {
                let normalized_version = suite_version_from_result_version(&version);
                let local_folder_version = suite_root
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|f| f.to_str())
                    .unwrap_or("");
                let is_same_version = !local_folder_version.is_empty()
                    && normalized_version.as_deref() == Some(local_folder_version);

                warnings.push(format!(
                    "Mismatched tools version for {suite} (ignored: {ignored}):\n\
                     Laundry file has version: {name} {version} ({build})\n\
                     Local tool has version: ({local_version})\n\
                     Please align laundry file and local tools.",
                    suite = suite,
                    ignored = is_same_version,
                    name = name,
                    version = version,
                    build = build,
                    local_version = local_version
                ));
            }
        }
    }

    Ok(warnings)
}

#[tauri::command]
fn run_suite(
    app: AppHandle,
    run_state: State<'_, RunState>,
    request: RunSuiteRequest,
) -> Result<(), String> {
    let root = resolve_auto_root(Some(request.auto_root.clone()))?;
    validate_request(&request)?;
    let run_id = request
        .run_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{}_{}", sanitize_name(&request.test_type), timestamp_compact()));
    {
        let mut active = run_state.active.lock().map_err(|err| err.to_string())?;
        let selected = selected_serials(&request);
        let busy = read_busy_registry(&root);
        let busy_selected = selected
            .iter()
            .filter(|serial| busy.devices.contains_key(serial.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !busy_selected.is_empty() {
            return Err(format!("Device busy: {}", busy_selected.join(", ")));
        }
        mark_busy_devices(&root, &request, &selected, &run_id)?;
        active.running = true;
        active.root = Some(root.clone());
        active.busy_serials.extend(selected);
        active.busy_serials.sort();
        active.busy_serials.dedup();
    }

    thread::spawn(move || {
        let busy_serials = selected_serials(&request);
        let result = run_suite_blocking(app.clone(), root, request.clone(), run_id.clone());
        let exit_code = match result {
            Ok(code) => code,
            Err(err) => {
                emit_log(&app, format!("[runner] {err}"));
                if err.contains("not found") {
                    let _ = app.emit("gba-tool-error", err.clone());
                }
                1
            }
        };

        if let Ok(mut active) = app.state::<RunState>().active.lock() {
            active.busy_serials.retain(|serial| !busy_serials.contains(serial));
            active.running = !active.busy_serials.is_empty() || !active.pids.is_empty();
            if !active.running {
                active.root = None;
            }
        }
        clear_busy_devices(
            &resolve_auto_root(Some(request.auto_root.clone())).unwrap_or_else(|_| PathBuf::from(&request.auto_root)),
            &busy_serials,
        );

        let result_dir = result_dir_for_run(Path::new(&request.auto_root), &run_id)
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let _ = app.emit(
            "gba-run-finished",
            RunFinished {
                run_id,
                test_type: request.test_type.clone(),
                exit_code,
                result_dir,
                summary: None,
                zip_file: None,
            },
        );
    });

    Ok(())
}

async fn start_api_server(app_handle: AppHandle) {
    let state = AppState { app_handle };
    
    let app = Router::new()
        .route("/api/run-suite", post(api_run_suite))
        .route("/api/devices", get(api_list_devices))
        .route("/api/preflight", post(api_preflight))
        .route("/api/cancel-run", post(api_cancel_run))
        .route("/api/reset-busy-state", post(api_reset_busy_state))
        .route("/api/set-device-lamp", post(api_set_device_lamp))
        .route("/api/analyze-laundry-zip", post(api_analyze_laundry_zip))
        .route("/api/check-laundry-mismatches", post(api_check_laundry_mismatches))
        .route("/api/logs", get(api_get_logs))
        .route("/api/run-logs", get(api_get_run_logs))
        .route("/api/run-zip", get(api_get_run_zip))
        .route("/api/status", get(api_get_status))
        .route("/api/diagnostics", get(api_diagnostics))
        .route("/api/download", get(api_download_file))
        .route("/api/screenshot", get(api_screenshot))
        .with_state(state);
        
    if let Ok(listener) = TcpListener::bind("0.0.0.0:3030").await {
        println!("Local API server listening on 0.0.0.0:3030");
        let _ = axum::serve(listener, app).await;
    }
}

async fn api_list_devices() -> axum::response::Json<Vec<DeviceInfo>> {
    match list_devices() {
        Ok(devices) => axum::response::Json(devices),
        Err(_) => axum::response::Json(vec![]),
    }
}

async fn api_run_suite(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ApiRunRequest>,
) -> axum::response::Json<serde_json::Value> {
    let app = state.app_handle.clone();
    let run_state_managed = app.state::<RunState>();
    
    let request = payload.suite_request;
    
    let root_res = resolve_auto_root(Some(request.auto_root.clone()));
    if root_res.is_err() {
        return axum::response::Json(serde_json::json!({ "error": "Invalid auto_root" }));
    }
    let root = root_res.unwrap();
    
    if let Err(e) = validate_request(&request) {
        return axum::response::Json(serde_json::json!({ "error": e }));
    }
    
    let run_id = request
        .run_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{}_{}", sanitize_name(&request.test_type), timestamp_compact()));
        
    {
        let mut active = match run_state_managed.active.lock() {
            Ok(a) => a,
            Err(_) => return axum::response::Json(serde_json::json!({ "error": "Internal state error" })),
        };
        let selected = selected_serials(&request);
        let busy = read_busy_registry(&root);
        let busy_selected = selected
            .iter()
            .filter(|serial| busy.devices.contains_key(serial.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !busy_selected.is_empty() {
            return axum::response::Json(serde_json::json!({ "error": format!("Device busy: {}", busy_selected.join(", ")) }));
        }
        if let Err(e) = mark_busy_devices(&root, &request, &selected, &run_id) {
            return axum::response::Json(serde_json::json!({ "error": e }));
        }
        active.running = true;
        active.root = Some(root.clone());
        active.busy_serials.extend(selected);
        active.busy_serials.sort();
        active.busy_serials.dedup();
    }
    
    let selected_serials_started = selected_serials(&request);
    let _ = app.emit("gba-run-started", serde_json::json!({
        "run_id": run_id.clone(),
        "test_type": request.test_type.clone(),
        "selected_devices": selected_serials_started,
    }));
    
    let run_id_clone = run_id.clone();
    let webhook_url = payload.webhook_url;
    
    thread::spawn(move || {
        let busy_serials = selected_serials(&request);
        let result = run_suite_blocking(app.clone(), root, request.clone(), run_id_clone.clone());
        let exit_code = match result {
            Ok(code) => code,
            Err(err) => {
                emit_log(&app, format!("[runner] {err}"));
                if err.contains("not found") {
                    let _ = app.emit("gba-tool-error", err.clone());
                }
                1
            }
        };

        let log_file = {
            let run_state = app.state::<RunState>();
            let mut active = run_state.active.lock().unwrap();
            active.busy_serials.retain(|serial| !busy_serials.contains(serial));
            active.running = !active.busy_serials.is_empty() || !active.pids.is_empty();
            let lf = active.log_file.clone();
            if !active.running {
                active.root = None;
                active.log_file = None;
            }
            lf
        };

        clear_busy_devices(
            &resolve_auto_root(Some(request.auto_root.clone())).unwrap_or_else(|_| PathBuf::from(&request.auto_root)),
            &busy_serials,
        );

        let result_dir = result_dir_for_run(Path::new(&request.auto_root), &run_id_clone)
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let result_zip = first_zip(Path::new(&result_dir)).and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string()));
        let summary = log_file.map(|lf| parse_summary(&lf, "Test", &busy_serials.join(","), &run_id_clone, &request.test_type));

        let finished_payload = RunFinished {
            run_id: run_id_clone.clone(),
            test_type: request.test_type.clone(),
            exit_code,
            result_dir: result_dir.clone(),
            summary,
            zip_file: result_zip,
        };
        let last_status_file = PathBuf::from(&request.auto_root).join(".gba-agentic-last-status");
        if let Ok(json_str) = serde_json::to_string(&finished_payload) {
            let _ = fs::write(last_status_file, json_str);
        }
        let _ = app.emit("gba-run-finished", finished_payload.clone());
        
        // Call webhook if provided
        if let Some(url) = webhook_url {
            let _ = thread::spawn(move || {
                let client = reqwest::blocking::Client::new();
                let _ = client.post(&url).json(&finished_payload).send();
            });
        }
    });

    axum::response::Json(serde_json::json!({
        "status": "started",
        "run_id": run_id,
    }))
}

async fn api_preflight(
    Json(payload): Json<ApiPreflightRequest>,
) -> axum::response::Json<serde_json::Value> {
    match preflight(payload.auto_root) {
        Ok(res) => axum::response::Json(serde_json::json!({ "result": res })),
        Err(e) => axum::response::Json(serde_json::json!({ "error": e })),
    }
}

#[derive(Deserialize)]
struct DiagnosticsParams {
    kind: String,
}

async fn api_diagnostics(
    Query(params): Query<DiagnosticsParams>,
) -> axum::response::Json<serde_json::Value> {
    let result = match params.kind.as_str() {
        "resource" => Command::new("sh")
            .args(["-c", "mem=$(free -h | awk 'NR==2 {print $3 \" / \" $2}'); disk=$(df -h / | awk 'NR==2 {print $3 \" / \" $2 \" (\" $5 \")\"}'); load=$(uptime | sed 's/.*load average: //'); printf '📊 PC RESOURCE\\n\\n%-14s │ %s\\n%-14s │ %s\\n%-14s │ %s\\n' 'Memory' \"$mem\" 'Disk /' \"$disk\" 'Load Average' \"$load\""])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
            .unwrap_or_else(|err| format!("PC resource failed: {err}")),
        "speed" => Command::new("sh")
            .args(["-c", "download=$(curl -L -sS -o /dev/null -w '%{speed_download}' --max-time 15 'https://speed.cloudflare.com/__down?bytes=10000000'); upload=$(head -c 10000000 /dev/zero | curl -L -sS -X POST --data-binary @- -o /dev/null -w '%{speed_upload}' --max-time 15 'https://speed.cloudflare.com/__up'); awk -v d=\"$download\" -v u=\"$upload\" 'BEGIN {printf \"🌐 INTERNET SPEED\\n\\n%-10s │ %.2f MB/s\\n%-10s │ %.2f MB/s\\n\", \"Download\", d*8/1000000, \"Upload\", u*8/1000000}'"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
            .unwrap_or_else(|err| format!("Internet speed failed: {err}")),
        "tools" => tools_version_table(),
        _ => "Unknown diagnostics action".to_string(),
    };
    axum::response::Json(serde_json::json!({ "kind": params.kind, "result": result }))
}

fn tools_version_table() -> String {
    let root = match resolve_auto_root(None) {
        Ok(root) => root,
        Err(err) => return format!("Tools version failed: {err}"),
    };
    let mut rows = Vec::new();
    for entry in WalkDir::new(&root).into_iter().flatten() {
        if !entry.file_type().is_file() || !entry.file_name().to_string_lossy().ends_with("-tradefed.jar") {
            continue;
        }
        let Ok(file) = File::open(entry.path()) else { continue };
        let Ok(mut jar) = ZipArchive::new(file) else { continue };
        let Ok(mut info) = jar.by_name("test-suite-info.properties") else { continue };
        let mut text = String::new();
        if std::io::Read::read_to_string(&mut info, &mut text).is_err() { continue; }
        let value = |key: &str| text.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().to_string())
        }).unwrap_or_default();
        let suite = entry.path().components().find_map(|part| {
            let value = part.as_os_str().to_string_lossy().to_uppercase();
            ["CTS", "GTS", "STS"].into_iter().find(|name| value == *name).map(str::to_string)
        }).unwrap_or_else(|| "TEST".to_string());
        rows.push(vec![suite, value("version"), value("build_number")]);
    }
    rows.sort_by(|a, b| a[0].cmp(&b[0]).then(a[1].cmp(&b[1])).then(a[2].cmp(&b[2])));
    let headers = ["Suite", "Version", "Build"];
    let widths = (0..headers.len()).map(|i| rows.iter().map(|row| row[i].len()).max().unwrap_or(0).max(headers[i].len())).collect::<Vec<_>>();
    let row = |values: &[String]| format!("│ {} │", values.iter().enumerate().map(|(i, value)| format!("{:width$}", value, width = widths[i])).collect::<Vec<_>>().join(" │ "));
    let mut output = row(&headers.iter().map(|v| v.to_string()).collect::<Vec<_>>());
    for values in rows { output.push('\n'); output.push_str(&row(&values)); }
    output
}

#[derive(serde::Deserialize, Default)]
struct CancelRunPayload {
    run_id: Option<String>,
}

async fn api_cancel_run(
    AxumState(state): AxumState<AppState>,
    payload: Option<axum::extract::Json<CancelRunPayload>>,
) -> axum::response::Json<serde_json::Value> {
    let app = state.app_handle;
    let run_state = app.state::<RunState>();
    let run_id = payload.map(|p| p.0.run_id).unwrap_or(None);
    match cancel_run(app.clone(), run_state, run_id) {
        Ok(_) => axum::response::Json(serde_json::json!({ "status": "cancelled" })),
        Err(e) => axum::response::Json(serde_json::json!({ "error": e })),
    }
}

async fn api_reset_busy_state(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<ApiResetBusyStateRequest>,
) -> axum::response::Json<serde_json::Value> {
    match reset_busy_state_helper(&state.app_handle, payload.auto_root) {
        Ok(_) => axum::response::Json(serde_json::json!({ "status": "reset" })),
        Err(e) => axum::response::Json(serde_json::json!({ "error": e })),
    }
}

async fn api_set_device_lamp(
    Json(payload): Json<ApiSetDeviceLampRequest>,
) -> axum::response::Json<serde_json::Value> {
    match set_device_lamp(payload.serial, payload.brighten) {
        Ok(_) => axum::response::Json(serde_json::json!({ "status": "updated" })),
        Err(e) => axum::response::Json(serde_json::json!({ "error": e })),
    }
}

async fn api_analyze_laundry_zip(
    Json(payload): Json<ApiAnalyzeLaundryZipRequest>,
) -> axum::response::Json<serde_json::Value> {
    match analyze_laundry_zip(payload.zip_path) {
        Ok(res) => axum::response::Json(serde_json::json!({ "result": res })),
        Err(e) => axum::response::Json(serde_json::json!({ "error": e })),
    }
}

async fn api_check_laundry_mismatches(
    Json(payload): Json<ApiCheckLaundryMismatchesRequest>,
) -> axum::response::Json<serde_json::Value> {
    match check_laundry_mismatches(payload.auto_root, payload.zip_path) {
        Ok(res) => axum::response::Json(serde_json::json!({ "result": res })),
        Err(e) => axum::response::Json(serde_json::json!({ "error": e })),
    }
}

use axum::response::IntoResponse;

#[derive(Deserialize)]
struct DownloadParams {
    path: String,
}

async fn api_download_file(
    Query(params): Query<DownloadParams>,
) -> impl IntoResponse {
    let path = PathBuf::from(&params.path);
    if let Ok(bytes) = std::fs::read(&path) {
        let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let disposition = format!("attachment; filename=\"{}\"", filename);
        axum::response::Response::builder()
            .header(axum::http::header::CONTENT_TYPE, "application/zip")
            .header(axum::http::header::CONTENT_DISPOSITION, disposition)
            .body(axum::body::Body::from(bytes))
            .unwrap()
    } else {
        axum::response::Response::builder()
            .status(axum::http::StatusCode::NOT_FOUND)
            .body(axum::body::Body::from("File not found"))
            .unwrap()
    }
}

async fn api_get_logs(
    AxumState(state): AxumState<AppState>,
) -> axum::response::Json<serde_json::Value> {
    let app = state.app_handle;
    let (log_file, root) = {
        let run_state = app.state::<RunState>();
        let active = run_state.active.lock().unwrap();
        (active.log_file.clone(), active.root.clone())
    };
    let root = root.or_else(|| resolve_auto_root(None).ok());
    let flow_options = root.as_deref().map(log_flow_options).unwrap_or_default();
    if let Some(path) = log_file {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = if lines.len() > 100 { lines.len() - 100 } else { 0 };
            let last_lines = lines[start..].join("\n");
            return axum::response::Json(serde_json::json!({ "logs": last_lines, "flow_options": flow_options }));
        }
    }
    axum::response::Json(serde_json::json!({ "logs": "No active logs found.", "flow_options": flow_options }))
}

#[derive(Deserialize)]
struct RunLogsParams {
    result_dir: Option<String>,
    run_id: Option<String>,
    suite: Option<String>,
}

#[derive(Deserialize)]
struct RunZipParams {
    run_id: String,
    zip_id: usize,
}

async fn api_get_run_zip(
    Query(params): Query<RunZipParams>,
) -> impl IntoResponse {
    let Some(result_dir) = resolve_log_result_dir(&params.run_id) else {
        return axum::response::Response::builder().status(axum::http::StatusCode::NOT_FOUND).body(axum::body::Body::from("Run not found")).unwrap();
    };
    let mut files = fs::read_dir(&result_dir).ok().into_iter().flatten()
        .flatten().map(|entry| entry.path()).filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("zip"))
        .collect::<Vec<_>>();
    files.sort();
    let Some(path) = files.get(params.zip_id) else {
        return axum::response::Response::builder().status(axum::http::StatusCode::NOT_FOUND).body(axum::body::Body::from("Zip not found")).unwrap();
    };
    match fs::read(path) {
        Ok(bytes) => axum::response::Response::builder().header(axum::http::header::CONTENT_TYPE, "application/zip").header(axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", path.file_name().unwrap_or_default().to_string_lossy())).body(axum::body::Body::from(bytes)).unwrap(),
        Err(_) => axum::response::Response::builder().status(axum::http::StatusCode::NOT_FOUND).body(axum::body::Body::from("Zip not found")).unwrap(),
    }
}

async fn api_get_run_logs(
    Query(params): Query<RunLogsParams>,
) -> axum::response::Json<serde_json::Value> {
    let result_dir = params
        .result_dir
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| params.run_id.as_deref().and_then(resolve_log_result_dir))
        .unwrap_or_default();
    let log_dir = result_dir.join("Log");
    let requested_suite = params.suite.as_deref().unwrap_or("RUNNER").to_uppercase();
    let requested_suite = match requested_suite.as_str() {
        "RUNNER" | "CTS" | "GTS" | "STS" | "ALL" => requested_suite,
        _ => "ALL".to_string(),
    };
    let mut combined_logs = String::new();
    let mut available_suites = HashSet::new();
    let flow_options = result_dir
        .parent()
        .and_then(Path::parent)
        .map(log_flow_options)
        .unwrap_or_default();
    
    // Always try to load the master run.log first
    let run_log = result_dir.join("run.log");
    if let Ok(content) = std::fs::read_to_string(&run_log) {
        available_suites.insert("RUNNER");
        if requested_suite == "ALL" || requested_suite == "RUNNER" {
        let lines: Vec<&str> = content.lines().collect();
        let start = if lines.len() > 80 { lines.len() - 80 } else { 0 };
        let last_lines = lines[start..].join("\n");
        combined_logs.push_str(&format!("--- LOG FILE: run.log ---\n{}\n\n", last_lines));
        }
    }

    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        let mut log_files = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("log") {
                log_files.push(path);
            }
        }
        
        // Sort files by modification time
        log_files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        
        for path in log_files {
            let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Some(suite) = log_suite_name(&filename) else { continue };
            available_suites.insert(suite);
            if requested_suite != "ALL" && requested_suite != suite {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                let start = if lines.len() > 80 { lines.len() - 80 } else { 0 };
                let last_lines = lines[start..].join("\n");
                combined_logs.push_str(&format!("--- LOG FILE: {} ---\n{}\n\n", filename, last_lines));
            }
        }
    }
    let mut suite_options = vec!["ALL".to_string()];
    suite_options.extend(
        ["RUNNER", "CTS", "GTS", "STS"]
            .into_iter()
            .filter(|suite| available_suites.contains(suite))
            .map(str::to_string),
    );
    let selected_suite = requested_suite;
    let zip_options = fs::read_dir(&result_dir).ok().into_iter().flatten()
        .flatten().map(|entry| entry.path()).filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("zip"))
        .collect::<Vec<_>>();
    let mut zip_options = zip_options;
    zip_options.sort();
    let zip_options = zip_options.iter().enumerate().map(|(id, path)| serde_json::json!({ "id": id, "title": path.file_name().unwrap_or_default().to_string_lossy() })).collect::<Vec<_>>();
    if combined_logs.is_empty() {
        axum::response::Json(serde_json::json!({
            "selected_suite": selected_suite,
            "suite_options": suite_options,
            "flow_options": flow_options,
            "zip_options": zip_options,
            "logs": "No log files found."
        }))
    } else {
        axum::response::Json(serde_json::json!({
            "selected_suite": selected_suite,
            "suite_options": suite_options,
            "flow_options": flow_options,
            "zip_options": zip_options,
            "logs": combined_logs
        }))
    }
}

fn log_flow_options(root: &Path) -> Vec<LogFlowOption> {
    let mut groups: HashMap<String, LogFlowOption> = HashMap::new();
    if let Ok(entries) = fs::read_dir(root.join("Results")) {
        for entry in entries.flatten() {
            let result_dir = entry.path();
            let metadata = result_dir.join(".gba-flow.json");
            if let Ok(content) = fs::read_to_string(metadata) {
                if let Ok(option) = serde_json::from_str::<LogFlowOption>(&content) {
                    groups.insert(option.run_id.clone(), option);
                }
            } else if result_dir.is_dir() && result_dir.join("Log").is_dir() {
                let parts = result_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .split('_')
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if let Some(devs) = parts.iter().position(|part| part.ends_with("devs")) {
                    if devs >= 2 {
                        let test_type = parts[..devs - 2].join(" ");
                        let model = &parts[devs - 2];
                        let pda = &parts[devs - 1];
                        let folder_name = result_dir
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let mut hasher = DefaultHasher::new();
                        folder_name.hash(&mut hasher);
                        let run_id = format!("legacy_{:x}", hasher.finish());
                        groups.entry(run_id.clone()).or_insert_with(|| LogFlowOption {
                            id: run_id.clone(),
                            title: format!("{} | {} | {}", test_type, model, pda),
                            run_id,
                            result_dir: Some(result_dir.display().to_string()),
                        });
                    }
                }
            }
        }
    }
    for device in read_busy_registry(root).devices.into_values() {
        let entry = groups.entry(device.run_id.clone()).or_insert_with(|| LogFlowOption {
            id: device.run_id.clone(),
            title: format!(
                "{} | {} | {} [{}]",
                device.test_type, device.model, device.pda, device.serial
            ),
            run_id: device.run_id.clone(),
            result_dir: device.result_dir.clone(),
        });
        if !entry.title.ends_with(&format!("[{}]", device.serial)) {
            entry.title = entry.title.trim_end_matches(']').to_string();
            entry.title.push_str(&format!(",{}]", device.serial));
        }
        if entry.result_dir.is_none() {
            entry.result_dir = device.result_dir;
        }
    }
    let mut options: Vec<_> = groups.into_values().collect();
    options.sort_by(|a, b| a.title.cmp(&b.title));
    options
}

fn resolve_log_result_dir(run_id: &str) -> Option<PathBuf> {
    let root = resolve_auto_root(None).ok()?;
    log_flow_options(&root)
        .into_iter()
        .find(|option| option.run_id == run_id)
        .and_then(|option| option.result_dir)
        .map(PathBuf::from)
}

fn log_suite_name(filename: &str) -> Option<&'static str> {
    let lower = filename.to_lowercase();
    if lower.contains("cts") {
        Some("CTS")
    } else if lower.contains("gts") {
        Some("GTS")
    } else if lower.contains("sts") {
        Some("STS")
    } else {
        None
    }
}

async fn api_get_status() -> axum::response::Json<serde_json::Value> {
    let root = match resolve_auto_root(None) {
        Ok(r) => r,
        Err(_) => return axum::response::Json(serde_json::json!({ "status": "IDLE", "error": "Invalid auto_root" })),
    };
    
    // 1. Check if there are active running devices
    let busy = read_busy_registry(&root);
    if !busy.devices.is_empty() {
        let devices: Vec<BusyDevice> = busy.devices.values().cloned().collect();
        return axum::response::Json(serde_json::json!({
            "status": "RUNNING",
            "running_devices": devices,
            "suites": running_suite_statuses(&busy),
            "log_options": log_flow_options(&root),
            "last_run": serde_json::Value::Null,
        }));
    }
    
    // 2. Check last finished status
    let last_status_file = root.join(".gba-agentic-last-status");
    if last_status_file.is_file() {
        if let Ok(content) = std::fs::read_to_string(&last_status_file) {
            if let Ok(last_run) = serde_json::from_str::<serde_json::Value>(&content) {
                let exit_code = last_run.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(0);
                let status_str = if exit_code == 0 { "DONE" } else { "FAILED" };
                return axum::response::Json(serde_json::json!({
                    "status": status_str,
                    "running_devices": Vec::<BusyDevice>::new(),
                    "log_options": log_flow_options(&root),
                    "last_run": last_run,
                }));
            }
        }
    }
    
    axum::response::Json(serde_json::json!({
        "status": "IDLE",
        "running_devices": Vec::<BusyDevice>::new(),
        "log_options": Vec::<LogFlowOption>::new(),
        "last_run": serde_json::Value::Null,
    }))
}

fn running_suite_statuses(registry: &BusyRegistry) -> Vec<serde_json::Value> {
    let mut grouped: HashMap<(String, String), (String, u64, Vec<String>, Option<String>)> = HashMap::new();
    for device in registry.devices.values() {
        for (suite, detail) in &device.suite_statuses {
            let key = (device.run_id.clone(), suite.clone());
            let entry = grouped.entry(key).or_insert_with(|| (detail.status.clone(), detail.elapsed_secs, Vec::new(), detail.zip_file.clone()));
            entry.0 = detail.status.clone();
            entry.1 = entry.1.max(detail.elapsed_secs);
            entry.2.push(device.serial.clone());
            if detail.zip_file.is_some() {
                entry.3 = detail.zip_file.clone();
            }
        }
    }
    grouped.into_iter().map(|((run_id, suite), (status, elapsed_secs, devices, zip_file))| {
        serde_json::json!({
            "run_id": run_id,
            "suite": suite,
            "status": status,
            "elapsed_secs": elapsed_secs,
            "devices": devices,
            "zip_file": zip_file,
        })
    }).collect()
}

#[derive(Deserialize)]
struct ScreenshotParams {
    serial: String,
}

async fn api_screenshot(
    Query(params): Query<ScreenshotParams>,
) -> impl IntoResponse {
    let output = std::process::Command::new("adb")
        .args(&["-s", &params.serial, "exec-out", "screencap", "-p"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            axum::response::Response::builder()
                .header(axum::http::header::CONTENT_TYPE, "image/png")
                .body(axum::body::Body::from(out.stdout))
                .unwrap()
        }
        _ => {
            axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::from("Failed to capture screenshot"))
                .unwrap()
        }
    }
}

#[tauri::command]
fn cancel_run(app: AppHandle, run_state: State<'_, RunState>, run_id: Option<String>) -> Result<(), String> {
    let pids: Vec<u32> = {
        let active = run_state.active.lock().map_err(|err| err.to_string())?;
        if !active.running {
            emit_log(&app, "[runner] No active run to cancel.");
            return Ok(());
        }
        if let Some(target) = run_id {
            active.pids.iter().filter(|(r, _)| r == &target).map(|(_, p)| *p).collect()
        } else {
            active.pids.iter().map(|(_, p)| *p).collect()
        }
    };

    emit_log(&app, format!("[runner] Terminating {} process(es).", pids.len()));
    for pid in pids {
        terminate_process_tree(pid);
    }
    if let Ok(mut active) = run_state.active.lock() {
        if let Some(root) = active.root.clone() {
            clear_busy_devices(&root, &active.busy_serials);
        }
        active.running = false;
        active.pids.clear();
        active.root = None;
        active.busy_serials.clear();
    }
    let _ = app.emit(
        "gba-suite-status",
        SuiteStatus {
            run_id: "cancel".to_string(),
            test_type: "ALL".to_string(),
            suite: "ALL".to_string(),
            status: "Cancelled".to_string(),
            devices: String::new(),
            elapsed_secs: 0,
            log_file: String::new(),
        },
    );
    Ok(())
}

#[tauri::command]
fn open_result(path: String) -> Result<(), String> {
    open_path(Path::new(&path))
}

fn reset_busy_state_helper(app: &tauri::AppHandle, auto_root: Option<String>) -> Result<(), String> {
    let run_state = app.state::<RunState>();
    if let Ok(mut active) = run_state.active.lock() {
        active.running = false;
        active.busy_serials.clear();
        active.pids.clear();
        active.root = None;
        active.log_file = None;
    }
    let root = resolve_auto_root(auto_root)?;
    let path = busy_registry_path(&root);
    if path.exists() {
        fs::remove_file(&path).map_err(|err| format!("Cannot remove {}: {err}", path.display()))?;
    }
    Ok(())
}

#[tauri::command]
fn reset_busy_state(app: tauri::AppHandle, auto_root: Option<String>) -> Result<(), String> {
    reset_busy_state_helper(&app, auto_root)
}

#[tauri::command]
fn set_device_lamp(serial: String, brighten: bool) -> Result<(), String> {
    if brighten {
        let _ = adb_device_output(&serial, &["shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
        let _ = adb_device_output(&serial, &["shell", "input", "keyevent", "KEYCODE_HOME"]);
        adb_device_output(
            &serial,
            &["shell", "settings", "put", "system", "screen_brightness_mode", "0"],
        )?;
        adb_device_output(
            &serial,
            &["shell", "settings", "put", "system", "screen_brightness", "255"],
        )?;
        adb_device_output(
            &serial,
            &["shell", "settings", "put", "system", "screen_off_timeout", "600000"],
        )?;
    } else {
        adb_device_output(
            &serial,
            &["shell", "settings", "put", "system", "screen_brightness_mode", "0"],
        )?;
        adb_device_output(
            &serial,
            &["shell", "settings", "put", "system", "screen_brightness", "10"],
        )?;
        adb_device_output(
            &serial,
            &["shell", "settings", "put", "system", "screen_off_timeout", "60000"],
        )?;
    }
    Ok(())
}

#[tauri::command]
fn open_scrcpy(serial: String) -> Result<(), String> {
    if which("scrcpy").is_none() {
        return Err("scrcpy command not found in PATH".to_string());
    }
    let mut command = Command::new("scrcpy");
    command
        .args(["-s", &serial])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
        .spawn()
        .map_err(|err| format!("Failed to start scrcpy for {serial}: {err}"))?;
    Ok(())
}

fn run_suite_blocking(app: AppHandle, root: PathBuf, request: RunSuiteRequest, run_id: String) -> Result<i32, String> {
    set_current_run_id(Some(run_id.clone()));
    fs::create_dir_all(root.join("Results")).map_err(|err| err.to_string())?;

    let all_devices = request
        .user_devices
        .iter()
        .chain(request.userdebug_devices.iter())
        .cloned()
        .collect::<Vec<_>>();
    let first = all_devices
        .first()
        .ok_or_else(|| "No selected devices".to_string())?;
    let first_props = device_props(first).unwrap_or_default();
    let model = first_non_empty(&[prop(&first_props, "ro.product.model"), "UNKNOWN".to_string()]);
    let pda = first_non_empty(&[prop(&first_props, "ro.build.PDA"), "UNKNOWN".to_string()]);
    let suffix = timestamp_compact();
    let session_name = format!(
        "{}_{}_{}_{}devs_{}_{}",
        sanitize_name(&request.test_type),
        sanitize_name(&model),
        sanitize_name(&pda),
        all_devices.len(),
        suffix,
        sanitize_name(&run_id)
    );
    let session_dir = root.join("Results").join(session_name);
    let log_dir = session_dir.join("Log");
    fs::create_dir_all(&log_dir).map_err(|err| format!("Cannot create result dir: {err}"))?;
    let flow_option = LogFlowOption {
        id: run_id.clone(),
        title: format!(
            "[{}] {} | {}\n{} [{}]",
            log_title_timestamp(),
            request.test_type,
            model,
            pda,
            all_devices.join(",")
        ),
        run_id: run_id.clone(),
        result_dir: Some(session_dir.display().to_string()),
    };
    let _ = fs::write(
        session_dir.join(".gba-flow.json"),
        serde_json::to_vec_pretty(&flow_option).unwrap_or_default(),
    );
    write_latest_result_hint(&root, &session_dir);
    
    update_busy_device_result_dir(&root, &run_id, &session_dir.display().to_string());
    
    // Register run.log immediately so early errors are captured
    let run_log = session_dir.join("run.log");
    register_log_file(&app, &run_log);
    
    emit_log(&app, format!("[runner] Result directory: {}", session_dir.display()));

    if request.wifi_enabled {
        emit_log(&app, "[wifi] Auto connect enabled.");
        let mut handles = Vec::new();
        for serial in &all_devices {
            let serial = serial.clone();
            let ssid = request.wifi_ssid.clone();
            let password = request.wifi_password.clone();
            let app_wifi = app.clone();
            handles.push(thread::spawn(move || {
                match connect_wifi(&serial, &ssid, &password) {
                    Ok(msg) => emit_log(&app_wifi, format!("[wifi][{serial}] {msg}")),
                    Err(err) => emit_log(&app_wifi, format!("[wifi][{serial}] failed: {}", redact(&err, &password))),
                }
            }));
        }
        for handle in handles {
            let _ = handle.join();
        }
    }

    if let Some(serial) = request.user_devices.first() {
        match generate_ro_xml_file(serial, &session_dir) {
            Ok(path) => emit_log(&app, format!("[roxml] Created {path}")),
            Err(err) => emit_log(&app, format!("[roxml] Warning: {err}")),
        }
    }

    prepare_devices(&app, &all_devices);

    let retry_args = retry_args(request.retry_count);
    let mut exit_codes = Vec::new();

    if matches!(request.test_type.as_str(), "STS" | "Laundry SMR") {
        ensure_ghidra_for_sts(&app)?;
    }

    match request.test_type.as_str() {
        "Laundry SMR" => {
            let outcome = run_laundry_smr(
                &app,
                &root,
                &session_dir,
                &log_dir,
                &request,
                &model,
                &pda,
                &run_id,
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "Laundry Normal" => {
            let outcome = run_laundry_normal(
                &app,
                &root,
                &session_dir,
                &log_dir,
                &request,
                &model,
                &pda,
                &run_id,
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "Cuci SMR" => {
            let outcome = run_cts_then_gts(
                &app,
                &root,
                &session_dir,
                &log_dir,
                &request.user_devices,
                "ctssmr",
                "run gts --subplan gtssmr",
                &retry_args,
                request.timeout_secs,
                &model,
                &pda,
                &run_id,
                &request.test_type,
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "MR" => {
            let outcome = run_cts_then_gts(
                &app,
                &root,
                &session_dir,
                &log_dir,
                &request.user_devices,
                "normal",
                "run gts --subplan normal",
                &retry_args,
                request.timeout_secs,
                &model,
                &pda,
                &run_id,
                &request.test_type,
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "SKU" => {
            let outcome = run_cts_then_gts(
                &app,
                &root,
                &session_dir,
                &log_dir,
                &request.user_devices,
                "ctssku",
                "run gts-variant",
                &retry_args,
                request.timeout_secs,
                &model,
                &pda,
                &run_id,
                &request.test_type,
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "STS" => {
            let outcome = run_sts(
                &app,
                &root,
                &session_dir,
                &log_dir,
                &request.userdebug_devices,
                &retry_args,
                request.timeout_secs,
                &model,
                &pda,
                &run_id,
                &request.test_type,
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "SMR" => {
            let sts_handle = if request.userdebug_devices.is_empty() {
                None
            } else {
                let app_sts = app.clone();
                let root_sts = root.clone();
                let session_sts = session_dir.clone();
                let log_sts = log_dir.clone();
                let userdebug = request.userdebug_devices.clone();
                let retry_sts = retry_args.clone();
                let model_sts = model.clone();
                let pda_sts = pda.clone();
                let timeout = request.timeout_secs;
                let run_id_sts = run_id.clone();
                let test_type_sts = request.test_type.clone();
                Some(thread::spawn(move || {
                    run_sts(
                        &app_sts,
                        &root_sts,
                        &session_sts,
                        &log_sts,
                        &userdebug,
                        &retry_sts,
                        timeout,
                        &model_sts,
                        &pda_sts,
                        &run_id_sts,
                        &test_type_sts,
                    )
                    .map(|outcome| outcome.exit_code)
                    .unwrap_or(1)
                }))
            };

            if !request.user_devices.is_empty() {
                let cts_gts = run_cts_then_gts(
                    &app,
                    &root,
                    &session_dir,
                    &log_dir,
                    &request.user_devices,
                    "ctssmr",
                    "run gts-smr",
                    &retry_args,
                    request.timeout_secs,
                    &model,
                    &pda,
                    &run_id,
                    &request.test_type,
                )?;
                exit_codes.push(cts_gts.exit_code);
            }
            if let Some(handle) = sts_handle {
                exit_codes.push(handle.join().unwrap_or(1));
            }
        }
        other => return Err(format!("Unknown test type: {other}")),
    }

    if exit_codes.is_empty() {
        return Err(format!("{} has no runnable device group", request.test_type));
    }

    let final_code = if exit_codes.iter().all(|code| *code == 0) { 0 } else { 1 };
    emit_log(&app, format!("[runner] Completed with exit={final_code}."));
    let _ = fs::remove_dir_all(root.join(".gba-workspaces").join(sanitize_name(&run_id)));
    Ok(final_code)
}

fn ensure_ghidra_for_sts(app: &AppHandle) -> Result<(), String> {
    emit_log(app, "[runner] Preparing Ghidra for STS...".to_string());
    let message = prepare_ghidra_for_sts()?;
    emit_log(app, message);
    Ok(())
}

fn prepare_ghidra_for_sts() -> Result<String, String> {
    let _guard = GHIDRA_PREP_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|err| format!("Ghidra preparation lock failed: {err}"))?;
    let updater = Path::new("/home/endri-pro/Documents/ghidra/download_ghidra.sh");
    if !updater.is_file() {
        return Err(format!("Ghidra updater not found: {}", updater.display()));
    }
    let run_updater = || {
        Command::new("bash")
            .arg(updater)
            .output()
            .map_err(|err| format!("Cannot start Ghidra updater: {err}"))
    };
    let output = run_updater()?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("Ghidra updater failed{}", if detail.is_empty() { String::new() } else { format!(": {detail}") }));
    }
    let target = Path::new("/tmp/tradefed_ghidra");
    let archive = || WalkDir::new(target)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .flatten()
        .find(|entry| entry.file_type().is_file() && is_zip_file(entry.path()))
        .map(|entry| entry.path().to_path_buf());
    let valid = |path: &Path| Command::new("unzip")
        .args(["-t", &path.display().to_string()])
        .output()
        .map(|result| result.status.success())
        .unwrap_or(false);
    let seed_cache = |source: &Path| -> Result<(), String> {
        let name = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "Invalid Ghidra archive name".to_string())?;
        let version = name
            .strip_prefix("ghidra_")
            .and_then(|value| value.split_once("_PUBLIC"))
            .map(|(value, _)| value)
            .ok_or_else(|| format!("Cannot derive Ghidra version from {name}"))?;
        let cache = Path::new("/tmp/ghidra_cache/https:/github.com/NationalSecurityAgency/ghidra/releases/download")
            .join(format!("Ghidra_{version}_build"))
            .join(name);
        let parent = cache.parent().ok_or_else(|| "Invalid Ghidra cache path".to_string())?;
        fs::create_dir_all(parent).map_err(|err| format!("Cannot create Ghidra cache: {err}"))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let temp = cache.with_file_name(format!(".{name}.{nonce}.tmp"));
        fs::copy(source, &temp).map_err(|err| format!("Cannot seed Ghidra cache: {err}"))?;
        fs::rename(temp, cache).map_err(|err| format!("Cannot activate Ghidra cache: {err}"))
    };
    let path = archive();
    if path.as_deref().is_some_and(valid) {
        seed_cache(path.as_deref().unwrap())?;
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
    let retry = run_updater()?;
    if !retry.status.success() {
        return Err("Ghidra updater produced an invalid ZIP and retry failed".to_string());
    }
    let path = archive().filter(|path| valid(path));
    if let Some(path) = path {
        seed_cache(&path)?;
        Ok(String::from_utf8_lossy(&retry.stdout).trim().to_string())
    } else {
        Err("Ghidra updater produced an invalid ZIP".to_string())
    }
}

#[derive(Debug, Clone)]
struct LaundrySource {
    _temp: std::sync::Arc<TempDir>,
    extract_root: PathBuf,
    cts_results: Vec<PathBuf>,
    gts_results: Vec<PathBuf>,
    sts_results: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct DeviceInfoSources {
    property: PathBuf,
    client_id: Option<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_normal(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    request: &RunSuiteRequest,
    model: &str,
    pda: &str,
    run_id: &str,
) -> Result<SuiteOutcome, String> {
    let devices = &request.user_devices;
    let source = prepare_laundry_source(app, request, session_dir)?;
    verify_laundry_suite_tools(app, root, devices, &source, &["GTS", "CTS"])?;
    emit_log(app, "[runner] Laundry Normal: initial GTS property run.");
    let deviceinfo = run_laundry_initial_gts(
        app,
        root,
        session_dir,
        log_dir,
        devices,
        "run gts --subplan property",
        request.timeout_secs,
        model,
        pda,
        run_id,
        &request.test_type,
    )?;

    let mut codes = Vec::new();
    codes.extend(run_laundry_retries_with_deviceinfo(
        app,
        root,
        session_dir,
        log_dir,
        "CTS",
        devices,
        &source.extract_root,
        &source.cts_results,
        &deviceinfo,
        request.timeout_secs,
        model,
        pda,
        run_id,
        &request.test_type,
    )?);
    codes.extend(run_laundry_retries_with_deviceinfo(
        app,
        root,
        session_dir,
        log_dir,
        "GTS",
        devices,
        &source.extract_root,
        &source.gts_results,
        &deviceinfo,
        request.timeout_secs,
        model,
        pda,
        run_id,
        &request.test_type,
    )?);

    Ok(SuiteOutcome {
        exit_code: if codes.iter().all(|code| *code == 0) { 0 } else { 1 },
        elapsed_secs: 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_smr(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    request: &RunSuiteRequest,
    model: &str,
    pda: &str,
    run_id: &str,
) -> Result<SuiteOutcome, String> {
    let source = prepare_laundry_source(app, request, session_dir)?;
    if !request.user_devices.is_empty() {
        verify_laundry_suite_tools(app, root, &request.user_devices, &source, &["GTS", "CTS"])?;
    }
    if !request.userdebug_devices.is_empty() {
        verify_laundry_suite_tools(app, root, &request.userdebug_devices, &source, &["STS"])?;
    }
    let mut codes = Vec::new();
    let sts_handle = if request.userdebug_devices.is_empty() {
        None
    } else {
        emit_log(app, "[runner] Laundry SMR: STS retry starts immediately on userdebug devices.");
        let app_sts = app.clone();
        let root_sts = root.to_path_buf();
        let session_sts = session_dir.to_path_buf();
        let log_sts = log_dir.to_path_buf();
        let devices_sts = request.userdebug_devices.clone();
        let source_root_sts = source.extract_root.clone();
        let source_sts = source.sts_results.clone();
        let timeout_sts = request.timeout_secs;
        let model_sts = model.to_string();
        let pda_sts = pda.to_string();
        let run_id_sts = run_id.to_string();
        let test_type_sts = request.test_type.clone();
        Some(thread::spawn(move || {
            set_current_run_id(Some(run_id_sts.clone()));
            run_laundry_retries_without_deviceinfo(
                &app_sts,
                &root_sts,
                &session_sts,
                &log_sts,
                "STS",
                &devices_sts,
                &source_root_sts,
                &source_sts,
                timeout_sts,
                &model_sts,
                &pda_sts,
                &run_id_sts,
                &test_type_sts,
            )
            .map(|values| values.into_iter().collect::<Vec<_>>())
            .unwrap_or_else(|err| {
                emit_log(&app_sts, format!("[runner] STS retry failed: {err}"));
                vec![1]
            })
        }))
    };

    if !request.user_devices.is_empty() {
        emit_log(app, "[runner] Laundry SMR: initial GTS gtsmr run.");
        let deviceinfo = run_laundry_initial_gts(
            app,
            root,
            session_dir,
            log_dir,
            &request.user_devices,
            "run gts --subplan gtsmr",
            request.timeout_secs,
            model,
            pda,
            run_id,
            &request.test_type,
        )?;

        codes.extend(run_laundry_retries_with_deviceinfo(
            app,
            root,
            session_dir,
            log_dir,
            "CTS",
            &request.user_devices,
            &source.extract_root,
            &source.cts_results,
            &deviceinfo,
            request.timeout_secs,
            model,
            pda,
            run_id,
            &request.test_type,
        )?);
        codes.extend(run_laundry_retries_with_deviceinfo(
            app,
            root,
            session_dir,
            log_dir,
            "GTS",
            &request.user_devices,
            &source.extract_root,
            &source.gts_results,
            &deviceinfo,
            request.timeout_secs,
            model,
            pda,
            run_id,
            &request.test_type,
        )?);
    }

    if let Some(handle) = sts_handle {
        codes.extend(handle.join().unwrap_or_else(|_| vec![1]));
    }

    Ok(SuiteOutcome {
        exit_code: if codes.iter().all(|code| *code == 0) { 0 } else { 1 },
        elapsed_secs: 0,
    })
}

fn extract_nested_zips(dir: &Path) -> Result<(), String> {
    loop {
        let mut zip_files = Vec::new();
        for entry in WalkDir::new(dir).into_iter().flatten() {
            if entry.file_type().is_file() {
                let path = entry.path().to_path_buf();
                if let Some(ext) = path.extension() {
                    if ext.to_string_lossy().to_lowercase() == "zip" {
                        zip_files.push(path);
                    }
                }
            }
        }
        if zip_files.is_empty() {
            break;
        }
        for zip_path in zip_files {
            let mut dest_dir = zip_path.clone();
            dest_dir.set_extension("");
            fs::create_dir_all(&dest_dir)
                .map_err(|err| format!("Cannot create dir {}: {err}", dest_dir.display()))?;
            extract_zip_safe(&zip_path, &dest_dir)?;
            let _ = fs::remove_file(&zip_path);
        }
    }
    Ok(())
}

fn prepare_laundry_source(app: &AppHandle, request: &RunSuiteRequest, session_dir: &Path) -> Result<LaundrySource, String> {
    let zip_path = request
        .laundry_zip_path
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{} needs laundry zip file", request.test_type))?;
    let zip_path = PathBuf::from(zip_path);
    if !zip_path.is_file() {
        return Err(format!("Laundry zip not found: {}", zip_path.display()));
    }
    emit_log(app, format!("[runner] Laundry zip selected: {}", zip_path.display()));
    let temp = std::sync::Arc::new(
        tempfile::Builder::new()
            .prefix("gba-laundry-")
            .tempdir_in(session_dir)
            .map_err(|err| format!("Cannot create laundry temp dir: {err}"))?,
    );
    emit_log(app, format!("[runner] Extracting zip to {}", temp.path().display()));
    extract_zip_safe(&zip_path, temp.path())?;
    
    emit_log(app, "[runner] Checking and extracting any nested zip files...");
    extract_nested_zips(temp.path())?;

    let (mut cts_results, mut gts_results, mut sts_results) = scan_laundry_results(temp.path());
    if !request.selected_laundry_results.is_empty() {
        let selected: HashSet<String> = request.selected_laundry_results.iter().cloned().collect();
        cts_results.retain(|path| selected.contains(&laundry_result_id(temp.path(), path)));
        gts_results.retain(|path| selected.contains(&laundry_result_id(temp.path(), path)));
        sts_results.retain(|path| selected.contains(&laundry_result_id(temp.path(), path)));
        emit_log(
            app,
            format!(
                "[runner] Custom laundry selection applied: {} result(s)",
                cts_results.len() + gts_results.len() + sts_results.len()
            ),
        );
    }
    emit_log(
        app,
        format!(
            "[runner] Scanned laundry results: CTS={} GTS={} STS={}",
            cts_results.len(),
            gts_results.len(),
            sts_results.len()
        ),
    );
    let extract_root = temp.path().to_path_buf();
    Ok(LaundrySource {
        _temp: temp,
        extract_root,
        cts_results,
        gts_results,
        sts_results,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_initial_gts(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    devices: &[String],
    gts_command: &str,
    timeout_secs: u64,
    model: &str,
    _pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<DeviceInfoSources, String> {
    let gts_root = resolve_gts_root(root, "")?;
    let gts_workspace = suite_workspace(root, &gts_root, run_id)?;
    let gts_exe = gts_workspace.join("tools/gts-tradefed");
    if !gts_exe.is_file() {
        return Err(format!("gts-tradefed not found: {}", gts_exe.display()));
    }
    if !gts_workspace.join("subplans/gtsmr.xml").is_file() {
        return Err(format!("GTS subplan not found: {}", gts_workspace.join("subplans/gtsmr.xml").display()));
    }
    let cmd = format!(
        "{gts_command} --shard-count {}{}",
        devices.len(),
        serial_args(devices)
    );
    let log_file = log_dir.join(format!("laundry_initial_gts_{}_{}devs.log", sanitize_name(model), devices.len()));
    let outcome = run_suite_process(
        app,
        "GTS",
        devices,
        &gts_exe,
        &gts_root,
        &cmd,
        true,
        &log_file,
        timeout_secs,
        run_id,
        test_type,
        false,
    )?;
    if outcome.exit_code != 0 {
        emit_log(app, "[runner] Initial GTS returned non-zero; trying to collect deviceinfo anyway.");
    }
    let property_deviceinfo = latest_property_deviceinfo(&gts_workspace.join("results"))
        .ok_or_else(|| "PropertyDeviceInfo.deviceinfo.json not found after initial GTS".to_string())?;
    let client_id_deviceinfo = latest_client_id_deviceinfo(&gts_root.join("results"));
    emit_log(app, format!("[runner] Deviceinfo source: {}", property_deviceinfo.display()));
    let stable_property = session_dir.join(format!(
        "PropertyDeviceInfo_{}_{}.deviceinfo.json",
        sanitize_name(&devices.join("_")),
        timestamp_compact()
    ));
    fs::copy(&property_deviceinfo, &stable_property)
        .map_err(|err| format!("Cannot preserve deviceinfo {}: {err}", property_deviceinfo.display()))?;
    let stable_client_id = client_id_deviceinfo.map(|source| {
        let target = session_dir.join(format!(
            "ClientIdDeviceInfo_{}_{}.deviceinfo.json",
            sanitize_name(&devices.join("_")),
            timestamp_compact()
        ));
        fs::copy(&source, &target)
            .map(|_| target)
            .map_err(|err| format!("Cannot preserve deviceinfo {}: {err}", source.display()))
    }).transpose()?;
    emit_log(app, format!("[runner] Deviceinfo preserved: {}", stable_property.display()));
    if let Some(path) = &stable_client_id {
        emit_log(app, format!("[runner] ClientId deviceinfo preserved: {}", path.display()));
    }
    Ok(DeviceInfoSources { property: stable_property, client_id: stable_client_id })
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_retries_with_deviceinfo(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    suite: &str,
    devices: &[String],
    source_root: &Path,
    source_results: &[PathBuf],
    deviceinfos: &DeviceInfoSources,
    timeout_secs: u64,
    model: &str,
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<Vec<i32>, String> {
    let with_info = source_results
        .iter()
        .filter(|result| property_deviceinfo_in_result(result).is_some())
        .count();
    emit_log(
        app,
        format!(
            "[runner] {suite}: {} result(s) queued for retry; {} with PropertyDeviceInfo will be replaced.",
            source_results.len(),
            with_info
        ),
    );
    run_laundry_retries(app, root, session_dir, log_dir, suite, devices, source_root, source_results, Some(deviceinfos), timeout_secs, model, pda, run_id, test_type)
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_retries_without_deviceinfo(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    suite: &str,
    devices: &[String],
    source_root: &Path,
    source_results: &[PathBuf],
    timeout_secs: u64,
    model: &str,
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<Vec<i32>, String> {
    emit_log(app, format!("[runner] {suite}: {} result(s) queued for retry.", source_results.len()));
    run_laundry_retries(app, root, session_dir, log_dir, suite, devices, source_root, source_results, None, timeout_secs, model, pda, run_id, test_type)
}

fn get_local_timestamp() -> String {
    if let Ok(output) = Command::new("date").arg("+%Y.%m.%d_%H.%M.%S").output() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("2026.05.26_{}", secs)
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_retries(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    suite: &str,
    devices: &[String],
    source_root: &Path,
    source_results: &[PathBuf],
    replacement_deviceinfos: Option<&DeviceInfoSources>,
    timeout_secs: u64,
    model: &str,
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<Vec<i32>, String> {
    let mut codes = Vec::new();
    for (index, source) in source_results.iter().enumerate() {
        emit_laundry_result_update(app, source_root, source, run_id, test_type, suite, "Staging result", 0, None);
        let suite_root = suite_root_for_laundry_result(root, suite, devices, source)?;
        let suite_workspace = suite_workspace(root, &suite_root, run_id)?;
        let executable = tradefed_tool_for_suite(&suite_workspace, suite)?;
        if !executable.is_file() {
            return Err(format!("{suite} tradefed not found: {}", executable.display()));
        }
        let results_dir = suite_workspace.join("results");
        fs::create_dir_all(&results_dir).map_err(|err| format!("Cannot create {}: {err}", results_dir.display()))?;
        let mut timestamp = get_local_timestamp();
        let mut target = results_dir.join(&timestamp);
        while target.exists() {
            thread::sleep(Duration::from_secs(1));
            timestamp = get_local_timestamp();
            target = results_dir.join(&timestamp);
        }
        emit_log(app, format!("[runner] {suite}: staging result {} -> {}", source.display(), target.display()));
        copy_dir_recursive(source, &target)
            .map_err(|err| format!("Cannot stage {} to {}: {err}", source.display(), target.display()))?;
        cleanup_deviceinfo_backups(&target);
        if let Some(replacements) = replacement_deviceinfos {
            for (name, source, target_info) in [
                ("PropertyDeviceInfo", Some(&replacements.property), property_deviceinfo_in_result(&target)),
                ("ClientIdDeviceInfo", replacements.client_id.as_ref(), client_id_deviceinfo_in_result(&target)),
            ] {
                if let (Some(source), Some(target_info)) = (source, target_info) {
                    fs::copy(source, &target_info)
                        .map_err(|err| format!("Cannot replace {}: {err}", target_info.display()))?;
                    emit_log(app, format!("[runner] {suite}: replaced {} ({name})", target_info.display()));
                }
            }
            cleanup_deviceinfo_backups(&target);
        }
        let result_dir_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&timestamp);
        let session_id = if suite == "CTS" {
            "0".to_string()
        } else {
            resolve_retry_session_id(
                app,
                suite,
                &executable,
                result_dir_name,
                &log_dir.join(format!("laundry_list_{}_{}_{}devs.log", suite.to_lowercase(), index + 1, devices.len())),
                run_id,
            )?
        };
        let cmd = format!(
            "run retry --retry {session_id} --retry-type NOT_EXECUTED --shard-count {}{}",
            devices.len(),
            serial_args(devices)
        );
        emit_log(app, format!("[runner] {suite}: retry session={session_id} result={}", target.display()));
        let log_file = log_dir.join(format!("laundry_retry_{}_{}_{}devs.log", suite.to_lowercase(), index + 1, devices.len()));
        let copy_started = SystemTime::now();
        let result_snapshot = ResultSnapshot::capture(&suite_workspace.join("results"));
        emit_laundry_result_update(app, source_root, source, run_id, test_type, suite, "Running", 0, None);
        let outcome = run_suite_process(app, suite, devices, &executable, &suite_workspace, &cmd, suite != "STS", &log_file, timeout_secs, run_id, test_type, true)?;
        copy_laundry_retry_artifact(app, root, session_dir, suite, &suite_workspace, &target, &result_snapshot, copy_started, model, pda, devices, index + 1)?;
        let summary = parse_summary(&log_file, suite, &devices.join(","), run_id, test_type);
        emit_laundry_result_update(
            app,
            source_root,
            source,
            run_id,
            test_type,
            suite,
            if outcome.exit_code == 0 { "Test Done" } else { "Failed" },
            outcome.elapsed_secs,
            Some(&summary),
        );
        codes.push(outcome.exit_code);
    }
    Ok(codes)
}

fn copy_laundry_retry_artifact(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    suite: &str,
    suite_root: &Path,
    result_dir: &Path,
    result_snapshot: &ResultSnapshot,
    copy_started: SystemTime,
    model: &str,
    pda: &str,
    devices: &[String],
    index: usize,
) -> Result<(), String> {
    emit_log(app, format!("[runner] {suite}: copying retry artifact for {}", result_dir.display()));
    let zip = result_snapshot.newest_zip(&suite_root.join("results"), copy_started)
        .or_else(|| first_zip(result_dir))
        .or_else(|| {
            let sibling = result_dir.with_extension("zip");
            if sibling.is_file() { Some(sibling) } else { None }
        });
    let Some(zip) = zip else {
        emit_log(app, format!("[{suite}] No zip found after retry in {}", result_dir.display()));
        return Err(format!("{suite} completed without a result ZIP: {}", result_dir.display()));
    };
    let result_name = result_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("result");
    let dst = session_dir.join(format!(
        "{}_retry{}_{}_{}_{}_{}.zip",
        suite,
        index,
        sanitize_name(model),
        sanitize_name(pda),
        sanitize_name(&devices.join("_")),
        sanitize_name(result_name)
    ));
    fs::copy(&zip, &dst)
        .map_err(|err| format!("Cannot copy {} to {}: {err}", zip.display(), dst.display()))?;
    if let Some(name) = dst.file_name().and_then(|value| value.to_str()) {
        update_busy_device_zip(root, suite, devices, name);
    }
    emit_log(app, format!("[{suite}] Retry result copied: {}", dst.display()));
    Ok(())
}

fn resolve_retry_session_id(
    app: &AppHandle,
    suite: &str,
    executable: &Path,
    result_dir_name: &str,
    log_file: &Path,
    run_id: &str,
) -> Result<String, String> {
    emit_log(app, format!("[runner] {suite}: resolving retry session for {result_dir_name}"));
    let mut last_output = String::new();
    for attempt in 1..=5 {
        let output = run_tradefed_console_command(app, suite, executable, "l r", log_file, 45, run_id)?;
        if let Some(session_id) = parse_retry_session_id(&output, result_dir_name) {
            emit_log(app, format!("[runner] {suite}: matched retry session {session_id} for {result_dir_name}"));
            return Ok(session_id);
        }
        last_output = output;
        emit_log(app, format!("[runner] {suite}: retry session not visible yet for {result_dir_name} (attempt {attempt}/5)."));
        thread::sleep(Duration::from_secs(1));
    }
    Err(format!(
        "{suite}: cannot find retry session for result directory {result_dir_name}. Check {}. Last l r output had {} bytes.",
        log_file.display(),
        last_output.len()
    ))
}

fn parse_retry_session_id(output: &str, result_dir_name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.contains(result_dir_name) {
            return None;
        }
        let session = trimmed.split_whitespace().next()?;
        if session.chars().all(|ch| ch.is_ascii_digit()) {
            Some(session.to_string())
        } else {
            None
        }
    })
}

fn run_tradefed_console_command(
    app: &AppHandle,
    suite: &str,
    executable: &Path,
    console_command: &str,
    log_file: &Path,
    timeout_secs: u64,
    run_id: &str,
) -> Result<String, String> {
    write_log_header(log_file, suite, console_command)?;
    let executable_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid executable path: {}", executable.display()))?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| format!("Invalid executable parent: {}", executable.display()))?;

    let mut command = Command::new(format!("./{executable_name}"));
    command
        .current_dir(executable_dir)
        .args(console_command.split_whitespace())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_child(&mut command);

    let mut child = command
        .spawn()
        .map_err(|err| format!("Failed to start {suite} console: {err}"))?;
    let pid = child.id();
    register_pid(app, run_id, pid);
    emit_log(app, format!("[{suite}] console pid={pid} command={console_command}"));

    let output = Arc::new(Mutex::new(String::new()));
    if let Some(stdout) = child.stdout.take() {
        let out_buf = Arc::clone(&output);
        let log_stdout = log_file.to_path_buf();
        let app_stdout = app.clone();
        let suite_stdout = suite.to_string();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                append_file_line(&log_stdout, &line);
                if let Ok(mut text) = out_buf.lock() {
                    text.push_str(&line);
                    text.push('\n');
                }
                emit_log_event(&app_stdout, format!("[{suite_stdout}] {line}"));
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let out_buf = Arc::clone(&output);
        let log_stderr = log_file.to_path_buf();
        let app_stderr = app.clone();
        let suite_stderr = suite.to_string();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                append_file_line(&log_stderr, &line);
                if let Ok(mut text) = out_buf.lock() {
                    text.push_str(&line);
                    text.push('\n');
                }
                emit_log_event(&app_stderr, format!("[{suite_stderr}] {line}"));
            }
        });
    }

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(err) => {
                unregister_pid(app, pid);
                return Err(format!("{suite} console wait failed: {err}"));
            }
        }
        if started.elapsed().as_secs() > timeout_secs {
            terminate_process_tree(child.id());
            let _ = child.wait();
            unregister_pid(app, pid);
            return Err(format!("{suite} console command timed out: {console_command}"));
        }
        thread::sleep(Duration::from_millis(200));
    }
    unregister_pid(app, pid);
    thread::sleep(Duration::from_millis(200));
    Ok(output.lock().map(|text| text.clone()).unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
fn run_cts_then_gts(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    devices: &[String],
    cts_subplan: &str,
    gts_command: &str,
    retry_args: &str,
    timeout_secs: u64,
    model: &str,
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<SuiteOutcome, String> {
    let first = devices.first().ok_or_else(|| "CTS/GTS needs user devices".to_string())?;
    let props = device_props(first).unwrap_or_default();
    let oneui = prop(&props, "ro.build.version.oneui");
    let major = if oneui == "80500" {
        "16.1".to_string()
    } else {
        android_major(&prop(&props, "ro.build.version.release"))
    };
    let cts_root = resolve_cts_root(root, &major)?;
    let cts_exe = cts_root.join("tools/cts-tradefed");
    let subplan = cts_root.join("subplans").join(format!("{cts_subplan}.xml"));
    if !subplan.is_file() {
        return Err(format!("CTS subplan not found: {}", subplan.display()));
    }
    if !cts_exe.is_file() {
        return Err(format!("cts-tradefed not found: {}", cts_exe.display()));
    }

    let serial_args = serial_args(devices);
    let cts_cmd = format!(
        "run cts --subplan {cts_subplan} --shard-count {}{}{}",
        devices.len(),
        serial_args,
        retry_args
    );
    let cts_log = log_dir.join(format!("cts_{}_{}devs.log", sanitize_name(model), devices.len()));
    emit_log(app, format!("[runner] CTS: starting subplan {cts_subplan} on {}", devices.join(",")));
    let cts_snapshot = ResultSnapshot::capture(&cts_root.join("results"));
    let cts_started = SystemTime::now();
    let cts_outcome = run_suite_process(
        app,
        "CTS",
        devices,
        &cts_exe,
        &cts_root,
        &cts_cmd,
        true,
        &cts_log,
        timeout_secs,
        run_id,
        test_type,
        true,
    )?;
    copy_suite_result(app, root, session_dir, "CTS", devices, model, pda, &cts_log, cts_outcome.elapsed_secs, run_id, test_type, Some((&cts_snapshot, cts_started)))?;

    if cts_outcome.exit_code != 0 {
        emit_log(app, "[gts] CTS returned non-zero; GTS will still be attempted.");
    }
    emit_log(app, format!("[runner] CTS: done; GTS: starting command '{gts_command}' on {}", devices.join(",")));

    let gts_root = resolve_gts_root(root, "")?;
    let gts_exe = gts_root.join("tools/gts-tradefed");
    if !gts_exe.is_file() {
        return Err(format!("gts-tradefed not found: {}", gts_exe.display()));
    }
    let gts_cmd = format!(
        "{gts_command} --shard-count {}{}{}",
        devices.len(),
        serial_args,
        retry_args
    );
    let gts_log = log_dir.join(format!("gts_{}_{}devs.log", sanitize_name(model), devices.len()));
    let gts_snapshot = ResultSnapshot::capture(&gts_root.join("results"));
    let gts_started = SystemTime::now();
    let gts_outcome = run_suite_process(
        app,
        "GTS",
        devices,
        &gts_exe,
        &gts_root,
        &gts_cmd,
        true,
        &gts_log,
        timeout_secs,
        run_id,
        test_type,
        true,
    )?;
    copy_suite_result(app, root, session_dir, "GTS", devices, model, pda, &gts_log, gts_outcome.elapsed_secs, run_id, test_type, Some((&gts_snapshot, gts_started)))?;

    Ok(SuiteOutcome {
        exit_code: if cts_outcome.exit_code == 0 && gts_outcome.exit_code == 0 {
            0
        } else {
            1
        },
        elapsed_secs: cts_outcome.elapsed_secs + gts_outcome.elapsed_secs,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_sts(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    devices: &[String],
    retry_args: &str,
    timeout_secs: u64,
    model: &str,
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<SuiteOutcome, String> {
    let first = devices.first().ok_or_else(|| "STS needs userdebug devices".to_string())?;
    let props = device_props(first).unwrap_or_default();
    let android = prop(&props, "ro.build.version.release");
    let major = android_major(&android);
    let spl = prop(&props, "ro.build.version.security_patch");
    let month = security_patch_month(&spl);
    let sts_root = root.join("STS").join(month).join(major).join("android-sts");
    let sts_exe = sts_root.join("tools/sts-tradefed");
    if !sts_exe.is_file() {
        return Err(format!("sts-tradefed not found: {}", sts_exe.display()));
    }

    let (sts_plan, shard_arg) = if devices.len() > 1 {
        ("sts", format!(" --shard-count {}", devices.len()))
    } else {
        ("sts-dynamic-incremental", String::new())
    };

    let sts_cmd = format!(
        "run {} --test-arg com.android.compatibility.common.tradefed.testtype.JarHostTest:set-option:android.security.sts.KernelLtsTest:acknowledge_kernel_update_requirement_warning_failure:true{}{}{}",
        sts_plan,
        shard_arg,
        serial_args(devices),
        retry_args
    );
    let sts_log = log_dir.join(format!("sts_{}_{}devs.log", sanitize_name(model), devices.len()));
    emit_log(app, format!("[runner] STS: starting {} on {}", sts_plan, devices.join(",")));
    let sts_snapshot = ResultSnapshot::capture(&sts_root.join("results"));
    let sts_started = SystemTime::now();
    let outcome = run_suite_process(
        app,
        "STS",
        devices,
        &sts_exe,
        &sts_root,
        &sts_cmd,
        false,
        &sts_log,
        timeout_secs,
        run_id,
        test_type,
        true,
    )?;
    copy_suite_result(app, root, session_dir, "STS", devices, model, pda, &sts_log, outcome.elapsed_secs, run_id, test_type, Some((&sts_snapshot, sts_started)))?;
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn run_suite_process(
    app: &AppHandle,
    suite: &str,
    devices: &[String],
    executable: &Path,
    _cwd: &Path,
    suite_command: &str,
    via_pipe: bool,
    log_file: &Path,
    timeout_secs: u64,
    run_id: &str,
    test_type: &str,
    publish_status: bool,
) -> Result<SuiteOutcome, String> {
    let devices_text = devices.join(",");
    if publish_status {
        emit_status(app, suite, "Waiting", &devices_text, 0, log_file, run_id, test_type);
    }
    emit_log(app, format!("[runner] {suite}: queued for shared ADB slot ({devices_text})"));
    write_log_header(log_file, suite, suite_command)?;

    let _gts_guard = if suite == "GTS" {
        let lock = GTS_RUN_LOCK.get_or_init(|| Mutex::new(()));
        if lock.try_lock().is_err() {
            emit_log(app, "[runner] GTS: waiting for another GTS run to finish.");
        }
        Some(lock.lock().unwrap_or_else(|err| err.into_inner()))
    } else {
        None
    };
    let _sts_guard = if suite == "STS" {
        let lock = STS_RUN_LOCK.get_or_init(|| Mutex::new(()));
        if lock.try_lock().is_err() {
            emit_log(app, "[runner] STS: waiting for another STS run to finish (shared Ghidra temp directory).");
        }
        Some(lock.lock().unwrap_or_else(|err| err.into_inner()))
    } else {
        None
    };
    if publish_status {
        emit_status(app, suite, "Starting", &devices_text, 0, log_file, run_id, test_type);
    }
    emit_log(app, format!("[runner] {suite}: launching tradefed for {devices_text}"));
    emit_log(app, format!("[{suite}] {suite_command}"));

    let executable_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid executable path: {}", executable.display()))?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| format!("Invalid executable parent: {}", executable.display()))?;

    let mut command = Command::new(format!("./{executable_name}"));
    command
        .current_dir(executable_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if via_pipe {
        command.stdin(Stdio::piped());
    } else {
        command.args(suite_command.split_whitespace());
    }
    configure_child(&mut command);

    let mut child = command
        .spawn()
        .map_err(|err| format!("Failed to start {suite}: {err}"))?;
    let pid = child.id();
    register_pid(app, run_id, pid);
    emit_log(app, format!("[{suite}] started pid={pid}"));

    if via_pipe {
        if let Some(mut stdin) = child.stdin.take() {
            let cmd = suite_command.to_string();
            thread::spawn(move || {
                let _ = writeln!(stdin, "{cmd}");
                thread::sleep(Duration::from_secs(timeout_secs));
            });
        }
    }

    register_log_file(app, log_file);
    pipe_child_output(app.clone(), suite.to_string(), log_file.to_path_buf(), &mut child, run_id.to_string());

    let started = Instant::now();
    let exit_code = wait_with_timeout(app, &mut child, suite, devices, log_file, timeout_secs, run_id, test_type, publish_status);
    unregister_pid(app, pid);
    let elapsed = started.elapsed().as_secs();

    if publish_status {
        let summary = parse_summary(log_file, suite, &devices_text, run_id, test_type);
        let _ = app.emit("gba-summary", summary);
        emit_status(
            app,
            suite,
            if exit_code == 0 { "Completed" } else { "Failed" },
            &devices_text,
            elapsed,
            log_file,
            run_id,
            test_type,
        );
    }

    Ok(SuiteOutcome { exit_code, elapsed_secs: elapsed })
}

fn wait_with_timeout(
    app: &AppHandle,
    child: &mut Child,
    suite: &str,
    devices: &[String],
    log_file: &Path,
    timeout_secs: u64,
    run_id: &str,
    test_type: &str,
    publish_status: bool,
) -> i32 {
    let started = Instant::now();
    let mut disconnected_since: Option<Instant> = None;
    let mut last_device_check = Instant::now() - Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(1),
            Ok(None) => {}
            Err(err) => {
                emit_log(app, format!("[{suite}] wait failed: {err}"));
                return 1;
            }
        }
        if suite_log_has_completion_marker(log_file) {
            emit_log(app, format!("[{suite}] completion marker detected; closing tradefed console."));
            terminate_process_tree(child.id());
            let _ = child.wait();
            return 0;
        }
        if last_device_check.elapsed().as_secs() >= 5 {
            last_device_check = Instant::now();
            let missing = disconnected_devices(devices);
            if missing.is_empty() {
                if disconnected_since.take().is_some() {
                    emit_log(app, format!("[runner] {suite}: device reconnected; continuing monitor."));
                    if publish_status {
                        emit_status(
                            app,
                            suite,
                            "Running",
                            &devices.join(","),
                            started.elapsed().as_secs(),
                            log_file,
                            run_id,
                            test_type,
                        );
                    }
                }
            } else {
                let since = disconnected_since.get_or_insert_with(Instant::now);
                let waited = since.elapsed().as_secs();
                emit_log(
                    app,
                    format!(
                        "[runner] {suite}: waiting device reconnect ({}/{DEVICE_RECONNECT_TIMEOUT_SECS}s): {}",
                        waited,
                        missing.join(",")
                    ),
                );
                if publish_status {
                    emit_status(
                        app,
                        suite,
                        "Waiting device reconnect",
                        &devices.join(","),
                        started.elapsed().as_secs(),
                        log_file,
                        run_id,
                        test_type,
                    );
                }
                if waited > DEVICE_RECONNECT_TIMEOUT_SECS {
                    emit_log(app, format!("[runner] {suite}: reconnect timeout; terminating tradefed."));
                    terminate_process_tree(child.id());
                    let _ = child.wait();
                    return 125;
                }
            }
        }
        if started.elapsed().as_secs() > timeout_secs {
            if publish_status {
                emit_status(
                    app,
                    suite,
                    "Timeout",
                    &devices.join(","),
                    started.elapsed().as_secs(),
                    log_file,
                    run_id,
                    test_type,
                );
            }
            terminate_process_tree(child.id());
            return 124;
        }
        if publish_status {
            emit_status(
                app,
                suite,
                "Running",
                &devices.join(","),
                started.elapsed().as_secs(),
                log_file,
                run_id,
                test_type,
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn disconnected_devices(devices: &[String]) -> Vec<String> {
    devices
        .iter()
        .filter_map(|serial| match adb_device_state(serial) {
            Ok(state) if state == "device" => None,
            Ok(state) => Some(format!("{serial}:{state}")),
            Err(err) => Some(format!("{serial}:{err}")),
        })
        .collect()
}

fn adb_device_state(serial: &str) -> Result<String, String> {
    adb_device_output(serial, &["get-state"]).map(|state| {
        if state.trim().is_empty() {
            "unknown".to_string()
        } else {
            state.trim().to_string()
        }
    })
}

fn suite_log_has_completion_marker(log_file: &Path) -> bool {
    let content = fs::read_to_string(log_file).unwrap_or_default();
    content
        .lines()
        .rev()
        .take(120)
        .any(|line| line.contains("Result/Log Location") || line.contains("=============== Summary ==============="))
}

fn pipe_child_output(app: AppHandle, suite: String, log_file: PathBuf, child: &mut Child, run_id: String) {
    if let Some(stdout) = child.stdout.take() {
        let app_stdout = app.clone();
        let suite_stdout = suite.clone();
        let log_stdout = log_file.clone();
        let run_id_stdout = run_id.clone();
        thread::spawn(move || {
            set_current_run_id(Some(run_id_stdout));
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                append_file_line(&log_stdout, &line);
                emit_log_event(&app_stdout, format!("[{suite_stdout}] {line}"));
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let app_stderr = app;
        let run_id_stderr = run_id.clone();
        thread::spawn(move || {
            set_current_run_id(Some(run_id_stderr));
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                append_file_line(&log_file, &line);
                emit_log_event(&app_stderr, format!("[{suite}] {line}"));
            }
        });
    }
}

fn copy_suite_result(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    suite: &str,
    devices: &[String],
    model: &str,
    pda: &str,
    log_file: &Path,
    elapsed_secs: u64,
    run_id: &str,
    test_type: &str,
    snapshot: Option<(&ResultSnapshot, SystemTime)>,
) -> Result<(), String> {
    emit_status(app, suite, "Copying result", &devices.join(","), elapsed_secs, log_file, run_id, test_type);
    emit_log(app, format!("[runner] {suite}: copying result for {}", devices.join(",")));
    let suite_root = suite_root_for_copy(root, suite, devices)?;
    let results = suite_root.join("results");
    if let Some((snapshot, started)) = snapshot {
        if let Some(zip) = snapshot.newest_zip(&results, started) {
            let dst = session_dir.join(format!(
                "{}_{}_{}_{}.zip",
                suite,
                sanitize_name(model),
                sanitize_name(pda),
                sanitize_name(&devices.join("_"))
            ));
            fs::copy(&zip, &dst)
                .map_err(|err| format!("Cannot copy {} to {}: {err}", zip.display(), dst.display()))?;
            emit_log(app, format!("[{suite}] Result copied: {}", dst.display()));
            emit_status(app, suite, "Test Done", &devices.join(","), elapsed_secs, log_file, run_id, test_type);
            emit_log(app, format!("[runner] {suite}: Test Done in {}", format_duration(elapsed_secs)));
            return Ok(());
        }
        emit_log(app, format!("[{suite}] No new zip detected by snapshot in {}; falling back to result move.", results.display()));
    }
    let tmp = suite_root.join(format!(
        "results_{}_{}_{}",
        suite.to_lowercase(),
        sanitize_name(&devices.join("_")),
        timestamp_compact()
    ));
    if results.is_dir() {
        fs::rename(&results, &tmp)
            .or_else(|_| copy_dir_recursive(&results, &tmp).and_then(|_| fs::remove_dir_all(&results)))
            .map_err(|err| format!("Cannot move {}: {err}", results.display()))?;
    }

    if tmp.is_dir() {
        if let Some(zip) = first_zip(&tmp) {
            let dst = session_dir.join(format!(
                "{}_{}_{}_{}.zip",
                suite,
                sanitize_name(model),
                sanitize_name(pda),
                sanitize_name(&devices.join("_"))
            ));
            fs::copy(&zip, &dst)
                .map_err(|err| format!("Cannot copy {} to {}: {err}", zip.display(), dst.display()))?;
            emit_log(app, format!("[{suite}] Result copied: {}", dst.display()));
        } else {
            emit_log(app, format!("[{suite}] No zip found in {}", tmp.display()));
        }
    } else {
        emit_log(app, format!("[{suite}] Result directory not found: {}", results.display()));
    }
    emit_status(app, suite, "Test Done", &devices.join(","), elapsed_secs, log_file, run_id, test_type);
    emit_log(app, format!("[runner] {suite}: Test Done in {}", format_duration(elapsed_secs)));
    Ok(())
}

fn suite_root_for_copy(root: &Path, suite: &str, devices: &[String]) -> Result<PathBuf, String> {
    let first = devices.first().ok_or_else(|| "No device for suite root".to_string())?;
    let props = device_props(first).unwrap_or_default();
    let oneui = prop(&props, "ro.build.version.oneui");
    let android = if oneui == "80500" {
        "16.1".to_string()
    } else {
        android_major(&prop(&props, "ro.build.version.release"))
    };
    match suite {
        "CTS" => resolve_cts_root(root, &android),
        "GTS" => resolve_gts_root(root, ""),
        "STS" => {
            let spl = prop(&props, "ro.build.version.security_patch");
            Ok(root
                .join("STS")
                .join(security_patch_month(&spl))
                .join(android)
                .join("android-sts"))
        }
        _ => Err(format!("Unknown suite: {suite}")),
    }
}

fn suite_root_for_laundry_result(root: &Path, suite: &str, devices: &[String], result_dir: &Path) -> Result<PathBuf, String> {
    if suite == "CTS" || suite == "GTS" {
        if let Some(version) = suite_version_from_result(result_dir) {
            return if suite == "CTS" {
                resolve_cts_root(root, &version)
            } else {
                resolve_gts_root(root, &version)
            };
        }
    }
    suite_root_for_copy(root, suite, devices)
}

fn resolve_cts_root(root: &Path, version_hint: &str) -> Result<PathBuf, String> {
    resolve_versioned_suite_root(root, "CTS", "android-cts", version_hint)
}

fn resolve_gts_root(root: &Path, version_hint: &str) -> Result<PathBuf, String> {
    resolve_versioned_suite_root(root, "GTS", "android-gts", version_hint)
}

fn resolve_versioned_suite_root(root: &Path, suite: &str, suite_dir: &str, version_hint: &str) -> Result<PathBuf, String> {
    let hint = version_hint.trim();
    let base = root.join(suite);

    if !hint.is_empty() {
        let exact = base.join(hint).join(suite_dir);
        if exact.is_dir() {
            return Ok(exact);
        }
    }

    let candidates = available_suite_versions(root, suite, suite_dir)
        .into_iter()
        .filter(|version| hint.is_empty() || suite_version_matches_hint(version, hint))
        .collect::<Vec<_>>();
    if let Some(best) = candidates.into_iter().min_by_key(|version| suite_version_rank(version)) {
        let path = base.join(&best).join(suite_dir);
        if path.is_dir() {
            return Ok(path);
        }
    }

    Err(format!(
        "{suite} tools for version {} not found under {}",
        if hint.is_empty() { "<default>" } else { hint },
        base.display()
    ))
}

fn available_suite_versions(root: &Path, suite: &str, suite_dir: &str) -> Vec<String> {
    let base = root.join(suite);
    let mut versions = fs::read_dir(base)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let path = entry.path();
            if path.join(suite_dir).is_dir() {
                entry.file_name().to_str().map(|value| value.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    versions.sort_by_key(|version| suite_version_rank(version));
    versions
}

fn suite_version_matches_hint(version: &str, hint: &str) -> bool {
    if version == hint {
        return true;
    }
    if hint.contains("_r") {
        return false;
    }
    version
        .strip_prefix(hint)
        .is_some_and(|rest| rest.starts_with("_r"))
}

fn suite_version_rank(version: &str) -> (u32, u32, u32) {
    let (base, revision) = version.split_once("_r").unwrap_or((version, "0"));
    let mut base_parts = base.split('.');
    let major = base_parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    let minor = base_parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    let rev = revision.parse::<u32>().unwrap_or(0);
    (major, minor, rev)
}

fn same_suite_os_version(left: &str, right: &str) -> bool {
    let (left_major, left_minor, _) = suite_version_rank(left);
    let (right_major, right_minor, _) = suite_version_rank(right);
    left_major == right_major && left_minor == right_minor
}

#[cfg(test)]
mod tests {
    use super::{same_suite_os_version, suite_workspace};
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn suite_workspace_isolated_per_run() {
        let temp = tempfile::tempdir().unwrap();
        let suite = temp.path().join("android-gts");
        fs::create_dir_all(suite.join("tools")).unwrap();
        fs::create_dir_all(suite.join("testcases")).unwrap();
        fs::create_dir_all(suite.join("results")).unwrap();
        fs::create_dir_all(suite.join("logs")).unwrap();
        fs::write(suite.join("tools/gts-tradefed"), "#!/bin/bash\n").unwrap();
        fs::write(suite.join("testcases/test.jar"), "jar").unwrap();

        let first = suite_workspace(temp.path(), &suite, "run-one").unwrap();
        let second = suite_workspace(temp.path(), &suite, "run-two").unwrap();
        assert_ne!(first, second);
        assert!(first.join("results").is_dir(), "{}", first.display());
        assert!(second.join("results").is_dir());
        assert!(!fs::symlink_metadata(first.join("tools/gts-tradefed")).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(first.join("testcases")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn cts_revisions_must_keep_the_same_os_version() {
        assert!(same_suite_os_version("16_r4", "16_r5"));
        assert!(same_suite_os_version("16.1_r3", "16.1_r4"));
        assert!(!same_suite_os_version("16_r5", "16.1_r3"));
    }
}

fn tradefed_tool_for_suite(suite_root: &Path, suite: &str) -> Result<PathBuf, String> {
    match suite {
        "CTS" => Ok(suite_root.join("tools/cts-tradefed")),
        "GTS" => Ok(suite_root.join("tools/gts-tradefed")),
        "STS" => Ok(suite_root.join("tools/sts-tradefed")),
        _ => Err(format!("Unknown suite: {suite}")),
    }
}

fn suite_version_from_result(result_dir: &Path) -> Option<String> {
    let (_, version, _) = get_suite_info_from_xml(&result_dir.join("test_result.xml"))?;
    suite_version_from_result_version(&version)
}

fn suite_version_from_result_version(version: &str) -> Option<String> {
    let clean = version.trim();
    if clean.is_empty() {
        return None;
    }
    let prefix = clean.split_whitespace().next().unwrap_or(clean);
    let normalized = prefix
        .trim_start_matches(|ch: char| !ch.is_ascii_digit())
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.' || *ch == '_' || *ch == 'r' || *ch == 'R')
        .collect::<String>()
        .replace("_R", "_r");
    if normalized.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        Some(normalized)
    } else {
        None
    }
}

fn parse_xml_attribute(line: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let pattern = format!("{}={}", attr, quote);
        if let Some(start_idx) = line.find(&pattern) {
            let val_start = start_idx + pattern.len();
            if let Some(end_idx) = line[val_start..].find(quote) {
                return Some(line[val_start..val_start + end_idx].to_string());
            }
        }
    }
    None
}

fn get_suite_info_from_xml(xml_path: &Path) -> Option<(String, String, String)> {
    let file = fs::File::open(xml_path).ok()?;
    let reader = BufReader::new(file);
    for line_result in reader.lines().take(50) {
        let line = line_result.ok()?;
        if line.contains("<Result ") {
            let name = parse_xml_attribute(&line, "suite_name").unwrap_or_default();
            let version = parse_xml_attribute(&line, "suite_version").unwrap_or_default();
            let build = parse_xml_attribute(&line, "suite_build_number").unwrap_or_default();
            return Some((name, version, build));
        }
    }
    None
}

fn verify_laundry_suite_tools(
    app: &AppHandle,
    root: &Path,
    devices: &[String],
    source: &LaundrySource,
    suites: &[&str],
) -> Result<(), String> {
    for suite in suites {
        let needed = match *suite {
            "CTS" => !source.cts_results.is_empty(),
            "GTS" => true,
            "STS" => !source.sts_results.is_empty(),
            _ => false,
        };
        if !needed {
            emit_log(app, format!("[preflight] {suite}: skipped; not found in laundry zip."));
            continue;
        }
        let result_dirs = match *suite {
            "CTS" => &source.cts_results,
            "GTS" => &source.gts_results,
            "STS" => &source.sts_results,
            _ => continue,
        };

        if result_dirs.is_empty() {
            let suite_root = suite_root_for_copy(root, suite, devices)?;
            let tool = tradefed_tool_for_suite(&suite_root, suite)?;
            if !tool.is_file() {
                return Err(format!(
                    "{suite} tool required but not found: {}",
                    tool.display()
                ));
            }
            emit_log(app, format!("[preflight] {suite}: tool ready {}", tool.display()));
            continue;
        }

        for dir in result_dirs {
            let suite_root = suite_root_for_laundry_result(root, suite, devices, dir)?;
            let tool = tradefed_tool_for_suite(&suite_root, suite)?;
            if !tool.is_file() {
                return Err(format!(
                    "{suite} tool required by laundry zip but not found: {}",
                    tool.display()
                ));
            }
            emit_log(app, format!("[preflight] {suite}: tool ready {}", tool.display()));

            let version_txt = suite_root.join("tools/version.txt");
            let local_version = if version_txt.is_file() {
                fs::read_to_string(&version_txt)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let xml_path = dir.join("test_result.xml");
            if xml_path.is_file() {
                if let Some((name, version, build)) = get_suite_info_from_xml(&xml_path) {
                    if !local_version.is_empty() && !build.is_empty() && build != local_version {
                        let normalized_version = suite_version_from_result_version(&version);
                        let local_folder_version = suite_root
                            .parent()
                            .and_then(|p| p.file_name())
                            .and_then(|f| f.to_str())
                            .unwrap_or("");
                        let warn_only_mismatch = !local_folder_version.is_empty()
                            && (normalized_version.as_deref() == Some(local_folder_version)
                                || (*suite == "CTS"
                                    && normalized_version.as_deref().is_some_and(|version| {
                                        same_suite_os_version(local_folder_version, version)
                                    })));
                        if warn_only_mismatch {
                            emit_log(
                                app,
                                format!(
                                    "[preflight][WARN] {suite}: ignoring {} build mismatch (laundry: {} {} / {}, local tool: {}).",
                                    normalized_version.as_deref().unwrap_or("version"),
                                    name, version, build, local_version
                                ),
                            );
                            continue;
                        }
                        return Err(format!(
                            "Mismatched tools version for {suite}:\n\
                             Laundry file has version: {name} {version} ({build})\n\
                             Local tool has version: ({local_version})\n\
                             Please align laundry file and local tools.",
                            suite = suite,
                            name = name,
                            version = version,
                            build = build,
                            local_version = local_version
                        ));
                    }
                    emit_log(
                        app,
                        format!(
                            "[preflight] {suite}: version checked and matched (laundry: {} {} / {}, local tool: {})",
                            name, version, build, local_version
                        ),
                    );
                }
            }
        }
    }
    Ok(())
}

fn extract_zip_safe(zip_path: &Path, dst: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|err| format!("Cannot open {}: {err}", zip_path.display()))?;
    let mut archive = ZipArchive::new(file).map_err(|err| format!("Invalid zip {}: {err}", zip_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| format!("Cannot read zip entry {index}: {err}"))?;
        let Some(enclosed) = entry.enclosed_name().map(|path| path.to_path_buf()) else {
            continue;
        };
        let output = dst.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|err| format!("Cannot create {}: {err}", output.display()))?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("Cannot create {}: {err}", parent.display()))?;
        }
        let mut out = fs::File::create(&output).map_err(|err| format!("Cannot create {}: {err}", output.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|err| format!("Cannot extract {}: {err}", output.display()))?;
    }
    Ok(())
}

fn scan_laundry_results(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let mut cts = Vec::new();
    let mut gts = Vec::new();
    let mut sts = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if !entry.file_type().is_file() || entry.file_name() != "test_result.xml" {
            continue;
        }
        let Some(result_dir) = entry.path().parent().map(Path::to_path_buf) else {
            continue;
        };
        
        let mut suite = None;
        if let Some((name, _, _)) = get_suite_info_from_xml(&entry.path()) {
            let lower_name = name.to_lowercase();
            if lower_name.contains("cts") || lower_name.contains("compatibility") {
                suite = Some("CTS".to_string());
            } else if lower_name.contains("gts") || lower_name.contains("google") {
                suite = Some("GTS".to_string());
            } else if lower_name.contains("sts") || lower_name.contains("security") {
                suite = Some("STS".to_string());
            }
        }
        
        if suite.is_none() {
            suite = suite_hint_from_path(&result_dir);
        }

        match suite.as_deref() {
            Some("CTS") => push_unique(&mut cts, result_dir),
            Some("GTS") => push_unique(&mut gts, result_dir),
            Some("STS") => push_unique(&mut sts, result_dir),
            _ => {}
        }
    }
    (cts, gts, sts)
}

fn scan_laundry_result_infos(root: &Path) -> Result<Vec<LaundryResultInfo>, String> {
    let mut rows = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if !entry.file_type().is_file() || entry.file_name() != "test_result.xml" {
            continue;
        }
        let Some(result_dir) = entry.path().parent() else {
            continue;
        };
        if let Some(info) = parse_laundry_result_info(root, result_dir, entry.path())? {
            rows.push(info);
        }
    }
    Ok(rows)
}

fn parse_laundry_result_info(root: &Path, result_dir: &Path, xml_path: &Path) -> Result<Option<LaundryResultInfo>, String> {
    let file = fs::File::open(xml_path).map_err(|err| format!("Cannot open {}: {err}", xml_path.display()))?;
    let reader = BufReader::new(file);
    let mut suite_name = String::new();
    let mut suite_version = String::new();
    let mut command_line_args = String::new();
    let mut start_ms = None;
    let mut end_ms = None;
    let mut passed = 0;
    let mut failed = 0;
    let mut saw_result = false;

    for line_result in reader.lines() {
        let line = line_result.map_err(|err| format!("Cannot read {}: {err}", xml_path.display()))?;
        if line.contains("<Result ") {
            saw_result = true;
            suite_name = parse_xml_attribute(&line, "suite_name").unwrap_or_default();
            suite_version = parse_xml_attribute(&line, "suite_version").unwrap_or_default();
            command_line_args = parse_xml_attribute(&line, "command_line_args").unwrap_or_default();
            start_ms = parse_xml_attribute(&line, "start").and_then(|value| value.parse::<u64>().ok());
            end_ms = parse_xml_attribute(&line, "end").and_then(|value| value.parse::<u64>().ok());
        } else if line.contains("<Summary ") {
            passed = parse_xml_attribute(&line, "pass")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            failed = parse_xml_attribute(&line, "failed")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }

    if !saw_result {
        return Ok(None);
    }

    let suite = classify_suite(&suite_name)
        .or_else(|| suite_hint_from_path(result_dir))
        .unwrap_or_else(|| "UNKNOWN".to_string());
    if suite == "UNKNOWN" {
        return Ok(None);
    }

    let result_name = result_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| suite.clone());
    let relative = laundry_result_id(root, result_dir);
    Ok(Some(LaundryResultInfo {
        id: relative.clone(),
        suite: suite.clone(),
        testcase: format!("{suite} {result_name}"),
        subtestcases: if command_line_args.trim().is_empty() {
            "-"
        } else {
            command_line_args.trim()
        }
        .to_string(),
        status: "Ready".to_string(),
        time: format_xml_duration(start_ms, end_ms),
        total: passed + failed,
        passed,
        failed,
        suite_version,
        result_dir: relative,
    }))
}

fn classify_suite(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    if lower.contains("cts") || lower.contains("compatibility") {
        Some("CTS".to_string())
    } else if lower.contains("gts") || lower.contains("google") {
        Some("GTS".to_string())
    } else if lower.contains("sts") || lower.contains("security") {
        Some("STS".to_string())
    } else {
        None
    }
}

fn laundry_result_id(root: &Path, result_dir: &Path) -> String {
    let path = result_dir.strip_prefix(root).unwrap_or(result_dir);
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn format_xml_duration(start_ms: Option<u64>, end_ms: Option<u64>) -> String {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) if end >= start => format_duration_hms((end - start) / 1000),
        _ => "-".to_string(),
    }
}

fn format_duration_hms(total: u64) -> String {
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn suite_hint_from_path(path: &Path) -> Option<String> {
    let text = path.display().to_string().to_lowercase();
    if text.contains("cts") {
        Some("CTS".to_string())
    } else if text.contains("gts") {
        Some("GTS".to_string())
    } else if text.contains("sts") {
        Some("STS".to_string())
    } else {
        None
    }
}

fn push_unique(items: &mut Vec<PathBuf>, value: PathBuf) {
    if !items.iter().any(|item| item == &value) {
        items.push(value);
    }
}

fn property_deviceinfo_in_result(result_dir: &Path) -> Option<PathBuf> {
    deviceinfo_in_result(result_dir, "PropertyDeviceInfo.deviceinfo.json")
}

fn client_id_deviceinfo_in_result(result_dir: &Path) -> Option<PathBuf> {
    deviceinfo_in_result(result_dir, "ClientIdDeviceInfo.deviceinfo.json")
}

fn deviceinfo_in_result(result_dir: &Path, filename: &str) -> Option<PathBuf> {
    let direct = result_dir.join("device-info-files").join(filename);
    if direct.is_file() {
        return Some(direct);
    }
    WalkDir::new(result_dir)
        .into_iter()
        .flatten()
        .find(|entry| entry.file_type().is_file() && entry.file_name() == filename)
        .map(|entry| entry.path().to_path_buf())
}

fn latest_property_deviceinfo(results_dir: &Path) -> Option<PathBuf> {
    latest_deviceinfo(results_dir, "PropertyDeviceInfo.deviceinfo.json")
}

fn latest_client_id_deviceinfo(results_dir: &Path) -> Option<PathBuf> {
    latest_deviceinfo(results_dir, "ClientIdDeviceInfo.deviceinfo.json")
}

fn latest_deviceinfo(results_dir: &Path, filename: &str) -> Option<PathBuf> {
    WalkDir::new(results_dir)
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == filename)
        .filter_map(|entry| {
            let path = entry.path().to_path_buf();
            let modified = fs::metadata(&path).and_then(|metadata| metadata.modified()).ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn cleanup_deviceinfo_backups(result_dir: &Path) {
    for entry in WalkDir::new(result_dir).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if (name.contains("PropertyDeviceInfo") || name.contains("ClientIdDeviceInfo")) && name.ends_with(".bak") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn generate_ro_xml_file(serial: &str, dir: &Path) -> Result<String, String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let xml = dir.join("ro.xml");
    let props = [
        "ro.build.fingerprint",
        "ro.build.version.base_os",
        "ro.build.version.security_patch",
        "ro.build.PDA",
        "ril.sw_ver",
        "ril.official_cscver",
        "ro.product.first_api_level",
        "ro.sts.property",
        "ro.csc.sales_code",
        "ro.oem.key1",
        "ro.oem.key2",
        "ro.csc.countryiso_code",
        "ro.csc.country_code",
        "ro.system.build.fingerprint",
        "ro.vendor.build.fingerprint",
        "ro.product.build.version.sdk",
        "ro.build.version.sdk_full",
        "partition.system.verified.root_digest",
        "partition.vendor.verified.root_digest",
        "partition.system_dlkm.verified.root_digest",
        "partition.vendor_dlkm.verified.root_digest",
        "partition.odm.verified.root_digest",
        "partition.product.verified.root_digest",
        "ro.build.characteristics",
        "ro.build.version.oneui",
        "ro.build.version.emergency_base_os",
        "partition.system_ext.verified.root_digest",
    ];
    let map = device_props(serial).unwrap_or_default();
    let mut text = String::from("<RO>\n\n");
    for key in props {
        let val = xml_escape(&prop(&map, key));
        text.push_str(&format!("<{key}>{val}\n</{key}>\n"));
    }

    let features = adb_device_output(serial, &["shell", "pm", "list", "features"]).unwrap_or_default();
    let is_watch = if features.contains("feature:android.hardware.type.watch") { "true" } else { "false" };
    text.push_str(&format!(
        "\n<isWatch>{}</isWatch>\n",
        is_watch
    ));
    let sms = adb_device_output(
        serial,
        &["shell", "cmd", "role", "get-role-holders", "android.app.role.SMS"],
    )
    .unwrap_or_default();
    let message = if sms.contains("com.google.android.apps.messaging") {
        "Android Message"
    } else if sms.contains("com.samsung.android.messaging") {
        "Samsung Message"
    } else {
        "Not Found"
    };
    text.push_str(&format!("\n<message>{message}</message>\n"));
    let browser = adb_device_output(
        serial,
        &[
            "shell",
            "cmd package resolve-activity http://example.com/ | grep packageName",
        ],
    )
    .unwrap_or_default();
    let browser_val = if browser.contains("com.android.chrome") {
        "Chrome"
    } else if browser.contains("com.sec.android.app.sbrowser") {
        "S-Browser"
    } else {
        "Not Found"
    };
    text.push_str(&format!("\n<browser>{browser_val}</browser>\n"));

    let client_ids = adb_device_output(serial, &["shell", "getprop | grep clientidbase"]).unwrap_or_default();
    for line in client_ids.lines() {
        if let Some((key, value)) = parse_getprop_line(line) {
            text.push_str(&format!("\n<{}>{}</{}>\n", key, xml_escape(&value), key));
        }
    }
    text.push_str("\n<ro.version>4.4</ro.version>\n</RO>\n");
    fs::write(&xml, text).map_err(|err| err.to_string())?;
    Ok(xml.display().to_string())
}

fn prepare_devices(app: &AppHandle, devices: &[String]) {
    let mut handles = Vec::new();
    for serial in devices {
        let serial = serial.clone();
        let app_prepare = app.clone();
        handles.push(thread::spawn(move || {
            emit_log(&app_prepare, format!("[prepare][{serial}] waking device"));
            let _ = adb_device_output(&serial, &["root"]);
            thread::sleep(Duration::from_secs(1));
            let _ = adb_device_output(&serial, &["unroot"]);
            let _ = adb_device_output(&serial, &["wait-for-device"]);
            let _ = adb_device_output(
                &serial,
                &[
                    "shell",
                    "settings put global stay_on_while_plugged_in 3; wm dismiss-keyguard; input keyevent KEYCODE_WAKEUP; input keyevent KEYCODE_HOME",
                ],
            );
            emit_log(&app_prepare, format!("[prepare][{serial}] ready"));
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
}

fn connect_wifi(serial: &str, ssid: &str, password: &str) -> Result<String, String> {
    let _ = adb_device_output(serial, &["shell", "svc", "wifi", "enable"]);
    thread::sleep(Duration::from_secs(1));
    let output = if password.is_empty() {
        adb_device_output(serial, &["shell", "cmd", "wifi", "connect-network", ssid, "open"])?
    } else {
        adb_device_output(
            serial,
            &[
                "shell",
                "cmd",
                "wifi",
                "connect-network",
                ssid,
                "wpa2",
                password,
            ],
        )?
    };
    let lower = output.to_lowercase();
    if lower.contains("failed") || lower.contains("error") {
        Err(redact(&output, password))
    } else {
        Ok(if output.trim().is_empty() {
            format!("connect requested for \"{ssid}\"")
        } else {
            redact(output.trim(), password)
        })
    }
}

fn parse_summary(log_file: &Path, suite: &str, devices: &str, run_id: &str, test_type: &str) -> SuiteSummary {
    let content = fs::read_to_string(log_file).unwrap_or_default();
    let mut in_block = false;
    let mut block = Vec::new();
    for line in content.lines() {
        if line.contains("=============== Summary ===============") {
            in_block = true;
        }
        if in_block {
            block.push(line.to_string());
        }
        if in_block && line.contains("============================================") {
            break;
        }
    }
    let joined = block.join("\n");
    SuiteSummary {
        run_id: run_id.to_string(),
        test_type: test_type.to_string(),
        suite: suite.to_string(),
        devices: devices.to_string(),
        run_time: summary_value(&joined, "Total Run time:").unwrap_or_else(|| "N/A".to_string()),
        modules: summary_value(&joined, "modules completed").unwrap_or_else(|| "N/A".to_string()),
        total: number_after(&joined, "Total Tests").unwrap_or(0),
        passed: number_after(&joined, "PASSED").unwrap_or(0),
        failed: number_after(&joined, "FAILED").unwrap_or(0),
    }
}

thread_local! {
    static CURRENT_RUN_ID: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
}

static BUSY_REGISTRY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn suite_workspace(root: &Path, suite_root: &Path, run_id: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let mut hasher = DefaultHasher::new();
        suite_root.hash(&mut hasher);
        let workspace = root.join(".gba-workspaces").join(sanitize_name(run_id)).join(format!("suite_{:x}", hasher.finish()));
        let install = workspace.join(suite_root.file_name().unwrap_or_default());
        if install.exists() {
            return Ok(install);
        }
        fs::create_dir_all(&install).map_err(|err| format!("Cannot create workspace {}: {err}", install.display()))?;
        for entry in fs::read_dir(suite_root).map_err(|err| format!("Cannot read suite root {}: {err}", suite_root.display()))?.flatten() {
            let name = entry.file_name();
            if name == "results" || name == "logs" {
                fs::create_dir(install.join(&name)).map_err(|err| format!("Cannot create workspace directory: {err}"))?;
                continue;
            }
            let target = install.join(&name);
            if name == "subplans" && entry.path().is_dir() {
                copy_dir_recursive(&entry.path(), &target)
                    .map_err(|err| format!("Cannot copy subplans: {err}"))?;
            } else if name == "tools" && entry.path().is_dir() {
                fs::create_dir(&target).map_err(|err| format!("Cannot create workspace tools: {err}"))?;
                for tool in fs::read_dir(entry.path()).map_err(|err| err.to_string())?.flatten() {
                    let tool_target = target.join(tool.file_name());
                    if matches!(tool.file_name().to_str(), Some("cts-tradefed" | "gts-tradefed" | "sts-tradefed" | "test-utils-script")) {
                        fs::copy(tool.path(), &tool_target).map_err(|err| format!("Cannot copy {}: {err}", tool.path().display()))?;
                    } else {
                        symlink(tool.path(), &tool_target).map_err(|err| format!("Cannot link {}: {err}", tool.path().display()))?;
                    }
                }
            } else {
                symlink(entry.path(), &target).map_err(|err| format!("Cannot link {}: {err}", entry.path().display()))?;
            }
        }
        Ok(install)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, run_id);
        Ok(suite_root.to_path_buf())
    }
}

#[derive(Clone, serde::Serialize)]
struct LogPayload {
    run_id: Option<String>,
    line: String,
}

pub fn set_current_run_id(run_id: Option<String>) {
    CURRENT_RUN_ID.with(|id| *id.borrow_mut() = run_id);
}

fn emit_log(app: &AppHandle, line: impl AsRef<str>) {
    let line = line.as_ref().replace("[runner]", "[AI Worker]");
    let run_id = CURRENT_RUN_ID.with(|id| id.borrow().clone());
    if let (Some(run_id), Ok(root)) = (run_id, resolve_auto_root(None)) {
        if let Some(result_dir) = result_dir_for_run(&root, &run_id) {
            append_file_line(&result_dir.join("run.log"), &line);
        }
    }
    emit_log_event(app, line);
}

fn result_dir_for_run(root: &Path, run_id: &str) -> Option<PathBuf> {
    fs::read_dir(root.join("Results")).ok()?.flatten().map(|entry| entry.path())
        .find(|dir| dir.join(".gba-flow.json").is_file() &&
            fs::read_to_string(dir.join(".gba-flow.json")).ok()
                .and_then(|text| serde_json::from_str::<LogFlowOption>(&text).ok())
                .is_some_and(|flow| flow.run_id == run_id))
}

fn emit_log_event(app: &AppHandle, line: impl Into<String>) {
    let run_id = CURRENT_RUN_ID.with(|id| id.borrow().clone());
    let _ = app.emit("gba-run-log", LogPayload {
        run_id,
        line: line.into(),
    });
}

fn log_title_timestamp() -> String {
    Command::new("date")
        .arg("+%d/%m/%Y, %H:%M:%S")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "00:00:00".to_string())
}

fn emit_status(
    app: &AppHandle,
    suite: &str,
    status: &str,
    devices: &str,
    elapsed_secs: u64,
    log_file: &Path,
    run_id: &str,
    test_type: &str,
) {
    let _ = app.emit(
        "gba-suite-status",
        SuiteStatus {
            run_id: run_id.to_string(),
            test_type: test_type.to_string(),
            suite: suite.to_string(),
            status: status.to_string(),
            devices: devices.to_string(),
            elapsed_secs,
            log_file: log_file.display().to_string(),
        },
    );
    
    if let Ok(root) = resolve_auto_root(None) {
        update_busy_device_suite(&root, run_id, suite, status, elapsed_secs, devices);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_laundry_result_update(
    app: &AppHandle,
    source_root: &Path,
    source_result: &Path,
    run_id: &str,
    test_type: &str,
    suite: &str,
    status: &str,
    elapsed_secs: u64,
    summary: Option<&SuiteSummary>,
) {
    let _ = app.emit(
        "gba-laundry-result-update",
        LaundryResultUpdate {
            run_id: run_id.to_string(),
            test_type: test_type.to_string(),
            id: laundry_result_id(source_root, source_result),
            suite: suite.to_string(),
            status: status.to_string(),
            time: if elapsed_secs > 0 { format_duration(elapsed_secs) } else { "-".to_string() },
            total: summary.map(|value| value.total).unwrap_or(0),
            passed: summary.map(|value| value.passed).unwrap_or(0),
            failed: summary.map(|value| value.failed).unwrap_or(0),
        },
    );
}

fn register_pid(app: &AppHandle, run_id: &str, pid: u32) {
    if let Ok(mut active) = app.state::<RunState>().active.lock() {
        active.pids.push((run_id.to_string(), pid));
    }
}

fn unregister_pid(app: &AppHandle, pid: u32) {
    if let Ok(mut active) = app.state::<RunState>().active.lock() {
        active.pids.retain(|(_, p)| *p != pid);
    }
}

fn register_log_file(app: &AppHandle, log_file: &Path) {
    if let Ok(mut active) = app.state::<RunState>().active.lock() {
        active.log_file = Some(log_file.to_path_buf());
    }
}

fn validate_request(request: &RunSuiteRequest) -> Result<(), String> {
    if request.test_type == "Laundry SMR" {
        validate_laundry_smr_pair(request)?;
    }
    match request.test_type.as_str() {
        "Laundry Normal" if request.user_devices.is_empty() => {
            Err("Laundry Normal needs at least one non-userdebug device".to_string())
        }
        "Laundry Normal" | "Laundry SMR" if request.laundry_zip_path.as_ref().is_none_or(|path| path.trim().is_empty()) => {
            Err(format!("{} needs laundry zip file", request.test_type))
        }
        "Cuci SMR" | "MR" | "SKU" if request.user_devices.is_empty() => {
            Err(format!("{} needs at least one non-userdebug device", request.test_type))
        }
        "STS" if request.userdebug_devices.is_empty() => {
            Err("STS needs at least one userdebug device".to_string())
        }
        "SMR" if request.user_devices.is_empty() && request.userdebug_devices.is_empty() => {
            Err(format!("{} needs at least one non-userdebug or userdebug device", request.test_type))
        }
        _ => Ok(()),
    }
}

fn validate_laundry_smr_pair(request: &RunSuiteRequest) -> Result<(), String> {
    let mut has_cts_or_gts = false;
    let mut has_sts = false;
    
    for id in &request.selected_laundry_results {
        let upper_id = id.to_uppercase();
        if upper_id.contains("STS") || upper_id.contains("SECURITY") {
            has_sts = true;
        }
        if upper_id.contains("CTS") || upper_id.contains("COMPATIBILITY") || upper_id.contains("GTS") || upper_id.contains("GOOGLE") {
            has_cts_or_gts = true;
        }
    }
    
    if request.selected_laundry_results.is_empty() {
        if request.user_devices.is_empty() && request.userdebug_devices.is_empty() {
            return Err("Laundry SMR needs at least one device".to_string());
        }
    } else if has_cts_or_gts && has_sts {
        if request.user_devices.is_empty() || request.userdebug_devices.is_empty() {
            return Err("Laundry SMR needs selected USER and USERDEBUG devices".to_string());
        }
    } else if has_cts_or_gts {
        if request.user_devices.is_empty() {
            return Err("Laundry SMR needs selected USER device for CTS/GTS".to_string());
        }
    } else if has_sts {
        if request.userdebug_devices.is_empty() {
            return Err("Laundry SMR needs selected USERDEBUG device for STS".to_string());
        }
    } else {
        if request.user_devices.is_empty() && request.userdebug_devices.is_empty() {
            return Err("Laundry SMR needs at least one device".to_string());
        }
    }

    let serials = selected_serials(request);
    let mut models = Vec::new();
    for serial in &serials {
        let props = device_props(serial).unwrap_or_default();
        let model = first_non_empty(&[prop(&props, "ro.product.model"), "UNKNOWN_MODEL".to_string()]);
        if !models.contains(&model) {
            models.push(model);
        }
    }
    if models.len() > 1 {
        return Err(format!("Laundry SMR devices must use the same model: {}", models.join(", ")));
    }
    Ok(())
}


fn selected_serials(request: &RunSuiteRequest) -> Vec<String> {
    request
        .user_devices
        .iter()
        .chain(request.userdebug_devices.iter())
        .cloned()
        .collect()
}

fn busy_registry_path(root: &Path) -> PathBuf {
    root.join("busy.json")
}

fn read_busy_registry(root: &Path) -> BusyRegistry {
    let path = busy_registry_path(root);
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<BusyRegistry>(&text).ok())
        .unwrap_or_default()
}

fn write_busy_registry(root: &Path, registry: &BusyRegistry) -> Result<(), String> {
    let path = busy_registry_path(root);
    let text = serde_json::to_string_pretty(registry).map_err(|err| err.to_string())?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, text).map_err(|err| format!("Cannot write {}: {err}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|err| format!("Cannot replace {}: {err}", path.display()))
}

fn update_busy_device_suite(root: &std::path::Path, run_id: &str, suite: &str, status: &str, elapsed_secs: u64, serials: &str) {
    let _guard = BUSY_REGISTRY_LOCK.get_or_init(|| Mutex::new(())).lock().ok();
    let mut busy = read_busy_registry(root);
    let serials = serials.split(',').collect::<HashSet<_>>();
    for device in busy.devices.values_mut() {
        if device.run_id == run_id && serials.contains(device.serial.as_str()) {
            device.current_suite = Some(suite.to_string());
            let detail = device.suite_statuses.entry(suite.to_string()).or_default();
            detail.status = status.to_string();
            detail.elapsed_secs = elapsed_secs;
        }
    }
    let _ = write_busy_registry(root, &busy);
}

fn update_busy_device_result_dir(root: &std::path::Path, run_id: &str, result_dir: &str) {
    let _guard = BUSY_REGISTRY_LOCK.get_or_init(|| Mutex::new(())).lock().ok();
    let mut busy = read_busy_registry(root);
    for device in busy.devices.values_mut() {
        if device.run_id == run_id {
            device.result_dir = Some(result_dir.to_string());
        }
    }
    let _ = write_busy_registry(root, &busy);
}

fn update_busy_device_zip(root: &Path, suite: &str, serials: &[String], zip_file: &str) {
    let _guard = BUSY_REGISTRY_LOCK.get_or_init(|| Mutex::new(())).lock().ok();
    let mut busy = read_busy_registry(root);
    for device in busy.devices.values_mut() {
        if serials.contains(&device.serial) {
            if let Some(detail) = device.suite_statuses.get_mut(suite) {
                detail.zip_file = Some(zip_file.to_string());
            }
        }
    }
    let _ = write_busy_registry(root, &busy);
}

fn mark_busy_devices(root: &Path, request: &RunSuiteRequest, serials: &[String], run_id: &str) -> Result<(), String> {
    let _guard = BUSY_REGISTRY_LOCK.get_or_init(|| Mutex::new(())).lock().map_err(|err| err.to_string())?;
    let mut registry = read_busy_registry(root);
    let started_at = timestamp_compact();
    for serial in serials {
        let props = device_props(serial).unwrap_or_default();
        registry.devices.insert(
            serial.clone(),
            BusyDevice {
                serial: serial.clone(),
                is_userdebug: request.userdebug_devices.iter().any(|item| item == serial)
                    || {
                        let build_type = prop(&props, "ro.build.type").to_lowercase();
                        prop(&props, "ro.build.fingerprint").to_lowercase().contains("userdebug")
                            || build_type.contains("userdebug")
                    },
                test_type: request.test_type.clone(),
                model: first_non_empty(&[
                    prop(&props, "ro.product.model"),
                    "UNKNOWN".to_string(),
                ]),
                pda: first_non_empty(&[prop(&props, "ro.build.PDA"), "UNKNOWN".to_string()]),
                run_id: run_id.to_string(),
                started_at: started_at.clone(),
                result_dir: None,
                current_suite: Some("SOURCE".to_string()),
                suite_statuses: HashMap::new(),
            },
        );
    }
    write_busy_registry(root, &registry)
}

fn clear_busy_devices(root: &Path, serials: &[String]) {
    if serials.is_empty() {
        return;
    }
    let Ok(_guard) = BUSY_REGISTRY_LOCK.get_or_init(|| Mutex::new(())).lock() else { return };
    let mut registry = read_busy_registry(root);
    for serial in serials {
        registry.devices.remove(serial);
    }
    let _ = write_busy_registry(root, &registry);
}

fn retry_args(count: u32) -> String {
    if count == 0 {
        String::new()
    } else {
        format!(
            " --enable-token-sharding --max-testcase-run-count {count} --retry-strategy RETRY_ANY_FAILURE"
        )
    }
}

fn serial_args(devices: &[String]) -> String {
    devices
        .iter()
        .map(|device| format!(" --serial {device}"))
        .collect::<String>()
}

fn device_props(serial: &str) -> Result<HashMap<String, String>, String> {
    let output = adb_device_output(serial, &["shell", "getprop"])?;
    let mut map = HashMap::new();
    for line in output.lines() {
        if let Some((key, value)) = parse_getprop_line(line) {
            map.insert(key, value);
        }
    }
    Ok(map)
}

fn parse_getprop_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim().trim_end_matches('\r');
    let without_left = trimmed.strip_prefix('[')?;
    let (key, rest) = without_left.split_once("]: [")?;
    let value = rest.strip_suffix(']').unwrap_or(rest);
    Some((key.to_string(), value.to_string()))
}

fn adb_device_output(serial: &str, args: &[&str]) -> Result<String, String> {
    run_output(Command::new(adb_path()).arg("-s").arg(serial).args(args))
}

fn device_ip(serial: &str) -> Result<String, String> {
    let output = adb_device_output(serial, &["shell", "ip", "-f", "inet", "addr", "show", "wlan0"])?;
    for part in output.split_whitespace() {
        if let Some(ip) = part.strip_suffix("/24").or_else(|| part.strip_suffix("/32")) {
            if ip.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
                return Ok(ip.to_string());
            }
        }
    }
    Ok(String::new())
}

fn prop(map: &HashMap<String, String>, key: &str) -> String {
    map.get(key).cloned().unwrap_or_default()
}

fn token_value(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|part| {
            let (name, value) = part.split_once(':')?;
            (name == key).then(|| value.to_string())
        })
        .unwrap_or_default()
}

fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn android_major(value: &str) -> String {
    let major = value.split('.').next().unwrap_or(value).trim();
    if major.is_empty() {
        "16".to_string()
    } else {
        major.to_string()
    }
}

fn security_patch_month(spl: &str) -> String {
    if spl.len() >= 7 {
        spl[5..7].to_string()
    } else {
        "01".to_string()
    }
}

fn format_duration(total: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

fn summary_value(block: &str, needle: &str) -> Option<String> {
    block.lines().find_map(|line| {
        if line.contains(needle) {
            line.split_once(':')
                .map(|(_, value)| value.trim().to_string())
                .or_else(|| Some(line.trim().to_string()))
        } else {
            None
        }
    })
}

fn number_after(block: &str, needle: &str) -> Option<u64> {
    block
        .lines()
        .find(|line| line.contains(needle))
        .and_then(|line| {
            line.split(|ch: char| !ch.is_ascii_digit())
                .find(|part| !part.is_empty())
                .and_then(|part| part.parse::<u64>().ok())
        })
}

fn write_log_header(path: &Path, suite: &str, command: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::write(
        path,
        format!("=== {suite} START {} ===\nCOMMAND: {command}\n", timestamp_compact()),
    )
    .map_err(|err| err.to_string())
}

fn append_file_line(path: &Path, line: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

#[derive(Debug, Clone, Default)]
struct ResultSnapshot {
    zips: HashMap<PathBuf, SystemTime>,
}

impl ResultSnapshot {
    fn capture(dir: &Path) -> Self {
        let mut zips = HashMap::new();
        for entry in WalkDir::new(dir).into_iter().flatten() {
            if !entry.file_type().is_file() || !is_zip_file(entry.path()) {
                continue;
            }
            if let Ok(modified) = fs::metadata(entry.path()).and_then(|metadata| metadata.modified()) {
                zips.insert(entry.path().to_path_buf(), modified);
            }
        }
        Self { zips }
    }

    fn newest_zip(&self, dir: &Path, since: SystemTime) -> Option<PathBuf> {
        let threshold = since.checked_sub(Duration::from_secs(3)).unwrap_or(since);
        let mut newest: Option<(SystemTime, PathBuf)> = None;
        for entry in WalkDir::new(dir).into_iter().flatten() {
            if !entry.file_type().is_file() || !is_zip_file(entry.path()) {
                continue;
            }
            let path = entry.path().to_path_buf();
            let modified = fs::metadata(&path).and_then(|metadata| metadata.modified()).ok()?;
            if modified < threshold {
                continue;
            }
            if self
                .zips
                .get(&path)
                .is_some_and(|previous| *previous == modified)
            {
                continue;
            }
            match newest.as_ref() {
                Some((current, _)) if modified <= *current => {}
                _ => newest = Some((modified, path)),
            }
        }
        newest.map(|(_, path)| path)
    }
}

fn first_zip(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).ok()?.flatten() {
            let item = entry.path();
            if item.is_dir() {
                stack.push(item);
            } else if is_zip_file(&item) {
                return Some(item);
            }
        }
    }
    None
}

fn is_zip_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn timestamp_compact() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    secs.to_string()
}

fn write_latest_result_hint(root: &Path, session_dir: &Path) {
    let _ = fs::create_dir_all(root.join(".gba-agentic-results"));
    if let Ok(flow) = fs::read_to_string(session_dir.join(".gba-flow.json")) {
        if let Ok(value) = serde_json::from_str::<LogFlowOption>(&flow) {
            let _ = fs::write(root.join(".gba-agentic-results").join(sanitize_name(&value.run_id)), session_dir.display().to_string());
        }
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn redact(value: impl AsRef<str>, secret: &str) -> String {
    if secret.is_empty() {
        value.as_ref().to_string()
    } else {
        value.as_ref().replace(secret, "********")
    }
}

fn check_command(name: &str) -> String {
    if which(name).is_some() {
        format!("OK command: {name}")
    } else {
        format!("MISSING command: {name}")
    }
}

fn check_file(name: &str, path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    if path.is_file() {
        format!("OK file: {name}")
    } else {
        format!("MISSING file: {} ({})", name, path.display())
    }
}

fn check_dir(name: &str, path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    if path.is_dir() {
        format!("OK dir: {name}")
    } else {
        format!("MISSING dir: {} ({})", name, path.display())
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn adb_path() -> String {
    env::var("ADB").unwrap_or_else(|_| "adb".to_string())
}

fn resolve_auto_root(value: Option<String>) -> Result<PathBuf, String> {
    let configured = value
        .filter(|item| !item.trim().is_empty())
        .unwrap_or(default_auto_root()?);
    let root = PathBuf::from(&configured);
    if root.is_dir() {
        Ok(root)
    } else {
        let fallback = PathBuf::from(default_auto_root()?);
        if fallback.is_dir() {
            Ok(fallback)
        } else {
            Err(format!("AUTO root does not exist: {}", root.display()))
        }
    }
}

fn run_output(command: &mut Command) -> Result<String, String> {
    let output = command.output().map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!("Command failed with status {}", output.status))
        } else {
            Err(stderr)
        }
    }
}

fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer");
        command.arg(path);
        command
    };

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    command
        .spawn()
        .map_err(|err| format!("Failed to open {}: {err}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn configure_child(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            libc_setpgid();
            Ok(())
        });
    }
}

#[cfg(windows)]
fn configure_child(command: &mut Command) {
    command.creation_flags(0x08000000);
}

#[cfg(unix)]
fn libc_setpgid() {
    unsafe extern "C" {
        fn setpgid(pid: i32, pgid: i32) -> i32;
    }
    unsafe {
        setpgid(0, 0);
    }
}

#[cfg(unix)]
fn terminate_process_tree(pid: u32) {
    let group = format!("-{pid}");
    let _ = Command::new("kill").args(["-TERM", &group]).status();
    thread::sleep(Duration::from_millis(800));
    let _ = Command::new("kill").args(["-KILL", &group]).status();
}

#[cfg(windows)]
fn terminate_process_tree(pid: u32) {
    let mut command = Command::new("taskkill");
    command.args(["/PID", &pid.to_string(), "/T", "/F"]);
    command.creation_flags(0x08000000);
    let _ = command.status();
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        // Work around WebKitGTK crashes or black windows on some Mesa/Wayland/DMABUF
        // combinations. Setting these here makes packaged deb/rpm launches behave
        // consistently without relying on a shell wrapper.
        env::set_var("GDK_BACKEND", env::var("GDK_BACKEND").unwrap_or_else(|_| "x11".to_string()));
        env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RunState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                start_api_server(handle).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            default_auto_root,
            preflight,
            list_devices,
            run_suite,
            cancel_run,
            reset_busy_state,
            set_device_lamp,
            open_scrcpy,
            open_result,
            analyze_laundry_zip,
            generate_ro_xml,
            check_laundry_mismatches
        ])
        .run(tauri::generate_context!())
        .expect("error while running GBA Agentic Runner");
}
