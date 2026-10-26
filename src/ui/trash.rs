// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! The GIO trash backend owns trash storage, metadata and mounted-volume handling.
use super::*;
impl Browser {
    pub(super) fn in_trash(&self) -> bool {
        self.active_pane()
            .is_some_and(|pane| files::is_trash(&pane.model.file))
    }
    pub(super) fn install_trash_actions(self: &Rc<Self>, app: &adw::Application) {
        self.action(app, "restore", &[], |b| b.restore_trash(false));
        self.action(app, "restore-to", &[], |b| b.restore_trash(true));
        self.action(app, "empty-trash", &[], |b| b.empty_trash());
    }
    pub(super) fn restore_trash(self: &Rc<Self>, choose_folder: bool) {
        if !self.in_trash() {
            return;
        }
        let sources = self.selected_files();
        if sources.is_empty() {
            return;
        }
        if !choose_folder {
            self.enqueue(Operation::Restore {
                sources,
                destination: None,
            });
            return;
        }
        let dialog = gtk::FileDialog::builder()
            .title(tr("Restore To"))
            .accept_label(tr("Restore"))
            .initial_folder(&gio::File::for_path(glib::home_dir()))
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        let window = self.window.clone();
        glib::MainContext::default().spawn_local(async move {
            match dialog.select_folder_future(Some(&window)).await {
                Ok(destination) => {
                    if let Some(b) = weak.upgrade() {
                        b.enqueue(Operation::Restore {
                            sources,
                            destination: Some(destination),
                        });
                    }
                }
                Err(e)
                    if !e.matches(gtk::DialogError::Dismissed)
                        && !e.matches(gtk::DialogError::Cancelled)
                        && !e.matches(gio::IOErrorEnum::Cancelled) =>
                {
                    if let Some(b) = weak.upgrade() {
                        b.error(&e.to_string());
                    }
                }
                _ => {}
            }
        });
    }
    fn empty_trash(self: &Rc<Self>) {
        if !self.in_trash() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let snapshot = async {
                let root = gio::File::for_uri("trash:///");
                let enumerator = root.enumerate_children_future("standard::name", gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS, glib::Priority::DEFAULT).await?;
                let mut files = Vec::new();
                loop {
                    let batch = enumerator.next_files_future(128, glib::Priority::DEFAULT).await?;
                    if batch.is_empty() { break; }
                    files.extend(batch.into_iter().map(|info| root.child(info.name())));
                }
                enumerator.close_future(glib::Priority::DEFAULT).await?;
                Ok::<_, glib::Error>(files)
            }.await;
            let Some(browser) = weak.upgrade() else { return; };
            match snapshot {
                Ok(files) if !files.is_empty() => {
                    let dialog = adw::AlertDialog::builder().heading(tr("Empty Trash?"))
                        .body(tr("Permanently delete all {count} items in Trash? This cannot be undone.").replace("{count}", &files.len().to_string())).build();
                    dialog.add_responses(&[("cancel", &tr("Cancel")), ("empty", &tr("Empty Trash"))]);
                    dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    let weak = Rc::downgrade(&browser);
                    dialog.connect_response(None, move |_, response| {
                        if response == "empty" { if let Some(b) = weak.upgrade() {
                            b.enqueue(Operation::DeleteTrash(files.clone()));
                        }}
                    });
                    dialog.present(Some(&browser.window));
                },
                Err(error) => browser.error(&error.to_string()),
                _ => {},
            }
        });
    }
}

pub(super) fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for (label, action) in [
        (mark("Properties"), "properties"),
        (mark("Restore"), "restore"),
        (mark("Restore To…"), "restore-to"),
        (mark("Select All"), "select-all"),
        (mark("Delete Permanently…"), "delete"),
        (mark("Empty Trash…"), "empty-trash"),
    ] {
        menu.append(Some(&tr(label)), Some(&format!("win.{action}")));
    }
    menu
}
