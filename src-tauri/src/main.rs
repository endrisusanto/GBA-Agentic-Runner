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
}

#[derive(Debug, Clone, Deserialize)]
struct RunSuiteRequest {
    auto_root: String,
    test_type: String,
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
    suite: String,
    status: String,
    devices: String,
    elapsed_secs: u64,
    log_file: String,
}

#[derive(Debug, Clone, Serialize)]
struct SuiteSummary {
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
    exit_code: i32,
    result_dir: String,
}

#[derive(Default)]
struct RunState {
    active: Mutex<ActiveRun>,
}

#[derive(Default)]
struct ActiveRun {
    running: bool,
    pids: Vec<u32>,
}

#[derive(Debug, Clone)]
struct SuiteOutcome {
    exit_code: i32,
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

    for version in ["14", "15", "16", "16.1"] {
        let cts_root = root.join("CTS").join(version).join("android-cts");
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
    {
        let mut active = run_state.active.lock().map_err(|err| err.to_string())?;
        if active.running {
            return Err("A run is already active".to_string());
        }
        active.running = true;
        active.pids.clear();
    }

    thread::spawn(move || {
        let result = run_suite_blocking(app.clone(), root, request.clone());
        let exit_code = match result {
            Ok(code) => code,
            Err(err) => {
                emit_log(&app, format!("[runner] {err}"));
                1
            }
        };

        if let Ok(mut active) = app.state::<RunState>().active.lock() {
            active.running = false;
            active.pids.clear();
        }

        let result_dir = latest_result_dir_hint(&request.auto_root);
        let _ = app.emit(
            "gba-run-finished",
            RunFinished {
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
        active.running = false;
        active.pids.clear();
    }
    let _ = app.emit(
        "gba-suite-status",
        SuiteStatus {
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

fn run_suite_blocking(app: AppHandle, root: PathBuf, request: RunSuiteRequest) -> Result<i32, String> {
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
            )?;
            exit_codes.push(outcome.exit_code);
        }
        "SMR" => {
            let app_sts = app.clone();
            let root_sts = root.clone();
            let session_sts = session_dir.clone();
            let log_sts = log_dir.clone();
            let userdebug = request.userdebug_devices.clone();
            let retry_sts = retry_args.clone();
            let model_sts = model.clone();
            let pda_sts = pda.clone();
            let timeout = request.timeout_secs;
            let sts_handle = thread::spawn(move || {
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
                )
                .map(|outcome| outcome.exit_code)
                .unwrap_or(1)
            });

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
            )?;
            exit_codes.push(cts_gts.exit_code);
            exit_codes.push(sts_handle.join().unwrap_or(1));
        }
        other => return Err(format!("Unknown test type: {other}")),
    }

    let final_code = if exit_codes.iter().all(|code| *code == 0) { 0 } else { 1 };
    emit_log(&app, format!("[runner] Completed with exit={final_code}."));
    Ok(final_code)
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
) -> Result<SuiteOutcome, String> {
    let first = devices.first().ok_or_else(|| "CTS/GTS needs user devices".to_string())?;
    let props = device_props(first).unwrap_or_default();
    let android = prop(&props, "ro.build.version.release");
    let major = android_major(&android);
    let cts_root = root.join("CTS").join(&major).join("android-cts");
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
    )?;
    copy_suite_result(app, root, session_dir, "CTS", devices, model, pda, &cts_log)?;

    if cts_outcome.exit_code != 0 {
        emit_log(app, "[gts] CTS returned non-zero; GTS will still be attempted.");
    }

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
    )?;
    copy_suite_result(app, root, session_dir, "GTS", devices, model, pda, &gts_log)?;

    Ok(SuiteOutcome {
        exit_code: if cts_outcome.exit_code == 0 && gts_outcome.exit_code == 0 {
            0
        } else {
            1
        },
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
    )?;
    copy_suite_result(app, root, session_dir, "STS", devices, model, pda, &sts_log)?;
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn run_suite_process(
    app: &AppHandle,
    suite: &str,
    devices: &[String],
    executable: &Path,
    cwd: &Path,
    suite_command: &str,
    via_pipe: bool,
    log_file: &Path,
    timeout_secs: u64,
) -> Result<SuiteOutcome, String> {
    let devices_text = devices.join(",");
    emit_status(app, suite, "Starting", &devices_text, 0, log_file);
    emit_log(app, format!("[{suite}] {suite_command}"));
    write_log_header(log_file, suite, suite_command)?;

    let mut command = Command::new(executable);
    command.current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
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
    let exit_code = wait_with_timeout(app, &mut child, suite, devices, log_file, timeout_secs);
    unregister_pid(app, pid);
    let elapsed = started.elapsed().as_secs();

    let summary = parse_summary(log_file, suite, &devices_text);
    let _ = app.emit("gba-summary", summary);
    emit_status(
        app,
        suite,
        if exit_code == 0 { "Completed" } else { "Failed" },
        &devices_text,
        elapsed,
        log_file,
    );

    Ok(SuiteOutcome { exit_code })
}

fn wait_with_timeout(
    app: &AppHandle,
    child: &mut Child,
    suite: &str,
    devices: &[String],
    log_file: &Path,
    timeout_secs: u64,
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
        if started.elapsed().as_secs() > timeout_secs {
            emit_status(
                app,
                suite,
                "Timeout",
                &devices.join(","),
                started.elapsed().as_secs(),
                log_file,
            );
            terminate_process_tree(child.id());
            return 124;
        }
        emit_status(
            app,
            suite,
            "Running",
            &devices.join(","),
            started.elapsed().as_secs(),
            log_file,
        );
        thread::sleep(Duration::from_secs(1));
    }
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
) -> Result<(), String> {
    emit_status(app, suite, "Copying result", &devices.join(","), 0, log_file);
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
    Ok(())
}

fn suite_root_for_copy(root: &Path, suite: &str, devices: &[String]) -> Result<PathBuf, String> {
    let first = devices.first().ok_or_else(|| "No device for suite root".to_string())?;
    let props = device_props(first).unwrap_or_default();
    let android = android_major(&prop(&props, "ro.build.version.release"));
    match suite {
        "CTS" => Ok(root.join("CTS").join(android).join("android-cts")),
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

fn parse_summary(log_file: &Path, suite: &str, devices: &str) -> SuiteSummary {
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
) {
    let _ = app.emit(
        "gba-suite-status",
        SuiteStatus {
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
    match request.test_type.as_str() {
        "Cuci SMR" | "MR" | "SKU" if request.user_devices.is_empty() => {
            Err(format!("{} needs at least one non-userdebug device", request.test_type))
        }
        "STS" if request.userdebug_devices.is_empty() => {
            Err("STS needs at least one userdebug device".to_string())
        }
        "SMR" if request.user_devices.is_empty() || request.userdebug_devices.is_empty() => {
            Err("SMR needs non-userdebug and userdebug devices".to_string())
        }
        _ => Ok(()),
    }
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
    let value = value
        .filter(|item| !item.trim().is_empty())
        .unwrap_or(default_auto_root()?);
    let root = PathBuf::from(value);
    if root.is_dir() {
        Ok(root)
    } else {
        Err(format!("AUTO root does not exist: {}", root.display()))
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
        // Work around WebKitGTK crashes on some Mesa/Wayland/DMABUF combinations.
        // Setting it here makes packaged deb/rpm launches behave like `npm run dev`.
        env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
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
            open_result,
            generate_ro_xml
        ])
        .run(tauri::generate_context!())
        .expect("error while running GBA Agentic Runner");
}
