#!/bin/bash
set -e

# Container starts as root so we can launch seatd, then drops to dev user.

# Ensure XDG runtime dir exists with correct perms (Wayland requires 0700)
mkdir -p /run/user/1000
chmod 0700 /run/user/1000
chown -R dev:dev /run/user/1000

# Open up DRI devices for the dev user (container is privileged, host perms vary)
chmod 666 /dev/dri/card* /dev/dri/renderD* 2>/dev/null || true

# Start seatd as root, allowing dev user via the video group
seatd -g video &
SEATD_PID=$!

# Wait for seatd socket
for i in $(seq 1 20); do
    [ -S /run/seatd.sock ] && break
    sleep 0.2
done
chmod 666 /run/seatd.sock 2>/dev/null || true

# Now run the rest as dev user
exec su dev -c '
set -e
export XDG_RUNTIME_DIR=/run/user/1000
export WAYLAND_DISPLAY=wayland-1
export QT_QPA_PLATFORM=wayland
export HOME=/home/dev
export PATH=/usr/local/bin/stubs:/usr/local/bin:/usr/bin:/bin

# Symlink game-shell as quickshell config
ln -sfn /opt/game-shell "$HOME/.config/quickshell/game-shell"

# Copy default targets.json if the mounted volume does not have one
if [ ! -f /opt/game-shell/targets.json ]; then
    cp /opt/game-shell-defaults/targets.json /opt/game-shell/targets.json
fi

# Start dbus session
eval "$(dbus-launch --sh-syntax)"

# Start Hyprland (libseat will connect to /run/seatd.sock)
Hyprland -c /etc/hyprland-dev.conf &
HYPRLAND_PID=$!

# Wait for Hyprland socket
for i in $(seq 1 30); do
    if find "$XDG_RUNTIME_DIR/hypr/" -name ".socket.sock" -print -quit 2>/dev/null | grep -q .; then
        break
    fi
    sleep 0.5
done

export HYPRLAND_INSTANCE_SIGNATURE=$(ls "$XDG_RUNTIME_DIR/hypr/" 2>/dev/null | tail -1)

# Start wayvnc — bind to whichever Hyprland output is active
wayvnc 0.0.0.0 5900 &

# Start noVNC websockify bridge
websockify --web=/opt/novnc 0.0.0.0:6080 localhost:5900 &

echo "Game Shell dev environment ready"
echo "  VNC:   vnc://localhost:5900"
echo "  noVNC: http://localhost:6080/vnc.html"
echo ""
echo "Use restart.sh to reload after QML changes"

wait $HYPRLAND_PID
'
