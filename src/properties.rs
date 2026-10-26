// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Asynchronous properties, folder usage and per-item permission editing.
mod permissions;
mod usage;
use crate::i18n::{count, tr};
use adw::prelude::*;

fn dialog(window: &adw::ApplicationWindow) -> (adw::AlertDialog, gtk::Box) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Properties"))
        .build();
    dialog.add_response("close", &tr("Close"));
    dialog.set_close_response("close");
    let fields = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&fields)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(350)
        .max_content_height(440)
        .propagate_natural_height(true)
        .build();
    dialog.set_extra_child(Some(&scroll));
    dialog.present(Some(window));
    (dialog, fields)
}
fn fill(fields: &gtk::Box, entries: Vec<(String, String)>) {
    while let Some(child) = fields.first_child() {
        fields.remove(&child);
    }
    for (name, value) in entries {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        row.append(
            &gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .css_classes(["dim-label"])
                .build(),
        );
        row.append(
            &gtk::Label::builder()
                .label(value)
                .xalign(0.0)
                .selectable(true)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .max_width_chars(65)
                .build(),
        );
        fields.append(&row);
    }
}
pub fn show_application(window: &adw::ApplicationWindow, app: &gio::AppInfo) {
    let (dialog, fields) = dialog(window);
    let mut entries = vec![
        (tr("Name"), app.display_name().to_string()),
        (
            tr("Description"),
            app.description()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into()),
        ),
        (
            tr("Application ID"),
            app.id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into()),
        ),
        (
            tr("Executable"),
            app.executable().to_string_lossy().into_owned(),
        ),
    ];
    if let Some(path) = app
        .downcast_ref::<gio::DesktopAppInfo>()
        .and_then(|desktop| desktop.filename())
    {
        entries.push((tr("Desktop File"), path.to_string_lossy().into_owned()));
    }
    fill(&fields, entries);
    if let Some(path) = app
        .downcast_ref::<gio::DesktopAppInfo>()
        .and_then(|desktop| desktop.filename())
    {
        let button = gtk::Button::with_label(&tr("Desktop File Properties…"));
        let window = window.downgrade();
        let dialog = dialog.downgrade();
        button.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.force_close();
            }
            if let Some(window) = window.upgrade() {
                show_file(&window, gio::File::for_path(&path));
            }
        });
        fields.append(&button);
    }
}
pub fn show_file(window: &adw::ApplicationWindow, file: gio::File) {
    let (dialog, fields) = dialog(window);
    fields.append(&gtk::Label::new(Some(&tr("Loading…"))));
    let cancel = gio::Cancellable::new();
    let token = cancel.clone();
    dialog.connect_closed(move |_| token.cancel());
    let weak = dialog.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let result = gio::CancellableFuture::new(
            file.query_info_future(
                "standard::*,time::*,unix::*,owner::*,access::*,trash::*",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            ),
            cancel.clone(),
        )
        .await;
        if weak.upgrade().is_none() || cancel.is_cancelled() {
            return;
        }
        match result {
            Ok(Ok(info)) => {
                fill(&fields, metadata(&file, &info));
                permissions::editor(&fields, &file, &info, &cancel);
                if info.file_type() == gio::FileType::Directory {
                    folder_usage(&fields, file, cancel);
                }
            }
            Ok(Err(error)) => fill(
                &fields,
                vec![(
                    tr("Unable to read properties"),
                    crate::errors::AppError::gio(&tr("Read properties"), &file, &error).message(),
                )],
            ),
            Err(_) => {}
        }
    });
}
fn metadata(file: &gio::File, info: &gio::FileInfo) -> Vec<(String, String)> {
    let mut entries = vec![
        (tr("Name"), info.display_name().to_string()),
        (tr("Location"), file.parse_name().to_string()),
        (
            tr("Type"),
            if info.has_attribute("standard::is-symlink") && info.is_symlink() {
                tr("Symbolic Link")
            } else if info.file_type() == gio::FileType::Directory {
                tr("Folder")
            } else {
                info.has_attribute("standard::content-type")
                    .then(|| info.content_type())
                    .flatten()
                    .map(|mime| gio::content_type_get_description(&mime).to_string())
                    .unwrap_or_else(|| tr("File"))
            },
        ),
    ];
    if info.file_type() != gio::FileType::Directory && info.has_attribute("standard::size") {
        entries.push((
            tr("Size"),
            format!(
                "{} ({} {})",
                glib::format_size(info.size().max(0) as u64),
                info.size().max(0),
                tr("bytes")
            ),
        ));
    }
    if info.file_type() == gio::FileType::Directory {
        entries.push((tr("Size"), tr("Calculating…")));
        entries.push((tr("Size on Disk"), tr("Calculating…")));
    }
    if info.file_type() != gio::FileType::Directory
        && info.has_attribute("standard::allocated-size")
    {
        entries.push((
            tr("Size on Disk"),
            glib::format_size(info.attribute_uint64("standard::allocated-size")).to_string(),
        ));
    }
    for (name, attribute) in [
        (tr("Modified"), "time::modified"),
        (tr("Created"), "time::created"),
        (tr("Accessed"), "time::access"),
    ] {
        if info.has_attribute(attribute) {
            if let Ok(date) =
                glib::DateTime::from_unix_local(info.attribute_uint64(attribute) as i64)
            {
                if let Ok(date) = date.format("%x %X %Z") {
                    entries.push((name, date.to_string()));
                }
            }
        }
    }
    for (name, attribute) in [
        (tr("Owner"), "owner::user"),
        (tr("Group"), "owner::group"),
        (tr("Deleted"), "trash::deletion-date"),
    ] {
        if let Some(value) = info.attribute_string(attribute) {
            entries.push((name, value.to_string()));
        }
    }
    if let Some(path) = info.attribute_file_path("standard::symlink-target") {
        entries.push((tr("Link Target"), path.to_string_lossy().into_owned()));
    }
    if let Some(path) = info.attribute_file_path("trash::orig-path") {
        entries.push((tr("Original Location"), path.to_string_lossy().into_owned()));
    }
    entries
}

fn field(fields: &gtk::Box, title: &str) -> gtk::Label {
    let mut row = fields.first_child();
    while let Some(widget) = row {
        if widget
            .first_child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
            .is_some_and(|label| label.text() == title)
        {
            return widget.last_child().unwrap().downcast().unwrap();
        }
        row = widget.next_sibling();
    }
    unreachable!("Properties metadata field")
}
fn folder_usage(fields: &gtk::Box, file: gio::File, dialog_cancel: gio::Cancellable) {
    let size = field(fields, &tr("Size"));
    let disk = field(fields, &tr("Size on Disk"));
    let stop = gtk::Button::with_label(&tr("Stop Calculating"));
    stop.set_halign(gtk::Align::Start);
    fields.insert_child_after(&stop, disk.parent().as_ref());
    let cancel = gio::Cancellable::new();
    let token = cancel.clone();
    let handler = dialog_cancel.connect_cancelled(move |_| token.cancel());
    let token = cancel.clone();
    stop.connect_clicked(move |_| token.cancel());
    glib::MainContext::default().spawn_local(async move {
        let mut last = usage::Usage::default();
        let result = usage::scan(&file, &cancel, |current| {
            last = current;
            size.set_label(&tr("Calculating… {size}").replace("{size}", &glib::format_size(current.bytes)));
            disk.set_label(&tr("Calculating… {size}").replace("{size}", &glib::format_size(current.allocated)));
        }).await;
        if let Some(handler) = handler { dialog_cancel.disconnect_cancelled(handler); }
        if dialog_cancel.is_cancelled() { return; }
        let stopped = result.as_ref().is_err_and(|error| error.matches(gio::IOErrorEnum::Cancelled));
        match result {
            Ok(current) => last = current,
            Err(error) if !stopped => {
                size.set_label(&crate::errors::AppError::gio(&tr("Calculate folder size"), &file, &error).message());
                disk.set_label(&tr("Unavailable"));
                stop.set_visible(false);
                return;
            },
            _ => {},
        }
        let counts = format!("{} · {}", count("{count} file", "{count} files", last.files.min(u32::MAX as u64) as u32), count("{count} folder", "{count} folders", last.folders.min(u32::MAX as u64) as u32));
        let partial = stopped || last.unreadable > 0 || last.skipped > 0;
        let prefix = if partial { tr("Partial result") } else { tr("Complete") };
        size.set_label(&format!("{prefix}: {} · {counts}", glib::format_size_full(last.bytes, glib::FormatSizeFlags::LONG_FORMAT)));
        disk.set_label(&if last.unknown_allocation > 0 {
            tr("At least {size}; some entries do not report allocated size.").replace("{size}", &glib::format_size(last.allocated))
        } else { format!("{prefix}: {}", glib::format_size(last.allocated)) });
        if partial {
            let label = gtk::Label::builder().label(tr("Stopped or incomplete: {unreadable} unreadable entries; {skipped} mount boundaries or depth limits.").replace("{unreadable}", &last.unreadable.to_string()).replace("{skipped}", &last.skipped.to_string())).wrap(true).xalign(0.0).max_width_chars(65).build();
            if let Some(parent) = stop.parent().and_downcast::<gtk::Box>() { parent.append(&label); }
        }
        stop.set_visible(false);
    });
}
