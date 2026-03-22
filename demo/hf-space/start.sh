#!/bin/bash
set -e

PORT="${PORT:-7860}"
DISPLAY_NUM=99
export DISPLAY=":${DISPLAY_NUM}"

SCREEN_W=1280
SCREEN_H=800

echo "══════════════════════════════════════"
echo "  W3C OS — Live Demo"
echo "  Native TS/TSX app, no browser engine"
echo "══════════════════════════════════════"

# Virtual framebuffer
Xvfb "${DISPLAY}" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -ac +extension GLX +render -noreset &
sleep 1

# Lightweight window manager (handles window positioning)
openbox --config-file /dev/null &
sleep 0.5

# Launch w3cos showcase
WINIT_UNIX_BACKEND=x11 /usr/bin/w3cos-showcase &
W3COS_PID=$!
sleep 2

# Maximize the window
xdotool search --name "W3C OS" windowsize "${SCREEN_W}" "${SCREEN_H}" windowmove 0 0 2>/dev/null || true

# VNC server (no password for public demo)
x11vnc -display "${DISPLAY}" -forever -shared -nopw -rfbport 5900 -xkb -noxrecord -noxfixes -noxdamage -q &
sleep 1

# noVNC web client
NOVNC_PATH="/usr/share/novnc"
websockify --web="${NOVNC_PATH}" "${PORT}" localhost:5900 &

echo ""
echo "Ready → http://localhost:${PORT}/vnc.html?autoconnect=true&resize=remote"
echo ""

# Stay alive; restart w3cos if it crashes
while true; do
    if ! kill -0 $W3COS_PID 2>/dev/null; then
        echo "Restarting w3cos-showcase..."
        WINIT_UNIX_BACKEND=x11 /usr/bin/w3cos-showcase &
        W3COS_PID=$!
        sleep 2
        xdotool search --name "W3C OS" windowsize "${SCREEN_W}" "${SCREEN_H}" windowmove 0 0 2>/dev/null || true
    fi
    sleep 5
done
