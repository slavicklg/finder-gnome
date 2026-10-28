// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Owned inverse steps produced only after successful filesystem mutations.
use crate::i18n::tr;
use gio::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp(Vec<(String, String)>);

pub fn stamp(file: &gio::File, recursive: bool) -> Result<Stamp, String> {
    fn visit(
        file: &gio::File,
        relative: String,
        recursive: bool,
        out: &mut Vec<(String, String)>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 256 {
            return Err(tr("The folder tree is too deep."));
        }
        let info = file.query_info("standard::type,standard::size,standard::symlink-target,id::file,etag::value,time::modified,time::modified-usec", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, gio::Cancellable::NONE).map_err(|e|e.to_string())?;
        // Directory mtime changes when our own child steps are undone. Empty-directory
        // removal below is deliberately non-recursive and protects new children.
        let value = if info.file_type() == gio::FileType::Directory {
            format!("dir:{:?}", info.attribute_string("id::file"))
        } else {
            format!(
                "{:?}:{:?}:{}:{}:{}:{:?}:{:?}",
                info.file_type(),
                info.attribute_string("id::file"),
                info.size(),
                info.attribute_uint64("time::modified"),
                info.attribute_uint32("time::modified-usec"),
                info.attribute_string("etag::value"),
                info.attribute_file_path("standard::symlink-target")
            )
        };
        out.push((relative.clone(), value));
        if recursive && info.file_type() == gio::FileType::Directory {
            let children = file
                .enumerate_children(
                    "standard::name",
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    gio::Cancellable::NONE,
                )
                .map_err(|e| e.to_string())?;
            while let Some(child) = children
                .next_file(gio::Cancellable::NONE)
                .map_err(|e| e.to_string())?
            {
                visit(
                    &file.child(child.name()),
                    format!("{relative}/{:?}", child.name()),
                    true,
                    out,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }
    let mut result = Vec::new();
    visit(file, String::new(), recursive, &mut result, 0)?;
    result.sort();
    Ok(Stamp(result))
}

pub fn verify(file: &gio::File, expected: &Stamp, recursive: bool) -> Result<(), String> {
    if &stamp(file, recursive)? != expected {
        return Err(tr(
            "The item has changed outside this operation. Undo/Redo was stopped.",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Step {
    Move {
        source: gio::File,
        target: gio::File,
        expected: Stamp,
    },
    Remove {
        file: gio::File,
        expected: Stamp,
        redo: Box<Step>,
    },
    Copy {
        source: gio::File,
        target: gio::File,
        expected: Stamp,
    },
    CreateDirectory(gio::File),
    Trash {
        file: gio::File,
        expected: Stamp,
    },
}
impl Step {
    pub fn locations(&self) -> Vec<gio::File> {
        match self {
            Self::Move { source, target, .. } | Self::Copy { source, target, .. } => vec![
                source.clone(),
                target.parent().unwrap_or_else(|| target.clone()),
            ],
            Self::Remove { file, .. } | Self::Trash { file, .. } => vec![file.clone()],
            Self::CreateDirectory(file) => vec![file.parent().unwrap_or_else(|| file.clone())],
        }
    }
}
