// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use gio::prelude::*;
use std::collections::HashSet;
const ATTRS: &str = "standard::name,standard::type,standard::is-symlink,standard::size,standard::allocated-size,id::file,id::filesystem";
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Usage {
    pub bytes: u64,
    pub allocated: u64,
    pub files: u64,
    pub folders: u64,
    pub unreadable: u64,
    pub unknown_allocation: u64,
    pub skipped: u64,
}
async fn cancellable<T>(
    future: impl std::future::Future<Output = Result<T, glib::Error>>,
    cancel: &gio::Cancellable,
) -> Result<T, glib::Error> {
    gio::CancellableFuture::new(future, cancel.clone())
        .await
        .unwrap_or_else(|_| Err(glib::Error::new(gio::IOErrorEnum::Cancelled, "Cancelled")))
}
fn identity(file: &gio::File, info: &gio::FileInfo) -> String {
    match (
        info.attribute_string("id::filesystem"),
        info.attribute_string("id::file"),
    ) {
        (Some(fs), Some(id)) => format!("{fs}:{id}"),
        _ => file.uri().to_string(),
    }
}
fn tally(info: &gio::FileInfo, usage: &mut Usage) {
    if info.file_type() == gio::FileType::Directory {
        usage.folders += 1;
    } else {
        usage.files += 1;
        if info.has_attribute("standard::size") {
            usage.bytes = usage.bytes.saturating_add(info.size().max(0) as u64);
        }
    }
    if info.has_attribute("standard::allocated-size") {
        usage.allocated = usage
            .allocated
            .saturating_add(info.attribute_uint64("standard::allocated-size"));
    } else {
        usage.unknown_allocation += 1;
    }
}
pub(super) async fn scan(
    root: &gio::File,
    cancel: &gio::Cancellable,
    mut progress: impl FnMut(Usage),
) -> Result<Usage, glib::Error> {
    let info = cancellable(
        root.query_info_future(
            ATTRS,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        ),
        cancel,
    )
    .await?;
    let filesystem = info.attribute_string("id::filesystem");
    let mut usage = Usage::default();
    tally(&info, &mut usage);
    let mut seen = HashSet::from([identity(root, &info)]);
    let mut pending = if info.file_type() == gio::FileType::Directory {
        vec![(root.clone(), 0)]
    } else {
        vec![]
    };
    while let Some((directory, depth)) = pending.pop() {
        let enumerator = match cancellable(
            directory.enumerate_children_future(
                ATTRS,
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            ),
            cancel,
        )
        .await
        {
            Ok(enumerator) => enumerator,
            Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => return Err(error),
            Err(_) => {
                usage.unreadable += 1;
                continue;
            }
        };
        loop {
            let infos = match cancellable(
                enumerator.next_files_future(128, glib::Priority::DEFAULT),
                cancel,
            )
            .await
            {
                Ok(infos) => infos,
                Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => return Err(error),
                Err(_) => {
                    usage.unreadable += 1;
                    break;
                }
            };
            if infos.is_empty() {
                break;
            }
            for info in infos {
                let file = directory.child(info.name());
                let directory = info.file_type() == gio::FileType::Directory
                    && !(info.has_attribute("standard::is-symlink") && info.is_symlink());
                if directory
                    && root.is_native()
                    && filesystem.is_some()
                    && info
                        .attribute_string("id::filesystem")
                        .is_some_and(|fs| Some(fs) != filesystem)
                {
                    usage.skipped += 1;
                    continue;
                }
                if !seen.insert(identity(&file, &info)) {
                    continue;
                }
                tally(&info, &mut usage);
                if directory {
                    if depth >= 256 {
                        usage.skipped += 1;
                    } else {
                        pending.push((file, depth + 1));
                    }
                }
            }
            progress(usage);
        }
        let _ = cancellable(enumerator.close_future(glib::Priority::DEFAULT), cancel).await;
    }
    if cancel.is_cancelled() {
        return Err(glib::Error::new(gio::IOErrorEnum::Cancelled, "Cancelled"));
    }
    Ok(usage)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_folders_removed_during_traversal_as_incomplete() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("removed");
        std::fs::create_dir(&child).unwrap();
        let mut removed = false;
        let usage = glib::MainContext::new()
            .block_on(scan(
                &gio::File::for_path(root.path()),
                &gio::Cancellable::new(),
                |_| {
                    if !removed {
                        std::fs::remove_dir(&child).unwrap();
                        removed = true;
                    }
                },
            ))
            .unwrap();
        assert_eq!(usage.unreadable, 1);
    }
    #[test]
    fn counts_sparse_files_once_without_following_links_and_can_cancel() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("outside"), vec![0; 100_000]).unwrap();
        let file = std::fs::File::create(root.path().join("sparse")).unwrap();
        file.set_len(1_048_576).unwrap();
        std::fs::hard_link(root.path().join("sparse"), root.path().join("hardlink")).unwrap();
        std::os::unix::fs::symlink(external.path(), root.path().join("link")).unwrap();
        let link = std::fs::symlink_metadata(root.path().join("link")).unwrap();
        let cancel = gio::Cancellable::new();
        let context = glib::MainContext::new();
        context.block_on(async {
            let result = scan(&gio::File::for_path(root.path()), &cancel, |_| {})
                .await
                .unwrap();
            assert_eq!(result.bytes, 1_048_576 + link.size());
            assert_eq!(result.folders, 1);
            assert_eq!(result.files, 2);
            assert!(result.allocated < result.bytes);
            cancel.cancel();
            assert!(
                scan(&gio::File::for_path(root.path()), &cancel, |_| {})
                    .await
                    .unwrap_err()
                    .matches(gio::IOErrorEnum::Cancelled)
            );
        });
    }
}
