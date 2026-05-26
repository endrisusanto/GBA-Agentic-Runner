#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

echo "[build] GBA Agentic Runner Linux build"

INSTALL_AFTER_BUILD="${INSTALL_AFTER_BUILD:-1}"
if [[ "${1:-}" == "--no-install" ]]; then
  INSTALL_AFTER_BUILD=0
elif [[ "${1:-}" == "--install" ]]; then
  INSTALL_AFTER_BUILD=1
fi

for cmd in npm cargo; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "[build] Missing required command: $cmd" >&2
    exit 1
  fi
done

if [[ -f package-lock.json ]]; then
  echo "[build] Installing Node dependencies with npm ci"
  npm ci
else
  echo "[build] Installing Node dependencies with npm install"
  npm install
fi

echo "[build] Checking Rust backend"
cargo check --manifest-path src-tauri/Cargo.toml

echo "[build] Cleaning old Linux bundle artifacts"
rm -rf src-tauri/target/release/bundle/deb src-tauri/target/release/bundle/rpm

echo "[build] Building deb/rpm bundles"
npm run build:linux

echo "[build] Artifacts:"
find src-tauri/target/release/bundle -type f \( -name '*.deb' -o -name '*.rpm' \) -print | sort

detect_linux_package_type() {
  local id_like=" ${ID_LIKE:-} "
  local id="${ID:-}"

  if [[ -f /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    id_like=" ${ID_LIKE:-} "
    id="${ID:-}"
  fi

  case "$id" in
    debian|ubuntu|linuxmint|pop|elementary|zorin)
      echo "deb"
      return
      ;;
    fedora|rhel|centos|rocky|almalinux|opensuse*|sles|mageia)
      echo "rpm"
      return
      ;;
  esac

  if [[ "$id_like" == *" debian "* ]]; then
    echo "deb"
  elif [[ "$id_like" == *" rhel "* || "$id_like" == *" fedora "* || "$id_like" == *" suse "* ]]; then
    echo "rpm"
  else
    echo "unknown"
  fi
}

install_deb() {
  local package_path="$1"
  if command -v apt >/dev/null 2>&1; then
    sudo apt install -y --reinstall "$package_path"
  elif command -v apt-get >/dev/null 2>&1; then
    sudo apt-get install -y --reinstall "$package_path"
  elif command -v dpkg >/dev/null 2>&1; then
    sudo dpkg -i "$package_path" || sudo apt-get install -f -y
  else
    echo "[build] Cannot install .deb: apt/dpkg not found" >&2
    return 1
  fi
}

install_rpm() {
  local package_path="$1"
  if command -v rpm >/dev/null 2>&1 && rpm -q gba-agentic-runner >/dev/null 2>&1; then
    sudo rpm -Uvh --replacepkgs "$package_path"
  elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y "$package_path"
  elif command -v zypper >/dev/null 2>&1; then
    sudo zypper --non-interactive install --allow-unsigned-rpm "$package_path"
  elif command -v yum >/dev/null 2>&1; then
    sudo yum install -y "$package_path"
  elif command -v rpm >/dev/null 2>&1; then
    sudo rpm -Uvh --replacepkgs "$package_path"
  else
    echo "[build] Cannot install .rpm: dnf/yum/zypper/rpm not found" >&2
    return 1
  fi
}

if [[ "$INSTALL_AFTER_BUILD" == "1" ]]; then
  package_type="$(detect_linux_package_type)"
  case "$package_type" in
    deb)
      package_path="$(find src-tauri/target/release/bundle/deb -type f -name '*.deb' -print | sort -V | tail -n 1)"
      if [[ -z "$package_path" ]]; then
        echo "[build] No .deb artifact found" >&2
        exit 1
      fi
      echo "[build] Installing Debian package: $package_path"
      install_deb "$package_path"
      ;;
    rpm)
      package_path="$(find src-tauri/target/release/bundle/rpm -type f -name '*.rpm' -print | sort -V | tail -n 1)"
      if [[ -z "$package_path" ]]; then
        echo "[build] No .rpm artifact found" >&2
        exit 1
      fi
      echo "[build] Installing RPM package: $package_path"
      install_rpm "$package_path"
      ;;
    *)
      echo "[build] Unknown Linux package family; skipping install. Use --no-install or install artifact manually." >&2
      ;;
  esac
else
  echo "[build] Install skipped."
fi
