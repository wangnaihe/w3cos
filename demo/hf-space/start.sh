#!/bin/bash
set -e

PORT="${PORT:-7860}"
DISPLAY_NUM=99
export DISPLAY=":${DISPLAY_NUM}"

SCREEN_W=1280
SCREEN_H=800

echo "══════════════════════════════════════"
echo "  W3C OS — Live Demo"
echo "  Native desktop shell, no browser"
echo "══════════════════════════════════════"

Xvfb "${DISPLAY}" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -ac +extension GLX +render -noreset &
sleep 1

openbox --config-file /dev/null &
sleep 0.5

WINIT_UNIX_BACKEND=x11 /usr/bin/w3cos-shell &
W3COS_PID=$!
sleep 2

xdotool search --name "W3C OS" windowsize "${SCREEN_W}" "${SCREEN_H}" windowmove 0 0 2>/dev/null || true

x11vnc -display "${DISPLAY}" -forever -shared -nopw -rfbport 5900 -xkb -noxrecord -noxfixes -noxdamage -q &
sleep 1

NOVNC_PATH="/usr/share/novnc"
websockify --web="${NOVNC_PATH}" "${PORT}" localhost:5900 &

echo ""
echo "Ready: http://localhost:${PORT}/vnc.html?autoconnect=true&resize=remote"
echo ""

while true; do
    if ! kill -0 $W3COS_PID 2>/dev/null; then
        echo "Restarting w3cos-shell..."
        WINIT_UNIX_BACKEND=x11 /usr/bin/w3cos-shell &
        W3COS_PID=$!
        sleep 2
        xdotool search --name "W3C OS" windowsize "${SCREEN_W}" "${SCREEN_H}" windowmove 0 0 2>/dev/null || true
    fi
    sleep 5
done
