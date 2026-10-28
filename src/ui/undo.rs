// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT
use super::*;
use crate::{operation_history::Step, tag_storage::Patch};
use std::collections::VecDeque;

type Pending = Box<dyn FnOnce(&Rc<Browser>)>;
#[derive(Default)]
pub(super) struct UndoHistory {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    pub busy: bool,
    queue: VecDeque<Pending>,
    replay: Option<(bool, String)>,
}
impl UndoHistory {
    pub fn clear_pending(&mut self) {
        self.queue.clear();
    }
}
struct Entry {
    title: String,
    action: Change,
}
enum Change {
    Files(Vec<Step>),
    Tags(Patch),
}

impl Browser {
    pub(super) fn serialize(self: &Rc<Self>, task: impl FnOnce(&Rc<Self>) + 'static) {
        if self.undo_history.borrow().busy {
            self.undo_history
                .borrow_mut()
                .queue
                .push_back(Box::new(task));
        } else {
            self.undo_history.borrow_mut().busy = true;
            self.update_undo_actions();
            task(self);
        }
    }
    pub(super) fn finish_serial(self: &Rc<Self>) {
        let next = self.undo_history.borrow_mut().queue.pop_front();
        if let Some(next) = next {
            next(self);
        } else {
            self.undo_history.borrow_mut().busy = false;
        }
        self.update_undo_actions();
        if self.closing.get() && !self.undo_history.borrow().busy {
            self.window.close();
        }
    }
    fn push_undo(&self, entry: Entry) {
        let mut history = self.undo_history.borrow_mut();
        history.redo.clear();
        history.undo.push(entry);
        if history.undo.len() > 100 {
            history.undo.remove(0);
        }
    }
    pub(super) fn record_file_history(
        &self,
        title: String,
        steps: Vec<Step>,
        remaining: Vec<Step>,
        irreversible: bool,
        unrecordable: bool,
    ) {
        let replay = self.undo_history.borrow_mut().replay.take();
        if irreversible {
            let mut history = self.undo_history.borrow_mut();
            history.undo.clear();
            history.redo.clear();
            drop(history);
            if unrecordable {
                self.error(&tr("This change could not be recorded for Undo. The operation history was cleared."));
            }
        } else if let Some((redo, title)) = replay {
            let mut history = self.undo_history.borrow_mut();
            if !remaining.is_empty() {
                let entry = Entry {
                    title: title.clone(),
                    action: Change::Files(remaining),
                };
                if redo {
                    history.redo.push(entry);
                } else {
                    history.undo.push(entry);
                }
            }
            if !steps.is_empty() {
                let entry = Entry {
                    title,
                    action: Change::Files(steps),
                };
                if redo {
                    history.undo.push(entry);
                } else {
                    history.redo.push(entry);
                }
            }
        } else if !steps.is_empty() {
            self.push_undo(Entry {
                title,
                action: Change::Files(steps),
            });
        }
        self.update_undo_actions();
    }
    pub(super) fn update_undo_actions(&self) {
        let history = self.undo_history.borrow();
        for (name, action) in self.actions.borrow().iter() {
            if name == "undo" {
                action.set_enabled(!history.busy && !history.undo.is_empty());
            }
            if name == "redo" {
                action.set_enabled(!history.busy && !history.redo.is_empty());
            }
        }
        let menu = &self.undo_menu;
        menu.remove_all();
        let undo = history
            .undo
            .last()
            .map(|e| tr("Undo {action}").replace("{action}", &e.title))
            .unwrap_or_else(|| tr("Undo"));
        let redo = history
            .redo
            .last()
            .map(|e| tr("Redo {action}").replace("{action}", &e.title))
            .unwrap_or_else(|| tr("Redo"));
        menu.append(Some(&undo), Some("win.undo"));
        menu.append(Some(&redo), Some("win.redo"));
    }
    pub(super) fn undo(self: &Rc<Self>, redo: bool) {
        if self.text_focused() {
            if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&self.window) {
                let name = if redo { "text.redo" } else { "text.undo" };
                if focus.activate_action(name, None).is_err() {
                    let _ = focus
                        .activate_action(if redo { "buffer.redo" } else { "buffer.undo" }, None);
                }
            }
            return;
        }
        if self.undo_history.borrow().busy {
            return;
        }
        let entry = {
            let mut history = self.undo_history.borrow_mut();
            if redo {
                history.redo.pop()
            } else {
                history.undo.pop()
            }
        };
        let Some(entry) = entry else { return };
        self.undo_history.borrow_mut().busy = true;
        self.update_undo_actions();
        match entry.action {
            Change::Files(steps) => {
                self.undo_history.borrow_mut().replay = Some((redo, entry.title.clone()));
                self.enqueue_complete_now(
                    Operation::Replay {
                        title: entry.title,
                        steps,
                    },
                    None,
                    None,
                );
            }
            Change::Tags(patch) => {
                let b = self.clone();
                glib::MainContext::default().spawn_local(async move {
                    match b.catalog.apply_patch(patch.clone()).await {
                        Ok(inverse) => {
                            let inverse = Entry {
                                title: entry.title,
                                action: Change::Tags(inverse),
                            };
                            let mut history = b.undo_history.borrow_mut();
                            if redo {
                                history.undo.push(inverse);
                            } else {
                                history.redo.push(inverse);
                            }
                        }
                        Err(error) => {
                            let entry = Entry {
                                title: entry.title,
                                action: Change::Tags(patch),
                            };
                            let mut history = b.undo_history.borrow_mut();
                            if redo {
                                history.redo.push(entry);
                            } else {
                                history.undo.push(entry);
                            }
                            drop(history);
                            b.error(&error);
                        }
                    }
                    b.tags_changed().await;
                    b.finish_serial();
                });
            }
        }
    }
    pub(super) fn edit_tags(self: &Rc<Self>, title: String, edit: crate::catalog::Edit) {
        self.edit_tags_complete(title, edit, |_| {});
    }
    pub(super) fn edit_tags_complete(
        self: &Rc<Self>,
        title: String,
        edit: crate::catalog::Edit,
        complete: impl FnOnce(bool) + 'static,
    ) {
        self.serialize(move |b| {
            let b = b.clone();
            glib::MainContext::default().spawn_local(async move {
                let succeeded = match b.catalog.edit(edit).await {
                    Ok(patch) => {
                        if !patch.is_empty() {
                            b.push_undo(Entry {
                                title,
                                action: Change::Tags(patch),
                            });
                        }
                        true
                    }
                    Err(error) => {
                        b.error(&error);
                        false
                    }
                };
                complete(succeeded);
                b.tags_changed().await;
                b.finish_serial();
            });
        });
    }
    async fn tags_changed(self: &Rc<Self>) {
        self.refresh_tags();
        self.refresh_tag_views();
        for pane in self.all_panes() {
            self.decorate(&pane);
        }
        self.notify_local_tags().await;
    }
    pub(super) async fn notify_local_tags(&self) {
        if self.catalog.pending_count().await.unwrap_or(0) > 0
            && !self.local_tags_notice.replace(true)
        {
            self.error(&tr("Some tags are stored locally because file attributes could not be written. They will be retried when the files are available."));
        }
    }
}
