<!-- SPDX-FileCopyrightText: 2026 Slavick -->
<!-- SPDX-License-Identifier: MIT -->

# Changelog

## 1.0.2 - 2026-10-29

Changes since 1.0.1.

### Added

- Undo/Redo for file operations and tag changes, with up to 100 actions shared
  across tabs in the current window. Shortcuts: Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y.
  Replacement, permanent deletion and Empty Trash remain irreversible.
- Per-column width controls in Columns view: drag the right edge to resize, or
  use the footer button to fit all displayed file and folder names, including
  entries outside the visible area. Widths remain in effect while the column
  is open.
- Automatic horizontal scrolling to reveal the new column when selecting a
  folder at the right edge.
- GNOME Online Accounts locations in the sidebar, support for OneDrive,
  Google Drive and WebDAV connection addresses, and bundled cloud provider icons.
  Cloud access requires the corresponding GVfs backend and an authorized account.

### Changed

- Store tags in the `user.finder-gnome.tags` extended attribute on supported
  local files and folders. Existing assignments migrate when files are encountered;
  SQLite remains the registry, index and fallback for unsupported locations.
- Use system-provided icons for sidebar locations, with provider icons for
  OneDrive and Google Drive.
- Remember the Quick Look window size between openings and application restarts.

### Fixed

- Open With dialog opened from Quick Look.
- Horizontal scrollbar overlapping column width controls and intercepting clicks.
- Column auto-scrolling running before GTK updated the new column's layout.
- Connection handling mixing up different cloud accounts and WebDAV endpoints.
