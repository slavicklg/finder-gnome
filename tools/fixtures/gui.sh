#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

# A complete X11 GUI session, including native input and media playback.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

# Preserve rustup's installation before assigning a private HOME to GUI children.
gui_cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
gui_rustup_home=${RUSTUP_HOME:-"$HOME/.rustup"}
gui_work=$(mktemp -d -t finder-gnome-gui.XXXXXXXX)
trap 'rm -rf -- "$gui_work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
gui_diagnostics=${FINDER_GNOME_GUI_DIAGNOSTICS:-"${CARGO_TARGET_DIR:-target}/gui-diagnostics"}
mkdir -p -- "$gui_diagnostics"
gui_diagnostics=$(realpath -- "$gui_diagnostics")
mkdir -m 0700 -- "$gui_work/home" "$gui_work/config" "$gui_work/data" \
    "$gui_work/cache" "$gui_work/state" "$gui_work/runtime"

gui_environment=(env
    "HOME=$gui_work/home" "CARGO_HOME=$gui_cargo_home" "RUSTUP_HOME=$gui_rustup_home"
    "XDG_CONFIG_HOME=$gui_work/config" "XDG_DATA_HOME=$gui_work/data"
    "XDG_CACHE_HOME=$gui_work/cache" "XDG_STATE_HOME=$gui_work/state"
    "XDG_RUNTIME_DIR=$gui_work/runtime" RUST_BACKTRACE=full
    GTK_A11Y=none GIO_USE_VFS=local GSETTINGS_BACKEND=memory GSK_RENDERER=cairo
    GDK_BACKEND=x11 FINDER_GNOME_NATIVE_DND=1)

{
    id
    printf 'Inherited HOME: %s\n' "$HOME"
    stat -c 'Inherited HOME owner/mode: %u:%g %a' "$HOME" || true
    stat -c 'GUI HOME owner/mode: %u:%g %a' "$gui_work/home"
    pkg-config --modversion gtk4 libadwaita-1
    dpkg-query -W libgtk-4-1 libgstreamer1.0-0 libpulse0
} > "$gui_diagnostics/environment.log" 2>&1

# Keep the original test result even if a diagnostic rerun succeeds. The first
# run is ordinary cargo test, without debugger-induced scheduling changes.
set +e
"${gui_environment[@]}" timeout --signal=TERM --kill-after=10s 180s \
    xvfb-run -a -s '-screen 0 1024x768x24' dbus-run-session -- \
    cargo test --locked gui_acceptance -- --ignored --nocapture \
    2>&1 | tee "$gui_diagnostics/test.log"
gui_status=${PIPESTATUS[0]}
set -e

if (( gui_status != 0 )) && grep -Eq 'SIGSEGV|signal: 11' "$gui_diagnostics/test.log"; then
    if command -v gdb >/dev/null; then
        # Ask Cargo for the executable: never guess hashes or target directories.
        if "${gui_environment[@]}" cargo test --locked --no-run --message-format=json \
            > "$gui_diagnostics/build.jsonl" 2> "$gui_diagnostics/build.log"; then
            if gui_executable=$(python3 - "$gui_diagnostics/build.jsonl" <<'PY'
import json, sys
executables = [r["executable"] for line in open(sys.argv[1])
               if (r := json.loads(line)).get("executable") and r.get("profile", {}).get("test")]
if len(executables) != 1:
    raise SystemExit("Expected exactly one GUI test executable")
print(executables[0])
PY
            ); then
                "${gui_environment[@]}" timeout --signal=TERM --kill-after=10s 180s \
                xvfb-run -a -s '-screen 0 1024x768x24' dbus-run-session -- \
                gdb --batch --return-child-result -ex 'set pagination off' \
                -ex 'set print thread-events off' -ex 'set debuginfod enabled off' \
                -ex run -ex 'thread apply all bt full' --args \
                "$gui_executable" gui_acceptance --ignored --nocapture \
                    > "$gui_diagnostics/gdb.log" 2>&1 || true
            else
                printf 'Could not locate the GUI executable.\n' > "$gui_diagnostics/gdb.log"
            fi
        fi
    else
        printf 'Install gdb to collect a native crash backtrace.\n' > "$gui_diagnostics/gdb.log"
    fi
fi

printf 'GUI diagnostics: %s\n' "$gui_diagnostics"
exit "$gui_status"
