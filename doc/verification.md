<!-- SPDX-FileCopyrightText: 2026 Slavick
     SPDX-License-Identifier: MIT -->

# Stages 1–3 verification

Checked on 2026-10-26. The source API baseline is GTK 4.14 / libadwaita 1.5 /
GLib 2.80. Rust dependencies are locked in Cargo.lock.

| Environment | Verification |
| --- | --- |
| Ubuntu 26.04 host, GTK 4.22.4, libadwaita 1.9.1 | Build, Clippy, filesystem tests, GTK acceptance on Xvfb |
| Ubuntu 24.04 container with distribution development packages | Build, filesystem tests, cross-filesystem move, GTK acceptance on Xvfb |
| Ubuntu 26.04, Xvfb at scale 2 | GTK acceptance and rendered windows |
| Ubuntu 26.04, private Weston 14 headless Wayland compositor | GTK acceptance and rendered windows |

The default suite has 40 tests plus five opt-in entries (including the internal SVG worker): GUI acceptance, isolated Trash acceptance,
cross-filesystem move, and an isolated-trash subprocess helper (called by its
parent test). Filesystem coverage includes recursive copy, merge/skip moves,
cancelled queued work, conflict cancellation, non-UTF-8 names, symlink handling,
root/self-copy protection and isolated trash. On filesystems without trash support,
the trash test checks that the input remains intact and no delete fallback occurs.

GUI acceptance checks Icons/List/Columns, preserved selection, changing column
branches, copy/cut/paste, clipboard ownership after moving, create/rename dialogs,
invalid names, cancellation of permanent deletion and pending conflicts, dark
appearance, filesystem monitoring, interrupted navigation, error states and
10,000-entry directories. Context-menu regression coverage activates the actual
menu widgets in all four views and uses native X11 right/left clicks when pointer
testing is enabled. It checks Open, Rename, Copy/Paste, New Folder, deletion
cancellation, Quick Look, repeated opening, selection preservation, background
selection clearing, Escape/outside dismissal and ordinary double-click opening.
Native pointer coverage runs on X11. Headless Wayland checks activate the actual
menu widgets using non-grabbing popups, since the compositor has no input serial.
Optional screenshots are written outside the repository.

Applications coverage checks the sidebar entry, filtered installed-launcher list,
hidden entries, case-insensitive search, launch buttons and the Open action,
launch errors, disabled filesystem commands, and return to the previous folder.
Actual launches use a temporary executable and desktop files rather than starting
installed user applications. The compact sidebar is checked in rendered windows.

Stage 2 checks independent tabs, duplication/closing, Back/Forward branching,
Columns chain restoration, per-folder display settings, persisted tab order and
Applications restoration. Preview fixtures cover images, bounded text, two-page
PDF rendering and manually started WAV/MP4 playback, including stopping on tab
changes. Native X11 pointer tests move a file into a folder and onto a tab after
hover activation; separate operation-path checks cover forced Copy and default
Move. Same-folder drops are rejected with default/Ctrl/Shift actions; a native
pointer test checks the forbidden cursor action, absence of highlighting and
absence of queued operations or conflict dialogs before testing a valid drop.
Set `FINDER_GNOME_NATIVE_DND=1` and install xdotool for pointer tests;
disable it for Wayland and HiDPI runs. Raster cache limits, cancellation and
large-image rejection are enforced in the preview service. These tests do not
certify every installed media codec or external application association.

Ubuntu 24.04 testing also covers cancelling a conflict while its dialog is still
opening: libadwaita 1.5 needs deferred closing to avoid leaving a cancelled dialog
on screen. Recreating drop bindings between drags also avoids stale crossing
state in GTK 4.14 ancestor controllers. UI strings in nested Rust modules
are included in the gettext template.

These are automated checks using real GTK libraries, not a full certification of
the GNOME desktop. Manual follow-up in an ordinary GNOME session should cover
physical keyboard/mouse behavior, GNOME Files clipboard and Drag & Drop exchange,
default file associations and fractional scaling. Future Ubuntu releases must run the same
compatibility suite before being declared verified.


Stage 3 adds SQLite schema/version checks, persistent tag assignments, partial
transfer journals, failed-child retry preservation, external-rename identity
repair without merging existing hard links, protocol/password validation,
classified I/O errors and bounded remote cache downloads. GUI acceptance covers
recursive case-insensitive search with hidden entries and a symlink loop, stop,
collection action restrictions, tag badges, assignment/management dialogs,
rename preservation, retained missing entries, per-task conflict cancellation,
and successful retry after repairing a failed destination.

Isolated real Samba/OpenSSH fixtures passed on Ubuntu 24.04 and 26.04. They
exercise authentication through GVfs, remote navigation, download/upload,
rename, directory move, delete, recursive search, explicit Gallery and Quick Look
image preview, and a read-only SMB destination that preserves its source.
Fixtures use disposable container users, loopback servers and a test-only password
handler; the installed application has no such handler. Native host builds use
normal Cargo environment settings, without temporary pkg-config paths.

Physical disk removal, encrypted/removable volume authentication, hardware eject,
system keyring persistence, SSH trust prompts, network loss during a large
transfer and interaction with unrelated processes holding a mount open still need
manual checks in the user's GNOME session. The fixture suite does not claim to
certify those scenarios. No user drives or remote accounts were modified by tests.


Sidebar and file-command updates additionally check persisted divider width,
cancel/confirm behavior for Eject without touching a real disk, context action
availability for archives/non-archives/empty selections, and shell-free archive
arguments containing spaces and shell metacharacters. The installed GNOME Archive
Manager was separately exercised with a ZIP create/extract round trip using a
temporary file. System Open With and destination choosers use GTK's native desktop
integration; their exact presentation depends on the desktop portal.


Trash acceptance runs exclusively in a disposable Docker container with GVfs and
a temporary XDG data directory. It checks sidebar navigation, original-location
metadata, restore of files/folders/symlinks, collision refusal without overwriting,
Restore To, cancel/confirm Empty Trash, cancel/confirm individual deletion and
trash drop routing. Symlink targets outside a deleted trash entry remain intact.
Root/nested/non-trash mutation rejection and forced-Copy drop rejection have unit
coverage. The fixture refuses to run on the desktop host and is included in CI.


Properties coverage opens file metadata asynchronously and checks the filename,
full path and byte size, then opens application properties for a temporary desktop
launcher. The dialog uses selectable plain text, reports metadata errors inline,
and cancels pending reads when closed. Folder sizes are scanned asynchronously with explicit partial-result reporting. Native context-menu tests identify the exact X11 surface by geometry
to avoid confusing menu windows with tooltips. Before clicking, they explicitly
scroll the model button into view and verify that GTK hit testing reaches it.
The permanent-delete scenario disables keyboard focus on its menu button, then
still requires a native mouse click to open the confirmation dialog and Cancel
to preserve the file. Pointer tests require an enabled command, not keyboard focus.
They also force an asynchronous directory reload with the menu open, check the
temporary empty selection and disabled Delete action, then wait for loading and
selection restoration before the native click. The readiness wait retains the
20-second timeout and reports selection, access flags and widget sensitivity on failure.
CI uses a 1024×768 Xvfb screen to exercise long-menu scrolling on both Ubuntu versions.


Folder-usage tests cover sparse allocation, hard-link deduplication, symlink
non-traversal, cancellation and a folder disappearing during enumeration.
Permission tests preserve special mode bits, leave children untouched and reject
symlink permission changes. GUI acceptance changes an actual file mode through
the checkboxes and Apply button, then verifies the complete logical size of a
folder. Permission edits apply only to the selected object; unsupported remote
mode setters report their backend error without attempting elevated access.


Debian packaging checks cover native builds on Ubuntu 24.04 and 26.04, generated
shared-library dependencies, desktop-file validation, root-owned payload paths,
all six icon sizes, bundled dependency notices and automatic version extraction.
Disposable containers install each package with APT, verify its installed files,
launch through the installed desktop entry in Xvfb, check the window's D-Bus
actions, then remove it and verify launcher/binary removal. Package installation
and removal tests never run against the desktop host.


SVG regression coverage retains `preview::tests::pdf_pages_and_image_limits`
unchanged, including its oversized 100000×100000 SVG. SVG tests cover px/physical
units/em/ex, fractional rounding, viewBox and percentage/omitted/mixed dimensions,
40 MP boundaries, malformed XML/dimensions, UTF-16, disguised SVG input, DTD/entity
rejection, external resource refusal, bounded node/depth/input size, cancellation
and forced termination of an unresponsive process group. No SVG is passed to
Pixbuf::file_info. Tests use the production subprocess protocol; the one ignored
SVG test is only the internal subprocess entry, not the regression test.
