#!/usr/bin/env bash
set -Eeuo pipefail

APP_USER=${1:-installer}
PROJECT_DIR=${2:-$(cd "$(dirname "$0")/.." && pwd)}
[[ $EUID -eq 0 ]] || { echo "Run with sudo: sudo $0 [user] [project-dir]" >&2; exit 1; }
[[ -x "$PROJECT_DIR/target/release/rustipc-installer" ]] || { echo "Build target/release/rustipc-installer first." >&2; exit 1; }
[[ -s "$PROJECT_DIR/Image/yocto_docker.img" ]] || { echo "Image/yocto_docker.img is missing." >&2; exit 1; }
id "$APP_USER" >/dev/null 2>&1 || { echo "Create '$APP_USER' in the Ubuntu installer first, with a password for local maintenance." >&2; exit 1; }
usermod -aG video,render,input "$APP_USER" 2>/dev/null || usermod -aG video,input "$APP_USER"

apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends xserver-xorg-core xinit x11-xserver-utils openbox xauth sudo util-linux coreutils libgl1-mesa-dri libglx-mesa0
apt-get clean
rm -rf /var/lib/apt/lists/*
install -d -o root -g root -m 0755 /opt/rustipc-installer/Image
install -o root -g root -m 0755 "$PROJECT_DIR/target/release/rustipc-installer" /opt/rustipc-installer/rustipc-installer
install -o root -g root -m 0644 "$PROJECT_DIR/Image/yocto_docker.img" /opt/rustipc-installer/Image/yocto_docker.img
install -o root -g root -m 0755 "$PROJECT_DIR/scripts/rustipc-flash" /usr/local/sbin/rustipc-flash
printf '%s ALL=(root) NOPASSWD: /usr/local/sbin/rustipc-flash *\n' "$APP_USER" > /etc/sudoers.d/rustipc-installer
chmod 0440 /etc/sudoers.d/rustipc-installer
visudo -cf /etc/sudoers.d/rustipc-installer

install -d -o "$APP_USER" -g "$APP_USER" "/home/$APP_USER/.config/openbox"
cat > "/home/$APP_USER/.xinitrc" <<'EOF'
#!/bin/sh
xset s off
xset -dpms
xset s noblank
exec openbox-session
EOF
cat > "/home/$APP_USER/.config/openbox/autostart" <<'EOF'
xset s off
xset -dpms
xset s noblank
(while :; do /opt/rustipc-installer/rustipc-installer; sleep 2; done) &
EOF
chown "$APP_USER:$APP_USER" "/home/$APP_USER/.xinitrc" "/home/$APP_USER/.config/openbox/autostart"
chmod 0755 "/home/$APP_USER/.xinitrc"

cat > /etc/systemd/system/rustipc-kiosk.service <<EOF
[Unit]
Description=RustIPC full-screen installer kiosk
After=systemd-user-sessions.service getty@tty1.service
Conflicts=getty@tty1.service

[Service]
User=$APP_USER
PAMName=login
WorkingDirectory=/opt/rustipc-installer
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
StandardInput=tty
StandardOutput=journal
StandardError=journal
Restart=on-failure
RestartSec=3
Environment=XDG_SESSION_TYPE=x11
ExecStart=/usr/bin/startx -- :0 vt1 -keeptty -nolisten tcp

[Install]
WantedBy=multi-user.target
EOF
systemctl set-default multi-user.target
systemctl enable rustipc-kiosk.service
echo "Kiosk installed. Reboot to launch. Logs: journalctl -u rustipc-kiosk -f"
