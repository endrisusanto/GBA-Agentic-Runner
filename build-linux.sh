#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

echo "[build] GBA Agentic Runner Linux build"

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

echo "[build] Building deb/rpm bundles"
npm run build:linux

echo "[build] Artifacts:"
find src-tauri/target/release/bundle -type f \( -name '*.deb' -o -name '*.rpm' \) -print
