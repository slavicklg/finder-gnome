// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use super::*;
use std::{cell::Cell, rc::Rc};
async fn cancellable<T>(
    future: impl std::future::Future<Output = Result<T, glib::Error>>,
    cancel: &gio::Cancellable,
) -> Result<T, glib::Error> {
    gio::CancellableFuture::new(future, cancel.clone())
        .await
        .unwrap_or_else(|_| Err(glib::Error::new(gio::IOErrorEnum::Cancelled, "Cancelled")))
}
pub(super) async fn apply(
    file: &gio::File,
    bits: u32,
    cancel: &gio::Cancellable,
) -> Result<u32, glib::Error> {
    let info = cancellable(
        file.query_info_future(
            "unix::mode,standard::type,standard::is-symlink",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        ),
        cancel,
    )
    .await?;
    if crate::files::is_trash(file)
        || !info.has_attribute("unix::mode")
        || (info.has_attribute("standard::is-symlink") && info.is_symlink())
    {
        return Err(glib::Error::new(
            gio::IOErrorEnum::NotSupported,
            &tr("Permissions cannot be changed for this item."),
        ));
    }
    let attributes = gio::FileInfo::new();
    attributes.set_attribute_uint32(
        "unix::mode",
        (info.attribute_uint32("unix::mode") & 0o7000) | (bits & 0o777),
    );
    cancellable(
        file.set_attributes_future(
            &attributes,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        ),
        cancel,
    )
    .await?;
    let actual = cancellable(
        file.query_info_future(
            "unix::mode",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        ),
        cancel,
    )
    .await?;
    Ok(actual.attribute_uint32("unix::mode") & 0o7777)
}
pub(super) fn editor(
    fields: &gtk::Box,
    file: &gio::File,
    info: &gio::FileInfo,
    cancel: &gio::Cancellable,
) {
    let heading = gtk::Label::builder()
        .label(tr("Permissions"))
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    fields.append(&heading);
    if !info.has_attribute("unix::mode")
        || crate::files::is_trash(file)
        || (info.has_attribute("standard::is-symlink") && info.is_symlink())
    {
        fields.append(
            &gtk::Label::builder()
                .label(tr(
                    "Editing permissions is unavailable for this item or backend.",
                ))
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
        return;
    }
    let mode = info.attribute_uint32("unix::mode") & 0o7777;
    let status = gtk::Label::builder()
        .label(format!("{mode:04o}"))
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .max_width_chars(65)
        .build();
    fields.append(&status);
    fields.append(
        &gtk::Label::builder()
            .label(tr("Changes apply to this item."))
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build(),
    );
    let grid = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(6)
        .build();
    for (column, title) in [
        (1, tr("Read")),
        (2, tr("Write")),
        (
            3,
            if info.file_type() == gio::FileType::Directory {
                tr("Access")
            } else {
                tr("Execute")
            },
        ),
    ] {
        grid.attach(&gtk::Label::new(Some(&title)), column, 0, 1, 1);
    }
    let button = gtk::Button::with_label(&tr("Apply Permissions"));
    button.set_sensitive(false);
    button.set_halign(gtk::Align::Start);
    button.set_margin_bottom(16);
    let busy = Rc::new(Cell::new(false));
    let mut checks = Vec::new();
    for (row, title, shift) in [
        (1, tr("Owner"), 6),
        (2, tr("Group"), 3),
        (3, tr("Others"), 0),
    ] {
        grid.attach(
            &gtk::Label::builder()
                .label(title.clone())
                .xalign(0.0)
                .build(),
            0,
            row,
            1,
            1,
        );
        for (column, bit, description) in [
            (1, 4, tr("Read")),
            (2, 2, tr("Write")),
            (3, 1, tr("Execute / access folder")),
        ] {
            let check = gtk::CheckButton::new();
            check.set_tooltip_text(Some(&format!("{title}: {description}")));
            let mask = bit << shift;
            check.set_active(mode & mask != 0);
            check.set_widget_name(&format!("permission-{mask:o}"));
            grid.attach(&check, column, row, 1, 1);
            let weak = button.downgrade();
            let busy = busy.clone();
            check.connect_toggled(move |_| {
                if !busy.get() {
                    if let Some(button) = weak.upgrade() {
                        button.set_sensitive(true);
                    }
                }
            });
            checks.push((check, mask));
        }
    }
    fields.append(&grid);
    fields.append(&button);
    let file = file.clone();
    let cancel = cancel.clone();
    let grid = grid.downgrade();
    button.connect_clicked(move |button| {
        let bits = checks
            .iter()
            .filter(|(check, _)| check.is_active())
            .fold(0, |mode, (_, bit)| mode | bit);
        busy.set(true);
        button.set_sensitive(false);
        if let Some(grid) = grid.upgrade() {
            grid.set_sensitive(false);
        }
        status.set_label(&tr("Saving permissions…"));
        let (file, cancel, grid, button, status, checks, busy) = (
            file.clone(),
            cancel.clone(),
            grid.clone(),
            button.downgrade(),
            status.clone(),
            checks.clone(),
            busy.clone(),
        );
        glib::MainContext::default().spawn_local(async move {
            let result = apply(&file, bits, &cancel).await;
            if cancel.is_cancelled() {
                return;
            }
            match result {
                Ok(mode) => {
                    for (check, mask) in &checks {
                        check.set_active(mode & mask != 0);
                    }
                    status.set_label(
                        &tr("Permissions updated: {mode}")
                            .replace("{mode}", &format!("{mode:04o}")),
                    );
                }
                Err(error) => {
                    status.set_label(
                        &crate::errors::AppError::gio(&tr("Change permissions"), &file, &error)
                            .message(),
                    );
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(true);
                    }
                }
            }
            busy.set(false);
            if let Some(grid) = grid.upgrade() {
                grid.set_sensitive(true);
            }
        });
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_only_requested_item_preserves_special_bits_and_refuses_links() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let child = folder.join("child");
        std::fs::write(&child, "data").unwrap();
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o1750)).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&child, &link).unwrap();
        glib::MainContext::new().block_on(async {
            let cancel = gio::Cancellable::new();
            assert_eq!(
                apply(&gio::File::for_path(&folder), 0o700, &cancel)
                    .await
                    .unwrap(),
                0o1700
            );
            assert_eq!(std::fs::metadata(&child).unwrap().mode() & 0o777, 0o640);
            assert!(
                apply(&gio::File::for_path(&link), 0o777, &cancel)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::metadata(&child).unwrap().mode() & 0o777, 0o640);
        });
    }
}
