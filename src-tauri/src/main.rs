#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tempfile::TempDir;
use walkdir::WalkDir;
use zip::ZipArchive;

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
    sales_code: String,
    model: String,
    pda: String,
    cp: String,
    csc: String,
    ip: String,
    busy: bool,
    busy_reason: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RunSuiteRequest {
    run_id: Option<String>,
    auto_root: String,
    test_type: String,
    laundry_zip_path: Option<String>,
    user_devices: Vec<String>,
    userdebug_devices: Vec<String>,
    retry_count: u32,
    wifi_enabled: bool,
    wifi_ssid: String,
    wifi_password: String,
    timeout_secs: u64,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BusyRegistry {
    devices: HashMap<String, BusyDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BusyDevice {
    serial: String,
    test_type: String,
    model: String,
    pda: String,
    run_id: String,
    started_at: String,
}

#[derive(Default)]
struct RunState {
    active: Mutex<ActiveRun>,
}

#[derive(Default)]
struct ActiveRun {
    running: bool,
    pids: Vec<u32>,
    root: Option<PathBuf>,
    busy_serials: Vec<String>,
}

#[derive(Debug, Clone)]
struct SuiteOutcome {
    exit_code: i32,
    elapsed_secs: u64,
}

#[tauri::command]
fn default_auto_root() -> Result<String, String> {
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
    lines.push(check_command("adb"));
    lines.push(check_command("java"));
    lines.push(check_dir("CTS", root.join("CTS")));
    lines.push(check_dir("GTS", root.join("GTS")));
    lines.push(check_dir("GTS/android-gts", root.join("GTS/android-gts")));
    lines.push(check_file(
        "gts-tradefed",
        root.join("GTS/android-gts/tools/gts-tradefed"),
    ));
    lines.push(check_dir("STS", root.join("STS")));
    lines.push(check_dir("Results", root.join("Results")));

    let cts_versions = available_cts_versions(&root);
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

        let busy_entry = busy.devices.get(&serial);
        devices.push(DeviceInfo {
            serial: serial.clone(),
            state,
            is_userdebug: fingerprint.contains("userdebug"),
            fingerprint,
            security_patch: prop(&props, "ro.build.version.security_patch"),
            android: prop(&props, "ro.build.version.release"),
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
        });
    }

    Ok(devices)
}

#[tauri::command]
fn generate_ro_xml(serial: String, output_dir: String) -> Result<String, String> {
    generate_ro_xml_file(&serial, Path::new(&output_dir))
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

        let result_dir = latest_result_dir_hint(&request.auto_root);
        let _ = app.emit(
            "gba-run-finished",
            RunFinished {
                run_id,
                test_type: request.test_type.clone(),
                exit_code,
                result_dir,
            },
        );
    });

    Ok(())
}

#[tauri::command]
fn cancel_run(app: AppHandle, run_state: State<'_, RunState>) -> Result<(), String> {
    let pids = {
        let active = run_state.active.lock().map_err(|err| err.to_string())?;
        if !active.running {
            emit_log(&app, "[runner] No active run to cancel.");
            return Ok(());
        }
        active.pids.clone()
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

#[tauri::command]
fn reset_busy_state(auto_root: Option<String>) -> Result<(), String> {
    let root = resolve_auto_root(auto_root)?;
    let path = busy_registry_path(&root);
    if path.exists() {
        fs::remove_file(&path).map_err(|err| format!("Cannot remove {}: {err}", path.display()))?;
    }
    Ok(())
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
        "{}_{}_{}_{}devs_{}",
        sanitize_name(&request.test_type),
        sanitize_name(&model),
        sanitize_name(&pda),
        all_devices.len(),
        suffix
    );
    let session_dir = root.join("Results").join(session_name);
    let log_dir = session_dir.join("Log");
    fs::create_dir_all(&log_dir).map_err(|err| format!("Cannot create result dir: {err}"))?;
    write_latest_result_hint(&root, &session_dir);
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
                    "run gts --subplan gtssmr",
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
    Ok(final_code)
}

#[derive(Debug, Clone)]
struct LaundrySource {
    _temp: std::sync::Arc<TempDir>,
    cts_results: Vec<PathBuf>,
    gts_results: Vec<PathBuf>,
    sts_results: Vec<PathBuf>,
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
    verify_laundry_suite_tools(app, root, &request.user_devices, &source, &["GTS", "CTS"])?;
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
        let source_sts = source.sts_results.clone();
        let timeout_sts = request.timeout_secs;
        let model_sts = model.to_string();
        let pda_sts = pda.to_string();
        let run_id_sts = run_id.to_string();
        let test_type_sts = request.test_type.clone();
        Some(thread::spawn(move || {
            run_laundry_retries_without_deviceinfo(
                &app_sts,
                &root_sts,
                &session_sts,
                &log_sts,
                "STS",
                &devices_sts,
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

        codes.extend(run_laundry_cts_filters(
            app,
            root,
            log_dir,
            &request.user_devices,
            request.timeout_secs,
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

    let (cts_results, gts_results, sts_results) = scan_laundry_results(temp.path());
    emit_log(
        app,
        format!(
            "[runner] Scanned laundry results: CTS={} GTS={} STS={}",
            cts_results.len(),
            gts_results.len(),
            sts_results.len()
        ),
    );
    Ok(LaundrySource {
        _temp: temp,
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
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<PathBuf, String> {
    let gts_root = root.join("GTS/android-gts");
    let gts_exe = gts_root.join("tools/gts-tradefed");
    if !gts_exe.is_file() {
        return Err(format!("gts-tradefed not found: {}", gts_exe.display()));
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
    let deviceinfo = latest_property_deviceinfo(&gts_root.join("results"))
        .ok_or_else(|| "PropertyDeviceInfo.deviceinfo.json not found after initial GTS".to_string())?;
    emit_log(app, format!("[runner] Deviceinfo source: {}", deviceinfo.display()));
    let stable_deviceinfo = session_dir.join(format!(
        "PropertyDeviceInfo_{}_{}.deviceinfo.json",
        sanitize_name(&devices.join("_")),
        timestamp_compact()
    ));
    fs::copy(&deviceinfo, &stable_deviceinfo)
        .map_err(|err| format!("Cannot preserve deviceinfo {}: {err}", deviceinfo.display()))?;
    emit_log(app, format!("[runner] Deviceinfo preserved: {}", stable_deviceinfo.display()));
    copy_suite_result(app, root, session_dir, "GTS", devices, model, pda, &log_file, outcome.elapsed_secs, run_id, test_type)?;
    Ok(stable_deviceinfo)
}

fn run_laundry_cts_filters(
    app: &AppHandle,
    root: &Path,
    log_dir: &Path,
    devices: &[String],
    timeout_secs: u64,
    run_id: &str,
    test_type: &str,
) -> Result<Vec<i32>, String> {
    let first = devices.first().ok_or_else(|| "Laundry SMR CTS needs user devices".to_string())?;
    let props = device_props(first).unwrap_or_default();
    let cts_root = resolve_cts_root(root, &android_major(&prop(&props, "ro.build.version.release")))?;
    let cts_exe = cts_root.join("tools/cts-tradefed");
    if !cts_exe.is_file() {
        return Err(format!("cts-tradefed not found: {}", cts_exe.display()));
    }
    let cmd = format!(
        "run cts --include-filter 'CtsEdiHostTestCases' --include-filter 'CtsLibcoreTestCases tests.targets.security.SignatureTestMD2withRSA#testSignature' --shard-count {}{}",
        devices.len(),
        serial_args(devices)
    );
    emit_log(app, "[runner] Laundry SMR: running CTS EDI/security filters.");
    let log_file = log_dir.join(format!("laundry_cts_filters_{}devs.log", devices.len()));
    let outcome = run_suite_process(app, "CTS", devices, &cts_exe, &cts_root, &cmd, true, &log_file, timeout_secs, run_id, test_type, true)?;
    Ok(vec![outcome.exit_code])
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_retries_with_deviceinfo(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    suite: &str,
    devices: &[String],
    source_results: &[PathBuf],
    deviceinfo: &Path,
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
    run_laundry_retries(app, root, session_dir, log_dir, suite, devices, source_results, Some(deviceinfo), timeout_secs, model, pda, run_id, test_type)
}

#[allow(clippy::too_many_arguments)]
fn run_laundry_retries_without_deviceinfo(
    app: &AppHandle,
    root: &Path,
    session_dir: &Path,
    log_dir: &Path,
    suite: &str,
    devices: &[String],
    source_results: &[PathBuf],
    timeout_secs: u64,
    model: &str,
    pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<Vec<i32>, String> {
    emit_log(app, format!("[runner] {suite}: {} result(s) queued for retry.", source_results.len()));
    run_laundry_retries(app, root, session_dir, log_dir, suite, devices, source_results, None, timeout_secs, model, pda, run_id, test_type)
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
    _session_dir: &Path,
    log_dir: &Path,
    suite: &str,
    devices: &[String],
    source_results: &[PathBuf],
    replacement_deviceinfo: Option<&Path>,
    timeout_secs: u64,
    _model: &str,
    _pda: &str,
    run_id: &str,
    test_type: &str,
) -> Result<Vec<i32>, String> {
    let mut codes = Vec::new();
    for (index, source) in source_results.iter().enumerate() {
        let suite_root = suite_root_for_laundry_result(root, suite, devices, source)?;
        let executable = tradefed_tool_for_suite(&suite_root, suite)?;
        if !executable.is_file() {
            return Err(format!("{suite} tradefed not found: {}", executable.display()));
        }
        let results_dir = suite_root.join("results");
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
        if let Some(replacement) = replacement_deviceinfo {
            if let Some(target_info) = property_deviceinfo_in_result(&target) {
                let backup = target_info.with_extension("deviceinfo.json.bak");
                fs::copy(&target_info, &backup)
                    .map_err(|err| format!("Cannot backup {}: {err}", target_info.display()))?;
                fs::copy(replacement, &target_info)
                    .map_err(|err| format!("Cannot replace {}: {err}", target_info.display()))?;
                emit_log(app, format!("[runner] {suite}: replaced {} backup={}", target_info.display(), backup.display()));
            } else {
                emit_log(app, format!("[runner] {suite}: no PropertyDeviceInfo in {}; retrying NOT_EXECUTED without replace.", target.display()));
            }
        }
        let cmd = format!(
            "run retry --retry 0 --retry-type NOT_EXECUTED --shard-count {}{}",
            devices.len(),
            serial_args(devices)
        );
        emit_log(app, format!("[runner] {suite}: retry command for {}", target.display()));
        let log_file = log_dir.join(format!("laundry_retry_{}_{}_{}devs.log", suite.to_lowercase(), index + 1, devices.len()));
        let outcome = run_suite_process(app, suite, devices, &executable, &suite_root, &cmd, suite != "STS", &log_file, timeout_secs, run_id, test_type, true)?;
        codes.push(outcome.exit_code);
    }
    Ok(codes)
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
    let android = prop(&props, "ro.build.version.release");
    let major = android_major(&android);
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
    copy_suite_result(app, root, session_dir, "CTS", devices, model, pda, &cts_log, cts_outcome.elapsed_secs, run_id, test_type)?;

    if cts_outcome.exit_code != 0 {
        emit_log(app, "[gts] CTS returned non-zero; GTS will still be attempted.");
    }
    emit_log(app, format!("[runner] CTS: done; GTS: starting command '{gts_command}' on {}", devices.join(",")));

    let gts_root = root.join("GTS/android-gts");
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
    copy_suite_result(app, root, session_dir, "GTS", devices, model, pda, &gts_log, gts_outcome.elapsed_secs, run_id, test_type)?;

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

    let sts_cmd = format!(
        "run sts-dynamic-incremental --test-arg com.android.compatibility.common.tradefed.testtype.JarHostTest:set-option:android.security.sts.KernelLtsTest:acknowledge_kernel_update_requirement_warning_failure:true --shard-count {}{}{}",
        devices.len(),
        serial_args(devices),
        retry_args
    );
    let sts_log = log_dir.join(format!("sts_{}_{}devs.log", sanitize_name(model), devices.len()));
    emit_log(app, format!("[runner] STS: starting dynamic incremental on {}", devices.join(",")));
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
    copy_suite_result(app, root, session_dir, "STS", devices, model, pda, &sts_log, outcome.elapsed_secs, run_id, test_type)?;
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
        emit_status(app, suite, "Starting", &devices_text, 0, log_file, run_id, test_type);
    }
    emit_log(app, format!("[runner] {suite}: launching tradefed for {devices_text}"));
    emit_log(app, format!("[{suite}] {suite_command}"));
    write_log_header(log_file, suite, suite_command)?;

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
    register_pid(app, pid);
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

    pipe_child_output(app.clone(), suite.to_string(), log_file.to_path_buf(), &mut child);

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

fn suite_log_has_completion_marker(log_file: &Path) -> bool {
    let content = fs::read_to_string(log_file).unwrap_or_default();
    content
        .lines()
        .rev()
        .take(120)
        .any(|line| line.contains("Result/Log Location") || line.contains("=============== Summary ==============="))
}

fn pipe_child_output(app: AppHandle, suite: String, log_file: PathBuf, child: &mut Child) {
    if let Some(stdout) = child.stdout.take() {
        let app_stdout = app.clone();
        let suite_stdout = suite.clone();
        let log_stdout = log_file.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                append_file_line(&log_stdout, &line);
                let _ = app_stdout.emit("gba-run-log", format!("[{suite_stdout}] {line}"));
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let app_stderr = app;
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                append_file_line(&log_file, &line);
                let _ = app_stderr.emit("gba-run-log", format!("[{suite}] {line}"));
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
) -> Result<(), String> {
    emit_status(app, suite, "Copying result", &devices.join(","), elapsed_secs, log_file, run_id, test_type);
    emit_log(app, format!("[runner] {suite}: copying result for {}", devices.join(",")));
    let suite_root = suite_root_for_copy(root, suite, devices)?;
    let results = suite_root.join("results");
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
    let android = android_major(&prop(&props, "ro.build.version.release"));
    match suite {
        "CTS" => resolve_cts_root(root, &android),
        "GTS" => Ok(root.join("GTS/android-gts")),
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
    if suite == "CTS" {
        if let Some(version) = cts_version_from_result(result_dir) {
            return resolve_cts_root(root, &version);
        }
    }
    suite_root_for_copy(root, suite, devices)
}

fn resolve_cts_root(root: &Path, version_hint: &str) -> Result<PathBuf, String> {
    let hint = version_hint.trim();
    let cts_base = root.join("CTS");
    if hint.is_empty() {
        return Err("CTS version hint is empty".to_string());
    }

    let exact = cts_base.join(hint).join("android-cts");
    if exact.is_dir() {
        return Ok(exact);
    }

    let candidates = available_cts_versions(root)
        .into_iter()
        .filter(|version| cts_version_matches_hint(version, hint))
        .collect::<Vec<_>>();
    if let Some(best) = candidates.into_iter().min_by_key(|version| cts_version_rank(version)) {
        let path = cts_base.join(&best).join("android-cts");
        if path.is_dir() {
            return Ok(path);
        }
    }

    Err(format!(
        "CTS tools for version {hint} not found under {}",
        cts_base.display()
    ))
}

fn available_cts_versions(root: &Path) -> Vec<String> {
    let cts_base = root.join("CTS");
    let mut versions = fs::read_dir(cts_base)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let path = entry.path();
            if path.join("android-cts").is_dir() {
                entry.file_name().to_str().map(|value| value.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    versions.sort_by_key(|version| cts_version_rank(version));
    versions
}

fn cts_version_matches_hint(version: &str, hint: &str) -> bool {
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

fn cts_version_rank(version: &str) -> (u32, u32, u32) {
    let (base, revision) = version.split_once("_r").unwrap_or((version, "0"));
    let mut base_parts = base.split('.');
    let major = base_parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    let minor = base_parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    let rev = revision.parse::<u32>().unwrap_or(0);
    (major, minor, rev)
}

fn tradefed_tool_for_suite(suite_root: &Path, suite: &str) -> Result<PathBuf, String> {
    match suite {
        "CTS" => Ok(suite_root.join("tools/cts-tradefed")),
        "GTS" => Ok(suite_root.join("tools/gts-tradefed")),
        "STS" => Ok(suite_root.join("tools/sts-tradefed")),
        _ => Err(format!("Unknown suite: {suite}")),
    }
}

fn cts_version_from_result(result_dir: &Path) -> Option<String> {
    let (_, version, _) = get_suite_info_from_xml(&result_dir.join("test_result.xml"))?;
    cts_version_from_suite_version(&version)
}

fn cts_version_from_suite_version(version: &str) -> Option<String> {
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
    let direct = result_dir
        .join("device-info-files")
        .join("PropertyDeviceInfo.deviceinfo.json");
    if direct.is_file() {
        return Some(direct);
    }
    WalkDir::new(result_dir)
        .into_iter()
        .flatten()
        .find(|entry| entry.file_type().is_file() && entry.file_name() == "PropertyDeviceInfo.deviceinfo.json")
        .map(|entry| entry.path().to_path_buf())
}

fn latest_property_deviceinfo(results_dir: &Path) -> Option<PathBuf> {
    WalkDir::new(results_dir)
        .into_iter()
        .flatten()
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == "PropertyDeviceInfo.deviceinfo.json")
        .filter_map(|entry| {
            let path = entry.path().to_path_buf();
            let modified = fs::metadata(&path).and_then(|metadata| metadata.modified()).ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn generate_ro_xml_file(serial: &str, dir: &Path) -> Result<String, String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let xml = dir.join(format!("ro_{}.xml", sanitize_name(serial)));
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
    let mut text = String::from("<RO>\n");
    for key in props {
        let val = xml_escape(&prop(&map, key));
        text.push_str(&format!("    <{key}>{val}</{key}>\n"));
    }

    let features = adb_device_output(serial, &["shell", "pm", "list", "features"]).unwrap_or_default();
    text.push_str(&format!(
        "\n    <isWatch>{}</isWatch>\n",
        features.contains("feature:android.hardware.type.watch")
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
    text.push_str(&format!("    <message>{message}</message>\n"));
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
    text.push_str(&format!("    <browser>{browser_val}</browser>\n"));

    let client_ids = adb_device_output(serial, &["shell", "getprop | grep clientidbase"]).unwrap_or_default();
    for line in client_ids.lines() {
        if let Some((key, value)) = parse_getprop_line(line) {
            text.push_str(&format!("    <{}>{}</{}>\n", key, xml_escape(&value), key));
        }
    }
    text.push_str("\n    <ro.version>4.4</ro.version>\n</RO>\n");
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

fn emit_log(app: &AppHandle, line: impl AsRef<str>) {
    let _ = app.emit("gba-run-log", line.as_ref().to_string());
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
}

fn register_pid(app: &AppHandle, pid: u32) {
    if let Ok(mut active) = app.state::<RunState>().active.lock() {
        active.pids.push(pid);
    }
}

fn unregister_pid(app: &AppHandle, pid: u32) {
    if let Ok(mut active) = app.state::<RunState>().active.lock() {
        active.pids.retain(|value| *value != pid);
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
    if request.user_devices.is_empty() || request.userdebug_devices.is_empty() {
        return Err("Laundry SMR needs selected USER and USERDEBUG devices".to_string());
    }
    let serials = selected_serials(request);
    let mut models = Vec::new();
    let mut families = Vec::new();
    for serial in &serials {
        let props = device_props(serial).unwrap_or_default();
        let model = first_non_empty(&[prop(&props, "ro.product.model"), "UNKNOWN_MODEL".to_string()]);
        let family = fingerprint_family(&prop(&props, "ro.build.fingerprint"), &model);
        if !models.contains(&model) {
            models.push(model);
        }
        if !families.contains(&family) {
            families.push(family);
        }
    }
    if models.len() > 1 {
        return Err(format!("Laundry SMR devices must use the same model: {}", models.join(", ")));
    }
    if families.len() > 1 {
        return Err(format!("Laundry SMR devices must use the same fingerprint family: {}", families.join(", ")));
    }
    Ok(())
}

fn fingerprint_family(fingerprint: &str, fallback_model: &str) -> String {
    let product_part = fingerprint.split(':').next().unwrap_or("").trim();
    let pieces = product_part
        .split('/')
        .filter(|part| !part.is_empty())
        .take(3)
        .collect::<Vec<_>>();
    if pieces.len() == 3 {
        pieces.join("/")
    } else {
        fallback_model.to_string()
    }
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
    fs::write(&path, text).map_err(|err| format!("Cannot write {}: {err}", path.display()))
}

fn mark_busy_devices(root: &Path, request: &RunSuiteRequest, serials: &[String], run_id: &str) -> Result<(), String> {
    let mut registry = read_busy_registry(root);
    let started_at = timestamp_compact();
    for serial in serials {
        let props = device_props(serial).unwrap_or_default();
        registry.devices.insert(
            serial.clone(),
            BusyDevice {
                serial: serial.clone(),
                test_type: request.test_type.clone(),
                model: first_non_empty(&[
                    prop(&props, "ro.product.model"),
                    "UNKNOWN".to_string(),
                ]),
                pda: first_non_empty(&[prop(&props, "ro.build.PDA"), "UNKNOWN".to_string()]),
                run_id: run_id.to_string(),
                started_at: started_at.clone(),
            },
        );
    }
    write_busy_registry(root, &registry)
}

fn clear_busy_devices(root: &Path, serials: &[String]) {
    if serials.is_empty() {
        return;
    }
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

fn first_zip(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path).ok()?.flatten() {
            let item = entry.path();
            if item.is_dir() {
                stack.push(item);
            } else if item.extension().is_some_and(|ext| ext == "zip") {
                return Some(item);
            }
        }
    }
    None
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
    let _ = fs::write(root.join(".gba-agentic-latest-result"), session_dir.display().to_string());
}

fn latest_result_dir_hint(root_value: &str) -> String {
    let root = Path::new(root_value);
    fs::read_to_string(root.join(".gba-agentic-latest-result"))
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
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
            generate_ro_xml
        ])
        .run(tauri::generate_context!())
        .expect("error while running GBA Agentic Runner");
}
