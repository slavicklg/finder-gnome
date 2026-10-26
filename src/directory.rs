// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use gio::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use crate::files::{ATTRIBUTES, FileItem};

#[derive(Clone, Debug)]
pub enum LoadEvent {
    Begin,
    Batch,
    Finished,
    Error(String),
    Notice(String),
    Moved { source: String, target: String },
}

async fn cancellable<T>(
    future: impl std::future::Future<Output = Result<T, glib::Error>>,
    cancel: &gio::Cancellable,
) -> Result<T, glib::Error> {
    gio::CancellableFuture::new(future, cancel.clone())
        .await
        .unwrap_or_else(|_| Err(glib::Error::new(gio::IOErrorEnum::Cancelled, "Cancelled")))
}
type Listener = Rc<dyn Fn(LoadEvent)>;

#[derive(Clone)]
pub enum Source {
    Directory,
    Search { query: String, hidden: bool },
    Files(Vec<String>),
}

pub struct DirectoryModel {
    pub source: RefCell<Source>,
    pub file: gio::File,
    pub store: gio::ListStore,
    cancel: RefCell<gio::Cancellable>,
    generation: Cell<u64>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    debounce: RefCell<Option<glib::SourceId>>,
    listener: RefCell<Option<Listener>>,
}

impl DirectoryModel {
    pub fn new(file: gio::File) -> Rc<Self> {
        Rc::new(Self {
            file,
            source: RefCell::new(Source::Directory),
            store: gio::ListStore::new::<glib::BoxedAnyObject>(),
            cancel: RefCell::new(gio::Cancellable::new()),
            generation: Cell::new(0),
            monitor: RefCell::new(None),
            debounce: RefCell::new(None),
            listener: RefCell::new(None),
        })
    }

    pub fn listen(&self, callback: impl Fn(LoadEvent) + 'static) {
        *self.listener.borrow_mut() = Some(Rc::new(callback));
    }

    fn emit(&self, event: LoadEvent) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener(event);
        }
    }

    pub fn reload(self: &Rc<Self>) {
        self.cancel.borrow().cancel();
        let cancel = gio::Cancellable::new();
        *self.cancel.borrow_mut() = cancel.clone();
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.emit(LoadEvent::Begin);
        self.store.remove_all();
        let weak = Rc::downgrade(self);
        match self.source.borrow().clone() {
            Source::Search { query, hidden } => {
                let model = self.clone();
                glib::MainContext::default().spawn_local(async move {
                    model.search(query, hidden, generation).await;
                });
                return;
            }
            Source::Files(uris) => {
                let model = self.clone();
                glib::MainContext::default().spawn_local(async move {
                    model.collection(uris, generation).await;
                });
                return;
            }
            Source::Directory => {}
        }
        self.file.enumerate_children_async(
            ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
            Some(&cancel),
            move |result| {
                let Some(model) = weak.upgrade().filter(|m| m.generation.get() == generation)
                else {
                    return;
                };
                match result {
                    Ok(enumerator) => model.next(enumerator, generation),
                    Err(error) if !error.matches(gio::IOErrorEnum::Cancelled) => {
                        model.emit(LoadEvent::Error(
                            crate::errors::AppError::gio(
                                &crate::i18n::tr("Read folder"),
                                &model.file,
                                &error,
                            )
                            .message(),
                        ))
                    }
                    _ => {}
                }
            },
        );
    }

    fn next(self: &Rc<Self>, enumerator: gio::FileEnumerator, generation: u64) {
        let weak = Rc::downgrade(self);
        let next_enumerator = enumerator.clone();
        enumerator.next_files_async(
            128,
            glib::Priority::DEFAULT,
            Some(&*self.cancel.borrow()),
            move |result| {
                let Some(model) = weak.upgrade().filter(|m| m.generation.get() == generation)
                else {
                    return;
                };
                match result {
                    Ok(infos) if infos.is_empty() => {
                        next_enumerator.close_async(
                            glib::Priority::DEFAULT,
                            gio::Cancellable::NONE,
                            |_| {},
                        );
                        model.watch();
                        model.emit(LoadEvent::Finished);
                    }
                    Ok(infos) => {
                        let items: Vec<_> = infos
                            .iter()
                            .map(|info| {
                                glib::BoxedAnyObject::new(FileItem::from_info(&model.file, info))
                            })
                            .collect();
                        model.store.splice(model.store.n_items(), 0, &items);
                        model.emit(LoadEvent::Batch);
                        model.next(next_enumerator, generation);
                    }
                    Err(error) if !error.matches(gio::IOErrorEnum::Cancelled) => {
                        model.emit(LoadEvent::Error(
                            crate::errors::AppError::gio(
                                &crate::i18n::tr("Read folder"),
                                &model.file,
                                &error,
                            )
                            .message(),
                        ))
                    }
                    _ => {}
                }
            },
        );
    }

    pub fn cancel(&self) {
        self.cancel.borrow().cancel();
        self.generation.set(self.generation.get().wrapping_add(1));
    }
    pub fn unavailable(&self) {
        self.cancel.borrow().cancel();
        self.generation.set(self.generation.get().wrapping_add(1));
        self.emit(LoadEvent::Error(crate::i18n::tr(
            "This location was disconnected. Connect again and retry.",
        )));
    }
    pub fn stop(&self) {
        self.cancel.borrow().cancel();
        self.generation.set(self.generation.get().wrapping_add(1));
        self.emit(LoadEvent::Finished);
        self.emit(LoadEvent::Notice(crate::i18n::tr(
            "Search stopped. Results collected so far are kept.",
        )));
    }
    async fn collection(self: Rc<Self>, uris: Vec<String>, generation: u64) {
        let cancel = self.cancel.borrow().clone();
        let mut missing = 0;
        for uri in uris {
            if self.generation.get() != generation || cancel.is_cancelled() {
                return;
            }
            let file = gio::File::for_uri(&uri);
            let info = match cancellable(
                file.query_info_future(
                    ATTRIBUTES,
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    glib::Priority::DEFAULT,
                ),
                &cancel,
            )
            .await
            {
                Ok(info) => info,
                Err(_) => {
                    missing += 1;
                    let info = gio::FileInfo::new();
                    info.set_name(file.basename().unwrap_or_default());
                    info.set_display_name(&file.basename().unwrap_or_default().to_string_lossy());
                    info
                }
            };
            if self.generation.get() != generation {
                return;
            }
            let parent = file.parent().unwrap_or_else(|| file.clone());
            let mut item = FileItem::from_info(&parent, &info);
            item.location_hint = parent.parse_name().to_string();
            self.store.append(&glib::BoxedAnyObject::new(item));
            self.emit(LoadEvent::Batch);
        }
        self.emit(LoadEvent::Finished);
        if missing > 0 {
            self.emit(LoadEvent::Notice(crate::i18n::tr(
                "Some tagged files are unavailable. Their tags have been kept.",
            )));
        }
    }
    async fn search(self: Rc<Self>, query: String, hidden: bool, generation: u64) {
        use crate::i18n::tr;
        let query = glib::casefold(query);
        let cancel = self.cancel.borrow().clone();
        let mut pending = vec![(self.file.clone(), 0usize)];
        let mut visited = std::collections::HashSet::new();
        let root_fs = cancellable(
            self.file.query_info_future(
                "id::filesystem",
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            ),
            &cancel,
        )
        .await
        .ok()
        .and_then(|i| i.attribute_string("id::filesystem"));
        let mut skipped = 0;
        let mut limit = false;
        while let Some((directory, depth)) = pending.pop() {
            if self.generation.get() != generation || cancel.is_cancelled() {
                return;
            }
            if depth > 256 || !visited.insert(directory.uri().to_string()) {
                skipped += 1;
                continue;
            }
            if depth > 0 && self.file.is_native() {
                let fs = cancellable(
                    directory.query_info_future(
                        "id::filesystem",
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    ),
                    &cancel,
                )
                .await
                .ok()
                .and_then(|i| i.attribute_string("id::filesystem"));
                if fs != root_fs {
                    continue;
                }
            }
            let enumerator = match cancellable(
                directory.enumerate_children_future(
                    ATTRIBUTES,
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    glib::Priority::DEFAULT,
                ),
                &cancel,
            )
            .await
            {
                Ok(e) => e,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            loop {
                if self.generation.get() != generation || cancel.is_cancelled() {
                    let _ = enumerator.close_future(glib::Priority::DEFAULT).await;
                    return;
                }
                let infos = match cancellable(
                    enumerator.next_files_future(128, glib::Priority::DEFAULT),
                    &cancel,
                )
                .await
                {
                    Ok(v) => v,
                    Err(_) => {
                        skipped += 1;
                        break;
                    }
                };
                if infos.is_empty() {
                    break;
                }
                let mut items = Vec::new();
                for info in infos {
                    if !hidden
                        && ((info.has_attribute("standard::is-hidden") && info.is_hidden())
                            || (info.has_attribute("standard::is-backup") && info.is_backup()))
                    {
                        continue;
                    }
                    let file = directory.child(info.name());
                    if info.file_type() == gio::FileType::Directory
                        && !(info.has_attribute("standard::is-symlink") && info.is_symlink())
                    {
                        pending.push((file, depth + 1));
                    }
                    if glib::casefold(info.display_name()).contains(query.as_str()) {
                        let mut item = FileItem::from_info(&directory, &info);
                        item.location_hint = directory.parse_name().to_string();
                        items.push(glib::BoxedAnyObject::new(item));
                        if self.store.n_items() + items.len() as u32 >= 20_000 {
                            limit = true;
                            break;
                        }
                    }
                }
                if self.generation.get() != generation {
                    return;
                }
                self.store.splice(self.store.n_items(), 0, &items);
                self.emit(LoadEvent::Batch);
                if limit {
                    break;
                }
            }
            let _ = enumerator.close_future(glib::Priority::DEFAULT).await;
            if limit {
                break;
            }
        }
        if self.generation.get() != generation {
            return;
        }
        self.emit(LoadEvent::Finished);
        if limit {
            self.emit(LoadEvent::Notice(tr(
                "Search stopped at 20,000 matches. Refine your query.",
            )));
        } else if skipped > 0 {
            self.emit(LoadEvent::Notice(tr(
                "Search finished. Some folders could not be read.",
            )));
        }
    }

    fn watch(self: &Rc<Self>) {
        if self.monitor.borrow().is_some() {
            return;
        }
        if let Ok(monitor) = self
            .file
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            monitor.set_rate_limit(250);
            let weak = Rc::downgrade(self);
            monitor.connect_changed(move |_, file, other, event| {
                let Some(model) = weak.upgrade() else {
                    return;
                };
                if matches!(
                    event,
                    gio::FileMonitorEvent::Renamed
                        | gio::FileMonitorEvent::Moved
                        | gio::FileMonitorEvent::MovedOut
                ) {
                    if let Some(other) = other {
                        model.emit(LoadEvent::Moved {
                            source: file.uri().to_string(),
                            target: other.uri().to_string(),
                        });
                    }
                }
                if let Some(id) = model.debounce.borrow_mut().take() {
                    id.remove();
                }
                let weak = Rc::downgrade(&model);
                *model.debounce.borrow_mut() = Some(glib::timeout_add_local_once(
                    Duration::from_millis(300),
                    move || {
                        if let Some(model) = weak.upgrade() {
                            model.debounce.borrow_mut().take();
                            model.reload();
                        }
                    },
                ));
            });
            *self.monitor.borrow_mut() = Some(monitor);
        }
    }
}

impl Drop for DirectoryModel {
    fn drop(&mut self) {
        self.cancel.get_mut().cancel();
        if let Some(monitor) = self.monitor.get_mut().take() {
            monitor.cancel();
        }
        if let Some(id) = self.debounce.get_mut().take() {
            id.remove();
        }
    }
}
