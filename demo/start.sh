#!/bin/bash
set -e

PORT="${PORT:-7860}"
DISPLAY_NUM=99
export DISPLAY=":${DISPLAY_NUM}"

SCREEN_WIDTH=1280
SCREEN_HEIGHT=800
SCREEN_DEPTH=24

echo "╔═══════════════════════════════════════╗"
echo "║     W3C OS — Live Demo                ║"
echo "║     Browser preview on port ${PORT}       ║"
echo "╚═══════════════════════════════════════╝"

# 1. Start virtual framebuffer
echo "[1/4] Starting Xvfb (${SCREEN_WIDTH}x${SCREEN_HEIGHT})..."
Xvfb "${DISPLAY}" -screen 0 "${SCREEN_WIDTH}x${SCREEN_HEIGHT}x${SCREEN_DEPTH}" -ac +extension GLX +render -noreset &
sleep 1

# 2. Launch the w3cos showcase app
echo "[2/4] Launching W3C OS showcase..."
WINIT_UNIX_BACKEND=x11 /usr/bin/w3cos-showcase &
sleep 2

# Resize window to fill the virtual screen
xdotool search --name "W3C OS" windowsize "${SCREEN_WIDTH}" "${SCREEN_HEIGHT}" windowmove 0 0 2>/dev/null || true

# 3. Start VNC server (no password for public demo)
echo "[3/4] Starting VNC server..."
x11vnc -display "${DISPLAY}" -forever -shared -nopw -rfbport 5900 -xkb -noxrecord -noxfixes -noxdamage &
sleep 1

# 4. Start noVNC websocket proxy
echo "[4/4] Starting noVNC on port ${PORT}..."
NOVNC_PATH=$(find /usr -path "*/novnc" -type d 2>/dev/null | head -1)
NOVNC_PATH="${NOVNC_PATH:-/usr/share/novnc}"

websockify --web="${NOVNC_PATH}" "${PORT}" localhost:5900 &

echo ""
echo "Ready! Open in browser: http://localhost:${PORT}/vnc.html?autoconnect=true&resize=remote"
echo ""

# Keep container alive
wait -n
