#!/bin/bash
# ==============================================================================
#   Android Test Suite Automation (CTS, GTS, STS)
#   Version: 13.0 - GUM + Zenity UI Overhaul
# ==============================================================================
set -uo pipefail

# -- Constants --
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly RESULTS_DIR="${SCRIPT_DIR}/Results"
readonly ENABLE_WIFI_CONNECTION=true
readonly WIFI_SSID="RTT / IEEE 802.11"
readonly WIFI_PASSWORD="1234qwer"
readonly REQUIRED_COMMANDS=("adb" "gum" "zenity" "column" "java" "tput")
readonly TEST_TIMEOUT=86400
readonly UNIQUE_SUFFIX=$(date +%Y%m%d_%H%M%S)
readonly C_OK=$'\033[0;32m' C_WARN=$'\033[0;33m' C_ERR=$'\033[0;31m'
readonly C_STEP=$'\033[0;34m' C_CYAN=$'\033[0;36m' C_NC=$'\033[0m'

# -- Logging --
log_msg() {
    local type="$1" msg="$2" color="$C_NC"
    local ts; ts=$(date +'%Y-%m-%d %H:%M:%S')
    case "$type" in INFO) color="$C_OK";; WARN) color="$C_WARN";; ERROR) color="$C_ERR";; STEP) color="$C_STEP";; esac
    [[ -n "${session_log_file:-}" ]] && echo "${ts} [${type}] ${msg}" >> "$session_log_file"
    printf "%s ${color}[%s]${C_NC} %s\n" "$ts" "$type" "$msg"
}

# -- Cleanup --
cleanup() {
    tput cnorm 2>/dev/null
    echo ""
    log_msg "WARN" "Skrip dihentikan. Membersihkan proses..."
    jobs -p | xargs -r kill -9 2>/dev/null || true
    pkill -P $$ 2>/dev/null || true
    exit 1
}
trap cleanup INT TERM

# -- Helpers --
format_duration() {
    local t=${1:-0}
    [[ "$t" =~ ^[0-9]+$ ]] || t=0
    printf "%02d:%02d:%02d" $((t/3600)) $(((t%3600)/60)) $((t%60))
}

gum_banner() {
    gum style --border double --border-foreground "#00BFFF" \
        --foreground "#00FF88" --bold --padding "1 3" --margin "1 0" \
        --align center "$@" >&2
}

gum_info() {
    gum style --foreground "#00BFFF" --bold "  ℹ  $1" >&2
}

gum_success() {
    gum style --foreground "#00FF88" --bold "  ✅ $1" >&2
}

gum_warn() {
    gum style --foreground "#FFD700" --bold "  ⚠  $1" >&2
}

gum_error() {
    gum style --foreground "#FF4444" --bold "  ❌ $1" >&2
}

start_log_monitor() {
    local log_file="$1"
    local title="${2:-Test Progress Monitor}"
    # Menggunakan tail -f untuk monitoring real-time di jendela Zenity
    (
        echo "=== MONITORING LOG: $title STARTED: $(date) ==="
        echo "Log File: $log_file"
        echo "----------------------------------------------------"
        tail -f "$log_file"
    ) | zenity --text-info --title="🚀 $title" \
        --width=900 --height=500 --auto-scroll --font="Monospace 9" \
        --ok-label="Close Monitor" 2>/dev/null &
    echo $!
}

# -- Parse Summary --
parse_and_log_summary() {
    local log_file="$1" suite="$2"
    declare -n smap=$3
    local block
    block=$(sed -n '/=============== Summary ===============/,/============================================/p' "$log_file" 2>/dev/null)
    [[ -z "$block" ]] && { log_msg "WARN" "Ringkasan tidak ditemukan untuk $suite."; return; }
    smap["run_time"]=$(echo "$block" | grep "Total Run time:" | sed 's/.*: //')
    smap["modules"]=$(echo "$block" | grep "modules completed" | sed 's/.*: //')
    smap["total"]=$(echo "$block" | grep "Total Tests" | grep -o '[0-9]\+' || echo "0")
    smap["passed"]=$(echo "$block" | grep "PASSED" | grep -o '[0-9]\+' || echo "0")
    smap["failed"]=$(echo "$block" | grep "FAILED" | grep -o '[0-9]\+' || echo "0")
    smap["assumption"]=$(echo "$block" | grep "ASSUMPTION_FAILURE" | grep -o '[0-9]\+' || echo "0")
    echo ""
    log_msg "INFO" "--- Ringkasan $suite ---"
    log_msg "INFO" "Waktu: ${smap[run_time]} | Modul: ${smap[modules]}"
    log_msg "INFO" "Total: ${smap[total]} | Lulus: ${smap[passed]} | Gagal: ${smap[failed]}"
}

# -- Wi-Fi --
connect_to_wifi() {
    local dev="$1"
    log_msg "INFO" "Koneksi Wi-Fi $dev..."
    adb -s "$dev" shell svc wifi enable
    local r=15
    while [[ $(adb -s "$dev" shell dumpsys wifi 2>/dev/null | grep "Wi-Fi is") != *"enabled"* && $r -gt 0 ]]; do
        sleep 1; ((r--))
    done
    [[ $r -eq 0 ]] && { log_msg "ERROR" "Wi-Fi gagal aktif di $dev."; return 1; }
    adb -s "$dev" shell cmd wifi connect-network "$WIFI_SSID" wpa2 "$WIFI_PASSWORD"
    local cr=30
    while ! adb -s "$dev" shell dumpsys wifi 2>/dev/null | grep -q "SSID: \"${WIFI_SSID}\""; do
        sleep 1; ((cr--))
        [[ $cr -eq 0 ]] && { log_msg "ERROR" "Gagal konek Wi-Fi $dev."; return 1; }
    done
    local ip; ip=$(adb -s "$dev" shell ip -f inet addr show wlan0 2>/dev/null | grep -Po 'inet \K[\d.]+')
    log_msg "INFO" "$dev terhubung ke '${WIFI_SSID}' IP: ${ip:-N/A}"
}

wait_for_device_reconnect() {
    local dev="$1"
    printf "\r$(tput el)\r"
    gum_warn "Perangkat $dev terputus. Menunggu..."
    tput civis
    while ! adb devices 2>/dev/null | grep -qw "$dev"; do sleep 0.3; done
    printf "\r$(tput el)\r"
    gum_success "Perangkat $dev kembali terhubung."
    sleep 5
    tput cnorm
}

safe_copy_result() {
    local src="$1" dst="$2" name="$3" logf="$4"
    shopt -s nullglob; local files=(${src}/*.zip); shopt -u nullglob
    if [[ ${#files[@]} -gt 0 ]]; then
        cp "${files[0]}" "$dst" 2>>"$logf" || log_msg "WARN" "Gagal copy $name"
        gum_success "Hasil $name: $dst"
    else
        gum_error "Tidak ada .zip di ${src} untuk $name"
    fi
}

# -- OPTIMIZED: batch getprop in single adb call --
generate_ro_xml() {
    local dev="$1" dir="$2" xml="${2}/ro_${1}.xml"
    log_msg "INFO" "Membuat ro.xml v4.4 untuk $dev..."
    
    # 1. Ambil Properti Dasar (Batched)
    local props=(
        "ro.build.fingerprint" "ro.build.version.base_os" "ro.build.version.security_patch" "ro.build.PDA"
        "ril.sw_ver" "ril.official_cscver" "ro.product.first_api_level" "ro.sts.property"
        "ro.csc.sales_code" "ro.oem.key1" "ro.oem.key2" "ro.csc.countryiso_code"
        "ro.csc.country_code" "ro.system.build.fingerprint" "ro.vendor.build.fingerprint"
        "ro.product.build.version.sdk" "ro.build.version.sdk_full" "partition.system.verified.root_digest"
        "partition.vendor.verified.root_digest" "partition.system_dlkm.verified.root_digest"
        "partition.vendor_dlkm.verified.root_digest" "partition.odm.verified.root_digest"
        "partition.product.verified.root_digest" "ro.build.characteristics" "ro.build.version.oneui"
        "ro.build.version.emergency_base_os" "partition.system_ext.verified.root_digest"
    )
    
    echo -e "<RO>\n" > "$xml"
    
    # Batch getprop calls
    local cmd=""; for p in "${props[@]}"; do cmd+="echo \"PROP:${p}:\$(getprop $p)\"; "; done
    local output; output=$(adb -s "$dev" shell "$cmd" 2>/dev/null | tr -d '\r')
    
    while IFS= read -r line; do
        if [[ "$line" == PROP:* ]]; then
            local pname pval
            pname=$(echo "$line" | cut -d: -f2)
            pval=$(echo "$line" | cut -d: -f3-)
            # XML Escape & for country_code
            [[ "$pname" == "ro.csc.country_code" ]] && pval=$(echo "$pval" | sed 's/&/&amp;/g')
            echo "    <${pname}>${pval}</${pname}>" >> "$xml"
        fi
    done <<< "$output"

    # 2. Check isWatch
    local is_watch="false"
    if adb -s "$dev" shell pm list features | grep -q "feature:android.hardware.type.watch"; then is_watch="true"; fi
    echo -e "\n    <isWatch>$is_watch</isWatch>" >> "$xml"

    # 3. Check Message App
    local msg_pkg; msg_pkg=$(adb -s "$dev" shell cmd role get-role-holders android.app.role.SMS 2>/dev/null | tr -d '\r')
    local msg_val="Not Found"
    [[ "$msg_pkg" == "com.google.android.apps.messaging" ]] && msg_val="Android Message"
    [[ "$msg_pkg" == "com.samsung.android.messaging" ]] && msg_val="Samsung Message"
    echo "    <message>$msg_val</message>" >> "$xml"

    # 4. Check Browser
    local browser; browser=$(adb -s "$dev" shell "cmd package resolve-activity http://example.com/ | grep packageName" 2>/dev/null)
    local browser_val="Not Found"
    [[ "$browser" =~ "com.android.chrome" ]] && browser_val="Chrome"
    [[ "$browser" =~ "com.sec.android.app.sbrowser" ]] && browser_val="S-Browser"
    echo "    <browser>$browser_val</browser>" >> "$xml"

    # 5. Dynamic Client IDs
    adb -s "$dev" shell "getprop | grep clientidbase" 2>/dev/null | tr -d '[]\r' | while read -r line; do
        local key val
        key=$(echo "$line" | awk -F': ' '{print $1}' | xargs)
        val=$(echo "$line" | awk -F': ' '{print $2}' | xargs)
        [[ -n "$key" ]] && echo "    <${key}>${val}</${key}>" >> "$xml"
    done

    echo -e "\n    <ro.version>4.4</ro.version>\n</RO>" >> "$xml"
    gum_success "ro.xml v4.4 dibuat: $xml"
}

verify_plan_file_exists() {
    local root="$1" plan="$2" suite="$3"
    local path="${root}/subplans/${plan}.xml"
    if [[ -f "$path" ]]; then
        gum_success "Subplan '$plan' ditemukan: $path"
        return 0
    fi
    gum_error "Subplan '$plan' TIDAK ditemukan: $path"
    return 1
}

check_dependencies() {
    log_msg "STEP" "Memeriksa dependensi..."
    local missing=()
    for cmd in "${REQUIRED_COMMANDS[@]}"; do
        command -v "$cmd" &>/dev/null || missing+=("$cmd")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        gum_error "Tidak ditemukan: ${missing[*]}"
        exit 1
    fi
    gum_success "Semua dependensi OK. Java: $(java -version 2>&1 | head -1)"
}

initialize_folders() {
    log_msg "STEP" "Mengecek prasyarat folder..."
    local folders=(
        "${RESULTS_DIR}"
        "${SCRIPT_DIR}/CTS/14"
        "${SCRIPT_DIR}/CTS/15"
        "${SCRIPT_DIR}/CTS/16"
        "${SCRIPT_DIR}/CTS/16.1"
        "${SCRIPT_DIR}/GTS"
        "${SCRIPT_DIR}/STS"
    )
    
    local created=false
    for f in "${folders[@]}"; do
        if [[ ! -d "$f" ]]; then
            mkdir -p "$f"
            created=true
        fi
    done
    
    # Buat subfolder bulan dan versi untuk STS
    local sts_versions=("14" "15" "16" "16.1")
    for m in {01..12}; do
        for v in "${sts_versions[@]}"; do
            if [[ ! -d "${SCRIPT_DIR}/STS/$m/$v" ]]; then
                mkdir -p "${SCRIPT_DIR}/STS/$m/$v"
                created=true
            fi
        done
    done

    if [[ "$created" == true ]]; then
        gum_info "Struktur folder default telah dibuat (kecuali folder android-*)."
    fi
}
# -- GUM: Test Type Selection --
select_test_type() {
    gum_banner "🤖 Android Test Suite Automation" "Google Build Approval Testing" "v13.0"
    local choice
    choice=$(gum choose --height 8 \
        --header "📋 Pilih Tipe Tes:" \
        --cursor.foreground "#00BFFF" \
        --selected.foreground "#00FF88" \
        --header.foreground "#FFD700" \
        "Cuci SMR  ─  CTS SMR → GTS SMR (Non-Userdebug)" \
        "MR        ─  Maintenance Release (CTS → GTS)" \
        "SMR       ─  Security Maintenance (CTS+STS → GTS)" \
        "SKU       ─  SKU Build (CTS → GTS)" \
        "STS       ─  Security Test Suites (Hanya STS)") || return 1
    # Ambil teks sebelum spasi ganda pertama
    local val
    val=$(echo "$choice" | awk -F'  ' '{print $1}' | xargs)
    echo "$val"
}

# -- GUM: Retry Options --
select_retry_option() {
    if gum confirm "Gunakan retry otomatis?" \
        --prompt.foreground "#00BFFF" \
        --selected.background "#00BFFF" \
        --affirmative "Ya, dengan retry" \
        --negative "Tidak"; then
        local count
        count=$(gum input --placeholder "5" \
            --prompt "Max retry count: " \
            --prompt.foreground "#FFD700" \
            --cursor.foreground "#00BFFF" \
            --width 30 \
            --value "5")
        [[ -z "$count" ]] && count=5
        echo "$count"
    else
        echo "0"
    fi
}

# -- OPTIMIZED: Batch device props in single ADB call --
get_filtered_devices() {
    local test_type="$1"; declare -n out_table=$2
    log_msg "STEP" "Mencari perangkat untuk '$test_type'..."
    local devices; mapfile -t devices < <(adb devices | awk 'NR>1 && /device$/ {print $1}')
    [[ ${#devices[@]} -eq 0 ]] && { gum_error "Tidak ada perangkat ADB."; exit 1; }

    local count=1
    for dev in "${devices[@]}"; do
        # Ambil 9 properti + ID perangkat = 10 kolom
        local props
        props=$(adb -s "$dev" shell "getprop ro.build.fingerprint; getprop ro.build.version.security_patch; getprop ro.build.version.release; getprop ro.csc.sales_code; getprop ro.product.model; getprop ro.build.PDA; getprop ril.sw_ver; getprop ril.official_cscver" 2>/dev/null | tr -d '\r')
        mapfile -t pl <<< "$props"
        
        local fp="${pl[0]}" ud=false
        [[ "$fp" == *"userdebug"* ]] && ud=true
        
        local add=false
        case "$test_type" in
            SMR) add=true;;
            STS) [[ "$ud" == true ]] && add=true;;
            MR|SKU|"Cuci SMR") [[ "$ud" == false ]] && add=true;;
        esac

        if [[ "$add" == true ]]; then
            # Format 10 kolom: No, ID, FP, SPL, OS, Sales, Model, AP(PDA), CP(SW), CSC(Official)
            out_table+=("$count" "$dev" "${pl[0]}" "${pl[1]}" "${pl[2]}" "${pl[3]}" "${pl[4]}" "${pl[5]}" "${pl[6]}" "${pl[7]}")
            ((count++))
        fi
    done
}

# -- ZENITY: Device Selection Checklist --
select_devices_from_table() {
    local prompt="$1"; local -n tbl=$2
    declare -n odev=$3 omod=$4 opda=$5
    odev=(); omod=(); opda=()

    local dc=$(( ${#tbl[@]} / 10 ))
    [[ $dc -eq 0 ]] && { gum_error "Tidak ada perangkat untuk: $prompt"; return 1; }

    # Bangun argumen untuk zenity checklist
    local zen_args=()
    for ((i=0; i<dc; i++)); do
        local base=$((i * 10))
        # Column mapping: FALSE(check), ID, Model, OS, SPL, Fingerprint
        zen_args+=("FALSE" "${tbl[$base+1]}" "${tbl[$base+6]}" "${tbl[$base+4]}" "${tbl[$base+3]}" "${tbl[$base+2]}")
    done

    local selected
    selected=$(zenity --list --checklist \
        --title="📱 Pilih Perangkat - $prompt" \
        --text="Silakan centang perangkat yang ingin diproses:" \
        --column="Pilih" --column="Device ID" --column="Model" --column="OS" --column="SPL" --column="Fingerprint" \
        --width=1000 --height=500 --separator="|" \
        "${zen_args[@]}" 2>/dev/null)

    [[ $? -ne 0 || -z "$selected" ]] && { gum_warn "Seleksi dibatalkan."; return 1; }

    # Proses hasil pilihan
    IFS='|' read -ra ids <<< "$selected"
    for sid in "${ids[@]}"; do
        for ((i=0; i<dc; i++)); do
            local base=$((i * 10))
            if [[ "${tbl[$base+1]}" == "$sid" ]]; then
                odev+=("${tbl[$base+1]}")
                omod+=("${tbl[$base+6]}")
                opda+=("${tbl[$base+7]}")
                break
            fi
        done
    done

    # Validasi keseragaman model (kecuali SMR)
    if [[ "$prompt" != "SMR" && ${#omod[@]} -gt 1 ]]; then
        local fm="${omod[0]}"
        for m in "${omod[@]}"; do
            if [[ "$m" != "$fm" ]]; then
                gum_error "Semua perangkat harus model yang sama ($fm), tapi ditemukan $m."
                return 1
            fi
        done
    fi

    return 0
}

# -- GUM: Device Info Display --
display_device_info() {
    declare -n dref=$1
    gum style --foreground "#00BFFF" --bold --border rounded \
        --border-foreground "#00FF88" --padding "0 2" \
        "📊 Detail Perangkat Terpilih"
    local csv="Device ID,IP Address,OS,SPL,Fingerprint"
    for dev in "${dref[@]}"; do
        local ip os spl fp
        ip=$(adb -s "$dev" shell ip -f inet addr show wlan0 2>/dev/null | grep -Po 'inet \K[\d.]+' || echo "N/A")
        os=$(adb -s "$dev" shell getprop ro.build.version.release 2>/dev/null | tr -d '\r')
        spl=$(adb -s "$dev" shell getprop ro.build.version.security_patch 2>/dev/null | tr -d '\r')
        fp=$(adb -s "$dev" shell getprop ro.build.fingerprint 2>/dev/null | tr -d '\r')
        csv+="\n${dev},${ip},${os},${spl},${fp}"
    done
    echo -e "$csv" | gum table --print \
        --border.foreground "#00FF88" \
        --header.foreground "#FFD700"
}

# -- OPTIMIZED: Parallel device preparation --
prepare_device() {
    local dev="$1"
    adb devices 2>/dev/null | grep -qw "$dev" || wait_for_device_reconnect "$dev"
    adb -s "$dev" root &>/dev/null || true; sleep 1
    adb -s "$dev" unroot &>/dev/null || true; sleep 1
    adb -s "$dev" wait-for-device; sleep 2
    adb -s "$dev" shell "settings put global stay_on_while_plugged_in 3; wm dismiss-keyguard" 2>/dev/null
    gum_success "Perangkat $dev siap."
}

prepare_all_devices() {
    local -n devs=$1
    log_msg "STEP" "Mempersiapkan ${#devs[@]} perangkat (paralel)..."
    local pids=()
    for dev in "${devs[@]}"; do
        prepare_device "$dev" &
        pids+=($!)
    done
    for pid in "${pids[@]}"; do wait "$pid"; done
    gum_success "Semua perangkat siap."
}

start_tradefed_via_pipe() {
    local cmd="$1" exe="$2" logf="$3"
    (echo "$cmd"; sleep "$TEST_TIMEOUT") | "$exe" &> "$logf" &
    echo $!
}

start_tradefed_via_args() {
    local cmd="$1" exe="$2" logf="$3"
    "$exe" $cmd &> "$logf" &
    echo $!
}
# -- Monitor Loop --
wait_and_process_tests() {
    declare -n pids_ref=$1 names_ref=$2 devices_ref=$3 logs_ref=$4 starts_ref=$5
    declare -n pending_gts_ref=$6 session_dir_ref=$7
    local zip_base="$8" logf="$9"
    declare -n dur_ref=${10} sum_ref=${11}

    tput civis
    local spin=("⠋" "⠙" "⠹" "⠸" "⠼" "⠴" "⠦" "⠧" "⠇" "⠏")
    local si=0 t0=$(date +%s)
    local log_monitor_pid=${LOG_MONITOR_PID_GLOBAL:-}
    declare -A suite_monitor_pids

    # Beri tahu user tentang shortcut
    gum style --foreground "#FFD700" --italic "  💡 Tip: Tekan 'L' di terminal untuk membuka kembali Log Monitor GUI" >&2

    while [[ ${#pids_ref[@]} -gt 0 ]]; do
        local now=$(date +%s) active=() done_idx=()
        local new_p=() new_n=() new_d=() new_l=() new_s=()

        for i in "${!pids_ref[@]}"; do
            local pid="${pids_ref[$i]}" status=""
            # Check device connectivity
            IFS=',' read -ra darr <<< "${devices_ref[$i]}"
            for dc in "${darr[@]}"; do
                adb devices 2>>"$logf" | grep -qw "$dc" || { wait_for_device_reconnect "$dc"; tput civis; }
            done

            if tail -n 50 "${logs_ref[$i]}" 2>/dev/null | grep -q "Result/Log Location"; then
                status="Selesai"
            elif ! ps -p "$pid" > /dev/null 2>&1; then
                status="Proses Selesai"
            elif [[ $((now - starts_ref[$i])) -gt $TEST_TIMEOUT ]]; then
                status="Timeout"
            fi

            if [[ -n "$status" ]]; then
                printf "\r$(tput el)\r"
                local dur=$((now - starts_ref[$i]))
                local key="${names_ref[$i]},${devices_ref[$i]}"
                dur_ref["$key"]=$dur
                declare -A sd; parse_and_log_summary "${logs_ref[$i]}" "${names_ref[$i]}" sd
                sum_ref["$key"]="run_time:${sd[run_time]:-N/A}|modules:${sd[modules]:-N/A}|total:${sd[total]:-0}|passed:${sd[passed]:-0}|failed:${sd[failed]:-0}"
                gum_success "${names_ref[$i]} [${devices_ref[$i]}] $status ($(format_duration $dur))"
                done_idx+=("$i")
            else
                active+=("$i")
            fi
        done

        # Handle completed: copy results + start pending GTS
        for i in "${done_idx[@]}"; do
            local pid="${pids_ref[$i]}" dl="${devices_ref[$i]}" nm="${names_ref[$i]}"
            local fd=${dl%%,*}
            if ! adb devices 2>>"$logf" | grep -qw "$fd"; then
                gum_warn "[$fd] terputus, skip hasil."
                [[ -v pending_gts_ref[$pid] ]] && unset "pending_gts_ref[$pid]"
                continue
            fi
            local ver sec sf root_d
            ver=$(adb -s "$fd" shell getprop ro.build.version.release 2>/dev/null | tr -d '\r')
            sec=$(adb -s "$fd" shell getprop ro.build.version.security_patch 2>/dev/null | tr -d '\r')
            sf=$(date -d "$sec" +%m 2>/dev/null || date +%m)
            case "$nm" in
                CTS) root_d="${SCRIPT_DIR}/CTS/${ver:0:2}/android-cts";;
                GTS) root_d="${SCRIPT_DIR}/GTS/android-gts";;
                STS) root_d="${SCRIPT_DIR}/STS/${sf}/${ver:0:2}/android-sts";;
            esac
            local tmpd="${root_d}/results_${nm,,}_${fd}_${UNIQUE_SUFFIX}"
            [[ -d "${root_d}/results" ]] && mv "${root_d}/results" "$tmpd" 2>>"$logf"
            local sdf="${dl//,/_}"
            safe_copy_result "$tmpd" "${session_dir_ref}/${nm}_${zip_base}_${sdf}.zip" "$nm" "$logf"

            if [[ -v pending_gts_ref[$pid] ]]; then
                local gi=${pending_gts_ref[$pid]}; unset "pending_gts_ref[$pid]"
                local gdl=${gi%%|*} gcmd=${gi#*|}
                gum_info "CTS selesai, memulai GTS pada [$gdl]..."
                local gr="${SCRIPT_DIR}/GTS/android-gts"
                [[ ! -d "$gr" ]] && { gum_error "Dir '$gr' tidak ada!"; continue; }
                IFS=',' read -ra ga <<< "$gdl"
                local gsc=${#ga[@]} gsa=""
                for d in "${ga[@]}"; do gsa+=" --serial $d"; done
                local gl="${session_dir_ref}/Log/gts_run_${gsc}devs.log"
                local gc="$gcmd --shard-count $gsc${gsa} ${RETRY_ARGS}"
                log_msg "INFO" "GTS cmd: $gc"
                local gp=$(start_tradefed_via_pipe "$gc" "${gr}/tools/gts-tradefed" "$gl")
                new_p+=("$gp"); new_n+=("GTS"); new_d+=("$gdl"); new_l+=("$gl"); new_s+=("$(date +%s)")
            fi
        done

        # Rebuild arrays
        local np=() nn=() nd=() nl=() ns=()
        for i in "${active[@]}"; do
            np+=("${pids_ref[$i]}"); nn+=("${names_ref[$i]}"); nd+=("${devices_ref[$i]}")
            nl+=("${logs_ref[$i]}"); ns+=("${starts_ref[$i]}")
        done
        pids_ref=("${np[@]}" "${new_p[@]}")
        names_ref=("${nn[@]}" "${new_n[@]}")
        devices_ref=("${nd[@]}" "${new_d[@]}")
        logs_ref=("${nl[@]}" "${new_l[@]}")
        starts_ref=("${ns[@]}" "${new_s[@]}")

        if [[ ${#pids_ref[@]} -gt 0 ]]; then
            local el=$((now - t0))
            si=$(((si + 1) % ${#spin[@]}))
            local ts; ts=$(date +'%H:%M:%S')
            
            # Bangun string status shortcut secara dinamis
            local shortcuts="[L=Sesi]"
            for idx in "${!names_ref[@]}"; do
                shortcuts+=" [$((idx+1)):${names_ref[$idx]}]"
            done
            
            printf "\r${C_CYAN}%s${C_NC} ${C_STEP}%s${C_NC} ${C_WARN}Monitoring ${#pids_ref[@]} sesi${C_NC} ⏱ $(format_duration $el) ${C_NC}${shortcuts} " "$ts" "${spin[$si]}"
        fi
        
        # Shortcut detection (non-blocking read)
        local key
        if read -t 0.3 -n 1 key; then
            case "$key" in
                [lL])
                    if [[ -z "$log_monitor_pid" ]] || ! ps -p "$log_monitor_pid" > /dev/null 2>&1; then
                        log_msg "INFO" "Membuka Session Log..."
                        log_monitor_pid=$(start_log_monitor "$logf" "Session Log")
                    fi
                    ;;
                [1-9])
                    local idx=$((key - 1))
                    if [[ $idx -lt ${#names_ref[@]} ]]; then
                        local sn="${names_ref[$idx]}"
                        local sl="${logs_ref[$idx]}"
                        # Gunakan pengaman :- untuk menghindari unbound variable error
                        local current_pid=${suite_monitor_pids[$sn]:-}
                        if [[ -z "$current_pid" ]] || ! ps -p "$current_pid" > /dev/null 2>&1; then
                            log_msg "INFO" "Membuka Log Detail: $sn..."
                            suite_monitor_pids["$sn"]=$(start_log_monitor "$sl" "Log Detail: $sn")
                        else
                            log_msg "WARN" "Log Monitor $sn sudah terbuka."
                        fi
                    else
                        log_msg "WARN" "Sesi ke-$key belum siap atau tidak ada."
                    fi
                    ;;
            esac
        fi
    done
    
    # Cleanup all monitors at the end
    for sn in "${!suite_monitor_pids[@]}"; do kill "${suite_monitor_pids[$sn]}" 2>/dev/null || true; done
    echo ""; tput cnorm
}

# -- Summary Display --
display_summary() {
    declare -n dr=$1
    local tcts=0 tgts=0 tsts=0
    for key in "${!dr[@]}"; do
        local sn=${key%%,*} d=${dr[$key]}
        case "$sn" in CTS) ((tcts+=d)) || true;; GTS) ((tgts+=d)) || true;; STS) ((tsts+=d)) || true;; esac
    done
    echo ""
    gum style --border double --border-foreground "#00FF88" \
        --foreground "#FFFFFF" --bold --padding "1 3" --align center \
        "📊 RINGKASAN WAKTU PENGETESAN"
    local summary=""
    [[ $tcts -gt 0 ]] && summary+="  CTS: $(format_duration $tcts)\n"
    [[ $tgts -gt 0 ]] && summary+="  GTS: $(format_duration $tgts)\n"
    [[ $tsts -gt 0 ]] && summary+="  STS: $(format_duration $tsts)\n"
    local gt=$((tcts + tgts + tsts))
    summary+="  ─────────────────────\n"
    summary+="  TOTAL: $(format_duration $gt)"
    echo -e "$summary" | gum style --foreground "#00BFFF" --padding "0 2"
}

# ==============================================================================
#   MAIN
# ==============================================================================
main() {
    check_dependencies
    initialize_folders
    local test_type
    test_type=$(select_test_type) || { gum_warn "Dibatalkan."; exit 1; }
    gum_info "Tipe tes: $test_type"

    local retry_count
    retry_count=$(select_retry_option)
    RETRY_ARGS=""
    if [[ "$retry_count" -gt 0 ]]; then
        RETRY_ARGS="--enable-token-sharding --max-testcase-run-count ${retry_count} --retry-strategy RETRY_ANY_FAILURE"
        gum_success "Retry: ${retry_count}x"
    else
        gum_info "Retry tidak digunakan."
    fi

    local user_devices=() user_models=() user_pdas=()
    local userdebug_devices=() userdebug_models=() userdebug_pdas=()

    if [[ "$test_type" == "SMR" ]]; then
        local all_dt=(); get_filtered_devices "$test_type" all_dt
        local ut=() udt=()
        for ((i=0; i<${#all_dt[@]}; i+=9)); do
            if [[ "${all_dt[$i+1]}" == *"userdebug"* ]]; then
                udt+=("${all_dt[@]:i:9}")
            else
                ut+=("${all_dt[@]:i:9}")
            fi
        done
        select_devices_from_table "NON-USERDEBUG (CTS/GTS)" ut user_devices user_models user_pdas || exit 1
        select_devices_from_table "USERDEBUG (STS)" udt userdebug_devices userdebug_models userdebug_pdas || exit 1
    else
        local dt=(); get_filtered_devices "$test_type" dt
        if [[ "$test_type" == "STS" ]]; then
            select_devices_from_table "USERDEBUG" dt userdebug_devices userdebug_models userdebug_pdas || exit 1
        else
            select_devices_from_table "NON-USERDEBUG" dt user_devices user_models user_pdas || exit 1
        fi
    fi

    local all_sel=(${user_devices[@]+"${user_devices[@]}"} ${userdebug_devices[@]+"${userdebug_devices[@]}"})
    [[ ${#all_sel[@]} -eq 0 ]] && { gum_error "Tidak ada perangkat dipilih."; exit 1; }
    gum_info "Total perangkat: ${#all_sel[@]} (${all_sel[*]})"

    if [[ "$ENABLE_WIFI_CONNECTION" == true ]]; then
        log_msg "STEP" "Koneksi Wi-Fi paralel..."
        local wpids=()
        for dev in "${all_sel[@]}"; do
            connect_to_wifi "$dev" &
            wpids+=($!)
        done
        for wp in "${wpids[@]}"; do wait "$wp" 2>/dev/null || gum_warn "Wi-Fi gagal pada salah satu device."; done
    fi

    display_device_info all_sel

    local fm="${user_models[0]:-${userdebug_models[0]}}"
    local fp="${user_pdas[0]:-${userdebug_pdas[0]}}"
    local sname="${test_type}_${fm}_${fp}_${#all_sel[@]}devs_${UNIQUE_SUFFIX}"
    local sdir="${RESULTS_DIR}/${sname}" slogdir="${RESULTS_DIR}/${sname}/Log"
    mkdir -p "$sdir" "$slogdir"
    export session_log_file="${sdir}/session_run.log"
    echo "--- Log Sesi v13.0 ---" > "$session_log_file"
    gum_info "Hasil: $sdir"

    [[ ${#user_devices[@]} -gt 0 ]] && generate_ro_xml "${user_devices[0]}" "$sdir"
    prepare_all_devices all_sel

    log_msg "STEP" "Memulai alur: $test_type"
    local pids=() names=() devices=() log_files=() start_times=()
    declare -A pending_gts_jobs all_durations all_summaries

    local csub="" gcmd="" rcts=false rgts=false rsts=false
    case "$test_type" in
        "Cuci SMR") rcts=true; rgts=true; csub="ctssmr"; gcmd="run gts --subplan gtssmr";;
        MR) rcts=true; rgts=true; csub="normal"; gcmd="run gts --subplan normal";;
        SMR) rcts=true; rgts=true; rsts=true; csub="ctssmr"; gcmd="run gts --subplan gtssmr";;
        SKU) rcts=true; rgts=true; csub="ctssku"; gcmd="run gts-variant";;
        STS) rsts=true;;
    esac

    if [[ "$rcts" == true ]]; then
        local fd=${user_devices[0]}
        local ver=$(adb -s "$fd" shell getprop ro.build.version.release 2>/dev/null | tr -d '\r')
        local cr="${SCRIPT_DIR}/CTS/${ver:0:2}/android-cts"
        [[ ! -d "$cr" ]] && { gum_error "CTS dir '$cr' tidak ada!"; exit 1; }
        verify_plan_file_exists "$cr" "$csub" "CTS" || exit 1
        local sc=${#user_devices[@]} sa=""
        for d in "${user_devices[@]}"; do sa+=" --serial $d"; done
        local cl="${slogdir}/cts_${user_models[0]}_${sc}devs.log"
        local cc="run cts --subplan $csub --shard-count $sc${sa} ${RETRY_ARGS}"
        log_msg "INFO" "CTS: $cc"
        local cp=$(start_tradefed_via_pipe "$cc" "${cr}/tools/cts-tradefed" "$cl")
        pids+=("$cp"); names+=("CTS"); devices+=("$(IFS=,; echo "${user_devices[*]}")"); log_files+=("$cl"); start_times+=("$(date +%s)")
        [[ "$rgts" == true ]] && pending_gts_jobs[$cp]="$(IFS=,; echo "${user_devices[*]}")|${gcmd}"
        gum_success "CTS dimulai (PID: $cp)"
    fi

    if [[ "$rsts" == true ]]; then
        local fd=${userdebug_devices[0]}
        local ver=$(adb -s "$fd" shell getprop ro.build.version.release 2>/dev/null | tr -d '\r')
        local sec=$(adb -s "$fd" shell getprop ro.build.version.security_patch 2>/dev/null | tr -d '\r')
        local sf=$(date -d "$sec" +%m 2>/dev/null || date +%m)
        local sr="${SCRIPT_DIR}/STS/${sf}/${ver:0:2}/android-sts"
        [[ ! -d "$sr" ]] && { gum_error "STS dir '$sr' tidak ada!"; exit 1; }
        local sc=${#userdebug_devices[@]} sa=""
        for d in "${userdebug_devices[@]}"; do sa+=" --serial $d"; done
        local sl="${slogdir}/sts_${userdebug_models[0]}_${sc}devs.log"
        # Menambahkan argumen khusus STS Kernel LTS sesuai permintaan
        local scmd="run sts-dynamic-incremental --test-arg com.android.compatibility.common.tradefed.testtype.JarHostTest:set-option:android.security.sts.KernelLtsTest:acknowledge_kernel_update_requirement_warning_failure:true --shard-count $sc${sa} ${RETRY_ARGS}"
        log_msg "INFO" "STS: $scmd"
        local sp=$(start_tradefed_via_args "$scmd" "${sr}/tools/sts-tradefed" "$sl")
        pids+=("$sp"); names+=("STS"); devices+=("$(IFS=,; echo "${userdebug_devices[*]}")"); log_files+=("$sl"); start_times+=("$(date +%s)")
        gum_success "STS dimulai (PID: $sp)"
    fi

    if [[ ${#pids[@]} -gt 0 ]]; then
        gum_info "Monitoring ${#pids[@]} sesi tes..."
        
        # Mulai Zenity Log Monitor di background
        local log_monitor_pid
        log_monitor_pid=$(start_log_monitor "$session_log_file")
        export LOG_MONITOR_PID_GLOBAL="$log_monitor_pid"
        
        wait_and_process_tests pids names devices log_files start_times pending_gts_jobs sdir "${fm}_${fp}" "$session_log_file" all_durations all_summaries
        
        # Tutup log monitor jika masih terbuka saat tes selesai
        kill "$LOG_MONITOR_PID_GLOBAL" 2>/dev/null || true
    else
        gum_warn "Tidak ada tes dijadwalkan."
    fi

    display_summary all_durations

    # -- Zenity Completion Notification --
    zenity --info --title "✅ Tes Selesai!" \
        --text "Semua tes telah selesai.\n\nHasil disimpan di:\n<b>${sdir}</b>" \
        --width 500 --icon-name "dialog-information" 2>/dev/null &
    local zpid=$!

    gum_banner "🎉 SEMUA TES SELESAI!" "Hasil: ${sdir}"

    wait "$zpid" 2>/dev/null || true

    if command -v xdg-open &>/dev/null; then
        xdg-open "$sdir" 2>/dev/null
    fi
}

main
