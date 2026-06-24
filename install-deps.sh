#!/usr/bin/env bash
set -Eeuo pipefail

echo "[deps] Installing GBA Agentic Runner compilation dependencies for Ubuntu/Debian..."

sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  libssl-dev \
  libdbus-1-dev \
  libglib2.0-dev \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libsoup-3.0-dev

echo "[deps] Native dependencies installed. You can now run: ./build-linux.sh"
