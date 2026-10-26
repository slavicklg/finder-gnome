#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

# Destructive integration tests are restricted to a disposable container.
set -euo pipefail
if [ ! -f /.dockerenv ]; then echo 'Run this fixture only in an isolated Docker container.' >&2; exit 1; fi
mkdir -p /output
export DEBIAN_FRONTEND=noninteractive
if ! dpkg-query -W gvfs-backends >/dev/null 2>&1; then
  apt-get update -qq
  apt-get install -y -qq gvfs gvfs-backends dbus-x11 xvfb xauth > /output/trash-packages.log 2>&1
fi
id finder >/dev/null 2>&1 || useradd -m finder
mkdir -p /output/target-stage3
chown -R finder:finder /output/target-stage3
fixture_data=$(mktemp -d /tmp/finder-trash-data.XXXXXX)
chown finder:finder "$fixture_data"
trap 'rm -rf "$fixture_data"' EXIT
cd "${FINDER_FIXTURE_WORKSPACE:-/workspace}"
runuser -u finder -- env CARGO_HOME=/cargo CARGO_TARGET_DIR=/output/target-stage3 \
  XDG_DATA_HOME="$fixture_data" PATH=/opt/rust/bin:/usr/local/bin:/usr/bin:/bin GTK_A11Y=none GIO_USE_VFS=gvfs \
  GSETTINGS_BACKEND=memory GDK_BACKEND=x11 GSK_RENDERER=cairo FINDER_GNOME_ISOLATED_TRASH=1 \
  FINDER_GNOME_SCREENSHOTS=/tmp/finder-trash-screenshots \
  xvfb-run -a dbus-run-session -- cargo test --offline --locked trash_acceptance -- --ignored --nocapture
