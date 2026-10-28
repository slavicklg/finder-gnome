// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Serialized SQLite catalog. Only owned records cross the worker boundary.
use gio::prelude::*;
use rusqlite::{Connection, params};
use std::{path::PathBuf, sync::mpsc};
#[derive(Clone, Debug)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub color: String,
}
#[derive(Clone, Debug)]
pub struct Identity {
    pub uri: String,
    pub stable: Option<String>,
}
impl Identity {
    #[cfg(test)]
    pub fn uri(file: &gio::File) -> Self {
        Self {
            uri: file.uri().to_string(),
            stable: None,
        }
    }
}
#[derive(Clone)]
pub enum Edit {
    Assign {
        files: Vec<Identity>,
        tags: Vec<i64>,
        enabled: bool,
    },
    Save {
        id: Option<i64>,
        name: String,
        color: String,
    },
    Delete(i64),
}
type Task = Box<dyn FnOnce(Result<&mut Connection, String>) + Send>;
#[derive(Clone)]
pub struct Catalog {
    sender: mpsc::Sender<Task>,
}
impl Catalog {
    pub fn new(path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel::<Task>();
        std::thread::Builder::new()
            .name("tag-catalog".into())
            .spawn(move || {
                let mut opened = (|| -> Result<Connection, String> {
                    std::fs::create_dir_all(path.parent().ok_or("Invalid catalog path")?)
                        .map_err(|e| e.to_string())?;
                    let mut db = Connection::open(&path).map_err(|e| e.to_string())?;
                    initialize(&mut db).map_err(|e| e.to_string())?;
                    Ok(db)
                })();
                for task in receiver {
                    task(opened.as_mut().map_err(|e| e.clone()));
                }
            })
            .expect("catalog worker");
        Self { sender }
    }
    pub async fn call<T: Send + 'static>(
        &self,
        task: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = async_channel::bounded(1);
        self.sender
            .send(Box::new(move |db| {
                let _ = tx.send_blocking(db.and_then(|db| task(db).map_err(|e| e.to_string())));
            }))
            .map_err(|e| e.to_string())?;
        rx.recv().await.map_err(|e| e.to_string())?
    }
    /// Snapshot fallback assignments before filesystem mutations. Watcher events
    /// may already have moved their database rows when worker events arrive.
    pub async fn fallback_tags(
        &self,
    ) -> Result<std::collections::HashMap<String, Vec<crate::tag_storage::Label>>, String> {
        self.call(|db| {
            let uris: Vec<String> = db
                .prepare("SELECT uri FROM objects WHERE storage<>'xattr'")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            uris.into_iter()
                .map(|uri| Ok((uri.clone(), crate::tag_storage::labels(db, &uri)?)))
                .collect()
        })
        .await
    }
    pub async fn import_file_tags(
        &self,
        source: String,
        uri: String,
        tags: Vec<crate::tag_storage::Label>,
    ) -> Result<(), String> {
        self.call(move |db| {
            let tx = db.transaction()?;
            let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE uri=?1 AND storage IN ('pending','legacy','fallback'))",[&source],|r|r.get(0))?;
            if !pending {crate::tag_storage::import_file(&tx,&source,tags)?;tx.execute("UPDATE objects SET storage='xattr' WHERE uri=?1",[&source])?;}
            tx.execute("INSERT OR IGNORE INTO objects(uri) VALUES(?1)",[&uri])?;
            tx.execute("DELETE FROM links WHERE object=(SELECT id FROM objects WHERE uri=?1)",[&uri])?;
            tx.execute("INSERT OR IGNORE INTO links(object,tag) SELECT dest.id,l.tag FROM links l JOIN objects src ON src.id=l.object JOIN objects dest ON dest.uri=?2 WHERE src.uri=?1",params![source,uri])?;
            crate::tag_storage::pending(&tx,&uri)?;
            tx.commit()?;
            let _ = crate::tag_storage::flush_pending(db);
            Ok(())
        })
        .await
    }
    pub async fn edit(&self, edit: Edit) -> Result<crate::tag_storage::Patch, String> {
        if let Edit::Save { name, color, .. } = &edit {
            if name.trim().is_empty() {
                return Err(crate::i18n::tr("Enter a tag name."));
            }
            if !crate::tag_storage::valid_color(color) {
                return Err(crate::i18n::tr("Choose one of the supported tag colors."));
            }
        }
        self.call(move|db| {
            let tx=db.transaction()?;
            if let Edit::Assign{files,..}=&edit {
                for file in files {
                    crate::tag_storage::observe(&tx,&file.uri)?;
                    object(&tx,file)?;
                    for uri in crate::tag_storage::linked_uris(&tx,&file.uri)? {crate::tag_storage::observe(&tx,&uri)?;}
                }
            } else {
                let uris:Vec<String>=tx.prepare("SELECT uri FROM objects")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
                for uri in uris {let _=crate::tag_storage::observe(&tx,&uri);}
            }
            let before=crate::tag_storage::snapshot(&tx)?;
            match edit {
                Edit::Assign{files,tags,enabled}=> {
                    for file in files {
                        let id=object(&tx,&file)?;
                        crate::tag_storage::pending(&tx,&file.uri)?;
                        for tag in &tags {
                            if enabled {tx.execute("INSERT OR IGNORE INTO links(object,tag) SELECT ?1,id FROM tags WHERE id=?2 AND deleted=0",params![id,tag])?;}
                            else {tx.execute("DELETE FROM links WHERE object=?1 AND tag=?2",params![id,tag])?;}
                        }
                        for uri in crate::tag_storage::linked_uris(&tx,&file.uri)? {
                            if uri==file.uri {continue;}
                            tx.execute("DELETE FROM links WHERE object=(SELECT id FROM objects WHERE uri=?1)",[&uri])?;
                            tx.execute("INSERT OR IGNORE INTO links(object,tag) SELECT o.id,l.tag FROM objects o JOIN links l ON l.object=?2 WHERE o.uri=?1",params![uri,id])?;
                            crate::tag_storage::pending(&tx,&uri)?;
                        }
                    }
                }
                Edit::Save{id,name,color}=> {
                    if let Some(id)=id {
                        tx.execute("UPDATE objects SET storage='pending' WHERE id IN (SELECT object FROM links WHERE tag=?1)",[id])?;
                        tx.execute("UPDATE tags SET name=?2,color=?3 WHERE id=?1",params![id,name,color])?;
                    } else {tx.execute("INSERT INTO tags(name,color,uuid) VALUES(?1,?2,?3)",params![name,color,glib::uuid_string_random().as_str()])?;}
                }
                Edit::Delete(id)=> {
                    tx.execute("UPDATE objects SET storage='pending' WHERE id IN (SELECT object FROM links WHERE tag=?1)",[id])?;
                    tx.execute("DELETE FROM links WHERE tag=?1",[id])?;
                    tx.execute("UPDATE tags SET deleted=1 WHERE id=?1",[id])?;
                }
            }
            let patch=crate::tag_storage::difference(before,crate::tag_storage::snapshot(&tx)?);
            tx.commit()?;
            // A file write failure must not conceal a committed, undoable change.
            let _=crate::tag_storage::flush_pending(db);
            Ok(patch)
        }).await.map_err(|error| if error.contains("UNIQUE constraint") {crate::i18n::tr("A tag with this name already exists.")} else {error})
    }
    pub async fn apply_patch(
        &self,
        patch: crate::tag_storage::Patch,
    ) -> Result<crate::tag_storage::Patch, String> {
        self.call(move |db| {
            let tx = db.transaction()?;
            let inverse = crate::tag_storage::apply(&tx, patch)?;
            tx.commit()?;
            let _ = crate::tag_storage::flush_pending(db);
            Ok(inverse)
        })
        .await
    }
    pub async fn pending_count(&self) -> Result<usize, String> {
        self.call(|db| {
            db.query_row(
                "SELECT COUNT(*) FROM objects WHERE storage='pending'",
                [],
                |r| r.get(0),
            )
        })
        .await
    }
    pub async fn tags(&self) -> Result<Vec<Tag>, String> {
        self.call(|db| {
            db.prepare("SELECT id,name,color FROM tags WHERE deleted=0 ORDER BY id")?
                .query_map([], |r| {
                    Ok(Tag {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        color: r.get(2)?,
                    })
                })?
                .collect()
        })
        .await
    }
    #[cfg(test)]
    pub async fn save_tag(
        &self,
        id: Option<i64>,
        name: String,
        color: String,
    ) -> Result<(), String> {
        self.edit(Edit::Save { id, name, color }).await.map(|_| ())
    }
    #[cfg(test)]
    pub async fn delete_tag(&self, id: i64) -> Result<(), String> {
        self.edit(Edit::Delete(id)).await.map(|_| ())
    }
    #[cfg(test)]
    pub async fn assign(
        &self,
        files: Vec<Identity>,
        tags: Vec<i64>,
        enabled: bool,
    ) -> Result<(), String> {
        self.edit(Edit::Assign {
            files,
            tags,
            enabled,
        })
        .await
        .map(|_| ())
    }
    pub async fn observe(&self, files: Vec<Identity>) -> Result<(), String> {
        self.call(move |db| {
            let tx = db.transaction()?;
            for file in files {
                let known:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE uri=?1 OR (stable IS NOT NULL AND stable=?2))",params![file.uri,file.stable],|r|r.get(0))?;
                if known && (file.stable.is_none() || gio::File::for_uri(&file.uri).query_exists(gio::Cancellable::NONE)) {object(&tx,&file)?;}
                crate::tag_storage::observe(&tx,&file.uri)?;
                let imported:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE uri=?1)",[&file.uri],|r|r.get(0))?;
                if imported && file.stable.is_some() && gio::File::for_uri(&file.uri).query_exists(gio::Cancellable::NONE) {object(&tx,&file)?;}
            }
            tx.commit()
        })
        .await
    }
    pub async fn badges(&self, uris: Vec<String>) -> Result<Vec<Vec<(String, String)>>, String> {
        self.call(move|db| {
            let mut rows=db.prepare("SELECT t.name,t.color FROM tags t JOIN links l ON t.id=l.tag JOIN objects o ON o.id=l.object WHERE o.uri=?1 AND t.deleted=0 ORDER BY t.id")?;
            uris.into_iter().map(|uri|rows.query_map([uri],|r|Ok((r.get(0)?,r.get(1)?)))?.collect()).collect()
        }).await
    }
    pub async fn labels(&self, uri: String) -> Result<Vec<Tag>, String> {
        self.call(move |db| {
        db.prepare("SELECT t.id,t.name,t.color FROM tags t JOIN links l ON t.id=l.tag JOIN objects o ON o.id=l.object WHERE o.uri=?1 AND t.deleted=0 ORDER BY t.id")?.query_map([uri],|r|Ok(Tag{id:r.get(0)?,name:r.get(1)?,color:r.get(2)?}))?.collect()
    }).await
    }
    pub async fn files(&self, tag: i64) -> Result<Vec<String>, String> {
        self.call(move |db| {
        db.prepare("SELECT o.uri FROM objects o JOIN links l ON o.id=l.object WHERE l.tag=?1 ORDER BY o.uri")?.query_map([tag],|r|r.get(0))?.collect()
    }).await
    }
    pub async fn changed(
        &self,
        source: String,
        target: Option<String>,
        moving: bool,
    ) -> Result<(), String> {
        self.call(move |db| {
            let tx = db.transaction()?;
            // Directory moves and atomic renames transfer whole known subtrees.
            let entries: Vec<(i64, String)> = tx
                .prepare("SELECT id,uri FROM objects")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let src = gio::File::for_uri(&source);
            for (id, uri) in entries {
                let file = gio::File::for_uri(&uri);
                let suffix = if file.equal(&src) {
                    Some(String::new())
                } else {
                    src.relative_path(&file)
                        .map(|p| p.to_string_lossy().into_owned())
                };
                let Some(suffix) = suffix else {
                    continue;
                };
                if let Some(target) = &target {
                    let dest = gio::File::for_uri(target);
                    let dest = if suffix.is_empty() {
                        dest
                    } else {
                        dest.resolve_relative_path(suffix)
                    };
                    let dest_uri = dest.uri().to_string();
                    if dest_uri == uri {
                        continue;
                    }
                    let dest_id = object(
                        &tx,
                        &Identity {
                            uri: dest_uri,
                            stable: None,
                        },
                    )?;
                    tx.execute("DELETE FROM links WHERE object=?1", [dest_id])?;
                    tx.execute(
                        "UPDATE objects SET stable=NULL, storage='pending' WHERE id=?1",
                        [dest_id],
                    )?;
                    tx.execute(
                        "INSERT OR IGNORE INTO links SELECT ?1,tag FROM links WHERE object=?2",
                        params![dest_id, id],
                    )?;
                }
                if moving {
                    tx.execute("DELETE FROM objects WHERE id=?1", [id])?;
                }
            }
            tx.commit()?;
            crate::tag_storage::flush_pending(db)?;
            Ok(())
        })
        .await
    }
}
fn object(db: &Connection, file: &Identity) -> rusqlite::Result<i64> {
    // A matching identity can repair an external rename; URI reuse is a new object.
    if let Some(stable) = &file.stable {
        let old: Option<(i64, String)> = db
            .query_row(
                "SELECT id,uri FROM objects WHERE stable=?1",
                [stable],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, uri)) = old {
            if uri == file.uri {
                return Ok(id);
            }
            if !gio::File::for_uri(&uri).query_exists(gio::Cancellable::NONE) {
                db.execute(
                    "DELETE FROM objects WHERE uri=?1 AND id<>?2",
                    params![file.uri, id],
                )?;
                db.execute(
                    "UPDATE objects SET uri=?1 WHERE id=?2",
                    params![file.uri, id],
                )?;
                return Ok(id);
            }
        }
    }
    if let Some(stable) = &file.stable {
        let old: Option<String> = db
            .query_row(
                "SELECT stable FROM objects WHERE uri=?1",
                [&file.uri],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if old.as_ref().is_some_and(|old| old != stable) {
            db.execute("DELETE FROM objects WHERE uri=?1", [&file.uri])?;
        }
    }
    db.execute("INSERT INTO objects(uri,stable) VALUES(?1,?2) ON CONFLICT(uri) DO UPDATE SET stable=COALESCE(excluded.stable,objects.stable)",params![file.uri,file.stable])?;
    db.query_row("SELECT id FROM objects WHERE uri=?1", [&file.uri], |r| {
        r.get(0)
    })
}
use rusqlite::OptionalExtension;
fn initialize(db: &mut Connection) -> rusqlite::Result<()> {
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA busy_timeout=3000;")?;
    let transaction = db.transaction()?;
    let db = &transaction;
    let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > 2 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS tags(id INTEGER PRIMARY KEY,name TEXT NOT NULL UNIQUE,color TEXT NOT NULL); CREATE TABLE IF NOT EXISTS objects(id INTEGER PRIMARY KEY,uri TEXT NOT NULL UNIQUE,stable TEXT); CREATE INDEX IF NOT EXISTS stable_objects ON objects(stable); CREATE TABLE IF NOT EXISTS links(object INTEGER REFERENCES objects(id) ON DELETE CASCADE,tag INTEGER REFERENCES tags(id) ON DELETE CASCADE,PRIMARY KEY(object,tag));")?;
    if version == 0 {
        for (name, color) in [
            ("Red", "#e01b24"),
            ("Orange", "#ff7800"),
            ("Yellow", "#f6d32d"),
            ("Green", "#33d17a"),
            ("Blue", "#3584e4"),
            ("Purple", "#9141ac"),
            ("Gray", "#77767b"),
        ] {
            db.execute(
                "INSERT INTO tags(name,color) VALUES(?1,?2)",
                params![name, color],
            )?;
        }
    }
    if version < 2 {
        db.execute_batch(concat!(
            "CREATE TEMP TABLE saved_links AS SELECT object,tag FROM links;",
            "DROP TABLE links;",
            "ALTER TABLE tags RENAME TO tags_v1;",
            "CREATE TABLE tags(id INTEGER PRIMARY KEY,name TEXT NOT NULL,color TEXT NOT NULL,uuid TEXT,deleted INTEGER NOT NULL DEFAULT 0);",
            "INSERT INTO tags(id,name,color) SELECT id,name,color FROM tags_v1;",
            "DROP TABLE tags_v1;",
            "CREATE UNIQUE INDEX active_tag_name ON tags(name) WHERE deleted=0;",
            "CREATE TABLE links(object INTEGER REFERENCES objects(id) ON DELETE CASCADE,tag INTEGER REFERENCES tags(id) ON DELETE CASCADE,PRIMARY KEY(object,tag));",
            "INSERT INTO links SELECT object,tag FROM saved_links;",
            "DROP TABLE saved_links;",
            "ALTER TABLE objects ADD COLUMN storage TEXT NOT NULL DEFAULT 'legacy';",
            "CREATE TABLE tag_aliases(uuid TEXT PRIMARY KEY,tag INTEGER NOT NULL REFERENCES tags(id));",
        ))?;
        let ids: Vec<i64> = db
            .prepare("SELECT id FROM tags")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            db.execute(
                "UPDATE tags SET uuid=?2 WHERE id=?1",
                params![id, glib::uuid_string_random().as_str()],
            )?;
        }
        db.execute_batch("CREATE UNIQUE INDEX tag_uuid ON tags(uuid);")?;
        db.pragma_update(None, "user_version", 2)?;
    }
    transaction.commit()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_migration_tags_and_partial_transfers() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::new(dir.path().join("catalog.sqlite"));
        glib::MainContext::new().block_on(async {
            assert_eq!(catalog.tags().await.unwrap().len(), 7);
            let src = gio::File::for_path("/tmp/catalog-source");
            catalog
                .assign(
                    vec![
                        Identity::uri(&src.child("a")),
                        Identity::uri(&src.child("b")),
                    ],
                    vec![1],
                    true,
                )
                .await
                .unwrap();
            catalog
                .changed(
                    src.child("a").uri().to_string(),
                    Some(src.child("c").uri().to_string()),
                    true,
                )
                .await
                .unwrap();
            assert!(
                catalog
                    .labels(src.child("a").uri().to_string())
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                catalog
                    .labels(src.child("b").uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                catalog
                    .labels(src.child("c").uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                1
            );
            catalog
                .changed(
                    src.child("c").uri().to_string(),
                    Some(src.child("d").uri().to_string()),
                    false,
                )
                .await
                .unwrap();
            assert_eq!(catalog.files(1).await.unwrap().len(), 3);
            catalog.delete_tag(1).await.unwrap();
            assert!(catalog.files(1).await.unwrap().is_empty());
        });
    }
    #[test]
    fn identity_repairs_external_rename_and_xattrs_are_shared_by_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let first = gio::File::for_path(dir.path().join("first"));
        let second = gio::File::for_path(dir.path().join("second"));
        std::fs::write(first.path().unwrap(), "data").unwrap();
        std::fs::hard_link(first.path().unwrap(), second.path().unwrap()).unwrap();
        let catalog = Catalog::new(dir.path().join("catalog.sqlite"));
        let identity = |file: &gio::File| Identity {
            uri: file.uri().to_string(),
            stable: Some("filesystem:inode:birthtime".into()),
        };
        glib::MainContext::new().block_on(async {
            catalog
                .assign(vec![identity(&first)], vec![1], true)
                .await
                .unwrap();
            catalog.observe(vec![identity(&second)]).await.unwrap();
            assert_eq!(
                catalog
                    .labels(second.uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                1
            );
            let renamed = gio::File::for_path(dir.path().join("renamed"));
            std::fs::rename(first.path().unwrap(), renamed.path().unwrap()).unwrap();
            catalog.observe(vec![identity(&renamed)]).await.unwrap();
            assert_eq!(
                catalog
                    .labels(renamed.uri().to_string())
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                catalog
                    .labels(first.uri().to_string())
                    .await
                    .unwrap()
                    .is_empty()
            );
            let reopened = Catalog::new(dir.path().join("catalog.sqlite"));
            assert_eq!(reopened.files(1).await.unwrap(), {
                let mut uris = vec![renamed.uri().to_string(), second.uri().to_string()];
                uris.sort();
                uris
            });
        });
    }
    #[test]
    fn catalog_preserves_unavailable_files_and_rejects_future_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tags.sqlite");
        let catalog = Catalog::new(path.clone());
        glib::MainContext::new().block_on(async {
            catalog
                .assign(
                    vec![Identity {
                        uri: "sftp://server/missing".into(),
                        stable: None,
                    }],
                    vec![2],
                    true,
                )
                .await
                .unwrap();
            assert_eq!(
                catalog.files(2).await.unwrap(),
                vec!["sftp://server/missing"]
            );
            assert!(
                catalog
                    .save_tag(None, "Red".into(), "#e01b24".into())
                    .await
                    .is_err()
            );
            assert_eq!(catalog.tags().await.unwrap().len(), 7);
        });
        drop(catalog);
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
        drop(db);
        let unsupported = Catalog::new(path);
        glib::MainContext::new().block_on(async {
            assert!(unsupported.tags().await.is_err());
        });
    }
}
