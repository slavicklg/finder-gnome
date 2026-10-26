// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use gio::prelude::*;
use std::cmp::Ordering;

use crate::i18n::tr;

pub const ATTRIBUTES: &str = "standard::*,time::modified,time::modified-usec,access::*,id::file,id::filesystem,time::created,time::created-usec,trash::*";

#[derive(Clone, Debug)]
pub struct FileItem {
    pub file: gio::File,
    pub name: String,
    pub is_dir: bool,
    pub hidden: bool,
    pub size: i64,
    pub modified: i64,
    pub modified_usec: u32,
    pub icon: gio::Icon,
    pub kind: String,
    pub mime: String,
    pub executable: bool,
    pub can_rename: Option<bool>,
    pub can_delete: Option<bool>,
    pub can_trash: Option<bool>,
    pub key: glib::FilenameCollationKey,
    pub identity: Option<String>,
    pub tags: Vec<(String, String)>,
    pub location_hint: String,
}

impl FileItem {
    pub fn from_info(parent: &gio::File, info: &gio::FileInfo) -> Self {
        let name = info.display_name().to_string();
        let is_dir = info.has_attribute("standard::type")
            && matches!(
                info.file_type(),
                gio::FileType::Directory | gio::FileType::Mountable | gio::FileType::Shortcut
            );
        let content_type = info
            .has_attribute("standard::content-type")
            .then(|| info.content_type())
            .flatten()
            .filter(|t| t.as_str() != "application/octet-stream")
            .or_else(|| Some(gio::content_type_guess(Some(info.name()), None::<&[u8]>).0));
        let target = info
            .attribute_string("standard::target-uri")
            .map(|uri| gio::File::for_uri(&uri))
            .filter(|file| !is_trash(parent) && supported(file));
        Self {
            file: target.unwrap_or_else(|| parent.child(info.name())),
            identity: info
                .attribute_string("id::filesystem")
                .zip(info.attribute_string("id::file"))
                .map(|(fs, id)| {
                    format!(
                        "{fs}:{id}:{}:{}",
                        info.attribute_uint64("time::created"),
                        info.attribute_uint32("time::created-usec")
                    )
                }),
            tags: Vec::new(),
            location_hint: if is_trash(parent) {
                let original = info
                    .attribute_file_path("trash::orig-path")
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let deleted = info
                    .attribute_string("trash::deletion-date")
                    .unwrap_or_default();
                tr("Original: {path} · Deleted: {date}")
                    .replace("{path}", &original)
                    .replace("{date}", &deleted)
            } else {
                String::new()
            },
            key: glib::FilenameCollationKey::from(&name),
            name,
            is_dir,
            hidden: (info.has_attribute("standard::is-hidden") && info.is_hidden())
                || (info.has_attribute("standard::is-backup") && info.is_backup()),
            size: if info.has_attribute("standard::size") {
                info.size()
            } else {
                0
            },
            modified: info.attribute_uint64("time::modified") as i64,
            modified_usec: info.attribute_uint32("time::modified-usec"),
            icon: info
                .has_attribute("standard::icon")
                .then(|| info.icon())
                .flatten()
                .unwrap_or_else(|| {
                    gio::ThemedIcon::new(if is_dir { "folder" } else { "text-x-generic" }).upcast()
                }),
            kind: if is_dir {
                tr("Folder")
            } else {
                content_type
                    .as_ref()
                    .map(|t| gio::content_type_get_description(t).to_string())
                    .unwrap_or_else(|| tr("File"))
            },
            mime: content_type
                .as_ref()
                .map(|t| {
                    gio::content_type_get_mime_type(t)
                        .unwrap_or_else(|| t.clone())
                        .to_string()
                })
                .unwrap_or_else(|| "application/octet-stream".into()),
            can_rename: info
                .has_attribute("access::can-rename")
                .then(|| info.boolean("access::can-rename")),
            can_delete: info
                .has_attribute("access::can-delete")
                .then(|| info.boolean("access::can-delete")),
            can_trash: info
                .has_attribute("access::can-trash")
                .then(|| info.boolean("access::can-trash")),
            executable: content_type.as_ref().is_some_and(|t| {
                gio::content_type_can_be_executable(t) && info.boolean("access::can-execute")
            }),
        }
    }

    pub fn size_text(&self) -> String {
        if self.is_dir {
            "—".into()
        } else {
            glib::format_size(self.size.max(0) as u64).to_string()
        }
    }

    pub fn date_text(&self) -> String {
        glib::DateTime::from_unix_local(self.modified)
            .and_then(|d| d.format("%x %R"))
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "—".into())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    #[default]
    Icons,
    List,
    Columns,
    Gallery,
}

pub fn compare_field(a: &FileItem, b: &FileItem, field: usize) -> Ordering {
    match field {
        1 => a.size.cmp(&b.size),
        2 => a.kind.cmp(&b.kind),
        3 => a.modified.cmp(&b.modified),
        _ => a.key.cmp(&b.key),
    }
    .then_with(|| a.key.cmp(&b.key))
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        Err(tr(
            "Enter a name without slashes. The names “.” and “..” are not allowed.",
        ))
    } else {
        Ok(())
    }
}

pub fn is_trash(file: &gio::File) -> bool {
    file.uri_scheme().as_deref() == Some("trash")
}
pub fn browsable(file: &gio::File) -> bool {
    supported(file) || (is_trash(file) && file.equal(&gio::File::for_uri("trash:///")))
}
pub fn supported(file: &gio::File) -> bool {
    matches!(file.uri_scheme().as_deref(), Some("file" | "smb" | "sftp"))
        && safe_uri(&file.uri()).is_ok()
}
pub fn is_root(file: &gio::File) -> bool {
    if file.parent().is_none() {
        return true;
    }
    if file.uri_scheme().as_deref() == Some("smb") {
        if let Ok(uri) = glib::Uri::parse(&file.uri(), glib::UriFlags::NONE) {
            return uri.path().split('/').filter(|p| !p.is_empty()).count() <= 1;
        }
    }
    false
}
pub fn safe_uri(text: &str) -> Result<(), String> {
    let uri = glib::Uri::parse(text, glib::UriFlags::HAS_PASSWORD)
        .map_err(|_| tr("Enter a valid folder URI."))?;
    if text == "trash:///" {
        return Ok(());
    }
    if !matches!(uri.scheme().as_str(), "file" | "smb" | "sftp") {
        return Err(tr("Use a local path, smb:// or sftp:// address."));
    }
    if uri.password().is_some() {
        return Err(tr(
            "Do not include passwords in addresses. Use the connection dialog.",
        ));
    }
    if uri.scheme() != "file" && uri.host().is_none_or(|h| h.is_empty()) {
        return Err(tr("Enter a server hostname."));
    }
    Ok(())
}
pub fn parse_location(text: &str) -> Result<gio::File, String> {
    if !text.starts_with('/') && !text.starts_with("~/") && text.contains("://") {
        safe_uri(text)?;
        return Ok(gio::File::for_uri(text));
    }
    let path = if text == "~" {
        glib::home_dir()
    } else if let Some(tail) = text.strip_prefix("~/") {
        glib::home_dir().join(tail)
    } else {
        std::path::PathBuf::from(text)
    };
    if !path.is_absolute() {
        return Err(tr("Enter an absolute folder path."));
    }
    Ok(gio::File::for_path(path))
}

pub fn clipboard_payload(files: &[gio::File], cut: bool) -> (String, String) {
    let uris: Vec<_> = files.iter().map(|f| f.uri().to_string()).collect();
    (
        format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n")),
        format!("{}\r\n", uris.join("\r\n")),
    )
}

pub fn parse_clipboard(data: &str, gnome: bool) -> Result<(Vec<gio::File>, bool), String> {
    let mut lines = data.lines();
    let cut = if gnome {
        match lines.next() {
            Some("cut") => true,
            Some("copy") => false,
            _ => return Err(tr("The clipboard does not contain files.")),
        }
    } else {
        false
    };
    let mut files = Vec::new();
    for line in lines {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let file = gio::File::for_uri(line);
        safe_uri(line)?;
        files.push(file);
    }
    if files.is_empty() {
        return Err(tr("The clipboard does not contain files."));
    }
    Ok((files, cut))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_names_without_trimming_user_names() {
        for name in ["", ".", "..", "a/b", "a\0b"] {
            assert!(validate_name(name).is_err());
        }
        for name in [" hello ", "résumé.txt", "文件", "a\\b"] {
            assert!(validate_name(name).is_ok());
        }
    }
    #[test]
    fn clipboard_roundtrip_and_supported_protocols() {
        let files = vec![
            gio::File::for_path("/tmp/a b"),
            gio::File::for_path("/tmp/文件"),
        ];
        let (gnome, uri) = clipboard_payload(&files, true);
        let (parsed, cut) = parse_clipboard(&gnome, true).unwrap();
        assert!(cut);
        assert!(parsed.iter().zip(&files).all(|(a, b)| a.equal(b)));
        let (parsed, cut) = parse_clipboard(&uri, false).unwrap();
        assert!(!cut);
        assert!(parsed.iter().zip(&files).all(|(a, b)| a.equal(b)));
        assert!(parse_clipboard("sftp://host/a", false).is_ok());
        assert!(parse_clipboard("smb://host/share/a", false).is_ok());
        assert!(parse_clipboard("sftp://user:secret@host/a", false).is_err());
    }
    #[test]
    fn location_supports_safe_protocols_and_absolute_paths() {
        assert!(parse_location("relative").is_err());
        assert!(parse_location("sftp://host/folder").is_ok());
        assert!(parse_location("smb://host/share").is_ok());
        assert!(parse_location("sftp://user:secret@host/folder").is_err());
        assert!(parse_location("https://host/folder").is_err());
        assert_eq!(
            parse_location("~/hello").unwrap().path().unwrap(),
            glib::home_dir().join("hello")
        );
    }
    #[test]
    fn filename_sort_is_natural() {
        assert!(
            glib::FilenameCollationKey::from("file2") < glib::FilenameCollationKey::from("file10")
        );
    }
}
