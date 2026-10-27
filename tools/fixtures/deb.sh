#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

# Installs and removes a real package only in a disposable container.
set -euo pipefail
if [[ ! -f /.dockerenv ]]; then echo 'Run package installation tests only in a disposable Docker container.' >&2; exit 1; fi
if (( $# != 1 )); then echo 'Usage: bash tools/fixtures/deb.sh /absolute/path/to/package.deb' >&2; exit 2; fi
package=$(realpath "$1")
export DEBIAN_FRONTEND=noninteractive
# Ubuntu Docker images normally omit documentation; retain this package's notices.
printf '%s\n' 'path-include=/usr/share/doc/finder-gnome' 'path-include=/usr/share/doc/finder-gnome/*' > /etc/dpkg/dpkg.cfg.d/zz-finder-gnome-test
if dpkg-query -W finder-gnome >/dev/null 2>&1; then apt-get remove -y finder-gnome >/dev/null; fi
apt-get update -qq
apt-get install -y --no-install-recommends "$package" xvfb xauth dbus-x11 libglib2.0-bin > /tmp/finder-gnome-deb-install.log 2>&1
[[ $(dpkg-query -W -f='${Status}' finder-gnome) == 'install ok installed' ]]
test -x /usr/bin/finder-gnome
desktop-file-validate /usr/share/applications/io.github.finder_gnome.desktop
verification=$(dpkg --verify finder-gnome)
if [[ -n $verification ]]; then printf '%s\n' "$verification" >&2; exit 1; fi
for size in 32 48 64 128 256 512; do
  test -s "/usr/share/icons/hicolor/${size}x${size}/apps/io.github.finder_gnome.png"
done
id finder >/dev/null 2>&1 || useradd -m finder
work=$(mktemp -d /tmp/finder-gnome-deb-test.XXXXXXXX)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work"/{config,data,state,cache,runtime,folder}
chown -R finder:finder "$work"
chmod 700 "$work/runtime"
runuser -u finder -- env GTK_A11Y=none GIO_USE_VFS=local GSETTINGS_BACKEND=memory GDK_BACKEND=x11 GSK_RENDERER=cairo \
  XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" XDG_STATE_HOME="$work/state" XDG_CACHE_HOME="$work/cache" XDG_RUNTIME_DIR="$work/runtime" \
  FINDER_GNOME_DEB_TEST_FOLDER="$work/folder" \
  xvfb-run -a dbus-run-session -- bash -s <<'SMOKE'
set -euo pipefail
gio launch /usr/share/applications/io.github.finder_gnome.desktop "$FINDER_GNOME_DEB_TEST_FOLDER"
ready=false
for attempt in {1..100}; do
  if gdbus call --session --dest io.github.finder_gnome --object-path /io/github/finder_gnome/window/1 --method org.gtk.Actions.Describe properties >/dev/null 2>&1; then ready=true; break; fi
  sleep .1
done
if [[ $ready != true ]]; then echo 'Installed application did not expose its window actions.' >&2; exit 1; fi
gdbus call --session --dest io.github.finder_gnome --object-path /io/github/finder_gnome/window/1 --method org.gtk.Actions.Activate close-tab '[]' '{}'
SMOKE
apt-get remove -y finder-gnome > /tmp/finder-gnome-deb-remove.log 2>&1
test ! -e /usr/bin/finder-gnome
test ! -e /usr/share/applications/io.github.finder_gnome.desktop
printf 'Package install, desktop launch and removal: OK\n'
