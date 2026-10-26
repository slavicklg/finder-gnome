// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use adw::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

use crate::i18n::tr;

#[derive(Clone)]
struct ApplicationItem {
    info: gio::AppInfo,
    key: glib::CollationKey,
    search: String,
}

fn application(object: &glib::Object) -> ApplicationItem {
    object
        .downcast_ref::<glib::BoxedAnyObject>()
        .expect("application model item")
        .borrow::<ApplicationItem>()
        .clone()
}

pub struct ApplicationsView {
    pub widget: gtk::Box,
    pub(crate) search: gtk::SearchEntry,
    pub(crate) selection: gtk::SingleSelection,
    store: gio::ListStore,
    filtered: gtk::FilterListModel,
    filter: gtk::CustomFilter,
    query: Rc<RefCell<String>>,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    window: glib::WeakRef<adw::ApplicationWindow>,
    changed: Box<dyn Fn(u32, bool)>,
    error: Rc<dyn Fn(&str)>,
    pending: Cell<bool>,
    context_menu: RefCell<Option<gtk::PopoverMenu>>,
    _monitor: gio::AppInfoMonitor,
}

impl ApplicationsView {
    pub fn new(
        window: &adw::ApplicationWindow,
        changed: impl Fn(u32, bool) + 'static,
        error: impl Fn(&str) + 'static,
    ) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(true);
        widget.set_vexpand(true);
        let search = gtk::SearchEntry::builder()
            .placeholder_text(tr("Search applications"))
            .css_classes(["applications-search"])
            .build();
        widget.append(&search);
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let query = Rc::new(RefCell::new(String::new()));
        let text = query.clone();
        let filter = gtk::CustomFilter::new(move |object| {
            application(object).search.contains(text.borrow().as_str())
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filtered.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let empty = adw::StatusPage::builder()
            .title(tr("No applications installed"))
            .icon_name("application-x-executable-symbolic")
            .build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(
            &adw::StatusPage::builder()
                .title(tr("Loading applications…"))
                .icon_name("application-x-executable-symbolic")
                .build(),
            Some("loading"),
        );
        stack.set_visible_child_name("loading");
        widget.append(&stack);
        let monitor = gio::AppInfoMonitor::get();
        let view = Rc::new(Self {
            widget,
            search,
            selection,
            store,
            filtered,
            filter,
            query,
            stack,
            empty,
            window: window.downgrade(),
            changed: Box::new(changed),
            error: Rc::new(error),
            pending: Cell::new(false),
            context_menu: RefCell::new(None),
            _monitor: monitor.clone(),
        });
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(&view);
        factory.connect_setup(move |_, object| {
            let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("application-row");
            let icon = gtk::Image::new();
            icon.set_pixel_size(32);
            row.append(&icon);
            let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
            labels.set_hexpand(true);
            labels.append(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["heading"])
                    .build(),
            );
            labels.append(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["dim-label"])
                    .build(),
            );
            row.append(&labels);
            let launch = gtk::Button::with_label(&tr("Launch"));
            launch.set_valign(gtk::Align::Center);
            let context_view = weak.clone();
            let context_item = list_item.downgrade();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);
            gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
            gesture.connect_pressed(move |gesture, _, x, y| {
                if let (Some(view), Some(item)) = (context_view.upgrade(), context_item.upgrade()) {
                    view.selection.set_selected(item.position());
                    if let Some(widget) = gesture.widget() {
                        view.popup(&widget, x, y);
                    }
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            });
            row.add_controller(gesture);
            let weak = weak.clone();
            let weak_item = list_item.downgrade();
            launch.connect_clicked(move |_| {
                if let (Some(view), Some(item)) = (weak.upgrade(), weak_item.upgrade()) {
                    if let Some(object) = item.item() {
                        view.launch(application(&object).info);
                    }
                }
            });
            row.append(&launch);
            list_item.set_child(Some(&row));
        });
        factory.connect_bind(|_, object| {
            let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let app = application(&list_item.item().unwrap()).info;
            let row = list_item.child().unwrap();
            let icon = row.first_child().unwrap().downcast::<gtk::Image>().unwrap();
            icon.set_from_gicon(
                &app.icon()
                    .unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast()),
            );
            let labels = icon.next_sibling().unwrap();
            let name = labels
                .first_child()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap();
            name.set_label(&app.display_name());
            let description = name
                .next_sibling()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap();
            let subtitle = app.description().or_else(|| app.id()).unwrap_or_default();
            description.set_label(&subtitle);
            description.set_visible(!subtitle.is_empty());
            row.set_tooltip_text(app.id().as_deref());
        });
        let list = gtk::ListView::new(Some(view.selection.clone()), Some(factory));
        list.set_single_click_activate(false);
        let weak = Rc::downgrade(&view);
        list.connect_activate(move |_, position| {
            if let Some(view) = weak.upgrade() {
                if let Some(object) = view.selection.item(position) {
                    view.launch(application(&object).info);
                }
            }
        });
        view.stack.add_named(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .build(),
            Some("list"),
        );
        let weak = Rc::downgrade(&view);
        view.search.connect_search_changed(move |search| {
            if let Some(view) = weak.upgrade() {
                *view.query.borrow_mut() = glib::casefold(search.text().trim()).to_string();
                view.filter.changed(gtk::FilterChange::Different);
                view.update();
            }
        });
        let weak = Rc::downgrade(&view);
        view.search.connect_activate(move |_| {
            if let Some(view) = weak.upgrade() {
                view.launch_selected();
            }
        });
        let weak = Rc::downgrade(&view);
        view.selection.connect_selected_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.update();
            }
        });
        let weak = Rc::downgrade(&view);
        monitor.connect_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.reload();
            }
        });
        view.reload();
        view
    }

    pub fn reload(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(view) = weak.upgrade() {
                view.pending.set(false);
                view.replace_apps(gio::AppInfo::all());
            }
        });
    }

    pub(crate) fn replace_apps(&self, apps: Vec<gio::AppInfo>) {
        let selected = self
            .selection
            .selected_item()
            .and_then(|o| application(&o).info.id());
        let mut seen = HashSet::new();
        let mut apps: Vec<_> = apps
            .into_iter()
            .filter(|app| app.should_show())
            .filter(|app| app.id().is_none_or(|id| seen.insert(id.to_string())))
            .map(|info| ApplicationItem {
                key: glib::CollationKey::from(info.display_name()),
                search: glib::casefold(format!(
                    "{} {} {}",
                    info.display_name(),
                    info.description().unwrap_or_default(),
                    info.id().unwrap_or_default()
                ))
                .to_string(),
                info,
            })
            .collect();
        apps.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| a.info.id().cmp(&b.info.id()))
        });
        let objects: Vec<_> = apps.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objects);
        if let Some(selected) = selected {
            for index in 0..self.selection.n_items() {
                if self
                    .selection
                    .item(index)
                    .is_some_and(|o| application(&o).info.id().as_ref() == Some(&selected))
                {
                    self.selection.set_selected(index);
                    break;
                }
            }
        }
        self.update();
    }

    pub fn count(&self) -> u32 {
        self.filtered.n_items()
    }
    pub fn is_loading(&self) -> bool {
        self.pending.get()
    }
    pub fn has_selection(&self) -> bool {
        self.selection.selected_item().is_some()
    }

    fn update(&self) {
        self.empty.set_title(&if self.query.borrow().is_empty() {
            tr("No applications installed")
        } else {
            tr("No matching applications")
        });
        self.stack
            .set_visible_child_name(if self.count() == 0 { "empty" } else { "list" });
        (self.changed)(self.count(), self.has_selection());
    }

    pub fn show_properties(&self) {
        let (Some(window), Some(object)) = (self.window.upgrade(), self.selection.selected_item())
        else {
            return;
        };
        let info = application(&object).info;
        crate::properties::show_application(&window, &info);
    }

    fn popup(&self, widget: &gtk::Widget, x: f64, y: f64) {
        let Some(point) =
            widget.compute_point(&self.widget, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        if let Some(window) = self.window.upgrade() {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        }
        let mut menu = self.context_menu.borrow_mut();
        let popover = menu.get_or_insert_with(|| {
            let model = gio::Menu::new();
            model.append(Some(&tr("Open")), Some("win.open"));
            model.append(Some(&tr("Properties")), Some("win.properties"));
            let popover = gtk::PopoverMenu::from_model(Some(&model));
            popover.set_parent(&self.widget);
            popover.set_has_arrow(false);
            let weak = popover.downgrade();
            self.widget.connect_unmap(move |_| {
                if let Some(popover) = weak.upgrade() {
                    popover.popdown();
                }
            });
            popover
        });
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            point.x() as i32,
            point.y() as i32,
            1,
            1,
        )));
        popover.popup();
    }
    pub fn launch_selected(&self) {
        if let Some(object) = self.selection.selected_item() {
            self.launch(application(&object).info);
        }
    }

    fn launch(&self, app: gio::AppInfo) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let report = self.error.clone();
        let name = app.display_name().to_string();
        app.launch_uris_async(
            &[],
            Some(&gtk::prelude::WidgetExt::display(&window).app_launch_context()),
            gio::Cancellable::NONE,
            move |result| {
                if let Err(error) = result {
                    report(
                        &tr("Unable to launch {application}: {error}")
                            .replace("{application}", &name)
                            .replace("{error}", &error.to_string()),
                    );
                }
            },
        );
    }
}

impl Drop for ApplicationsView {
    fn drop(&mut self) {
        if let Some(menu) = self.context_menu.get_mut().take() {
            menu.unparent();
        }
    }
}
