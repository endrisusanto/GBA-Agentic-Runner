#!/usr/bin/env bash
# Check if port 3030 is already open
if ss -tuln | grep -q :3030; then
  echo "GBA Agentic Runner is already running on port 3030."
  exit 0
fi

echo "GBA Agentic Runner is offline. Starting it..."

# Kill any stale gba-agentic-runner processes to prevent multiple windows opening
killall -q gba-agentic-runner
sleep 0.5

# Set required display environments
export DISPLAY="${DISPLAY:-:0}"
export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-0}"
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1

# Start the runner via installed binary
echo "Starting via installed binary..."
nohup /usr/bin/gba-agentic-runner >/tmp/gba-agentic-runner-n8n.log 2>&1 & disown

# Wait up to 15 seconds for port 3030 to open (vite + cargo build might take a few seconds)
for i in {1..30}; do
  if ss -tuln | grep -q :3030; then
    echo "GBA Agentic Runner started successfully on port 3030."
    exit 0
  fi
  sleep 0.5
done

echo "Error: GBA Agentic Runner failed to start or bind to port 3030 within timeout."
exit 1
