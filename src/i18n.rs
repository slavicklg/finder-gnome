// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

pub fn init() {
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    let directory = std::env::var_os("FINDER_GNOME_LOCALEDIR")
        .or_else(|| std::env::var_os("UBUNTU_FINDER_LOCALEDIR"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/share/locale"));
    let _ = gettextrs::bindtextdomain("finder-gnome", directory);
    let _ = gettextrs::bind_textdomain_codeset("finder-gnome", "UTF-8");
    let _ = gettextrs::textdomain("finder-gnome");
}

pub fn tr(message: &str) -> String {
    gettextrs::gettext(message)
}

/// Marks static menu/column labels for gettext extraction before runtime lookup.
pub fn mark(message: &'static str) -> &'static str {
    message
}

pub fn count(single: &str, plural: &str, n: u32) -> String {
    gettextrs::ngettext(single, plural, n).replace("{count}", &n.to_string())
}
