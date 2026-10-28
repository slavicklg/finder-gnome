<!-- SPDX-FileCopyrightText: 2026 Slavick
     SPDX-License-Identifier: MIT -->

# Finder Gnome

A personal, Finder-inspired file manager written in Rust with GTK4 and
libadwaita. Target systems: **Ubuntu 24.04 LTS and Ubuntu 26.04 LTS or newer,
GNOME**. The API baseline stays at GTK 4.14, libadwaita 1.5 and GLib 2.80.
New Ubuntu releases should run the compatibility suite before being considered
verified.

The Cargo package and executable are named `finder-gnome`; the GNOME application
ID is `io.github.finder_gnome`. Existing settings, tags, sessions and preview
caches retain their `ubuntu-finder` storage directories for compatibility with
previous versions.

## Installation

Requires **Ubuntu 24.04 LTS (`noble`) or Ubuntu 26.04 LTS (`resolute`), GNOME,
`amd64`**. APT installs GTK4, libadwaita and the other runtime dependencies declared
by the package built for your Ubuntu release. Installation registers the app as a
folder handler; it does not change your default file manager.

### Automatic installation

Download the installer and inspect `install.sh` in a text editor before running it:

```bash
curl -fsSLO https://raw.githubusercontent.com/slavicklg/finder-gnome/master/tools/install.sh
chmod +x install.sh
./install.sh --dry-run
./install.sh
```

The installer checks Ubuntu's ID, version and architecture before making changes.
It installs only missing download/key prerequisites, downloads the public key over
HTTPS and checks its full pinned fingerprint before installing the keyring. It
creates a DEB822 APT source, updates the package index and installs `finder-gnome`.
Run as a regular user with `sudo` available, or as root. `--dry-run` makes no system
changes, downloads nothing and never requests privileges. Running the installer
again preserves unchanged key/source files and can upgrade the installed package.
Conflicting repository entries require manual review; other APT sources are never
removed or rewritten.

Repository: <https://repo.web7dev.com>, component `main`, architecture `amd64`.
Public key: [finder-gnome.gpg](https://repo.web7dev.com/finder-gnome.gpg).
The full, maintainer-confirmed fingerprint is:

```text
F550EC3355FD905A3B39D4B5679BD5860B6EECE1
```

### Manual installation

Run the following in Bash. The platform checks map Ubuntu 24.04 to `noble` and
26.04 to `resolute`. If this repository is already configured, use the automatic
installer to reconcile its own files, or review the existing entry before
continuing; do not add a second `.list`/`.sources` entry.

```bash
(
set -Eeuo pipefail
. /etc/os-release
case "$ID:$VERSION_ID" in
    ubuntu:24.04) suite=noble ;;
    ubuntu:26.04) suite=resolute ;;
    *) echo 'Only Ubuntu 24.04 and 26.04 are supported.' >&2; exit 1 ;;
esac
[[ $(dpkg --print-architecture) == amd64 ]] || {
    echo 'Only amd64 is supported.' >&2; exit 1;
}
[[ ! -e /etc/apt/sources.list.d/finder-gnome.sources &&
   ! -L /etc/apt/sources.list.d/finder-gnome.sources &&
   ! -L /etc/apt/keyrings/finder-gnome.gpg ]] || {
    echo 'Review existing Finder Gnome source/keyring paths first.' >&2; exit 1;
}
if grep -Fl 'repo.web7dev.com' /etc/apt/sources.list \
    /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources 2>/dev/null; then
    echo 'Review the existing repository entry before proceeding.' >&2; exit 1
fi
sudo apt update
sudo apt install curl gnupg ca-certificates

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -m 700 "$work/gnupg"
expected='F550EC3355FD905A3B39D4B5679BD5860B6EECE1'
curl --disable --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
    --connect-timeout 10 --max-time 60 --max-filesize 1048576 \
    https://repo.web7dev.com/finder-gnome.gpg --output "$work/download"
gpg --no-options --homedir "$work/gnupg" --batch --no-autostart \
    --with-colons --show-keys "$work/download" > "$work/key.info"
actual=$(awk -F: '$1 == "pub" {primary=1; next} primary && $1 == "fpr" {print $10; exit}' "$work/key.info")
count=$(awk -F: '$1 == "pub" {n++} END {print n+0}' "$work/key.info")
printf 'Expected fingerprint: %s\nReceived fingerprint: %s\n' "$expected" "$actual"
[[ $count == 1 && $actual == "$expected" ]] || {
    echo 'Public key verification failed.' >&2; exit 1;
}
# Normalize either armored or binary input into a public-only binary keyring.
gpg --no-options --homedir "$work/gnupg" --batch --no-autostart --import "$work/download"
gpg --no-options --homedir "$work/gnupg" --batch --no-autostart \
    --export "$expected" > "$work/finder-gnome.gpg"
test -s "$work/finder-gnome.gpg"
sudo install -d -m 0755 /etc/apt/keyrings
sudo install -m 0644 "$work/finder-gnome.gpg" /etc/apt/keyrings/finder-gnome.gpg
cat > "$work/finder-gnome.sources" <<EOF
Types: deb
URIs: https://repo.web7dev.com
Suites: $suite
Components: main
Architectures: amd64
Signed-By: /etc/apt/keyrings/finder-gnome.gpg
EOF
sudo install -m 0644 "$work/finder-gnome.sources" /etc/apt/sources.list.d/finder-gnome.sources
sudo apt update
sudo apt install finder-gnome
)
```

Launch **Finder Gnome** from Applications, or run `finder-gnome` in a terminal.

### Updating

The configured repository participates in normal APT updates. To upgrade this
application explicitly:

```bash
sudo apt update
sudo apt install --only-upgrade finder-gnome
```

### Uninstalling

```bash
sudo apt remove finder-gnome
```

Optionally disconnect this repository by removing only its own source and key:

```bash
sudo rm /etc/apt/sources.list.d/finder-gnome.sources
sudo rm /etc/apt/keyrings/finder-gnome.gpg
sudo apt update
```

Application settings, tags, sessions and other user data are not removed.

## Build and run

Install a current stable Rust toolchain through rustup, then install the system
development dependencies:

```sh
sudo apt update
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev librsvg2-2 gettext python3 \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-libav
# Ubuntu 24.04 only (merged into GTK on Ubuntu 26.04):
# sudo apt install libgtk-4-media-gstreamer
cargo run --locked
cargo run --locked -- "$HOME/Documents"
```

The binary is `target/debug/finder-gnome` (`target/release/finder-gnome` after
`cargo build --release --locked`). Development builds do not install the program,
change the default file manager or modify GNOME settings.

## Desktop launcher and Debian packages

The launcher is [resources/io.github.finder_gnome.desktop](resources/io.github.finder_gnome.desktop).
It uses the application ID `io.github.finder_gnome` and launches `finder-gnome %U`
with the selected folders/URIs. The package installs it into
`/usr/share/applications` and installs the PNG icons into the system hicolor theme.
The fields follow the [Desktop Entry Specification](https://specifications.freedesktop.org/desktop-entry/latest-single/).

Build on the target Ubuntu release using the development dependencies above:

```sh
sudo apt install dpkg-dev desktop-file-utils
bash tools/build-deb.sh
# Optional: CARGO_NET_OFFLINE=true bash tools/build-deb.sh
```

The script makes a native release build, determines runtime library dependencies
with `dpkg-shlibdeps` and writes
`dist/finder-gnome_1.0.0-1ubuntu<release>_<architecture>.deb`.
The version comes from Cargo.toml. `FINDER_GNOME_DEB_OUTPUT_DIR` overrides the
output directory; `SOURCE_DATE_EPOCH` controls archive timestamps. Building needs
no root privileges and installs nothing. Cross-compilation is not supported.
Build separately on Ubuntu 24.04 and 26.04 to use each release's library ABI.

Install the package built for your Ubuntu release, for example:

```sh
sudo apt install ./dist/finder-gnome_1.0.0-1ubuntu26.04_amd64.deb
# Remove the installed application:
sudo apt remove finder-gnome
```

APT installs the required runtime libraries, GVfs backends and Archive Manager.
Media plugins and GVfs FUSE support are recommended dependencies. The Ubuntu 24.04
package additionally recommends `libgtk-4-media-gstreamer`. Translations present
as `po/<language>.po` are compiled into the package. Copyright, project license
and bundled Rust dependency notices are installed under
`/usr/share/doc/finder-gnome`. User settings, tags and sessions retain their
existing paths and are not deleted on uninstall. Installation registers the app
as a handler for folders and does not set it as the default file manager.

CI builds a separate package for each Ubuntu matrix entry, installs it in the
container, launches it through the installed `.desktop` file and removes it.
The artifacts are uploaded as `finder-gnome-ubuntu-24.04` and
`finder-gnome-ubuntu-26.04`. `tools/fixtures/deb.sh` refuses to install test packages
outside a disposable Docker container.

## Features (stages 1–3)

- Home and XDG favorites, filesystem root, editable location and clickable path.
- Compact sidebar and an Applications section with installed launchers, search,
  a Launch button, double-click and Enter activation.
- Icons, sortable List, Miller Columns and Gallery views with multiple selection.
- Independent tabs, Back/Forward history and restoration of the previous session.
- Image thumbnails, Gallery and Quick Look for images, text, PDFs, audio and video.
- Local file Drag & Drop onto folders, sidebar destinations and tabs.
- Per-folder display settings, global defaults and System/Light/Dark appearance.
- Default-application opening, hidden-file toggle, live directory monitoring.
- New folder, rename, recursive copy/move, GNOME-compatible clipboard, system
  trash and confirmed permanent deletion.
- Sequential background operations, per-file progress, cancellation and conflict
  decisions: replace, skip, keep both or merge folders.
- English source UI with gettext-ready localization.

Folders open with a double click in Icons/List. In Columns, selecting a folder
opens its children to the right; Enter/Right enters that column and Left returns
focus to its parent. Paste targets the **active column's directory**. A failed
location remains visible with its error and a retry button; Up and the sidebar
remain available.

| Shortcut | Action |
| --- | --- |
| Ctrl+1 / Ctrl+2 / Ctrl+3 / Ctrl+4 | Icons / List / Columns / Gallery |
| Alt+Left / Alt+Right | Back / Forward |
| Ctrl+T / Ctrl+W | New tab / Close tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / Previous tab |
| Space / Escape | Quick Look / Close Quick Look |
| Left / Right in Quick Look | Previous / Next file |
| PageUp / PageDown in PDF preview | Previous / Next page |
| Ctrl+, | Preferences |
| Alt+Enter | Properties |
| Ctrl+F / Ctrl+K | Search / Connect to Server |
| Ctrl+L / Alt+Up / F5 | Location / Parent / Reload |
| Ctrl+H / Ctrl+A | Show hidden / Select all |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / Cut / Paste |
| Ctrl+Z / Ctrl+Shift+Z (or Ctrl+Y) | Undo / Redo |
| Ctrl+Shift+N / F2 | New folder / Rename |
| Enter | Open |
| Delete / Shift+Delete | Trash / Delete permanently |

## Network, devices, search and tags

Install the runtime integration backends on the desktop:

```sh
sudo apt install gvfs gvfs-backends gvfs-fuse file-roller
```

**Connect to Server** (Ctrl+K) accepts `smb://server/share`,
`sftp://user@server/path`, `onedrive://…`, `google-drive://…` and WebDAV
(`dav://…` / `davs://…`, for example Nextcloud). Authentication, SSH host trust and credential storage
use GNOME/GVfs dialogs and the system keyring. Passwords in addresses are rejected;
recent connections store addresses only. Restored inactive network tabs wait until
selected before connecting. Disconnected tabs remain available with Retry.

Cloud accounts configured in GNOME **Online Accounts** appear automatically under
**Locations**, including accounts waiting to be mounted. OneDrive and Google Drive
have bundled provider icons; other locations use their system-provided icons.
Open an account to mount it through GVfs. Use the account's complete URI when
entering an address manually; a bare `onedrive://` has no account identifier.
Cloud access requires the corresponding installed GVfs backend and an authorized
system account. Google Drive availability depends on the distribution's GVfs build;
some recent builds omit that backend. Existing local/FUSE mounts remain accessible
as local folders and use the mount icon supplied by the system.

Drag the divider beside the sidebar to resize it. Its width is saved in the
application settings and restored on the next launch.

**Properties** (Alt+Enter) shows metadata for a selected file or folder:
location, type, file size, timestamps, owner/group, Unix permissions and symlink
target, where available. With no selection, it shows the current folder.
Folder sizes are calculated asynchronously, with separate logical and allocated
sizes. Hard links count once; symlinks are not followed. Native filesystem
boundaries and trees deeper than 256 levels are skipped. Stop Calculating or close
the dialog to cancel. Unreadable/skipped entries produce a partial-result notice;
backends that omit allocated size show a lower bound rather than a full total.

The Permissions section edits Read/Write/Execute (Access for folders) for Owner,
Group and Others. **Apply Permissions** changes the selected item only, retaining
special mode bits where permitted by the filesystem and re-reading the actual
mode after saving. Backend/ownership errors appear in the dialog. No privilege
escalation is attempted. Permissions are not edited on symlinks, trash entries or
backends without Unix mode support. Application properties include a Desktop File
Properties button for inspecting and editing the launcher's file permissions. Applications also has a right-click menu
with Open and Properties, including launcher ID, executable and desktop-file path.

The file context menu includes **Open With…** (the system application chooser),
**Copy To…** and **Move To…** (destination folder chooser and the application's
background operation queue). **Compress…** and **Extract…** open GNOME Archive
Manager (`file-roller`), which handles archive format, password, output-folder and
overwrite dialogs. Extract is enabled for selected archive files; compression
accepts files and folders. Archive operations run in Archive Manager rather than
the application's task list; supported formats depend on its installed backends.

**Locations** follows GIO's volume monitor and offers mount, unmount and eject.
Disconnect and Eject always ask for confirmation first.
If this application's operations use the device, it offers cancellation and waits
for those operations before unmounting. It never forces an unmount. Other processes
can still keep a device busy; the resulting system error is reported.

**Search** (Ctrl+F) recursively matches names, case-insensitively, in This Folder
or Home. Results arrive in batches and include their enclosing location. Use
**Show Enclosing Folder** to return to a normal directory. Search supports Icons,
List and Gallery, respects hidden-file settings, and can be stopped. It does not
follow directory symlinks or cross native filesystem boundaries. It stops at
20,000 matches or 256 directory levels with a notice; unreadable branches are
reported. Content search and indexing are outside this stage.

Use **Tags…** in the context menu to assign multiple tags and **Manage Tags**
to create, rename, recolor or delete tags. Deleting a tag leaves files intact.
On supported local files and folders, assignments are stored in the
`user.finder-gnome.tags` extended attribute as versioned JSON with tag UUIDs,
names and colors. Hard links share these attributes; symbolic links use local
storage without changing their targets.

SQLite remains the tag registry, collection index and fallback at
`$XDG_DATA_HOME/ubuntu-finder/catalog.sqlite` (normally
`~/.local/share/ubuntu-finder/catalog.sqlite`). Existing assignments are migrated
when files are encountered. Unsupported filesystems, network locations and
unwritable attributes retain their assignments locally, with a notice.
Malformed or newer-version attributes are preserved and reported.

App copies inherit tags; moves and renames preserve them, including completed
parts of interrupted transfers. Replacing a target replaces its tag assignments
with the source's. External moves within a filesystem preserve xattrs; moves
between filesystems and copies preserve them only when the other program and
destination support it. Files are indexed at their new location when encountered;
there is no whole-disk search for moved files. Sidebar collections retain offline
entries. Renaming, recoloring or deleting a tag updates available files immediately
and retries unavailable files when encountered. A different computer may see the
old metadata until those files have been updated.

## Trash

The sidebar's **Trash** opens the system GIO/GVfs trash, including accessible
mounted volumes. Icons and List show trashed items; List also shows their original
path and deletion date. Use **Restore** to return selected items to their original
locations or **Restore To…** to choose another folder. A restore never overwrites
an existing file; a missing original parent or occupied name leaves the item in
Trash and reports an error. Restore To can be used to recover it elsewhere.

**Delete Permanently…** deletes selected trash entries after confirmation.
**Empty Trash…** confirms and deletes a snapshot of all entries, including ones
hidden by the current display filter. Items arriving after the snapshot remain.
Both use cancellable background tasks; completed deletions cannot be undone.
Deleting a trashed folder removes its contents without following symlink targets.
Dragging files onto the sidebar Trash moves them to trash; forced Copy drops are
rejected. Double-clicking a trash entry restores it. To open or edit a file,
restore it first. Trash tabs, navigation history and view settings are restored
with the session. The `gvfs` runtime backend is required for browsing Trash.

## Preview and display settings

Gallery follows the selection; Space opens Quick Look in a separate window.
Images support Fit/100%, PDFs page navigation, and text a bounded preview.
Audio/video always start paused and require Play; playback stops when leaving
the preview or switching tabs. Supported codecs depend on the installed
GStreamer plugins. Open and Open With launch external applications explicitly.
Image decoding is limited to 40 million pixels; text previews to 256 KiB.
SVG is handled separately from GdkPixbuf by the system librsvg (ABI 2.52+, present
in Ubuntu 24.04 and 26.04), using a bounded renderer subprocess. Intrinsic pixel
sizes are checked before allocating a Cairo image. Physical units use 96 DPI;
font-based units use librsvg's computed CSS font size. Relative/omitted dimensions
use viewBox as the natural viewport, infer an unspecified axis from its aspect
ratio when the other axis is absolute, or use 300×150 without viewBox. Percentages
remain relative to that chosen viewport and are resolved by librsvg.

SVG input is limited to 4 MiB, 100,000 XML nodes and 128 nesting levels. DTDs and
external resources are disabled. The child has a 768 MiB address-space limit,
3-second CPU limit and 5-second wall deadline; cancellation terminates its whole
process group. UTF-8/16/32 XML is validated; compressed SVG previews are explicitly
unavailable. Other image formats retain their existing Pixbuf decoding path.

Network previews require an explicit Gallery Preview click or Quick Look; remote
thumbnails are disabled. Non-text remote previews download at most 50 MiB,
including when the server reports no size. The on-disk remote preview cache in
`$XDG_CACHE_HOME/ubuntu-finder/previews` is limited to 512 MiB and evicts least
recently used entries. Media playback still requires Play.
Thumbnails are loaded asynchronously, with two workers and a 128 MiB raster cache.
Animated images use their first frame.

The menu's View Options controls mode, sorting, hidden files, icon size,
thumbnails, list columns and column widths. Settings are saved per folder;
Preferences controls defaults, appearance and session restoration. Configuration
is stored in `$XDG_CONFIG_HOME/ubuntu-finder/settings.json` (normally
`~/.config/ubuntu-finder/settings.json`). Tabs, active tab, selection, scroll and
navigation position are stored in `$XDG_STATE_HOME/ubuntu-finder/session.json`
(normally `~/.local/state/ubuntu-finder/session.json`). History is independent
within each running tab. Invalid or unreadable saved files fall back to defaults
and report an error. Command-line folders open in new tabs alongside restored tabs.

Drag & Drop defaults to Move on the same filesystem and Copy across filesystems
or when the filesystem is unknown. Ctrl forces Copy; Shift forces Move. Hovering
a tab switches to it. A folder containing any of the dragged items is not an
allowed drop destination, even with Ctrl/Shift; it is not highlighted and no
transfer or conflict dialog is started. Applications is not a file-drop destination. All transfers
use the same background operation queue and conflict dialogs as Paste.

## Operation semantics

**Undo/Redo** in the Actions menu applies to file operations and tag changes.
The most recent 100 actions are shared by all tabs and panes until the window
closes. Creation, copy, move, rename, Trash, Restore, tag assignment and tag
management are reversible. A new change discards Redo. Text fields retain their
own editing history. Undo/Redo is disabled while queued changes are running.

Partial operations record only completed changes. Undo checks the affected
objects and stops if they have changed or a destination is occupied; new files
inside a created folder are never recursively removed. A failed Undo can be
retried after resolving the conflict. Replacement, permanent deletion and Empty
Trash cannot be undone and clear operation history after a successful mutation.
If a backend cannot identify the exact Trash entry, the operation is completed
with a notice that its history could not be retained.

Cancellation keeps completed changes, which remain available to Undo. A copy writes a temporary
sibling and commits it only when complete, so a cancelled copy does not truncate
an existing destination. A cross-filesystem move copies before removing each
successfully copied source. Skipped or failed children stay at the source. Symbolic
links are copied/deleted as links, including broken links. Partial failures are
reported with affected paths. Permanent deletion requires confirmation; an
unavailable trash never falls back to deletion. File conflicts created concurrently
after validation fail safely instead of silently overwriting an unapproved target.

Background Tasks lists each task with its own progress and Cancel button.
Failed tasks offer Retry Failed Items (or Reconnect and Retry); retries skip
successfully copied children. There is no automatic retry of mutations or rollback
of completed changes. Tasks and retry checkpoints are retained for the current
process only. Remote symbolic-link aliases in directory transfer paths are
rejected when their destination cannot be established safely through GIO.

## Verification

The installer has isolated tests using real GPG verification with temporary keys,
fake package/download commands and temporary filesystem paths. They require Python
3 and GnuPG and never install the application or modify the host's APT settings:

```sh
bash -n tools/install.sh
shellcheck tools/install.sh  # optional local tool; required in CI
python3 tools/test-install.py
```

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked cross_filesystem_move -- --ignored --nocapture

sudo apt install xvfb xauth dbus-x11 adwaita-icon-theme ffmpeg xdotool librsvg2-common gdb
bash tools/fixtures/gui.sh
```

The display test covers all views, preserved selection, column replacement,
clipboard transfer, create/rename, external changes, superseded navigation and
10,000-entry directories, independent tabs, history, saved settings, session
restoration, PDF/image/text/media previews and native X11 folder/tab dragging.
Set `FINDER_GNOME_SCREENSHOTS=/tmp/finder-screenshots`
to save rendered windows. Disable `FINDER_GNOME_NATIVE_DND` for Wayland or
scaled tests. Repeat with `GDK_SCALE=2` and in a real GNOME Wayland session
for display checks. CI runs the build and Xvfb acceptance on both Ubuntu
versions; Xvfb does not certify Wayland or Nautilus interoperability.

The X11 runner gives GUI children their own HOME and XDG directories, retaining
the original Cargo/rustup locations. It uses a 1024×768 display, native input and
the real media backend. Logs and library versions are saved under
`target/gui-diagnostics` (or `$CARGO_TARGET_DIR/gui-diagnostics`);
`FINDER_GNOME_GUI_DIAGNOSTICS` overrides that directory. After a SIGSEGV, an
additional GDB run collects native thread backtraces without changing the original
failure status. CI uploads failed-run logs as `gtk-diagnostics-<ubuntu-version>`.

The container-only `tools/fixtures/network.sh` starts loopback Samba and
OpenSSH fixtures with disposable credentials and runs the GUI network acceptance
suite. It must never be used on the desktop host. CI provisions it on both Ubuntu
versions. See [verification details](doc/verification.md) for tested coverage and
manual checks.

See [localization](po/README.md) for translation tooling.

## Stage boundaries

Stages 1–3 include local and GVfs files, device management, filename search,
application tags, Undo/Redo and background task handling. Large-directory performance work remains outside the current implementation. Executables are not launched automatically.
Hard-link relationships, filesystem-specific metadata and crash recovery are not
preserved as a backup tool would preserve them. Debian packaging is implemented;
registering as the default file manager and performance work remain for stage 4.


## License

Copyright © 2026 Slavick. Licensed under the [MIT License](LICENSE), permitting
free use, modification and redistribution, including commercial use, with the
copyright and license notice retained. Third-party dependencies retain their
respective licenses.
