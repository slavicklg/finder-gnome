// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use crate::{files, i18n::tr};
use gio::prelude::*;
use gtk::{gdk, glib};

pub fn provider(files: &[gio::File]) -> gdk::ContentProvider {
    let (_, uris) = files::clipboard_payload(files, false);
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&gdk::FileList::from_array(files).to_value()),
        gdk::ContentProvider::for_bytes(
            "text/uri-list",
            &glib::Bytes::from_owned(uris.into_bytes()),
        ),
    ])
}

pub async fn read_files(drop: &gdk::Drop) -> Result<Vec<gio::File>, String> {
    if drop.formats().contains_type(gdk::FileList::static_type()) {
        let value = drop
            .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
            .await
            .map_err(|e| e.to_string())?;
        let files = value
            .get::<gdk::FileList>()
            .map_err(|e| e.to_string())?
            .files();
        validate(&files)?;
        return Ok(files);
    }
    let (stream, _) = drop
        .read_future(&["text/uri-list"], glib::Priority::DEFAULT)
        .await
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    loop {
        let part = stream
            .read_bytes_future(8192, glib::Priority::DEFAULT)
            .await
            .map_err(|e| e.to_string())?;
        if part.is_empty() {
            break;
        }
        if bytes.len() + part.len() > 8 * 1024 * 1024 {
            return Err(tr("The dropped file list is too large."));
        }
        bytes.extend_from_slice(&part);
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    files::parse_clipboard(text, false).map(|(files, _)| files)
}
pub fn validate(files: &[gio::File]) -> Result<(), String> {
    if files.is_empty() || files.iter().any(|f| !files::supported(f)) {
        return Err(tr("Only local, SMB and SFTP files can be dropped here."));
    }
    Ok(())
}

pub fn is_source_folder(files: &[gio::File], destination: &gio::File) -> bool {
    files.iter().any(|file| {
        file.parent()
            .is_some_and(|parent| parent.equal(destination))
    })
}

pub fn trash_action(allowed: gdk::DragAction, modifiers: gdk::ModifierType) -> gdk::DragAction {
    if allowed.contains(gdk::DragAction::MOVE)
        && !modifiers.contains(gdk::ModifierType::CONTROL_MASK)
    {
        gdk::DragAction::MOVE
    } else {
        gdk::DragAction::empty()
    }
}

pub fn action(
    allowed: gdk::DragAction,
    modifiers: gdk::ModifierType,
    same_filesystem: bool,
) -> gdk::DragAction {
    let explicit =
        modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
    let desired = if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
        gdk::DragAction::COPY
    } else if modifiers.contains(gdk::ModifierType::SHIFT_MASK) || same_filesystem {
        gdk::DragAction::MOVE
    } else {
        gdk::DragAction::COPY
    };
    if allowed.contains(desired) {
        desired
    } else if explicit {
        gdk::DragAction::empty()
    } else if allowed.contains(gdk::DragAction::COPY) {
        gdk::DragAction::COPY
    } else if allowed.contains(gdk::DragAction::MOVE) {
        gdk::DragAction::MOVE
    } else {
        gdk::DragAction::empty()
    }
}

pub async fn same_filesystem(files: &[gio::File], destination: &gio::File) -> bool {
    let Ok(target) = destination
        .query_info_future(
            "id::filesystem",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
    else {
        return false;
    };
    let Some(id) = target.attribute_string("id::filesystem") else {
        return false;
    };
    for file in files {
        let Ok(info) = file
            .query_info_future(
                "id::filesystem",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            )
            .await
        else {
            return false;
        };
        if info.attribute_string("id::filesystem").as_ref() != Some(&id) {
            return false;
        }
    }
    true
}

pub struct Completion {
    drop: Option<gdk::Drop>,
    action: gdk::DragAction,
    on_finish: Option<Box<dyn FnOnce()>>,
}
impl Completion {
    pub fn new(
        drop: gdk::Drop,
        action: gdk::DragAction,
        on_finish: Option<Box<dyn FnOnce()>>,
    ) -> Self {
        Self {
            drop: Some(drop),
            action,
            on_finish,
        }
    }
    pub fn finish(&mut self, success: bool) {
        if let Some(drop) = self.drop.take() {
            drop.finish(if success {
                self.action
            } else {
                gdk::DragAction::empty()
            });
            if let Some(done) = self.on_finish.take() {
                done();
            }
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.finish(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trash_drop_requires_move_and_rejects_forced_copy() {
        let allowed = gdk::DragAction::COPY | gdk::DragAction::MOVE;
        assert_eq!(
            trash_action(allowed, gdk::ModifierType::empty()),
            gdk::DragAction::MOVE
        );
        assert_eq!(
            trash_action(allowed, gdk::ModifierType::SHIFT_MASK),
            gdk::DragAction::MOVE
        );
        assert!(trash_action(allowed, gdk::ModifierType::CONTROL_MASK).is_empty());
        assert!(trash_action(gdk::DragAction::COPY, gdk::ModifierType::empty()).is_empty());
    }
    #[test]
    fn source_folder_is_not_a_drop_destination() {
        let folder = gio::File::for_path("/tmp/finder-source");
        let other = gio::File::for_path("/tmp/finder-other");
        assert!(is_source_folder(&[folder.child("file.txt")], &folder));
        assert!(is_source_folder(&[folder.child("subfolder")], &folder));
        assert!(is_source_folder(
            &[other.child("other.txt"), folder.child("file.txt")],
            &folder,
        ));
        assert!(!is_source_folder(&[other.child("file.txt")], &folder));
        assert!(!is_source_folder(
            &[folder.child("file.txt")],
            &folder.child("subfolder")
        ));
    }
    #[test]
    fn copy_move_negotiation_respects_modifiers_and_source() {
        let both = gdk::DragAction::COPY | gdk::DragAction::MOVE;
        assert_eq!(
            action(both, gdk::ModifierType::empty(), true),
            gdk::DragAction::MOVE
        );
        assert_eq!(
            action(both, gdk::ModifierType::empty(), false),
            gdk::DragAction::COPY
        );
        assert_eq!(
            action(both, gdk::ModifierType::CONTROL_MASK, true),
            gdk::DragAction::COPY
        );
        assert_eq!(
            action(both, gdk::ModifierType::SHIFT_MASK, false),
            gdk::DragAction::MOVE
        );
        assert!(action(gdk::DragAction::COPY, gdk::ModifierType::SHIFT_MASK, true).is_empty());
        assert!(validate(&[gio::File::for_uri("sftp://example/file")]).is_ok());
        assert!(validate(&[gio::File::for_uri("https://example/file")]).is_err());
    }
}
