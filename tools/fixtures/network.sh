#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

# Run only inside an ephemeral test container. Never against a user's desktop.
set -euo pipefail
if [ ! -f /.dockerenv ]; then echo 'This fixture must run in an isolated Docker container.' >&2; exit 1; fi
export DEBIAN_FRONTEND=noninteractive
mkdir -p /output
cd "${FINDER_FIXTURE_WORKSPACE:-/workspace}"
if ! command -v smbpasswd >/dev/null || ! dpkg-query -W gvfs-backends >/dev/null 2>&1; then
apt-get update -qq
apt-get install -y -qq gvfs gvfs-backends gvfs-fuse openssh-server samba dbus-x11 xauth xvfb adwaita-icon-theme ffmpeg libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev build-essential pkg-config xdotool > /output/network-packages.log 2>&1
fi
apt-get install -y -qq gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-libav >> /output/network-packages.log 2>&1
. /etc/os-release
if [ "$VERSION_ID" = "24.04" ]; then apt-get install -y -qq libgtk-4-media-gstreamer >> /output/network-packages.log 2>&1; fi
id finder >/dev/null 2>&1 || useradd -m finder
printf 'finder:finder-test-only\n' | chpasswd
mkdir -p /run/sshd /srv/finder/share/Folder /srv/finder/readonly /home/finder/.ssh /output/target-stage3
rm -rf /srv/finder/share/*
mkdir -p /srv/finder/share/Folder
printf 'remote fixture\n' > /srv/finder/share/origin.txt
chown -R finder:finder /srv/finder/share /home/finder /output/target-stage3
chmod 755 /srv/finder/readonly
if [ ! -s /run/finder-sshd.pid ] || ! kill -0 "$(cat /run/finder-sshd.pid)" 2>/dev/null; then
/usr/sbin/sshd -p 2222 -o PasswordAuthentication=yes -o UsePAM=no -o PidFile=/run/finder-sshd.pid
fi
ssh-keyscan -p 2222 127.0.0.1 > /home/finder/.ssh/known_hosts 2>/dev/null
chown finder:finder /home/finder/.ssh/known_hosts
printf 'finder-test-only\nfinder-test-only\n' | smbpasswd -a -s finder >/dev/null
cat > /etc/samba/smb.conf <<'CONFIG'
[global]
    workgroup = WORKGROUP
    server role = standalone server
    interfaces = lo
    bind interfaces only = yes
    smb ports = 445
    log file = /tmp/smbd.log
[share]
    path = /srv/finder/share
    read only = no
    valid users = finder
[readonly]
    path = /srv/finder/readonly
    read only = yes
    valid users = finder
CONFIG
/usr/sbin/smbd -D
runuser -u finder -- env CARGO_HOME=/cargo CARGO_TARGET_DIR=/output/target-stage3 \
  PATH=/opt/rust/bin:/usr/local/bin:/usr/bin:/bin GIO_USE_VFS=gvfs GTK_A11Y=none GSETTINGS_BACKEND=memory GDK_BACKEND=x11 GSK_RENDERER=cairo \
  FINDER_GNOME_TEST_SFTP=sftp://finder@127.0.0.1:2222/srv/finder/share \
  FINDER_GNOME_TEST_SMB=smb://finder@127.0.0.1/share \
  FINDER_GNOME_TEST_PASSWORD=finder-test-only \
  FINDER_GNOME_SCREENSHOTS=/tmp/finder-stage3-network-screenshots \
  xvfb-run -a dbus-run-session -- cargo test --offline --locked gui_acceptance -- --ignored --nocapture
