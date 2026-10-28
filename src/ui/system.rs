// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use super::*;
use crate::{catalog::Identity, directory::Source, errors::ErrorKind};

pub(super) struct TaskRow {
    pub title: String,
    pub operation: Operation,
    pub token: gio::Cancellable,
    pub label: gtk::Label,
    pub progress: gtk::ProgressBar,
    pub cancel: gtk::Button,
    pub retry: gtk::Button,
    pub failed: RefCell<Vec<Operation>>,
    pub done: Cell<bool>,
}
pub(super) fn catalog_path(storage: &Storage) -> std::path::PathBuf {
    #[cfg(test)]
    {
        storage
            .settings_path
            .parent()
            .unwrap()
            .join("catalog.sqlite")
    }
    #[cfg(not(test))]
    {
        let _ = storage;
        glib::user_data_dir().join("ubuntu-finder/catalog.sqlite")
    }
}
fn section(container: &gtk::Box, title: &str) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    container.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["section-title"])
            .build(),
    );
}
impl Browser {
    pub(super) fn is_collection(&self) -> bool {
        self.active_pane().is_some_and(|p| {
            matches!(
                *p.location.borrow(),
                Location::Search { .. } | Location::Tag(_)
            )
        })
    }
    pub(super) fn load_pane(self: &Rc<Self>, pane: &Rc<Pane>) {
        if self.defer_load.get() {
            pane.pending.set(true);
            pane.loading.set(false);
            pane.error.set_label(&tr("Select this tab to connect."));
            pane.stack.set_visible_child_name("error");
            return;
        }
        pane.pending.set(false);
        pane.connection_cancel.borrow().cancel();
        *pane.connection_cancel.borrow_mut() = gio::Cancellable::new();
        if pane.model.file.is_native()
            || files::is_trash(&pane.model.file)
            || matches!(*pane.model.source.borrow(), Source::Files(_))
        {
            self.refresh_permissions(pane);
            pane.model.reload();
            return;
        }
        pane.loading.set(true);
        pane.stack.set_visible_child_name("loading");
        let service = self.connections.clone();
        let window = self.window.clone();
        let pane = Rc::downgrade(pane);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Some(pane) = pane.upgrade() else {
                return;
            };
            let cancel = pane.connection_cancel.borrow().clone();
            match service.ensure(&pane.model.file, &window, &cancel).await {
                Ok(()) => {
                    if let Some(b) = weak.upgrade() {
                        b.refresh_permissions(&pane);
                        let uri = pane.model.file.uri().to_string();
                        let mut settings = b.settings.borrow_mut();
                        settings.recent_servers.retain(|s| s != &uri);
                        settings.recent_servers.insert(0, uri);
                        settings.recent_servers.truncate(10);
                        drop(settings);
                        b.schedule_persist();
                    }
                    pane.model.reload();
                }
                Err(error) => {
                    if cancel.is_cancelled() {
                        return;
                    }
                    pane.loading.set(false);
                    pane.pending.set(true);
                    pane.error.set_label(&error.message());
                    pane.stack.set_visible_child_name("error");
                }
            }
        });
    }
    pub(super) fn refresh_permissions(self: &Rc<Self>, pane: &Rc<Pane>) {
        let permissions = Rc::downgrade(pane);
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            if let Some(pane) = permissions.upgrade() {
                if let Ok(info) = pane
                    .model
                    .file
                    .query_info_future(
                        "access::can-write",
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    pane.writable.set(
                        info.has_attribute("access::can-write")
                            .then(|| info.boolean("access::can-write")),
                    );
                }
                if let Some(b) = weak.upgrade() {
                    b.update_status();
                }
            }
        });
    }
    pub(super) fn open_collection(self: &Rc<Self>, location: Location, selected: HashSet<String>) {
        let root = match &location {
            Location::Search { root, .. } => gio::File::for_uri(root),
            _ => self.current.borrow().clone(),
        };
        self.record_departure();
        let replaying = self.replaying.replace(true);
        if self.mode.get() == ViewMode::Columns {
            self.mode.set(ViewMode::List);
            self.folder_settings.borrow_mut().mode = ViewMode::List;
        }
        let deferred = self.defer_load.replace(true);
        self.navigate(root, selected);
        self.defer_load.set(deferred);
        let Some(pane) = self.active_pane() else {
            return;
        };
        *pane.location.borrow_mut() = location.clone();
        match &location {
            Location::Search { query, .. } => {
                *pane.model.source.borrow_mut() = Source::Search {
                    query: query.clone(),
                    hidden: self.hidden.get(),
                };
                self.title.set_label(&tr("Search Results"));
                self.load_pane(&pane);
            }
            Location::Tag(id) => {
                self.title.set_label(&tr("Tagged Files"));
                let catalog = self.catalog.clone();
                let id = *id;
                let pane = Rc::downgrade(&pane);
                let weak = Rc::downgrade(self);
                glib::MainContext::default().spawn_local(async move {
                    match catalog.files(id).await {
                        Ok(files) => {
                            if let (Some(pane), Some(b)) = (pane.upgrade(), weak.upgrade()) {
                                *pane.model.source.borrow_mut() = Source::Files(files);
                                b.load_pane(&pane);
                            }
                        }
                        Err(e) => {
                            if let Some(b) = weak.upgrade() {
                                b.error(&e);
                            }
                        }
                    }
                });
            }
            _ => return,
        }
        self.replaying.set(replaying);
        if !replaying {
            self.history.borrow_mut().visit(self.capture_entry());
        }
        self.current_tab.borrow().set_title(&self.title.text());
        self.history
            .borrow_mut()
            .replace_current(self.capture_entry());
        self.update_status();
        self.schedule_persist();
    }
    pub(super) fn install_system(self: &Rc<Self>, app: &adw::Application) {
        self.action(app, "connect-server", &["<Control>k"], |b| {
            b.connect_server()
        });
        self.action(app, "search", &["<Control>f"], |b| {
            if !b.is_collection() {
                *b.search_origin.borrow_mut() = Some(b.current.borrow().clone());
                b.search_entry.set_text("");
            }
            b.search_bar.set_reveal_child(true);
            b.search_entry.grab_focus();
        });
        self.action(app, "stop-search", &[], |b| {
            if b.is_collection() {
                if let Some(p) = b.active_pane() {
                    p.model.stop();
                }
            }
        });
        self.action(app, "close-search", &[], |b| {
            b.search_bar.set_reveal_child(false);
            if b.is_collection() {
                if b.history.borrow().can_back() {
                    b.history_step(false);
                } else {
                    let root = b.current.borrow().clone();
                    b.navigate(root, HashSet::new());
                }
            }
        });
        self.action(app, "tags", &[], |b| b.tags_dialog());
        self.action(app, "manage-tags", &[], |b| b.manage_tags());
        self.action(app, "show-parent", &[], |b| {
            if let Some(file) = b.selected().first() {
                if let Some(parent) = file.file.parent() {
                    b.navigate(parent, HashSet::from([file.file.uri().to_string()]));
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.search_entry.connect_search_changed(move |_| {
            if let Some(b) = weak.upgrade() {
                b.schedule_search();
            }
        });
        let weak = Rc::downgrade(self);
        self.search_scope.connect_selected_notify(move |_| {
            if let Some(b) = weak.upgrade() {
                b.schedule_search();
            }
        });
        self.refresh_locations();
        self.refresh_tags();
        let weak = Rc::downgrade(self);
        self.volumes.connect_mount_added(move |_, _| {
            if let Some(b) = weak.upgrade() {
                b.refresh_locations();
            }
        });
        let weak = Rc::downgrade(self);
        self.volumes.connect_volume_added(move |_, _| {
            if let Some(b) = weak.upgrade() {
                b.refresh_locations();
            }
        });
        let weak = Rc::downgrade(self);
        self.volumes.connect_volume_removed(move |_, _| {
            if let Some(b) = weak.upgrade() {
                b.refresh_locations();
            }
        });
        let weak = Rc::downgrade(self);
        self.volumes.connect_mount_removed(move |_, mount| {
            if let Some(b) = weak.upgrade() {
                let root = mount.root();
                for pane in b.all_panes() {
                    if pane.model.file.equal(&root) || pane.model.file.has_prefix(&root) {
                        pane.model.unavailable();
                        pane.pending.set(true);
                    }
                }
                b.refresh_locations();
            }
        });
        let weak = Rc::downgrade(self);
        self.volumes.connect_mount_changed(move |_, _| {
            if let Some(b) = weak.upgrade() {
                b.refresh_locations();
            }
        });
    }
    pub(super) fn all_panes(&self) -> Vec<Rc<Pane>> {
        let mut panes = self.panes.borrow().clone();
        for tab in self.tabs.borrow().iter() {
            if let Some(state) = tab.state.borrow().as_ref() {
                for p in &state.panes {
                    if !panes.iter().any(|v| Rc::ptr_eq(v, p)) {
                        panes.push(p.clone());
                    }
                }
            }
        }
        panes
    }
    pub(super) fn sync_search(&self) {
        self.search_bar.set_reveal_child(false);
        if let Some(pane) = self.active_pane() {
            if let Location::Search { root, query } = &*pane.location.borrow() {
                *self.search_origin.borrow_mut() = Some(gio::File::for_uri(root));
                self.search_scope.set_selected(0);
                if self.search_entry.text() != query.as_str() {
                    self.search_entry.set_text(query);
                }
                self.search_bar.set_reveal_child(true);
            }
        }
    }
    fn schedule_search(self: &Rc<Self>) {
        if !self.search_bar.reveals_child() {
            return;
        }
        if self.search_scope.selected()==0 && self.active_pane().is_some_and(|p|matches!(&*p.location.borrow(),Location::Search{query,..} if query==self.search_entry.text().as_str())){return;}

        if let Some(timer) = self.search_timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        *self.search_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            std::time::Duration::from_millis(300),
            move || {
                if let Some(b) = weak.upgrade() {
                    b.search_timer.borrow_mut().take();
                    let query = b.search_entry.text().to_string();
                    if query.trim().is_empty() {
                        return;
                    }
                    let root = if b.search_scope.selected() == 1 {
                        gio::File::for_path(glib::home_dir())
                    } else {
                        b.search_origin
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| b.current.borrow().clone())
                    };
                    let repeating = b
                        .active_pane()
                        .is_some_and(|p| matches!(*p.location.borrow(), Location::Search { .. }));
                    let replaying = b.replaying.replace(repeating);
                    b.open_collection(
                        Location::Search {
                            root: root.uri().to_string(),
                            query,
                        },
                        HashSet::new(),
                    );
                    b.replaying.set(replaying);
                }
            },
        ));
    }
    fn connect_server(self: &Rc<Self>) {
        let dialog=adw::AlertDialog::builder().heading(tr("Connect to Server")).body(tr("Enter an smb:// or sftp:// address. Passwords are requested securely by the system.")).build();
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let entry = gtk::Entry::builder()
            .placeholder_text("sftp://user@server/folder")
            .activates_default(true)
            .build();
        box_.append(&entry);
        let error = gtk::Label::builder()
            .wrap(true)
            .css_classes(["error"])
            .build();
        box_.append(&error);
        for uri in self.settings.borrow().recent_servers.clone() {
            if files::safe_uri(&uri).is_err() {
                continue;
            }
            let button = gtk::Button::with_label(&uri);
            let input = entry.clone();
            button.connect_clicked(move |_| input.set_text(&uri));
            box_.append(&button);
        }
        dialog.set_extra_child(Some(&box_));
        dialog.add_responses(&[("cancel", &tr("Cancel")), ("connect", &tr("Connect"))]);
        dialog.set_default_response(Some("connect"));
        dialog.set_close_response("cancel");
        let weak_dialog = dialog.downgrade();
        entry.connect_changed(move |e| {
            let result = files::parse_location(&e.text());
            if let Some(d) = weak_dialog.upgrade() {
                d.set_response_enabled("connect", result.is_ok());
            }
            error.set_label(&result.err().unwrap_or_default());
        });
        dialog.set_response_enabled("connect", false);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, r| {
            if r == "connect" {
                if let (Some(b), Ok(file)) = (weak.upgrade(), files::parse_location(&entry.text()))
                {
                    b.open_in_new_tab(file);
                }
            }
        });
        dialog.present(Some(&self.window));
    }
    fn refresh_locations(self: &Rc<Self>) {
        section(&self.locations_box, &tr("Locations"));
        let connect = gtk::Button::with_label(&tr("Connect to Server…"));
        connect.add_css_class("flat");
        connect.set_action_name(Some("win.connect-server"));
        self.locations_box.append(&connect);
        for mount in self.volumes.mounts() {
            if mount.is_shadowed() {
                continue;
            }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let open = gtk::Button::new();
            open.set_child(Some(
                &gtk::Label::builder()
                    .label(mount.name())
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(18)
                    .build(),
            ));
            open.add_css_class("flat");
            open.set_hexpand(true);
            let file = mount.root();
            let weak = Rc::downgrade(self);
            let target = file.clone();
            open.connect_clicked(move |_| {
                if let Some(b) = weak.upgrade() {
                    b.navigate(target.clone(), HashSet::new());
                }
            });
            self.add_drop_target(&open, move |_, _| Some(file.clone()));
            row.append(&open);
            if mount.can_unmount() || mount.can_eject() {
                let button = gtk::Button::from_icon_name("media-eject-symbolic");
                button.add_css_class("flat");
                button.set_tooltip_text(Some(&tr("Disconnect or Eject")));
                let weak = Rc::downgrade(self);
                button.connect_clicked(move |_| {
                    if let Some(b) = weak.upgrade() {
                        b.confirm_disconnect_mount(mount.clone());
                    }
                });
                row.append(&button);
            }
            self.locations_box.append(&row);
        }
        for volume in self.volumes.volumes() {
            if volume.get_mount().is_some() || !volume.can_mount() {
                continue;
            }
            let button = gtk::Button::with_label(&volume.name());
            button.add_css_class("flat");
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                let Some(b) = weak.upgrade() else {
                    return;
                };
                let volume = volume.clone();
                glib::MainContext::default().spawn_local(async move {
                    let op = gtk::MountOperation::new(Some(&b.window));
                    match volume
                        .mount_future(gio::MountMountFlags::NONE, Some(&op))
                        .await
                    {
                        Ok(()) => {
                            if let Some(m) = volume.get_mount() {
                                b.navigate(m.root(), HashSet::new());
                            }
                        }
                        Err(e) => {
                            if !e.matches(gio::IOErrorEnum::Cancelled) {
                                b.error(&e.to_string());
                            }
                        }
                    }
                    b.refresh_locations();
                });
            });
            self.locations_box.append(&button);
        }
    }
    fn mount_jobs(&self, root: &gio::File) -> Vec<gio::Cancellable> {
        self.task_rows
            .borrow()
            .iter()
            .filter(|task| {
                !task.done.get()
                    && task
                        .operation
                        .locations()
                        .iter()
                        .any(|file| file.equal(root) || file.has_prefix(root))
            })
            .map(|task| task.token.clone())
            .collect()
    }
    fn confirm_disconnect_mount(self: &Rc<Self>, mount: gio::Mount) {
        let weak = Rc::downgrade(self);
        self.confirm_disconnect(&mount.name(), mount.can_eject(), move || {
            if let Some(browser) = weak.upgrade() {
                browser.disconnect_mount(mount.clone());
            }
        });
    }
    pub(super) fn confirm_disconnect(
        &self,
        name: &str,
        eject: bool,
        confirmed: impl Fn() + 'static,
    ) {
        let dialog = adw::AlertDialog::builder()
            .heading(if eject {
                tr("Eject Disk?")
            } else {
                tr("Disconnect Disk?")
            })
            .body(tr("Disconnect “{name}” from the system?").replace("{name}", name))
            .build();
        dialog.add_responses(&[
            ("cancel", &tr("Cancel")),
            (
                "disconnect",
                &if eject { tr("Eject") } else { tr("Disconnect") },
            ),
        ]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(None, move |_, response| {
            if response == "disconnect" {
                confirmed();
            }
        });
        dialog.present(Some(&self.window));
    }
    fn disconnect_mount(self: &Rc<Self>, mount: gio::Mount) {
        if !self.mount_jobs(&mount.root()).is_empty() {
            let dialog = adw::AlertDialog::builder()
                .heading(tr("File operations are running"))
                .body(tr(
                    "Wait, or cancel running operations before disconnecting this device.",
                ))
                .build();
            dialog.add_responses(&[
                ("stay", &tr("Stay")),
                ("cancel", &tr("Cancel Operations and Disconnect")),
            ]);
            dialog.set_close_response("stay");
            let weak = Rc::downgrade(self);
            dialog.connect_response(None, move |_, r| {
                if r == "cancel" {
                    if let Some(b) = weak.upgrade() {
                        for job in b.mount_jobs(&mount.root()) {
                            b.cancel_job(&job);
                        }
                        let b = Rc::downgrade(&b);
                        let mount = mount.clone();
                        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                            let Some(b) = b.upgrade() else {
                                return glib::ControlFlow::Break;
                            };
                            if b.mount_jobs(&mount.root()).is_empty() {
                                b.disconnect_mount(mount.clone());
                                glib::ControlFlow::Break
                            } else {
                                glib::ControlFlow::Continue
                            }
                        });
                    }
                }
            });
            dialog.present(Some(&self.window));
            return;
        }
        let b = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let op = gtk::MountOperation::new(Some(&b.window));
            let result = if mount.can_eject() {
                mount
                    .eject_with_operation_future(gio::MountUnmountFlags::NONE, Some(&op))
                    .await
            } else {
                mount
                    .unmount_with_operation_future(gio::MountUnmountFlags::NONE, Some(&op))
                    .await
            };
            if let Err(e) = result {
                if !e.matches(gio::IOErrorEnum::Cancelled) {
                    b.error(&e.to_string());
                }
            }
            b.refresh_locations();
        });
    }
    pub(super) fn report_catalog_error(&self, error: &str) {
        if !self.catalog_error_shown.replace(true) {
            self.error(
                &tr("Tags could not be updated. File operations remain available.\n{error}")
                    .replace("{error}", error),
            );
        }
    }
    pub(super) fn refresh_tag_views(self: &Rc<Self>) {
        for pane in self.all_panes() {
            let location = pane.location.borrow().clone();
            if let Location::Tag(id) = location {
                let catalog = self.catalog.clone();
                let weak = Rc::downgrade(&pane);
                glib::MainContext::default().spawn_local(async move{if let (Ok(files),Some(pane))=(catalog.files(id).await,weak.upgrade()){let changed=!matches!(&*pane.model.source.borrow(),Source::Files(old) if *old==files);if changed{*pane.model.source.borrow_mut()=Source::Files(files);pane.model.reload();}}});
            }
        }
    }
    pub(super) fn refresh_tags(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let catalog = self.catalog.clone();
        glib::MainContext::default().spawn_local(async move {
            let Some(b) = weak.upgrade() else {
                return;
            };
            match catalog.tags().await {
                Ok(tags) => {
                    section(&b.tags_box, &tr("Tags"));
                    for tag in tags {
                        let button = gtk::Button::new();
                        let label = gtk::Label::builder()
                            .use_markup(true)
                            .xalign(0.0)
                            .ellipsize(gtk::pango::EllipsizeMode::End)
                            .max_width_chars(18)
                            .build();
                        label.set_markup(&format!(
                            "<span foreground=\"{}\">●</span> {}",
                            glib::markup_escape_text(&tag.color),
                            glib::markup_escape_text(&tag.name)
                        ));
                        button.set_child(Some(&label));
                        button.add_css_class("flat");
                        let weak = Rc::downgrade(&b);
                        button.connect_clicked(move |_| {
                            if let Some(b) = weak.upgrade() {
                                b.open_collection(Location::Tag(tag.id), HashSet::new());
                            }
                        });
                        b.tags_box.append(&button);
                    }
                    let manage = gtk::Button::with_label(&tr("Manage Tags…"));
                    manage.add_css_class("flat");
                    manage.set_action_name(Some("win.manage-tags"));
                    b.tags_box.append(&manage);
                }
                Err(e) => b.report_catalog_error(&e),
            }
        });
    }
    pub(super) fn decorate(self: &Rc<Self>, pane: &Rc<Pane>) {
        if files::is_trash(&pane.model.file) {
            return;
        }
        let volumes: Vec<_> = self
            .volumes
            .mounts()
            .iter()
            .filter_map(|mount| {
                mount
                    .uuid()
                    .or_else(|| mount.volume().and_then(|v| v.uuid()))
                    .map(|id| (mount.root(), id))
            })
            .collect();
        for i in 0..pane.model.store.n_items() {
            if let Some(object) = pane.model.store.item(i) {
                let mut file = object
                    .downcast_ref::<glib::BoxedAnyObject>()
                    .unwrap()
                    .borrow_mut::<FileItem>();
                if file.file.is_native() {
                    if let Some((root, uuid)) = volumes
                        .iter()
                        .filter(|(root, _)| file.file.has_prefix(root))
                        .max_by_key(|(root, _)| root.uri().len())
                    {
                        if let Some(relative) = root.relative_path(&file.file) {
                            file.identity =
                                Some(format!("volume:{uuid}:{}", relative.to_string_lossy()));
                        }
                    }
                }
            }
        }
        let identities: Vec<_> = (0..pane.model.store.n_items())
            .filter_map(|i| pane.model.store.item(i))
            .map(|o| {
                let file = item(&o);
                Identity {
                    uri: file.file.uri().to_string(),
                    stable: file.identity.clone(),
                }
            })
            .collect();
        let catalog = self.catalog.clone();
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        glib::MainContext::default().spawn_local(async move {
            if let Err(e) = catalog.observe(identities).await {
                if let Some(b) = weak.upgrade() {
                    b.report_catalog_error(&e);
                }
                return;
            }
            let Some(pane) = weak_pane.upgrade() else {
                return;
            };
            let objects: Vec<_> = (0..pane.model.store.n_items())
                .filter_map(|i| pane.model.store.item(i))
                .collect();
            let uris: Vec<String> = objects
                .iter()
                .map(|o| item(o).file.uri().to_string())
                .collect();
            let badges = catalog.badges(uris).await;
            if let Ok(badges) = badges {
                let selected: HashSet<_> = pane
                    .selected()
                    .iter()
                    .map(|f| f.file.uri().to_string())
                    .collect();
                let old = pane.restoring.replace(true);
                let mut changed = false;
                for (object, tags) in objects.iter().zip(badges) {
                    let mut file = object
                        .downcast_ref::<glib::BoxedAnyObject>()
                        .unwrap()
                        .borrow_mut::<FileItem>();
                    if file.tags != tags {
                        file.tags = tags;
                        changed = true;
                    }
                }
                if changed {
                    pane.model.store.items_changed(
                        0,
                        pane.model.store.n_items(),
                        pane.model.store.n_items(),
                    );
                    for i in 0..pane.selection.n_items() {
                        if selected.contains(
                            pane.selection
                                .item(i)
                                .map(|o| item(&o).file.uri().to_string())
                                .as_deref()
                                .unwrap_or(""),
                        ) {
                            pane.selection.select_item(i, false);
                        }
                    }
                }
                pane.restoring.set(old);
            }
        });
    }
    fn tags_dialog(self: &Rc<Self>) {
        let files: Vec<_> = self
            .selected()
            .iter()
            .map(|f| Identity {
                uri: f.file.uri().to_string(),
                stable: f.identity.clone(),
            })
            .collect();
        if files.is_empty() {
            return;
        }
        let b = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let tags = match b.catalog.tags().await {
                Ok(t) => t,
                Err(e) => {
                    b.error(&e);
                    return;
                }
            };
            let dialog = adw::AlertDialog::builder()
                .heading(tr("Tags"))
                .body(tr("Changes apply immediately to the selected files."))
                .build();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
            let mut assigned = Vec::new();
            for file in &files {
                assigned.push(b.catalog.labels(file.uri.clone()).await.unwrap_or_default());
            }
            for tag in tags {
                let check = gtk::CheckButton::with_label(&tag.name);
                let count = assigned
                    .iter()
                    .filter(|tags| tags.iter().any(|t| t.id == tag.id))
                    .count();
                check.set_active(count == files.len());
                check.set_inconsistent(count > 0 && count < files.len());
                let files = files.clone();
                let weak = Rc::downgrade(&b);
                check.connect_toggled(move |check| {
                    check.set_inconsistent(false);
                    let files = files.clone();
                    let enabled = check.is_active();
                    let weak = weak.clone();
                    glib::MainContext::default().spawn_local(async move {
                        if let Some(b) = weak.upgrade() {
                            b.edit_tags(
                                tr("Change Tags"),
                                crate::catalog::Edit::Assign {
                                    files,
                                    tags: vec![tag.id],
                                    enabled,
                                },
                            );
                        }
                    });
                });
                content.append(&check);
            }
            dialog.set_extra_child(Some(&content));
            dialog.add_response("close", &tr("Close"));
            dialog.set_close_response("close");
            dialog.present(Some(&b.window));
        });
    }
    fn manage_tags(self: &Rc<Self>) {
        let b = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let tags = match b.catalog.tags().await {
                Ok(t) => t,
                Err(e) => {
                    b.error(&e);
                    return;
                }
            };
            let dialog = adw::AlertDialog::builder()
                .heading(tr("Manage Tags"))
                .body(tr(
                    "Deleting a tag removes its assignments. Files are kept.",
                ))
                .build();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
            for tag in tags.into_iter().map(Some).chain(std::iter::once(None)) {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let entry = gtk::Entry::builder()
                    .placeholder_text(tr("New tag name"))
                    .hexpand(true)
                    .build();
                if let Some(t) = &tag {
                    entry.set_text(&t.name);
                }
                row.append(&entry);
                let colors = [
                    "#e01b24", "#ff7800", "#f6d32d", "#33d17a", "#3584e4", "#9141ac", "#77767b",
                ];
                let color = gtk::DropDown::from_strings(&[
                    &tr("Red"),
                    &tr("Orange"),
                    &tr("Yellow"),
                    &tr("Green"),
                    &tr("Blue"),
                    &tr("Purple"),
                    &tr("Gray"),
                ]);
                if let Some(t) = &tag {
                    color.set_selected(colors.iter().position(|v|*v==t.color).unwrap_or(0)as u32);
                }
                row.append(&color);
                let save = gtk::Button::with_label(&tr("Save"));
                let id = tag.as_ref().map(|t| t.id);
                let weak = Rc::downgrade(&b);
                let d = dialog.downgrade();
                save.connect_clicked(move |_| {
                    let name = entry.text().trim().to_string();
                    if name.is_empty() {
                        return;
                    }
                    let color = colors[color.selected() as usize].to_string();
                    let weak = weak.clone();
                    let d = d.clone();
                    glib::MainContext::default().spawn_local(async move {
                        if let Some(b) = weak.upgrade() {
                            b.edit_tags_complete(
                                tr("Edit Tag"),
                                crate::catalog::Edit::Save { id, name, color },
                                move |success| {
                                    if success {
                                        if let Some(d) = d.upgrade() {
                                            d.force_close();
                                        }
                                    }
                                },
                            );
                        }
                    });
                });
                row.append(&save);
                if let Some(tag) = tag {
                    let delete = gtk::Button::with_label(&tr("Delete"));
                    let weak = Rc::downgrade(&b);
                    let d = dialog.downgrade();
                    delete.connect_clicked(move |_| {
                        let weak = weak.clone();
                        let d = d.clone();
                        glib::MainContext::default().spawn_local(async move {
                            if let Some(b) = weak.upgrade() {
                                b.edit_tags_complete(
                                    tr("Delete Tag"),
                                    crate::catalog::Edit::Delete(tag.id),
                                    move |success| {
                                        if success {
                                            if let Some(d) = d.upgrade() {
                                                d.force_close();
                                            }
                                        }
                                    },
                                );
                            }
                        });
                    });
                    row.append(&delete);
                }
                content.append(&row);
            }
            let scroll = gtk::ScrolledWindow::builder()
                .child(&content)
                .max_content_height(450)
                .propagate_natural_height(true)
                .build();
            dialog.set_extra_child(Some(&scroll));
            dialog.add_response("close", &tr("Close"));
            dialog.set_close_response("close");
            dialog.present(Some(&b.window));
        });
    }
    pub(super) fn add_task(
        self: &Rc<Self>,
        operation: &Operation,
        cancel: &gio::Cancellable,
    ) -> Rc<TaskRow> {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.set_hexpand(true);
        let id = self.task_rows.borrow().len() + 1;
        let label = gtk::Label::builder()
            .label(format!("#{id} · {}", operation.title()))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(60)
            .build();
        let progress = gtk::ProgressBar::new();
        text.append(&label);
        text.append(&progress);
        row.append(&text);
        let button = gtk::Button::with_label(&tr("Cancel"));
        let c = cancel.clone();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.cancel_job(&c);
            }
        });
        row.append(&button);
        let retry = gtk::Button::with_label(&tr("Retry Failed Items"));
        retry.set_visible(false);
        row.append(&retry);
        let task = Rc::new(TaskRow {
            title: format!("#{id} · {}", operation.title()),
            operation: operation.clone(),
            token: cancel.clone(),
            label,
            progress,
            cancel: button,
            retry: retry.clone(),
            failed: RefCell::new(Vec::new()),
            done: Cell::new(false),
        });
        let weak = Rc::downgrade(self);
        let t = Rc::downgrade(&task);
        retry.connect_clicked(move |retry| {
            if let (Some(b), Some(t)) = (weak.upgrade(), t.upgrade()) {
                retry.set_sensitive(false);
                for operation in t.failed.borrow().clone() {
                    b.enqueue(operation);
                }
            }
        });
        self.task_list.append(&row);
        self.task_rows.borrow_mut().push(task.clone());
        task
    }
    pub(super) fn submit(
        self: &Rc<Self>,
        operation: Operation,
        cancel: gio::Cancellable,
        events: async_channel::Sender<Event>,
    ) {
        let b = self.clone();
        glib::MainContext::default().spawn_local(async move {
            for file in operation.locations() {
                if cancel.is_cancelled() {
                    break;
                }
                if let Err(error) = b.connections.ensure(&file, &b.window, &cancel).await {
                    let _ = events
                        .send(Event::Failed {
                            operation: operation.clone(),
                            error: error.clone(),
                        })
                        .await;
                    let _ = events
                        .send(Event::Finished {
                            completed: 0,
                            skipped: 0,
                            errors: if error.kind == ErrorKind::Cancelled {
                                vec![]
                            } else {
                                vec![error.message()]
                            },
                            cancelled: error.kind == ErrorKind::Cancelled,
                        })
                        .await;
                    return;
                }
            }
            let fallback_tags = match b.catalog.fallback_tags().await {
                Ok(tags) => tags,
                Err(error) => {
                    b.report_catalog_error(&error);
                    Default::default()
                }
            };
            if b.worker
                .send(Job {
                    fallback_tags,
                    operation,
                    cancel,
                    events: events.clone(),
                })
                .is_err()
            {
                let _ = events
                    .send(Event::Finished {
                        completed: 0,
                        skipped: 0,
                        errors: vec![tr("The file operation worker is unavailable.")],
                        cancelled: false,
                    })
                    .await;
            }
        });
    }
}
