// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use super::*;
use crate::dnd;

struct Hover {
    drop: gdk::Drop,
    files: Option<Vec<gio::File>>,
    same: HashMap<String, bool>,
    pending: HashSet<String>,
}
type HoverState = Rc<RefCell<Option<Hover>>>;

fn probe(state: &HoverState, destination: &gio::File) {
    let uri = destination.uri().to_string();
    let work = {
        let mut state = state.borrow_mut();
        let Some(hover) = state.as_mut() else {
            return;
        };
        if hover.same.contains_key(&uri) || hover.pending.contains(&uri) {
            return;
        }
        let Some(files) = hover.files.clone() else {
            return;
        };
        hover.pending.insert(uri.clone());
        (files, hover.drop.clone())
    };
    let destination = destination.clone();
    let state = state.clone();
    glib::MainContext::default().spawn_local(async move {
        let same = dnd::same_filesystem(&work.0, &destination).await;
        if let Some(hover) = state.borrow_mut().as_mut().filter(|h| h.drop == work.1) {
            hover.pending.remove(&uri);
            hover.same.insert(uri, same);
        }
    });
}

fn position_at(widget: &gtk::Widget, x: f64, y: f64) -> Option<(u32, gtk::Widget)> {
    let mut current = widget.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(child) = current {
        if let Some(index) = child
            .widget_name()
            .strip_prefix("finder-item-")
            .and_then(|s| s.parse().ok())
        {
            return Some((index, child));
        }
        if child == *widget {
            break;
        }
        current = child.parent();
    }
    None
}

type Destination = Rc<dyn Fn(f64, f64) -> Option<gio::File>>;
type Leave = Option<Rc<dyn Fn()>>;
pub(super) struct DropBinding {
    widget: glib::WeakRef<gtk::Widget>,
    target: gtk::DropTargetAsync,
    destination: Destination,
    leave: Leave,
    state: HoverState,
}

impl Browser {
    pub(super) fn install_file_dnd(self: &Rc<Self>, pane: &Rc<Pane>, widget: &gtk::Widget) {
        if files::is_trash(&pane.model.file) {
            return;
        }
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        let weak_widget = widget.downgrade();
        source.connect_prepare(move |_, x, y| {
            let (Some(browser), Some(pane), Some(widget)) =
                (weak.upgrade(), weak_pane.upgrade(), weak_widget.upgrade())
            else {
                return None;
            };
            let (position, _) = position_at(&widget, x, y)?;
            browser.activate_pane(&pane);
            if !pane.selection.is_selected(position) {
                pane.selection.select_item(position, true);
            }
            let files = pane
                .selected()
                .into_iter()
                .map(|i| i.file)
                .collect::<Vec<_>>();
            if files.is_empty() {
                None
            } else {
                Some(dnd::provider(&files))
            }
        });
        let weak = Rc::downgrade(self);
        source.connect_drag_end(move |_, _, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(browser) = weak.upgrade() {
                    browser.rebuild_drop_targets();
                }
            });
        });
        // A URI drag source never deletes files in drag-end. The receiving
        // FileOperationService owns copy/move and removes only copied sources.
        widget.add_controller(source);
        let weak_pane = Rc::downgrade(pane);
        let weak_widget = widget.downgrade();
        self.add_drop_target(widget, move |x, y| {
            let pane = weak_pane.upgrade()?;
            let widget = weak_widget.upgrade()?;
            if let Some((position, _)) = position_at(&widget, x, y) {
                if let Some(object) = pane.selection.item(position) {
                    let file = item(&object);
                    if file.is_dir {
                        return Some(file.file);
                    }
                }
            }
            Some(pane.model.file.clone())
        });
    }

    pub(super) fn add_drop_target(
        self: &Rc<Self>,
        widget: &impl IsA<gtk::Widget>,
        destination: impl Fn(f64, f64) -> Option<gio::File> + 'static,
    ) {
        self.bind_drop_target(widget.as_ref(), Rc::new(destination), None);
    }

    fn rebuild_drop_targets(self: &Rc<Self>) {
        // GTK 4.14 can retain a finished drop in ancestor controllers. Rebuild
        // bindings between drags through public APIs, including inactive tabs.
        let bindings = std::mem::take(&mut *self.drop_bindings.borrow_mut());
        for binding in bindings {
            if let Some(widget) = binding.widget.upgrade() {
                let drop = binding.state.borrow().as_ref().map(|h| h.drop.clone());
                if let Some(drop) = drop {
                    binding.target.emit_by_name::<()>("drag-leave", &[&drop]);
                }
                widget.remove_controller(&binding.target);
                self.bind_drop_target(&widget, binding.destination, binding.leave);
            }
        }
    }

    fn bind_drop_target(
        self: &Rc<Self>,
        widget: &gtk::Widget,
        destination: Destination,
        leave: Leave,
    ) {
        let binding_destination = destination.clone();
        let formats = gdk::ContentFormats::builder()
            .add_type(gdk::FileList::static_type())
            .add_mime_type("text/uri-list")
            .build();
        let target =
            gtk::DropTargetAsync::new(Some(formats), gdk::DragAction::COPY | gdk::DragAction::MOVE);
        target.set_propagation_phase(if widget.is::<adw::TabBar>() {
            gtk::PropagationPhase::Bubble
        } else {
            gtk::PropagationPhase::Capture
        });
        let state: HoverState = Rc::new(RefCell::new(None));
        let highlight: Rc<RefCell<Option<gtk::Widget>>> = Rc::new(RefCell::new(None));
        let coordinates = Rc::new(Cell::new((0.0, 0.0)));
        let feedback_state = state.clone();
        let resolver = destination.clone();
        let hovered = highlight.clone();
        let weak_widget = widget.downgrade();
        let position = coordinates.clone();
        let feedback = Rc::new(
            move |controller: &gtk::DropTargetAsync, drop: &gdk::Drop, x, y| {
                position.set((x, y));
                if let Some(old) = hovered.borrow_mut().take() {
                    old.remove_css_class("drop-target");
                }
                let Some(destination) = resolver(x, y) else {
                    return gdk::DragAction::empty();
                };
                let permitted = feedback_state
                    .borrow()
                    .as_ref()
                    .filter(|h| h.drop == *drop)
                    .and_then(|h| h.files.as_ref())
                    .is_some_and(|files| !dnd::is_source_folder(files, &destination));
                if !permitted {
                    return gdk::DragAction::empty();
                }
                if !files::is_trash(&destination) {
                    probe(&feedback_state, &destination);
                }
                let same = feedback_state
                    .borrow()
                    .as_ref()
                    .and_then(|h| h.same.get(destination.uri().as_str()).copied())
                    .unwrap_or(false);
                let action = if files::is_trash(&destination) {
                    dnd::trash_action(drop.actions(), controller.current_event_state())
                } else {
                    dnd::action(drop.actions(), controller.current_event_state(), same)
                };
                if !action.is_empty() {
                    if let Some(widget) = weak_widget.upgrade() {
                        let row = position_at(&widget, x, y)
                            .map(|(_, row)| row)
                            .unwrap_or(widget);
                        row.add_css_class("drop-target");
                        *hovered.borrow_mut() = Some(row);
                    }
                }
                action
            },
        );
        let state_enter = state.clone();
        let enter_feedback = feedback.clone();
        let loaded_position = coordinates.clone();
        target.connect_drag_enter(move |controller, drop, x, y| {
            loaded_position.set((x, y));
            *state_enter.borrow_mut() = Some(Hover {
                drop: drop.clone(),
                files: None,
                same: HashMap::new(),
                pending: HashSet::new(),
            });
            let state = state_enter.clone();
            let drop_clone = drop.clone();
            let controller = controller.clone();
            let refresh = enter_feedback.clone();
            let position = loaded_position.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Ok(files) = dnd::read_files(&drop_clone).await {
                    let current = {
                        let mut state = state.borrow_mut();
                        if let Some(hover) = state.as_mut().filter(|h| h.drop == drop_clone) {
                            hover.files = Some(files);
                            true
                        } else {
                            false
                        }
                    };
                    if current {
                        let (x, y) = position.get();
                        let action = refresh(&controller, &drop_clone, x, y);
                        drop_clone.status(
                            if action.is_empty() {
                                gdk::DragAction::empty()
                            } else {
                                drop_clone.actions()
                            },
                            action,
                        );
                    }
                }
            });
            gdk::DragAction::empty()
        });
        let motion_feedback = feedback.clone();
        target.connect_drag_motion(move |controller, drop, x, y| {
            motion_feedback(controller, drop, x, y)
        });
        let hovered = highlight.clone();
        let state_leave = state.clone();
        target.connect_drag_leave(move |_, _| {
            state_leave.borrow_mut().take();
            if let Some(old) = hovered.borrow_mut().take() {
                old.remove_css_class("drop-target");
            }
        });
        let weak = Rc::downgrade(self);
        let state_drop = state.clone();
        target.connect_drop(move |controller, drop, x, y| {
            let Some(destination) = destination(x, y) else {
                return false;
            };
            let files = state_drop
                .borrow()
                .as_ref()
                .filter(|h| h.drop == *drop)
                .and_then(|h| h.files.clone());
            if files
                .as_ref()
                .is_some_and(|files| dnd::is_source_folder(files, &destination))
            {
                return false;
            }
            let modifiers = controller.current_event_state();
            let drop = drop.clone();
            let weak = weak.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = match files {
                    Some(files) => Ok(files),
                    None => dnd::read_files(&drop).await,
                };
                let Some(browser) = weak.upgrade() else {
                    drop.finish(gdk::DragAction::empty());
                    return;
                };
                match result {
                    Ok(files) => browser.receive_drop(
                        files,
                        destination,
                        modifiers,
                        drop.actions(),
                        Some(drop),
                    ),
                    Err(error) => {
                        drop.finish(gdk::DragAction::empty());
                        browser.error(&error);
                    }
                }
            });
            true
        });
        // Scroll continuously while the pointer stays near an edge.
        let weak_widget = widget.downgrade();
        let state_scroll = state.clone();
        let timer = Rc::new(RefCell::new(None::<glib::SourceId>));
        let timer_enter = timer.clone();
        target.connect_drag_enter(move |controller, drop, x, y| {
            if let Some(old) = timer_enter.borrow_mut().take() {
                old.remove();
            }
            let widget = weak_widget.clone();
            let state = state_scroll.clone();
            let position = coordinates.clone();
            *timer_enter.borrow_mut() = Some(glib::timeout_add_local(
                std::time::Duration::from_millis(30),
                move || {
                    let Some(widget) = widget.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    if state.borrow().is_none() {
                        return glib::ControlFlow::Continue;
                    }
                    let (_, y) = position.get();
                    let height = f64::from(widget.height());
                    let delta = if y < 28.0 {
                        -18.0
                    } else if y > height - 28.0 {
                        18.0
                    } else {
                        return glib::ControlFlow::Continue;
                    };
                    let mut ancestor = widget.parent();
                    while let Some(widget) = ancestor {
                        if let Some(scroll) = widget.downcast_ref::<gtk::ScrolledWindow>() {
                            let a = scroll.vadjustment();
                            a.set_value(
                                (a.value() + delta)
                                    .clamp(a.lower(), (a.upper() - a.page_size()).max(a.lower())),
                            );
                            break;
                        }
                        ancestor = widget.parent();
                    }
                    glib::ControlFlow::Continue
                },
            ));
            feedback(controller, drop, x, y)
        });
        target.connect_drag_leave(move |_, _| {
            if let Some(timer) = timer.borrow_mut().take() {
                timer.remove();
            }
        });
        if let Some(callback) = leave.clone() {
            target.connect_drag_leave(move |_, _| callback());
        }
        widget.add_controller(target.clone());
        self.drop_bindings.borrow_mut().push(DropBinding {
            widget: widget.downgrade(),
            target,
            destination: binding_destination,
            leave,
            state,
        });
    }

    pub(super) fn receive_drop(
        self: &Rc<Self>,
        files: Vec<gio::File>,
        destination: gio::File,
        modifiers: gdk::ModifierType,
        allowed: gdk::DragAction,
        drop: Option<gdk::Drop>,
    ) {
        if let Err(error) = dnd::validate(&files) {
            if let Some(drop) = drop {
                drop.finish(gdk::DragAction::empty());
            }
            self.error(&error);
            return;
        }
        if dnd::is_source_folder(&files, &destination) {
            if let Some(drop) = drop {
                drop.finish(gdk::DragAction::empty());
            }
            return;
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let trash = files::is_trash(&destination);
            let action = if trash {
                dnd::trash_action(allowed, modifiers)
            } else {
                let same = dnd::same_filesystem(&files, &destination).await;
                dnd::action(allowed, modifiers, same)
            };
            let Some(browser) = weak.upgrade() else {
                if let Some(drop) = drop {
                    drop.finish(gdk::DragAction::empty());
                }
                return;
            };
            if action.is_empty() {
                if let Some(drop) = drop {
                    drop.finish(action);
                }
                return;
            }
            let weak = Rc::downgrade(&browser);
            let completion = drop.map(|drop| {
                dnd::Completion::new(
                    drop,
                    action,
                    Some(Box::new(move || {
                        glib::idle_add_local_once(move || {
                            if let Some(browser) = weak.upgrade() {
                                browser.rebuild_drop_targets();
                            }
                        });
                    })),
                )
            });
            browser.enqueue_complete(
                if trash {
                    Operation::Trash(files)
                } else {
                    Operation::Copy {
                        sources: files,
                        destination,
                        moving: action == gdk::DragAction::MOVE,
                    }
                },
                None,
                completion,
            );
        });
    }

    pub(super) fn install_tab_drop(self: &Rc<Self>, bar: &adw::TabBar) {
        // libadwaita supplies the page identity and hover preload through its
        // public API; our async target owns the actual drop.
        bar.setup_extra_drop_target(
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
            &[gdk::FileList::static_type()],
        );
        bar.set_extra_drag_preload(true);
        let hovered: Rc<RefCell<Option<adw::TabPage>>> = Rc::new(RefCell::new(None));
        let timer = Rc::new(RefCell::new(None::<glib::SourceId>));
        let weak = Rc::downgrade(self);
        let hover = hovered.clone();
        let hover_timer = timer.clone();
        bar.connect_extra_drag_value(move |bar, page, value| {
            if value.is_none() {
                return gdk::DragAction::empty();
            }
            // Let libadwaita identify the tab, then hand ownership to our
            // async ancestor target so finish waits for the file operation.
            fn hand_off(widget: &gtk::Widget) {
                let controllers = widget.observe_controllers();
                for i in 0..controllers.n_items() {
                    if let Some(target) = controllers
                        .item(i)
                        .and_then(|o| o.downcast::<gtk::DropTarget>().ok())
                        .filter(|t| t.current_drop().is_some())
                    {
                        glib::idle_add_local_once(move || {
                            if target.current_drop().is_some() {
                                target.reject();
                            }
                        });
                    }
                }
                let mut child = widget.first_child();
                while let Some(widget) = child {
                    hand_off(&widget);
                    child = widget.next_sibling();
                }
            }
            hand_off(bar.upcast_ref());
            let Some(browser) = weak.upgrade() else {
                return gdk::DragAction::empty();
            };
            let entry = browser.entry_for_page(page);
            let destination = entry.and_then(|e| e.location.file());
            let files = value
                .and_then(|v| v.get::<gdk::FileList>().ok())
                .map(|f| f.files());
            if destination
                .as_ref()
                .zip(files.as_ref())
                .is_none_or(|(destination, files)| dnd::is_source_folder(files, destination))
            {
                hover.borrow_mut().take();
                if let Some(id) = hover_timer.borrow_mut().take() {
                    id.remove();
                }
                return gdk::DragAction::empty();
            }
            if hover.borrow().as_ref() != Some(page) {
                *hover.borrow_mut() = Some(page.clone());
                if let Some(id) = hover_timer.borrow_mut().take() {
                    id.remove();
                }
                let weak = Rc::downgrade(&browser);
                let fired_timer = hover_timer.clone();
                let page = page.clone();
                *hover_timer.borrow_mut() = Some(glib::timeout_add_local_once(
                    std::time::Duration::from_millis(600),
                    move || {
                        fired_timer.borrow_mut().take();
                        if let Some(browser) = weak.upgrade() {
                            if browser.tab_view.page_position(&page) >= 0 {
                                browser.tab_view.set_selected_page(&page);
                            }
                        }
                    },
                ));
            }
            gdk::DragAction::COPY
        });
        let weak = Rc::downgrade(self);
        let drop_hover = hovered.clone();
        self.bind_drop_target(
            bar.upcast_ref(),
            Rc::new(move |_, _| {
                let browser = weak.upgrade()?;
                let page = hovered.borrow().clone()?;
                browser.entry_for_page(&page)?.location.file()
            }),
            Some(Rc::new(move || {
                drop_hover.borrow_mut().take();
                if let Some(id) = timer.borrow_mut().take() {
                    id.remove();
                }
            })),
        );
        bar.connect_extra_drag_drop(|_, _, _| false);
    }

    pub(super) fn entry_for_page(&self, page: &adw::TabPage) -> Option<NavEntry> {
        if *page == *self.current_tab.borrow() {
            return Some(self.capture_entry());
        }
        self.tabs
            .borrow()
            .iter()
            .find(|tab| tab.page == *page)
            .and_then(|tab| tab.state.borrow().as_ref().map(Self::entry_for_state))
    }
}
