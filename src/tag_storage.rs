// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! File-backed tag assignments; SQLite is the index and fallback/pending journal.
use crate::i18n::tr;
use gio::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{ffi::CString, os::unix::ffi::OsStrExt};

const ATTRIBUTE: &std::ffi::CStr = c"user.finder-gnome.tags";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Label {
    uuid: String,
    name: String,
    color: String,
}
#[derive(Serialize, Deserialize)]
struct Payload {
    version: u32,
    tags: Vec<Label>,
}

fn db_error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(message.into())))
}
fn path(uri: &str) -> Option<CString> {
    let file = gio::File::for_uri(uri);
    let path = file.path()?;
    if std::fs::symlink_metadata(&path)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    CString::new(path.as_os_str().as_bytes()).ok()
}
enum Read {
    Unsupported,
    Missing,
    Present(Vec<Label>),
}
fn read(uri: &str) -> rusqlite::Result<Read> {
    let Some(path) = path(uri) else {
        return Ok(Read::Unsupported);
    };
    // lgetxattr/lsetxattr never follow a symlink swapped in by another process.
    let size =
        unsafe { libc::lgetxattr(path.as_ptr(), ATTRIBUTE.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENODATA) => Ok(Read::Missing),
            Some(libc::ENOTSUP | libc::EACCES | libc::EPERM | libc::ENOENT) => {
                Ok(Read::Unsupported)
            }
            _ => Err(db_error(error.to_string())),
        };
    }
    if size > 65536 {
        return Err(db_error(tr("Tag attribute is too large")));
    }
    let mut bytes = vec![0; size as usize];
    let size = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            ATTRIBUTE.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if size < 0 {
        return Err(db_error(std::io::Error::last_os_error().to_string()));
    }
    bytes.truncate(size as usize);
    let payload: Payload = serde_json::from_slice(&bytes).map_err(|e| db_error(e.to_string()))?;
    if payload.version != 1 {
        return Err(db_error(tr("Unsupported tag attribute version")));
    }
    if payload.tags.iter().any(|tag| {
        !glib::uuid_string_is_valid(&tag.uuid)
            || tag.name.trim().is_empty()
            || !valid_color(&tag.color)
    }) {
        return Err(db_error(tr("Invalid tag attribute")));
    }
    Ok(Read::Present(payload.tags))
}
pub fn valid_color(color: &str) -> bool {
    matches!(
        color,
        "#e01b24" | "#ff7800" | "#f6d32d" | "#33d17a" | "#3584e4" | "#9141ac" | "#77767b"
    )
}
pub fn file_labels(file: &gio::File) -> Result<Option<Vec<Label>>, String> {
    match read(file.uri().as_str()).map_err(|e| e.to_string())? {
        Read::Present(tags) => Ok(Some(tags)),
        _ => Ok(None),
    }
}
pub fn import_file(db: &Connection, uri: &str, tags: Vec<Label>) -> rusqlite::Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO objects(uri,storage) VALUES(?1,'pending')",
        [uri],
    )?;
    let object: i64 = db.query_row("SELECT id FROM objects WHERE uri=?1", [uri], |r| r.get(0))?;
    db.execute("DELETE FROM links WHERE object=?1", [object])?;
    for label in tags {
        let tag = import(db, &label)?;
        db.execute("INSERT OR IGNORE INTO links(object,tag) SELECT ?1,id FROM tags WHERE id=?2 AND deleted=0",params![object,tag])?;
    }
    pending(db, uri)?;
    Ok(())
}
pub fn labels(db: &Connection, uri: &str) -> rusqlite::Result<Vec<Label>> {
    db.prepare("SELECT t.uuid,t.name,t.color FROM tags t JOIN links l ON l.tag=t.id JOIN objects o ON o.id=l.object WHERE o.uri=?1 AND t.deleted=0 ORDER BY t.id")?
        .query_map([uri],|r| Ok(Label{uuid:r.get(0)?,name:r.get(1)?,color:r.get(2)?}))?.collect()
}
/// Include discovered hard links so subsequent flushes cannot restore a stale
/// assignment from another URI of the same inode. Symlinks stay independent.
pub fn linked_uris(db: &Connection, uri: &str) -> rusqlite::Result<Vec<String>> {
    use std::os::unix::fs::MetadataExt;
    let mut result = vec![uri.to_string()];
    let Some(file) = gio::File::for_uri(uri).path() else {
        return Ok(result);
    };
    let Ok(meta) = std::fs::symlink_metadata(file) else {
        return Ok(result);
    };
    if !meta.is_file() {
        return Ok(result);
    }
    let uris: Vec<String> = db
        .prepare("SELECT uri FROM objects WHERE uri<>?1")?
        .query_map([uri], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for other in uris {
        if let Some(path) = gio::File::for_uri(&other).path() {
            if std::fs::symlink_metadata(path)
                .is_ok_and(|m| m.is_file() && m.dev() == meta.dev() && m.ino() == meta.ino())
            {
                result.push(other);
            }
        }
    }
    Ok(result)
}
pub fn pending(db: &Connection, uri: &str) -> rusqlite::Result<()> {
    db.execute("UPDATE objects SET storage='pending' WHERE uri=?1", [uri])?;
    Ok(())
}
pub fn flush(db: &Connection, uri: &str) -> rusqlite::Result<bool> {
    // Validate before writing: malformed/foreign-version attributes are never destroyed.
    let current = read(uri)?;
    if matches!(current, Read::Unsupported) {
        return Ok(false);
    }
    let desired = labels(db, uri)?;
    if matches!(current,Read::Present(ref tags) if tags==&desired) {
        db.execute("UPDATE objects SET storage='xattr' WHERE uri=?1", [uri])?;
        return Ok(true);
    }
    let Some(path) = path(uri) else {
        return Ok(false);
    };
    let bytes = serde_json::to_vec(&Payload {
        version: 1,
        tags: desired,
    })
    .map_err(|e| db_error(e.to_string()))?;
    if bytes.len() > 65536 {
        return Ok(false);
    }
    let written = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            ATTRIBUTE.as_ptr(),
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
        )
    };
    if written != 0 {
        return Ok(false);
    }
    db.execute("UPDATE objects SET storage='xattr' WHERE uri=?1", [uri])?;
    Ok(true)
}
fn import(db: &Connection, label: &Label) -> rusqlite::Result<i64> {
    if let Some(id) = db.query_row("SELECT id FROM tags WHERE uuid=?1 UNION ALL SELECT tag FROM tag_aliases WHERE uuid=?1 LIMIT 1",[&label.uuid],|r|r.get(0)).optional()? {return Ok(id);}
    if let Some(id) = db
        .query_row(
            "SELECT id FROM tags WHERE name=?1 AND color=?2 AND deleted=0",
            params![label.name, label.color],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        db.execute(
            "INSERT INTO tag_aliases(uuid,tag) VALUES(?1,?2)",
            params![label.uuid, id],
        )?;
        return Ok(id);
    }
    let mut name = label.name.clone();
    let mut suffix = 2;
    while db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tags WHERE name=?1 AND deleted=0)",
        [&name],
        |r| r.get::<_, bool>(0),
    )? {
        name = format!("{} ({suffix})", label.name);
        suffix += 1;
    }
    db.execute(
        "INSERT INTO tags(name,color,uuid) VALUES(?1,?2,?3)",
        params![name, label.color, label.uuid],
    )?;
    Ok(db.last_insert_rowid())
}
pub fn observe(db: &Connection, uri: &str) -> rusqlite::Result<()> {
    let storage: Option<String> = db
        .query_row("SELECT storage FROM objects WHERE uri=?1", [uri], |r| {
            r.get(0)
        })
        .optional()?;
    let incoming = read(uri)?;
    if storage.as_deref() == Some("pending") {
        flush(db, uri)?;
        return Ok(());
    }
    match incoming {
        Read::Unsupported => {}
        Read::Missing if matches!(storage.as_deref(), Some("legacy" | "fallback")) => {
            flush(db, uri)?;
        }
        Read::Missing if storage.as_deref() == Some("xattr") => {
            db.execute(
                "DELETE FROM links WHERE object=(SELECT id FROM objects WHERE uri=?1)",
                [uri],
            )?;
        }
        Read::Missing => {}
        Read::Present(tags) => {
            db.execute(
                "INSERT OR IGNORE INTO objects(uri,storage) VALUES(?1,'xattr')",
                [uri],
            )?;
            let object: i64 =
                db.query_row("SELECT id FROM objects WHERE uri=?1", [uri], |r| r.get(0))?;
            db.execute("DELETE FROM links WHERE object=?1", [object])?;
            for label in tags {
                let tag = import(db, &label)?;
                db.execute("INSERT OR IGNORE INTO links(object,tag) SELECT ?1,id FROM tags WHERE id=?2 AND deleted=0",params![object,tag])?;
            }
            db.execute("UPDATE objects SET storage='xattr' WHERE id=?1", [object])?;
            // Refresh cached name/color and strip local tombstones from the payload.
            flush(db, uri)?;
        }
    }
    Ok(())
}
pub fn flush_pending(db: &Connection) -> rusqlite::Result<Vec<String>> {
    let uris: Vec<String> = db
        .prepare("SELECT uri FROM objects WHERE storage='pending'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut local = Vec::new();
    for uri in uris {
        if !flush(db, &uri).unwrap_or(false) {
            local.push(uri);
        }
    }
    Ok(local)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagRecord {
    id: i64,
    name: String,
    color: String,
    uuid: String,
    deleted: bool,
}
#[derive(Clone, Debug)]
pub struct Patch {
    tags: Vec<(TagRecord, TagRecord)>,
    links: Vec<(String, i64, bool, bool)>,
}
impl Patch {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.links.is_empty()
    }
}
#[derive(Clone)]
pub struct Snapshot {
    tags: Vec<TagRecord>,
    links: Vec<(String, i64)>,
}
pub fn snapshot(db: &Connection) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        tags:db.prepare("SELECT id,name,color,uuid,deleted FROM tags ORDER BY id")?.query_map([],|r|Ok(TagRecord{id:r.get(0)?,name:r.get(1)?,color:r.get(2)?,uuid:r.get(3)?,deleted:r.get(4)?}))?.collect::<rusqlite::Result<_>>()?,
        links:db.prepare("SELECT o.uri,l.tag FROM links l JOIN objects o ON o.id=l.object ORDER BY o.uri,l.tag")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?,
    })
}
pub fn difference(before: Snapshot, after: Snapshot) -> Patch {
    let mut tags = Vec::new();
    for new in &after.tags {
        let old = before
            .tags
            .iter()
            .find(|old| old.id == new.id)
            .cloned()
            .unwrap_or_else(|| {
                let mut old = new.clone();
                old.deleted = true;
                old
            });
        if old != *new {
            tags.push((new.clone(), old));
        }
    }
    let mut keys = before.links.clone();
    keys.extend(after.links.clone());
    keys.sort();
    keys.dedup();
    let links = keys
        .into_iter()
        .filter_map(|(uri, tag)| {
            let old = before.links.contains(&(uri.clone(), tag));
            let new = after.links.contains(&(uri.clone(), tag));
            (old != new).then_some((uri, tag, new, old))
        })
        .collect();
    Patch { tags, links }
}
pub fn apply(db: &Connection, patch: Patch) -> rusqlite::Result<Patch> {
    let mut uris: Vec<String> = patch.links.iter().map(|(uri, ..)| uri.clone()).collect();
    for (tag, _) in &patch.tags {
        uris.extend(
            db.prepare("SELECT o.uri FROM objects o JOIN links l ON l.object=o.id WHERE l.tag=?1")?
                .query_map([tag.id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?,
        );
    }
    uris.sort();
    uris.dedup();
    for uri in uris {
        observe(db, &uri)?;
    }
    let current = snapshot(db)?;
    for (expected, _) in &patch.tags {
        if !current.tags.contains(expected) {
            return Err(db_error(tr("The tag changed. Undo/Redo was stopped.")));
        }
    }
    for (uri, tag, expected, _) in &patch.links {
        if current.links.contains(&(uri.clone(), *tag)) != *expected {
            return Err(db_error(tr(
                "Tag assignments changed. Undo/Redo was stopped.",
            )));
        }
    }
    for (_, desired) in &patch.tags {
        if desired.deleted
            && current.links.iter().any(|(uri, tag)| {
                *tag == desired.id
                    && !patch
                        .links
                        .iter()
                        .any(|(known, id, expected, _)| known == uri && id == tag && *expected)
            })
        {
            return Err(db_error(tr(
                "Tag assignments changed. Undo/Redo was stopped.",
            )));
        }
    }
    for (_, desired) in &patch.tags {
        db.execute("UPDATE objects SET storage='pending' WHERE id IN (SELECT object FROM links WHERE tag=?1)",[desired.id])?;
        db.execute(
            "UPDATE tags SET name=?2,color=?3,deleted=?4 WHERE id=?1",
            params![desired.id, desired.name, desired.color, desired.deleted],
        )?;
    }
    for (uri, tag, _, desired) in &patch.links {
        db.execute("INSERT OR IGNORE INTO objects(uri) VALUES(?1)", [uri])?;
        if *desired {
            db.execute(
                "INSERT OR IGNORE INTO links(object,tag) SELECT id,?2 FROM objects WHERE uri=?1",
                params![uri, tag],
            )?;
        } else {
            db.execute(
                "DELETE FROM links WHERE object=(SELECT id FROM objects WHERE uri=?1) AND tag=?2",
                params![uri, tag],
            )?;
        }
        pending(db, uri)?;
    }
    Ok(Patch {
        tags: patch.tags.into_iter().map(|(a, b)| (b, a)).collect(),
        links: patch
            .links
            .into_iter()
            .map(|(uri, tag, a, b)| (uri, tag, b, a))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, Edit, Identity};
    use std::fs;
    fn put(file: &gio::File, bytes: &[u8]) {
        let path = path(file.uri().as_str()).unwrap();
        assert_eq!(
            unsafe {
                libc::lsetxattr(
                    path.as_ptr(),
                    ATTRIBUTE.as_ptr(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                    0,
                )
            },
            0
        );
    }
    #[test]
    fn xattrs_survive_external_move_and_import_into_another_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let root = gio::File::for_path(temp.path());
        fs::write(temp.path().join("file"), "data").unwrap();
        let catalog = Catalog::new(temp.path().join("one.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog.assign(vec![Identity::uri(&root.child("file"))],vec![1],true).await.unwrap();
            assert!(matches!(read(root.child("file").uri().as_str()).unwrap(),Read::Present(tags) if tags.len()==1));
            fs::rename(temp.path().join("file"),temp.path().join("moved")).unwrap();
            let second=Catalog::new(temp.path().join("two.sqlite"));
            second.observe(vec![Identity::uri(&root.child("moved"))]).await.unwrap();
            assert_eq!(second.labels(root.child("moved").uri().to_string()).await.unwrap()[0].name,"Red");
        });
    }
    #[test]
    fn tag_edits_undo_redo_and_deleted_uuid_does_not_resurrect() {
        let temp = tempfile::tempdir().unwrap();
        let file = gio::File::for_path(temp.path().join("file"));
        fs::write(file.path().unwrap(), "data").unwrap();
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            let assigned=catalog.edit(Edit::Assign{files:vec![Identity::uri(&file)],tags:vec![1],enabled:true}).await.unwrap();
            let removed=catalog.apply_patch(assigned).await.unwrap();assert!(catalog.labels(file.uri().to_string()).await.unwrap().is_empty());
            assert!(matches!(read(file.uri().as_str()).unwrap(),Read::Present(tags) if tags.is_empty()));
            catalog.apply_patch(removed).await.unwrap();
            let rename=catalog.edit(Edit::Save{id:Some(1),name:"Important".into(),color:"#3584e4".into()}).await.unwrap();
            assert!(matches!(read(file.uri().as_str()).unwrap(),Read::Present(tags) if tags[0].name=="Important"));
            let redo=catalog.apply_patch(rename).await.unwrap();assert_eq!(catalog.labels(file.uri().to_string()).await.unwrap()[0].name,"Red");catalog.apply_patch(redo).await.unwrap();
            let old=fs::read(file.path().unwrap()).unwrap();assert_eq!(old,b"data");
            let stale=match read(file.uri().as_str()).unwrap(){Read::Present(tags)=>serde_json::to_vec(&Payload{version:1,tags}).unwrap(),_=>panic!()};
            let deleted=catalog.edit(Edit::Delete(1)).await.unwrap();
            put(&file,&stale);catalog.observe(vec![Identity::uri(&file)]).await.unwrap();assert!(catalog.labels(file.uri().to_string()).await.unwrap().is_empty());
            catalog.apply_patch(deleted).await.unwrap();assert_eq!(catalog.labels(file.uri().to_string()).await.unwrap()[0].name,"Important");
        });
    }
    #[test]
    fn malformed_future_attributes_are_preserved_and_symlinks_use_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let file = gio::File::for_path(temp.path().join("file"));
        fs::write(file.path().unwrap(), "data").unwrap();
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            for bytes in [
                b"not json".as_slice(),
                br#"{"version":99,"tags":[]}"#.as_slice(),
            ] {
                put(&file, bytes);
                assert!(catalog.observe(vec![Identity::uri(&file)]).await.is_err());
                assert!(
                    catalog
                        .assign(vec![Identity::uri(&file)], vec![1], true)
                        .await
                        .is_err()
                );
                let path = path(file.uri().as_str()).unwrap();
                let mut output = vec![0; bytes.len()];
                assert_eq!(
                    unsafe {
                        libc::lgetxattr(
                            path.as_ptr(),
                            ATTRIBUTE.as_ptr(),
                            output.as_mut_ptr().cast(),
                            output.len(),
                        )
                    },
                    bytes.len() as isize
                );
                assert_eq!(output, bytes);
            }
            let link = gio::File::for_path(temp.path().join("link"));
            std::os::unix::fs::symlink(file.path().unwrap(), link.path().unwrap()).unwrap();
            catalog
                .assign(vec![Identity::uri(&link)], vec![2], true)
                .await
                .unwrap();
            assert_eq!(
                catalog.labels(link.uri().to_string()).await.unwrap().len(),
                1
            );
            assert!(matches!(
                read(link.uri().as_str()).unwrap(),
                Read::Unsupported
            ));
        });
    }
    #[test]
    fn deferred_updates_and_tag_creation_undo_preserve_names() {
        let temp = tempfile::tempdir().unwrap();
        let file = gio::File::for_path(temp.path().join("file"));
        fs::write(file.path().unwrap(), "data").unwrap();
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog.assign(vec![Identity::uri(&file)],vec![1],true).await.unwrap();fs::rename(file.path().unwrap(),temp.path().join("offline")).unwrap();
            catalog.save_tag(Some(1),"Later".into(),"#e01b24".into()).await.unwrap();assert_eq!(catalog.pending_count().await.unwrap(),1);
            fs::rename(temp.path().join("offline"),file.path().unwrap()).unwrap();catalog.observe(vec![Identity::uri(&file)]).await.unwrap();
            assert!(matches!(read(file.uri().as_str()).unwrap(),Read::Present(tags) if tags[0].name=="Later"));
            let created=catalog.edit(Edit::Save{id:None,name:"Custom".into(),color:"#77767b".into()}).await.unwrap();
            let redo=catalog.apply_patch(created).await.unwrap();assert!(!catalog.tags().await.unwrap().iter().any(|t|t.name=="Custom"));
            catalog.apply_patch(redo).await.unwrap();assert!(catalog.tags().await.unwrap().iter().any(|t|t.name=="Custom"));
            catalog.delete_tag(1).await.unwrap();catalog.save_tag(None,"Later".into(),"#e01b24".into()).await.unwrap();
        });
    }
    #[test]
    fn shared_inode_edits_and_undo_keep_all_indexed_links_consistent() {
        let temp = tempfile::tempdir().unwrap();
        let first = gio::File::for_path(temp.path().join("first"));
        let second = gio::File::for_path(temp.path().join("second"));
        fs::write(first.path().unwrap(), "data").unwrap();
        fs::hard_link(first.path().unwrap(), second.path().unwrap()).unwrap();
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog
                .assign(vec![Identity::uri(&first)], vec![1], true)
                .await
                .unwrap();
            catalog.observe(vec![Identity::uri(&second)]).await.unwrap();
            let assigned = catalog
                .edit(Edit::Assign {
                    files: vec![Identity::uri(&first)],
                    tags: vec![2],
                    enabled: true,
                })
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .labels(second.uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                2
            );
            let renamed = catalog
                .edit(Edit::Save {
                    id: Some(1),
                    name: "Shared".into(),
                    color: "#e01b24".into(),
                })
                .await
                .unwrap();
            assert!(
                matches!(read(first.uri().as_str()).unwrap(),Read::Present(tags) if tags.len()==2)
            );
            catalog.apply_patch(renamed).await.unwrap();
            catalog.apply_patch(assigned).await.unwrap();
            assert_eq!(
                catalog.labels(first.uri().to_string()).await.unwrap().len(),
                1
            );
            assert_eq!(
                catalog
                    .labels(second.uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                1
            );
        });
    }
    #[test]
    fn transfer_preserves_pending_local_assignments_over_stale_file_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let first = gio::File::for_path(temp.path().join("first"));
        let target = gio::File::for_path(temp.path().join("target"));
        fs::write(first.path().unwrap(), "data").unwrap();
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog
                .assign(vec![Identity::uri(&first)], vec![1], true)
                .await
                .unwrap();
            let uri = first.uri().to_string();
            catalog
                .call(move |db| {
                    db.execute(
                        "INSERT INTO links SELECT id,2 FROM objects WHERE uri=?1",
                        [&uri],
                    )?;
                    pending(db, &uri)
                })
                .await
                .unwrap();
            let tags = file_labels(&first).unwrap().unwrap();
            assert_eq!(tags.len(), 1);
            fs::rename(first.path().unwrap(), target.path().unwrap()).unwrap();
            catalog
                .import_file_tags(first.uri().to_string(), target.uri().to_string(), tags)
                .await
                .unwrap();
            catalog
                .changed(
                    first.uri().to_string(),
                    Some(target.uri().to_string()),
                    true,
                )
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .labels(target.uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                2
            );
            assert!(
                matches!(read(target.uri().as_str()).unwrap(),Read::Present(tags) if tags.len()==2)
            );
        });
    }
    #[test]
    fn unknown_uuids_and_name_conflicts_are_imported_without_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let file = gio::File::for_path(temp.path().join("file"));
        fs::write(file.path().unwrap(), "data").unwrap();
        let uuid = glib::uuid_string_random().to_string();
        put(
            &file,
            &serde_json::to_vec(&Payload {
                version: 1,
                tags: vec![Label {
                    uuid,
                    name: "Red".into(),
                    color: "#3584e4".into(),
                }],
            })
            .unwrap(),
        );
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog.observe(vec![Identity::uri(&file)]).await.unwrap();
            catalog.observe(vec![Identity::uri(&file)]).await.unwrap();
            assert_eq!(catalog.tags().await.unwrap().len(), 8);
            assert_eq!(
                catalog.labels(file.uri().to_string()).await.unwrap()[0].name,
                "Red (2)"
            );
        });
    }
    #[test]
    fn untagged_transfer_clears_destination_without_writing_to_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = gio::File::for_path(temp.path().join("source"));
        let target = gio::File::for_path(temp.path().join("target"));
        fs::write(source.path().unwrap(), "new").unwrap();
        fs::write(target.path().unwrap(), "old").unwrap();
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog.assign(vec![Identity::uri(&target)],vec![1],true).await.unwrap();
            fs::remove_file(target.path().unwrap()).unwrap();fs::copy(source.path().unwrap(),target.path().unwrap()).unwrap();
            catalog.import_file_tags(source.uri().to_string(),target.uri().to_string(),vec![]).await.unwrap();catalog.changed(source.uri().to_string(),Some(target.uri().to_string()),false).await.unwrap();
            assert!(catalog.labels(target.uri().to_string()).await.unwrap().is_empty());
            assert!(matches!(read(source.uri().as_str()).unwrap(),Read::Missing));
            assert!(matches!(read(target.uri().as_str()).unwrap(),Read::Present(tags) if tags.is_empty()));
        });
    }
    #[test]
    fn migration_from_v1_preserves_assignments_and_backfills_xattrs() {
        let temp = tempfile::tempdir().unwrap();
        let file = gio::File::for_path(temp.path().join("file"));
        fs::write(file.path().unwrap(), "data").unwrap();
        let db = Connection::open(temp.path().join("catalog.sqlite")).unwrap();
        db.execute_batch("PRAGMA user_version=1;CREATE TABLE tags(id INTEGER PRIMARY KEY,name TEXT NOT NULL UNIQUE,color TEXT NOT NULL);CREATE TABLE objects(id INTEGER PRIMARY KEY,uri TEXT NOT NULL UNIQUE,stable TEXT);CREATE TABLE links(object INTEGER REFERENCES objects(id) ON DELETE CASCADE,tag INTEGER REFERENCES tags(id) ON DELETE CASCADE,PRIMARY KEY(object,tag));INSERT INTO tags VALUES(42,'Legacy','#e01b24');").unwrap();
        db.execute(
            "INSERT INTO objects(id,uri) VALUES(1,?1)",
            [file.uri().as_str()],
        )
        .unwrap();
        db.execute("INSERT INTO links VALUES(1,42)", []).unwrap();
        drop(db);
        let catalog = Catalog::new(temp.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            catalog.observe(vec![Identity::uri(&file)]).await.unwrap();assert_eq!(catalog.labels(file.uri().to_string()).await.unwrap()[0].id,42);
            assert!(matches!(read(file.uri().as_str()).unwrap(),Read::Present(tags) if tags[0].name=="Legacy"));
        });
    }
}
