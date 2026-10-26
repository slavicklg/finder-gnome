// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

use adw::prelude::*;
use gtk::{gdk, glib};
mod drag_drop;
mod file_commands;
mod system;
mod trash;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::mpsc,
};

use crate::{
    applications::ApplicationsView,
    directory::{DirectoryModel, LoadEvent},
    files::{self, FileItem, ViewMode},
    i18n::{count, mark, tr},
    operations::{self, Decision, Event, Job, Operation},
    preview::{PreviewData, PreviewPane, PreviewService, PreviewTicket},
    state::{
        ColumnState, FolderSettings, Location, NavEntry, NavigationHistory, Session, Settings,
        Storage,
    },
};

#[derive(Clone)]
enum SidebarLocation {
    Folder(gio::File),
    Applications,
}

pub struct Browser {
    pub window: adw::ApplicationWindow,
    title: gtk::Label,
    location: gtk::Entry,
    title_stack: gtk::Stack,
    breadcrumbs: gtk::Box,
    breadcrumb_scroll: gtk::ScrolledWindow,
    content: RefCell<gtk::Box>,
    columns_scroll: RefCell<gtk::ScrolledWindow>,
    status: gtk::Label,
    mode_buttons: Vec<gtk::ToggleButton>,
    mode: Cell<ViewMode>,
    hidden: Cell<bool>,
    panes: RefCell<Vec<Rc<Pane>>>,
    active: Cell<usize>,
    current: RefCell<gio::File>,
    sidebar: gtk::ListBox,
    sidebar_paned: gtk::Paned,
    places: RefCell<Vec<SidebarLocation>>,
    applications: RefCell<Option<Rc<ApplicationsView>>>,
    actions: RefCell<Vec<(String, gio::SimpleAction)>>,
    worker: mpsc::Sender<Job>,
    jobs: RefCell<Vec<gio::Cancellable>>,
    operation_revealer: gtk::Revealer,
    operation_label: gtk::Label,
    operation_progress: gtk::ProgressBar,
    operation_cancel: gtk::Button,
    conflict_dialogs: RefCell<Vec<(gio::Cancellable, adw::AlertDialog)>>,
    drop_bindings: RefCell<Vec<drag_drop::DropBinding>>,
    closing: Cell<bool>,
    tab_view: adw::TabView,
    tabs: RefCell<Vec<Rc<BrowserTab>>>,
    current_tab: RefCell<adw::TabPage>,
    switching: Cell<bool>,
    replaying: Cell<bool>,
    history: RefCell<NavigationHistory>,
    folder_settings: RefCell<FolderSettings>,
    settings: RefCell<Settings>,
    storage: Storage,
    startup_session: RefCell<Option<Session>>,
    save_timer: RefCell<Option<glib::SourceId>>,
    previews: PreviewService,
    quick_look: RefCell<Option<Rc<QuickLook>>>,
    connections: Rc<crate::connections::Connections>,
    catalog: crate::catalog::Catalog,
    catalog_error_shown: Cell<bool>,
    locations_box: gtk::Box,
    tags_box: gtk::Box,
    volumes: gio::VolumeMonitor,
    defer_load: Cell<bool>,
    search_bar: gtk::Revealer,
    search_entry: gtk::SearchEntry,
    search_scope: gtk::DropDown,
    search_timer: RefCell<Option<glib::SourceId>>,
    search_origin: RefCell<Option<gio::File>>,
    task_list: gtk::Box,
    task_rows: RefCell<Vec<Rc<system::TaskRow>>>,
}

struct QuickLook {
    window: adw::Window,
    preview: Rc<PreviewPane>,
    files: Vec<FileItem>,
    index: Cell<usize>,
}

struct BrowserTab {
    page: adw::TabPage,
    state: RefCell<Option<TabState>>,
}

#[derive(Clone)]
struct TabState {
    content: gtk::Box,
    scroll: gtk::ScrolledWindow,
    panes: Vec<Rc<Pane>>,
    active: usize,
    current: gio::File,
    applications: Option<Rc<ApplicationsView>>,
    mode: ViewMode,
    hidden: bool,
    folder_settings: FolderSettings,
    history: NavigationHistory,
}

struct Pane {
    model: Rc<DirectoryModel>,
    widget: gtk::Box,
    stack: gtk::Stack,
    error: gtk::Label,
    selection: gtk::MultiSelection,
    sorted: gtk::SortListModel,
    filter: gtk::CustomFilter,
    show_hidden: Rc<Cell<bool>>,
    restoring: Cell<bool>,
    saved: RefCell<HashSet<String>>,
    loading: Cell<bool>,
    last_folder: RefCell<Option<String>>,
    restore_scroll: Cell<Option<f64>>,
    preview: RefCell<Option<Rc<PreviewPane>>>,
    context_menu: RefCell<Option<gtk::PopoverMenu>>,
    location: RefCell<Location>,
    pending: Cell<bool>,
    connection_cancel: RefCell<gio::Cancellable>,
    notice: RefCell<String>,
    writable: Cell<Option<bool>>,
}

fn item(object: &glib::Object) -> FileItem {
    object
        .downcast_ref::<glib::BoxedAnyObject>()
        .expect("file model item")
        .borrow::<FileItem>()
        .clone()
}

fn icon_button(icon: &str, label: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tr(label))
        .action_name(action)
        .build()
}

impl Browser {
    pub fn new(app: &adw::Application, initial: gio::File) -> Rc<Self> {
        let invalid = !files::browsable(&initial);
        let initial = if invalid {
            gio::File::for_path(glib::home_dir())
        } else {
            initial
        };
        let builder = gtk::Builder::from_resource("/io/github/finder_gnome/window.ui");
        let window: adw::ApplicationWindow = builder.object("window").unwrap();
        window.set_application(Some(app));
        let root: gtk::Box = builder.object("root").unwrap();
        let (storage, storage_errors) = Storage::new();
        let (settings, settings_warning) = storage.read_settings();
        let (session, session_warning) = storage.read_session();
        let tab_view = adw::TabView::new();
        let tab_bar = adw::TabBar::builder()
            .view(&tab_view)
            .autohide(true)
            .expand_tabs(false)
            .build();
        tab_bar.set_end_action_widget(Some(&icon_button(
            "list-add-symbolic",
            "New Tab",
            "win.new-tab",
        )));
        let header = adw::HeaderBar::new();
        header.add_css_class("toolbar");
        header.pack_start(&icon_button("go-previous-symbolic", "Back", "win.back"));
        header.pack_start(&icon_button("go-next-symbolic", "Forward", "win.forward"));
        header.pack_start(&icon_button("go-up-symbolic", "Parent Folder", "win.up"));

        let title = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["title"])
            .build();
        let location = gtk::Entry::builder()
            .placeholder_text(tr("Enter a folder path"))
            .width_chars(35)
            .build();
        let title_stack = gtk::Stack::new();
        title_stack.add_named(&title, Some("title"));
        title_stack.add_named(&location, Some("location"));
        title_stack.set_visible_child_name("title");
        header.set_title_widget(Some(&title_stack));

        let views = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        views.add_css_class("view-switch");
        let mut mode_buttons: Vec<gtk::ToggleButton> = Vec::new();
        for (index, (icon, label)) in [
            ("view-grid-symbolic", mark("Icons")),
            ("view-list-symbolic", mark("List")),
            ("view-dual-symbolic", mark("Columns")),
            ("view-reveal-symbolic", mark("Gallery")),
        ]
        .iter()
        .enumerate()
        {
            let button = gtk::ToggleButton::builder()
                .icon_name(*icon)
                .tooltip_text(tr(label))
                .build();
            button.set_action_name(Some(&format!("win.view-{index}")));
            views.append(&button);
            mode_buttons.push(button);
        }
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&main_menu())
                .tooltip_text(tr("Actions"))
                .build(),
        );
        header.pack_end(&views);
        header.pack_end(&icon_button(
            "system-search-symbolic",
            "Search",
            "win.search",
        ));
        root.append(&header);

        let body = gtk::Paned::new(gtk::Orientation::Horizontal);
        body.set_wide_handle(true);
        body.set_resize_start_child(false);
        body.set_shrink_start_child(false);
        body.set_shrink_end_child(false);
        body.set_position(settings.sidebar_width.max(140));
        body.set_vexpand(true);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("sidebar");
        sidebar.set_hexpand(false);
        sidebar.set_width_request(140);
        let section = gtk::Label::builder()
            .label(tr("Favorites"))
            .xalign(0.0)
            .css_classes(["section-title"])
            .build();
        sidebar.append(&section);
        let places = gtk::ListBox::new();
        places.add_css_class("navigation-sidebar");
        places.set_selection_mode(gtk::SelectionMode::Single);
        sidebar.append(&places);
        let locations_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let tags_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.append(&locations_box);
        sidebar.append(&tags_box);
        body.set_start_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&sidebar)
                .hexpand(false)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        ));

        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.set_hexpand(true);
        main.append(&tab_bar);
        let breadcrumbs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        breadcrumbs.add_css_class("breadcrumbs");
        let breadcrumb_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&breadcrumbs)
            .build();
        main.append(&breadcrumb_scroll);
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(tr("Search filenames…"))
            .hexpand(true)
            .build();
        let search_scope = gtk::DropDown::from_strings(&[&tr("This Folder"), &tr("Home")]);
        let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        search_row.append(&search_entry);
        search_row.append(&search_scope);
        search_row.append(&icon_button(
            "process-stop-symbolic",
            "Stop Search",
            "win.stop-search",
        ));
        search_row.append(&icon_button(
            "window-close-symbolic",
            "Close Search",
            "win.close-search",
        ));
        let search_bar = gtk::Revealer::builder().child(&search_row).build();
        main.append(&search_bar);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.add_css_class("content");
        let columns_scroll = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build();
        let initial_page = tab_view.append(&columns_scroll);
        initial_page.set_title(&tr("Home"));
        main.append(&tab_view);
        main.set_width_request(320);
        body.set_end_child(Some(&main));
        root.append(&body);

        let operation_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        operation_box.add_css_class("operation-panel");
        let operation_text = gtk::Box::new(gtk::Orientation::Vertical, 5);
        operation_text.set_hexpand(true);
        let operation_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let operation_progress = gtk::ProgressBar::new();
        operation_text.append(&operation_label);
        operation_text.append(&operation_progress);
        let operation_cancel = gtk::Button::with_label(&tr("Cancel All"));
        operation_cancel.set_valign(gtk::Align::Center);
        operation_box.append(&operation_text);
        operation_box.append(&operation_cancel);
        let operation_revealer = gtk::Revealer::builder()
            .child(&operation_box)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();
        root.append(&operation_revealer);
        let task_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let task_scroll = gtk::ScrolledWindow::builder()
            .child(&task_list)
            .max_content_height(220)
            .propagate_natural_height(true)
            .build();
        let task_popover = gtk::Popover::builder()
            .child(&task_scroll)
            .width_request(460)
            .build();
        let task_button = gtk::MenuButton::builder()
            .icon_name("view-list-bullet-symbolic")
            .tooltip_text(tr("Background Tasks"))
            .popover(&task_popover)
            .build();
        header.pack_end(&task_button);
        let status = gtk::Label::builder()
            .label(tr("Loading…"))
            .xalign(0.5)
            .css_classes(["statusbar"])
            .build();
        root.append(&status);

        let catalog = crate::catalog::Catalog::new(system::catalog_path(&storage));
        let browser = Rc::new(Self {
            window,
            title,
            location,
            title_stack,
            breadcrumbs,
            breadcrumb_scroll,
            content: RefCell::new(content),
            columns_scroll: RefCell::new(columns_scroll),
            status,
            mode_buttons,
            mode: Cell::new(ViewMode::Icons),
            hidden: Cell::new(false),
            panes: RefCell::new(Vec::new()),
            active: Cell::new(0),
            current: RefCell::new(initial.clone()),
            sidebar: places.clone(),
            sidebar_paned: body,
            places: RefCell::new(Vec::new()),
            applications: RefCell::new(None),
            actions: RefCell::new(Vec::new()),
            worker: operations::start_worker(),
            jobs: RefCell::new(Vec::new()),
            operation_revealer,
            operation_label,
            operation_progress,
            operation_cancel,
            conflict_dialogs: RefCell::new(Vec::new()),
            drop_bindings: RefCell::new(Vec::new()),
            closing: Cell::new(false),
            tab_view,
            tabs: RefCell::new(vec![Rc::new(BrowserTab {
                page: initial_page.clone(),
                state: RefCell::new(None),
            })]),
            current_tab: RefCell::new(initial_page),
            switching: Cell::new(false),
            replaying: Cell::new(false),
            history: RefCell::new(NavigationHistory::default()),
            folder_settings: RefCell::new(settings.for_file(&initial)),
            settings: RefCell::new(settings),
            storage,
            startup_session: RefCell::new(Some(session)),
            save_timer: RefCell::new(None),
            previews: PreviewService::new(),
            quick_look: RefCell::new(None),
            connections: Rc::new(crate::connections::Connections::default()),
            catalog,
            catalog_error_shown: Cell::new(false),
            locations_box,
            tags_box,
            volumes: gio::VolumeMonitor::get(),
            defer_load: Cell::new(false),
            search_bar,
            search_entry,
            search_scope,
            search_timer: RefCell::new(None),
            search_origin: RefCell::new(None),
            task_list,
            task_rows: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&browser);
        browser.sidebar_paned.connect_position_notify(move |paned| {
            if let Some(browser) = weak.upgrade() {
                browser.settings.borrow_mut().sidebar_width = paned.position();
                browser.schedule_persist();
            }
        });
        browser.install_actions(app);
        browser.install_file_commands(app);
        browser.install_trash_actions(app);
        browser.populate_sidebar(&places);
        browser.connect_signals();
        browser.navigate(initial, HashSet::new());
        browser.connect_tabs();
        browser.install_tab_drop(&tab_bar);
        browser.apply_theme();
        browser.install_system(app);
        if invalid {
            browser.error(&tr(
                "Use a local path, smb:// or sftp:// address without a password.",
            ));
        }
        let weak = Rc::downgrade(&browser);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(error) = storage_errors.recv().await {
                let Some(browser) = weak.upgrade() else {
                    break;
                };
                browser.error(
                    &tr("Unable to save application state: {error}").replace("{error}", &error),
                );
            }
        });
        let warnings: Vec<_> = [settings_warning, session_warning]
            .into_iter()
            .flatten()
            .collect();
        if !warnings.is_empty() {
            let weak = Rc::downgrade(&browser);
            glib::idle_add_local_once(move || {
                if let Some(browser) = weak.upgrade() {
                    browser.error(
                        &tr(
                            "Some saved settings could not be read. Defaults were loaded.\n{files}",
                        )
                        .replace("{files}", &warnings.join("\n")),
                    );
                }
            });
        }
        browser
    }

    fn capture_entry(&self) -> NavEntry {
        NavEntry {
            location: if self.applications.borrow().is_some() {
                Location::Applications
            } else {
                self.active_pane()
                    .map(|p| p.location.borrow().clone())
                    .filter(|l| !matches!(l, Location::Directory(_)))
                    .unwrap_or_else(|| Location::directory(&self.current.borrow()))
            },
            last_directory: self.current.borrow().uri().to_string(),
            settings: self.folder_settings.borrow().clone(),
            columns: self
                .panes
                .borrow()
                .iter()
                .map(|pane| ColumnState {
                    uri: pane.model.file.uri().to_string(),
                    selected: pane
                        .selected()
                        .iter()
                        .map(|i| i.file.uri().to_string())
                        .collect(),
                    scroll: pane
                        .stack
                        .child_by_name("files")
                        .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
                        .map(|w| w.vadjustment().value())
                        .unwrap_or(0.0),
                })
                .collect(),
            active_column: self.active.get(),
            horizontal_scroll: self.columns_scroll.borrow().hadjustment().value(),
        }
    }

    fn snapshot_tab(&self) -> TabState {
        TabState {
            content: self.content.borrow().clone(),
            scroll: self.columns_scroll.borrow().clone(),
            panes: self.panes.borrow().clone(),
            active: self.active.get(),
            current: self.current.borrow().clone(),
            applications: self.applications.borrow().clone(),
            mode: self.mode.get(),
            hidden: self.hidden.get(),
            folder_settings: self.folder_settings.borrow().clone(),
            history: self.history.borrow().clone(),
        }
    }

    fn save_active_tab(&self) {
        if let Some(tab) = self
            .tabs
            .borrow()
            .iter()
            .find(|tab| tab.page == *self.current_tab.borrow())
        {
            *tab.state.borrow_mut() = Some(self.snapshot_tab());
        }
    }

    fn connect_tabs(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.tab_view.connect_selected_page_notify(move |view| {
            if let Some(browser) = weak.upgrade() {
                if !browser.switching.get() {
                    if let Some(page) = view.selected_page() {
                        browser.switch_tab(&page);
                    }
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_close_page(move |view, page| {
            let Some(browser) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if view.n_pages() == 1 {
                view.close_page_finish(page, false);
                browser.window.close();
            } else {
                if let Some(tab) = browser.tabs.borrow().iter().find(|tab| tab.page == *page) {
                    if let Some(state) = tab.state.borrow().as_ref() {
                        for pane in &state.panes {
                            pane.connection_cancel.borrow().cancel();
                            pane.model.cancel();
                        }
                    }
                }
                view.close_page_finish(page, true);
                browser.tabs.borrow_mut().retain(|tab| tab.page != *page);
                browser.schedule_persist();
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_page_reordered(move |_, _, _| {
            if let Some(browser) = weak.upgrade() {
                browser.schedule_persist();
            }
        });
        let menu = gio::Menu::new();
        menu.append(Some(&tr("Duplicate Tab")), Some("win.duplicate-tab"));
        menu.append(Some(&tr("Close Tab")), Some("win.close-tab"));
        self.tab_view.set_menu_model(Some(&menu));
        let weak = Rc::downgrade(self);
        self.tab_view.connect_setup_menu(move |view, page| {
            if let (Some(browser), Some(page)) = (weak.upgrade(), page) {
                view.set_selected_page(page);
                browser.switch_tab(page);
            }
        });
    }

    fn switch_tab(self: &Rc<Self>, page: &adw::TabPage) {
        if *self.current_tab.borrow() == *page || self.switching.get() {
            return;
        }
        self.stop_previews();
        self.save_active_tab();
        let state = self
            .tabs
            .borrow()
            .iter()
            .find(|tab| tab.page == *page)
            .and_then(|tab| tab.state.borrow().clone());
        let Some(state) = state else {
            return;
        };
        self.switching.set(true);
        *self.content.borrow_mut() = state.content;
        *self.columns_scroll.borrow_mut() = state.scroll;
        *self.panes.borrow_mut() = state.panes;
        self.active.set(state.active);
        *self.current.borrow_mut() = state.current;
        *self.applications.borrow_mut() = state.applications;
        self.mode.set(state.mode);
        self.hidden.set(state.hidden);
        *self.folder_settings.borrow_mut() = state.folder_settings;
        *self.history.borrow_mut() = state.history;
        *self.current_tab.borrow_mut() = page.clone();
        self.switching.set(false);
        self.refresh_chrome();
        self.sync_search();
        if let Some(pane) = self.active_pane() {
            if pane.pending.get() {
                self.load_pane(&pane);
            }
        }
        self.schedule_persist();
        self.focus_active();
        if let Some(pane) = self.active_pane() {
            if let Some(preview) = pane.preview.borrow().as_ref() {
                preview.show_file(pane.selected().first().cloned());
            }
        }
    }

    fn new_tab(self: &Rc<Self>, entry: NavEntry) {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.add_css_class("content");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&content)
            .hexpand(true)
            .vexpand(true)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        self.switching.set(true);
        let page = self.tab_view.append(&scroll);
        page.set_title(&tr("New Tab"));
        let current = Location::Directory(entry.last_directory.clone())
            .file()
            .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
        self.tabs.borrow_mut().push(Rc::new(BrowserTab {
            page: page.clone(),
            state: RefCell::new(Some(TabState {
                content,
                scroll,
                panes: Vec::new(),
                active: 0,
                current,
                applications: None,
                mode: entry.settings.mode,
                hidden: entry.settings.hidden,
                folder_settings: entry.settings.clone(),
                history: NavigationHistory::default(),
            })),
        }));
        self.tab_view.set_selected_page(&page);
        self.switching.set(false);
        self.switch_tab(&page);
        self.restore_entry(entry);
        self.history.borrow_mut().visit(self.capture_entry());
        self.refresh_chrome();
        self.schedule_persist();
    }

    pub fn open_in_new_tab(self: &Rc<Self>, file: gio::File) {
        if !files::browsable(&file) {
            self.error(&tr(
                "Use a local path, smb:// or sftp:// address without a password.",
            ));
            return;
        }
        let settings = self.settings.borrow().for_file(&file);
        self.new_tab(NavEntry::new(
            Location::directory(&file),
            file.uri().to_string(),
            settings,
        ));
    }

    fn refresh_chrome(self: &Rc<Self>) {
        if self.applications.borrow().is_some() {
            self.title.set_label(&tr("Applications"));
            self.title_stack.set_visible_child_name("title");
            self.window
                .set_title(Some(&format!("{} — Finder Gnome", tr("Applications"))));
            while let Some(child) = self.breadcrumbs.first_child() {
                self.breadcrumbs.remove(&child);
            }
            self.breadcrumbs
                .append(&gtk::Label::new(Some(&tr("Applications"))));
            let index = self
                .places
                .borrow()
                .iter()
                .position(|p| matches!(p, SidebarLocation::Applications));
            self.sidebar.select_row(
                index
                    .and_then(|i| self.sidebar.row_at_index(i as i32))
                    .as_ref(),
            );
        } else {
            self.update_path();
        }
        for (index, button) in self.mode_buttons.iter().enumerate() {
            button.set_active(
                self.applications.borrow().is_none() && index == self.mode.get() as usize,
            );
        }
        self.current_tab.borrow().set_title(&self.title.text());
        self.update_status();
    }

    fn record_departure(&self) {
        if !self.replaying.get() {
            self.history
                .borrow_mut()
                .replace_current(self.capture_entry());
        }
    }
    fn record_arrival(self: &Rc<Self>) {
        if !self.replaying.get() {
            self.history.borrow_mut().visit(self.capture_entry());
        }
        self.current_tab.borrow().set_title(&self.title.text());
        self.schedule_persist();
        self.update_status();
    }
    fn history_step(self: &Rc<Self>, forward: bool) {
        self.record_departure();
        let entry = self.history.borrow_mut().step(forward);
        if let Some(entry) = entry {
            self.restore_entry(entry);
        }
    }

    fn restore_entry(self: &Rc<Self>, entry: NavEntry) {
        let replaying = self.replaying.replace(true);
        *self.folder_settings.borrow_mut() = entry.settings.clone();
        self.mode.set(entry.settings.mode);
        self.hidden.set(entry.settings.hidden);
        if let Some(last) = Location::Directory(entry.last_directory.clone()).file() {
            *self.current.borrow_mut() = last;
        }
        match &entry.location {
            Location::Applications => self.show_applications(),
            Location::Search { .. } | Location::Tag(_) => {
                self.open_collection(entry.location.clone(), HashSet::new())
            }
            Location::Directory(_) => {
                if let Some(file) = entry.location.file() {
                    let columns: Vec<_> = if entry.settings.mode == ViewMode::Columns {
                        entry
                            .columns
                            .iter()
                            .filter(|col| Location::Directory(col.uri.clone()).file().is_some())
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let root = columns
                        .first()
                        .and_then(|c| Location::Directory(c.uri.clone()).file())
                        .unwrap_or_else(|| file.clone());
                    let selected = entry
                        .columns
                        .get(if columns.is_empty() {
                            entry.active_column
                        } else {
                            0
                        })
                        .map(|c| c.selected.iter().cloned().collect())
                        .unwrap_or_default();
                    self.navigate(root, selected);
                    for col in columns.iter().skip(1) {
                        if let Some(file) = Location::Directory(col.uri.clone()).file() {
                            let pane = self.make_pane(file, col.selected.iter().cloned().collect());
                            self.content.borrow().append(&pane.widget);
                            self.panes.borrow_mut().push(pane.clone());
                            pane.model.reload();
                        }
                    }
                    let panes = self.panes.borrow().clone();
                    for (index, pane) in panes.iter().enumerate() {
                        if let Some(col) = entry.columns.get(if columns.is_empty() {
                            entry.active_column
                        } else {
                            index
                        }) {
                            pane.restore_scroll.set(Some(col.scroll));
                        }
                        *pane.last_folder.borrow_mut() =
                            panes.get(index + 1).map(|p| p.model.file.uri().to_string());
                    }
                    self.active
                        .set(entry.active_column.min(panes.len().saturating_sub(1)));
                    *self.current.borrow_mut() = file;
                    let scroll = self.columns_scroll.borrow().clone();
                    glib::idle_add_local_once(move || {
                        scroll.hadjustment().set_value(entry.horizontal_scroll)
                    });
                }
            }
        }
        self.replaying.set(replaying);
        self.refresh_chrome();
        self.schedule_persist();
    }

    pub fn restore_session(self: &Rc<Self>, explicit: Option<gio::File>) {
        let session = self.startup_session.borrow_mut().take().unwrap_or_default();
        self.window.set_default_size(session.width, session.height);
        if self.settings.borrow().restore_tabs && !session.tabs.is_empty() {
            self.defer_load.set(true);
            self.restore_entry(session.tabs[0].clone());
            self.history.borrow_mut().visit(self.capture_entry());
            for entry in session.tabs.iter().skip(1) {
                self.new_tab(entry.clone());
            }
            self.tab_view
                .set_selected_page(&self.tab_view.nth_page(session.active as i32));
            self.defer_load.set(false);
            if let Some(pane) = self.active_pane() {
                if pane.pending.get() {
                    self.load_pane(&pane);
                }
            }
            if let Some(file) = explicit {
                self.open_in_new_tab(file);
            }
        }
    }

    fn schedule_persist(self: &Rc<Self>) {
        if let Some(id) = self.save_timer.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        *self.save_timer.borrow_mut() = Some(glib::timeout_add_local_once(
            std::time::Duration::from_millis(300),
            move || {
                if let Some(browser) = weak.upgrade() {
                    browser.save_timer.borrow_mut().take();
                    browser.persist_now();
                }
            },
        ));
    }
    fn persist_now(&self) {
        self.save_active_tab();
        let active_page = self.current_tab.borrow().clone();
        let mut session = Session {
            width: self.window.width().max(680),
            height: self.window.height().max(440),
            ..Default::default()
        };
        for index in 0..self.tab_view.n_pages() {
            let page = self.tab_view.nth_page(index);
            if page == active_page {
                session.active = index as usize;
            }
            if let Some(state) = self
                .tabs
                .borrow()
                .iter()
                .find(|t| t.page == page)
                .and_then(|t| t.state.borrow().clone())
            {
                session.tabs.push(Self::entry_for_state(&state));
            }
        }
        self.storage.save(&self.settings.borrow(), &session);
    }
    fn entry_for_state(state: &TabState) -> NavEntry {
        NavEntry {
            location: if state.applications.is_some() {
                Location::Applications
            } else {
                state
                    .panes
                    .get(state.active)
                    .map(|p| p.location.borrow().clone())
                    .filter(|l| !matches!(l, Location::Directory(_)))
                    .unwrap_or_else(|| Location::directory(&state.current))
            },
            last_directory: state.current.uri().to_string(),
            settings: state.folder_settings.clone(),
            active_column: state.active,
            horizontal_scroll: state.scroll.hadjustment().value(),
            columns: state
                .panes
                .iter()
                .map(|p| ColumnState {
                    uri: p.model.file.uri().to_string(),
                    selected: p
                        .selected()
                        .iter()
                        .map(|i| i.file.uri().to_string())
                        .collect(),
                    scroll: p
                        .stack
                        .child_by_name("files")
                        .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
                        .map(|w| w.vadjustment().value())
                        .unwrap_or(0.0),
                })
                .collect(),
        }
    }
    fn save_folder_settings(self: &Rc<Self>) {
        if self.is_collection() {
            self.schedule_persist();
            return;
        }
        self.folder_settings.borrow_mut().mode = self.mode.get();
        self.folder_settings.borrow_mut().hidden = self.hidden.get();
        if self.applications.borrow().is_none() {
            self.settings.borrow_mut().folders.insert(
                self.current.borrow().uri().to_string(),
                self.folder_settings.borrow().clone(),
            );
        }
        self.schedule_persist();
    }
    fn apply_theme(&self) {
        adw::StyleManager::default().set_color_scheme(
            match self.settings.borrow().theme.as_str() {
                "light" => adw::ColorScheme::ForceLight,
                "dark" => adw::ColorScheme::ForceDark,
                _ => adw::ColorScheme::Default,
            },
        );
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.location.connect_activate(move |entry| {
            if let Some(browser) = weak.upgrade() {
                match files::parse_location(&entry.text()) {
                    Ok(file) => {
                        browser.title_stack.set_visible_child_name("title");
                        browser.navigate(file, HashSet::new());
                    }
                    Err(error) => browser.error(&error),
                }
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(browser) = weak.upgrade() {
                    browser.title_stack.set_visible_child_name("title");
                    browser.focus_active();
                }
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let weak = Rc::downgrade(self);
        self.operation_cancel.connect_clicked(move |_| {
            if let Some(browser) = weak.upgrade() {
                if browser.jobs.borrow().is_empty() {
                    browser.operation_revealer.set_reveal_child(false);
                } else {
                    browser.cancel_all();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(browser) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if browser.jobs.borrow().is_empty() {
                browser.persist_now();
                browser.storage.flush();
                return glib::Propagation::Proceed;
            }
            let dialog = adw::AlertDialog::builder()
                .heading(tr("File operations are running"))
                .body(tr(
                    "Closing will cancel pending work. Completed changes will be kept.",
                ))
                .build();
            dialog.add_responses(&[("stay", &tr("Stay")), ("close", &tr("Cancel and Close"))]);
            dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("stay"));
            dialog.set_close_response("stay");
            let weak = Rc::downgrade(&browser);
            dialog.connect_response(None, move |_, response| {
                if response == "close" {
                    if let Some(browser) = weak.upgrade() {
                        browser.closing.set(true);
                        browser.cancel_all();
                    }
                }
            });
            dialog.present(Some(&browser.window));
            glib::Propagation::Stop
        });
    }

    fn preferences(self: &Rc<Self>) {
        let dialog = adw::PreferencesDialog::builder()
            .title(tr("Preferences"))
            .build();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(tr("General"))
            .build();
        let theme = adw::ComboRow::builder()
            .title(tr("Appearance"))
            .model(&gtk::StringList::new(&[
                &tr("System"),
                &tr("Light"),
                &tr("Dark"),
            ]))
            .build();
        theme.set_selected(match self.settings.borrow().theme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        });
        let weak = Rc::downgrade(self);
        theme.connect_selected_notify(move |row| {
            if let Some(browser) = weak.upgrade() {
                browser.settings.borrow_mut().theme =
                    ["system", "light", "dark"][row.selected().min(2) as usize].into();
                browser.apply_theme();

                browser.schedule_persist();
            }
        });
        group.add(&theme);
        let restore = adw::SwitchRow::builder()
            .title(tr("Restore tabs on startup"))
            .active(self.settings.borrow().restore_tabs)
            .build();
        let weak = Rc::downgrade(self);
        restore.connect_active_notify(move |row| {
            if let Some(browser) = weak.upgrade() {
                browser.settings.borrow_mut().restore_tabs = row.is_active();
                browser.schedule_persist();
            }
        });
        group.add(&restore);
        page.add(&group);
        dialog.add(&page);
        dialog.present(Some(&self.window));
    }

    fn change_view(self: &Rc<Self>, change: impl FnOnce(&mut FolderSettings)) {
        let mut settings = self.folder_settings.borrow().clone();
        change(&mut settings);
        settings.normalize();
        self.replace_view(settings, true);
    }
    fn replace_view(self: &Rc<Self>, settings: FolderSettings, override_folder: bool) {
        let mut entry = self.capture_entry();
        entry.settings = settings.clone();
        self.mode.set(settings.mode);
        self.hidden.set(settings.hidden);
        *self.folder_settings.borrow_mut() = settings.clone();
        if override_folder && !self.is_collection() {
            self.settings
                .borrow_mut()
                .folders
                .insert(self.current.borrow().uri().to_string(), settings);
        } else if !self.is_collection() {
            self.settings
                .borrow_mut()
                .folders
                .remove(self.current.borrow().uri().as_str());
        }
        self.restore_entry(entry);
        self.history
            .borrow_mut()
            .replace_current(self.capture_entry());
        self.schedule_persist();
    }
    fn view_options(self: &Rc<Self>) {
        if self.applications.borrow().is_some() {
            return;
        }
        let options = self.folder_settings.borrow().clone();
        let dialog = adw::PreferencesDialog::builder()
            .title(tr("View Options"))
            .build();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(tr("This Folder"))
            .description(self.current.borrow().parse_name())
            .build();
        let mode = adw::ComboRow::builder()
            .title(tr("View"))
            .model(&gtk::StringList::new(&[
                &tr("Icons"),
                &tr("List"),
                &tr("Columns"),
                &tr("Gallery"),
            ]))
            .selected(options.mode as u32)
            .build();
        let weak = Rc::downgrade(self);
        mode.connect_selected_notify(move |row| {
            if let Some(b) = weak.upgrade() {
                b.change_view(|s| {
                    s.mode = [
                        ViewMode::Icons,
                        ViewMode::List,
                        ViewMode::Columns,
                        ViewMode::Gallery,
                    ][row.selected().min(3) as usize]
                });
            }
        });
        group.add(&mode);
        let sort = adw::ComboRow::builder()
            .title(tr("Sort By"))
            .model(&gtk::StringList::new(&[
                &tr("Name"),
                &tr("Size"),
                &tr("Kind"),
                &tr("Date Modified"),
            ]))
            .selected(options.sort_field as u32)
            .build();
        let weak = Rc::downgrade(self);
        sort.connect_selected_notify(move |row| {
            if let Some(b) = weak.upgrade() {
                b.change_view(|s| s.sort_field = row.selected().min(3) as usize);
            }
        });
        group.add(&sort);
        for (title, value, field) in [
            (mark("Descending"), options.descending, 0),
            (mark("Folders First"), options.folders_first, 1),
            (mark("Show Hidden Files"), options.hidden, 2),
            (mark("Show Thumbnails"), options.thumbnails, 3),
        ] {
            let row = adw::SwitchRow::builder()
                .title(tr(title))
                .active(value)
                .build();
            let weak = Rc::downgrade(self);
            row.connect_active_notify(move |row| {
                if let Some(b) = weak.upgrade() {
                    b.change_view(|s| match field {
                        0 => s.descending = row.is_active(),
                        1 => s.folders_first = row.is_active(),
                        2 => s.hidden = row.is_active(),
                        _ => s.thumbnails = row.is_active(),
                    });
                }
            });
            group.add(&row);
        }
        let size = adw::SpinRow::builder()
            .title(tr("Icon Size"))
            .adjustment(&gtk::Adjustment::new(
                f64::from(options.icon_size),
                32.0,
                128.0,
                16.0,
                16.0,
                0.0,
            ))
            .build();
        let weak = Rc::downgrade(self);
        size.connect_value_notify(move |row| {
            if let Some(b) = weak.upgrade() {
                b.change_view(|s| s.icon_size = row.value() as i32);
            }
        });
        group.add(&size);
        let width = adw::SpinRow::builder()
            .title(tr("Columns Width"))
            .adjustment(&gtk::Adjustment::new(
                f64::from(options.column_pane_width),
                180.0,
                600.0,
                20.0,
                40.0,
                0.0,
            ))
            .build();
        let weak = Rc::downgrade(self);
        width.connect_value_notify(move |row| {
            if let Some(b) = weak.upgrade() {
                b.change_view(|s| s.column_pane_width = row.value() as i32);
            }
        });
        group.add(&width);
        page.add(&group);
        let columns = adw::PreferencesGroup::builder()
            .title(tr("List Columns"))
            .build();
        for (index, title) in [
            (1, mark("Size")),
            (2, mark("Kind")),
            (3, mark("Date Modified")),
        ] {
            let row = adw::SwitchRow::builder()
                .title(tr(title))
                .active(options.visible_columns[index])
                .build();
            let weak = Rc::downgrade(self);
            row.connect_active_notify(move |row| {
                if let Some(b) = weak.upgrade() {
                    b.change_view(|s| s.visible_columns[index] = row.is_active());
                }
            });
            columns.add(&row);
        }
        page.add(&columns);
        let defaults = adw::PreferencesGroup::new();
        let save = gtk::Button::with_label(&tr("Use as Defaults"));
        let weak = Rc::downgrade(self);
        save.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.settings.borrow_mut().defaults = b.folder_settings.borrow().clone();
                b.schedule_persist();
            }
        });
        defaults.add(&save);
        let reset = gtk::Button::with_label(&tr("Reset Folder View"));
        let weak = Rc::downgrade(self);
        let weak_dialog = dialog.downgrade();
        reset.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                let defaults = b.settings.borrow().defaults.clone();
                b.replace_view(defaults, false);
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.force_close();
                }
            }
        });
        defaults.add(&reset);
        page.add(&defaults);
        dialog.add(&page);
        dialog.present(Some(&self.window));
    }

    fn action(
        self: &Rc<Self>,
        app: &adw::Application,
        name: &str,
        shortcuts: &[&str],
        callback: impl Fn(&Rc<Self>) + 'static,
    ) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(browser) = weak.upgrade() {
                callback(&browser);
            }
        });
        self.window.add_action(&action);
        app.set_accels_for_action(&format!("win.{name}"), shortcuts);
        self.actions.borrow_mut().push((name.into(), action));
    }

    fn install_actions(self: &Rc<Self>, app: &adw::Application) {
        self.action(app, "quick-look", &["space"], |b| {
            if !b.text_focused() {
                b.toggle_quick_look();
            }
        });
        self.action(app, "preferences", &["<Control>comma"], |b| b.preferences());
        self.action(app, "view-options", &[], |b| b.view_options());
        self.action(app, "new-tab", &["<Control>t"], |b| {
            b.new_tab(b.capture_entry())
        });
        self.action(app, "duplicate-tab", &[], |b| b.new_tab(b.capture_entry()));
        self.action(app, "close-tab", &["<Control>w"], |b| {
            b.tab_view.close_page(&b.current_tab.borrow().clone())
        });
        self.action(app, "next-tab", &["<Control>Tab"], |b| {
            b.tab_view.select_next_page();
        });
        self.action(app, "previous-tab", &["<Control><Shift>Tab"], |b| {
            b.tab_view.select_previous_page();
        });
        self.action(app, "back", &["<Alt>Left"], |b| b.history_step(false));
        self.action(app, "forward", &["<Alt>Right"], |b| b.history_step(true));
        self.action(app, "open-new-tab", &[], |b| {
            for file in b.selected().into_iter().filter(|i| i.is_dir) {
                b.open_in_new_tab(file.file);
            }
        });
        self.action(app, "up", &["<Alt>Up"], |b| {
            let current = b.current.borrow().clone();
            if b.applications.borrow().is_some() {
                b.navigate(current, HashSet::new());
                return;
            }
            if let Some(parent) = current.parent() {
                b.navigate(parent, HashSet::from([current.uri().to_string()]));
            }
        });
        self.action(app, "location", &["<Control>l"], |b| {
            b.location.set_text(&b.current.borrow().parse_name());
            b.title_stack.set_visible_child_name("location");
            b.location.grab_focus();
            b.location.select_region(0, -1);
        });
        self.action(app, "reload", &["F5"], |b| {
            if let Some(applications) = b.applications.borrow().as_ref() {
                applications.reload();
                return;
            }
            for pane in b.panes.borrow().iter() {
                pane.model.reload();
            }
        });
        self.action(app, "hidden", &["<Control>h"], |b| {
            b.hidden.set(!b.hidden.get());
            b.save_folder_settings();
            if let Some(pane) = b.active_pane() {
                let search = {
                    let mut source = pane.model.source.borrow_mut();
                    if let crate::directory::Source::Search { hidden, .. } = &mut *source {
                        *hidden = b.hidden.get();
                        true
                    } else {
                        false
                    }
                };
                if search {
                    pane.show_hidden.set(b.hidden.get());
                    pane.filter.changed(gtk::FilterChange::Different);
                    pane.model.reload();
                    return;
                }
            }
            let panes = b.panes.borrow().clone();
            for pane in panes {
                pane.restoring.set(true);
                pane.show_hidden.set(b.hidden.get());
                pane.filter.changed(gtk::FilterChange::Different);
                pane.restoring.set(pane.loading.get());
                if !pane.loading.get() {
                    pane.stack
                        .set_visible_child_name(if pane.selection.n_items() == 0 {
                            "empty"
                        } else {
                            "files"
                        });
                    if b.mode.get() == ViewMode::Columns {
                        let active = b.active_pane();
                        b.selection_changed(&pane);
                        if let Some(active) = active {
                            b.activate_pane(&active);
                        }
                    }
                }
            }
            b.update_status();
        });
        for (index, mode) in [
            ViewMode::Icons,
            ViewMode::List,
            ViewMode::Columns,
            ViewMode::Gallery,
        ]
        .into_iter()
        .enumerate()
        {
            self.action(
                app,
                &format!("view-{index}"),
                &[&format!("<Control>{}", index + 1)],
                move |b| {
                    if b.mode.get() == mode {
                        return;
                    }
                    let selected = b
                        .selected()
                        .iter()
                        .map(|i| i.file.uri().to_string())
                        .collect();
                    let file = b.current.borrow().clone();
                    b.mode.set(mode);
                    b.folder_settings.borrow_mut().mode = mode;
                    b.save_folder_settings();
                    if b.is_collection() {
                        b.open_collection(b.capture_entry().location, selected);
                    } else {
                        b.navigate(file, selected);
                    }
                },
            );
        }
        self.action(app, "open", &["Return", "KP_Enter"], |b| {
            if !b.text_focused() {
                b.open_selected();
            }
        });
        self.action(app, "select-all", &["<Control>a"], |b| {
            if b.text_focused() {
                return;
            }
            if let Some(pane) = b.active_pane() {
                pane.selection.select_all();
            }
        });
        self.action(app, "copy", &["<Control>c"], |b| {
            if !b.text_focused() {
                b.copy(false);
            }
        });
        self.action(app, "cut", &["<Control>x"], |b| {
            if !b.text_focused() {
                b.copy(true);
            }
        });
        self.action(app, "paste", &["<Control>v"], |b| {
            if !b.text_focused() {
                b.paste();
            }
        });
        self.action(app, "new-folder", &["<Control><Shift>n"], |b| {
            b.name_dialog(false)
        });
        self.action(app, "rename", &["F2"], |b| {
            if !b.text_focused() {
                b.name_dialog(true);
            }
        });
        self.action(app, "trash", &["Delete"], |b| {
            if !b.text_focused() {
                if b.in_trash() {
                    b.confirm_delete();
                    return;
                }
                let selected = b.selected_files();
                if !selected.is_empty() {
                    b.enqueue(Operation::Trash(selected));
                }
            }
        });
        self.action(app, "delete", &["<Shift>Delete"], |b| {
            if !b.text_focused() {
                b.confirm_delete();
            }
        });
        self.action(app, "about", &[], |b| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Finder Gnome")
                .application_icon("io.github.finder_gnome")
                .version(env!("CARGO_PKG_VERSION"))
                .comments(tr("A Finder-inspired file manager for Ubuntu."))
                .developer_name("Slavick")
                .developers(["Slavick"])
                .copyright("Copyright © 2026 Slavick")
                .license_type(gtk::License::MitX11)
                .build();
            dialog.present(Some(&b.window));
        });
    }

    fn text_focused(&self) -> bool {
        let mut widget = gtk::prelude::GtkWindowExt::focus(&self.window);
        while let Some(focus) = widget {
            if focus.is::<gtk::Editable>() || focus.is::<gtk::TextView>() {
                return true;
            }
            widget = focus.parent();
        }
        false
    }

    fn populate_sidebar(self: &Rc<Self>, list: &gtk::ListBox) {
        let mut places = vec![
            (
                tr("Home"),
                "user-home-symbolic",
                SidebarLocation::Folder(gio::File::for_path(glib::home_dir())),
            ),
            (
                tr("Applications"),
                "view-app-grid-symbolic",
                SidebarLocation::Applications,
            ),
        ];
        for (directory, label, icon) in [
            (
                glib::UserDirectory::Desktop,
                mark("Desktop"),
                "user-desktop-symbolic",
            ),
            (
                glib::UserDirectory::Documents,
                mark("Documents"),
                "folder-documents-symbolic",
            ),
            (
                glib::UserDirectory::Downloads,
                mark("Downloads"),
                "folder-download-symbolic",
            ),
            (
                glib::UserDirectory::Music,
                mark("Music"),
                "folder-music-symbolic",
            ),
            (
                glib::UserDirectory::Pictures,
                mark("Pictures"),
                "folder-pictures-symbolic",
            ),
            (
                glib::UserDirectory::Videos,
                mark("Videos"),
                "folder-videos-symbolic",
            ),
        ] {
            if let Some(path) =
                glib::user_special_dir(directory).filter(|p| p.is_dir() && p != &glib::home_dir())
            {
                let file = gio::File::for_path(path);
                if !places
                    .iter()
                    .any(|(_, _, p)| matches!(p, SidebarLocation::Folder(f) if f.equal(&file)))
                {
                    places.push((tr(label), icon, SidebarLocation::Folder(file)));
                }
            }
        }
        places.push((
            tr("File System"),
            "drive-harddisk-symbolic",
            SidebarLocation::Folder(gio::File::for_path("/")),
        ));
        places.push((
            tr("Trash"),
            "user-trash-symbolic",
            SidebarLocation::Folder(gio::File::for_uri("trash:///")),
        ));
        let locations: Vec<_> = places
            .iter()
            .map(|(_, _, location)| location.clone())
            .collect();
        *self.places.borrow_mut() = locations.clone();
        for (label, icon, _) in &places {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
            row.append(&gtk::Image::from_icon_name(icon));
            row.append(&gtk::Label::new(Some(label)));
            list.append(&row);
        }
        for (index, location) in locations.iter().enumerate() {
            if let SidebarLocation::Folder(file) = location {
                let file = file.clone();
                if let Some(row) = list.row_at_index(index as i32) {
                    self.add_drop_target(&row, move |_, _| Some(file.clone()));
                }
            }
        }
        let weak = Rc::downgrade(self);
        list.connect_row_activated(move |_, row| {
            if let Some(browser) = weak.upgrade() {
                match &locations[row.index() as usize] {
                    SidebarLocation::Folder(file) => browser.navigate(file.clone(), HashSet::new()),
                    SidebarLocation::Applications => browser.show_applications(),
                }
            }
        });
        list.set_activate_on_single_click(true);
    }

    pub fn navigate(self: &Rc<Self>, file: gio::File, selected: HashSet<String>) {
        if !files::browsable(&file) {
            self.error(&tr(
                "Use a local path, smb:// or sftp:// address without a password.",
            ));
            return;
        }
        if !self.defer_load.get() {
            self.search_bar.set_reveal_child(false);
            if let Some(timer) = self.search_timer.borrow_mut().take() {
                timer.remove();
            }
        }
        self.stop_previews();
        self.record_departure();
        if !self.replaying.get() {
            let settings = self.settings.borrow().for_file(&file);
            self.mode.set(settings.mode);
            self.hidden.set(settings.hidden);
            *self.folder_settings.borrow_mut() = settings;
        }
        if files::is_trash(&file)
            && matches!(self.mode.get(), ViewMode::Columns | ViewMode::Gallery)
        {
            self.mode.set(ViewMode::List);
            self.folder_settings.borrow_mut().mode = ViewMode::List;
        }
        self.applications.borrow_mut().take();
        for pane in self.panes.borrow().iter() {
            pane.connection_cancel.borrow().cancel();
            pane.model.cancel();
        }
        self.panes.borrow_mut().clear();
        while let Some(child) = self.content.borrow().first_child() {
            self.content.borrow().remove(&child);
        }
        self.active.set(0);
        *self.current.borrow_mut() = file.clone();
        self.columns_scroll.borrow().set_hscrollbar_policy(
            if self.mode.get() == ViewMode::Columns {
                gtk::PolicyType::Automatic
            } else {
                gtk::PolicyType::Never
            },
        );
        let pane = self.make_pane(file, selected);
        self.content.borrow().append(&pane.widget);
        self.panes.borrow_mut().push(pane.clone());
        for (i, button) in self.mode_buttons.iter().enumerate() {
            button.set_active(i == self.mode.get() as usize);
        }
        self.update_path();
        self.load_pane(&pane);
        self.update_status();
        self.focus_active();
        self.record_arrival();
    }

    fn show_applications(self: &Rc<Self>) {
        self.stop_previews();
        self.record_departure();
        self.applications.borrow_mut().take();
        for pane in self.panes.borrow().iter() {
            pane.connection_cancel.borrow().cancel();
            pane.model.cancel();
        }
        self.panes.borrow_mut().clear();
        while let Some(child) = self.content.borrow().first_child() {
            self.content.borrow().remove(&child);
        }
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        self.title_stack.set_visible_child_name("title");
        self.title.set_label(&tr("Applications"));
        self.window
            .set_title(Some(&format!("{} — Finder Gnome", tr("Applications"))));
        self.breadcrumbs
            .append(&gtk::Label::new(Some(&tr("Applications"))));
        self.columns_scroll
            .borrow()
            .set_hscrollbar_policy(gtk::PolicyType::Never);
        for button in &self.mode_buttons {
            button.set_active(false);
        }
        let weak = Rc::downgrade(self);
        let weak_error = Rc::downgrade(self);
        let applications = ApplicationsView::new(
            &self.window,
            move |_, _| {
                if let Some(browser) = weak.upgrade() {
                    browser.update_status();
                }
            },
            move |message| {
                if let Some(browser) = weak_error.upgrade() {
                    browser.error(message);
                }
            },
        );
        self.content.borrow().append(&applications.widget);
        *self.applications.borrow_mut() = Some(applications.clone());
        let index = self
            .places
            .borrow()
            .iter()
            .position(|p| matches!(p, SidebarLocation::Applications));
        self.sidebar.select_row(
            index
                .and_then(|i| self.sidebar.row_at_index(i as i32))
                .as_ref(),
        );
        self.update_status();
        applications.search.grab_focus();
        self.record_arrival();
    }

    fn active_pane(&self) -> Option<Rc<Pane>> {
        self.panes.borrow().get(self.active.get()).cloned()
    }

    fn focus_active(&self) {
        if let Some(pane) = self.active_pane() {
            if let Some(child) = pane.stack.child_by_name("files") {
                child.child_focus(gtk::DirectionType::TabForward);
            }
        }
    }

    fn selected(&self) -> Vec<FileItem> {
        self.active_pane()
            .map(|pane| pane.selected())
            .unwrap_or_default()
    }
    fn selected_files(&self) -> Vec<gio::File> {
        self.selected().into_iter().map(|i| i.file).collect()
    }

    fn update_path(self: &Rc<Self>) {
        let file = self.current.borrow().clone();
        let selected = self
            .places
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(index, location)| match location {
                SidebarLocation::Folder(place) => Some((index, place)),
                SidebarLocation::Applications => None,
            })
            .filter(|(_, place)| file.equal(*place) || file.has_prefix(*place))
            .max_by_key(|(_, place)| place.uri().len())
            .map(|(index, _)| index as i32);
        self.sidebar
            .select_row(selected.and_then(|i| self.sidebar.row_at_index(i)).as_ref());
        self.title.set_label(
            &file
                .basename()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| tr("File System")),
        );
        if let Some(pane) = self.active_pane() {
            match &*pane.location.borrow() {
                Location::Search { .. } => self.title.set_label(&tr("Search Results")),
                Location::Tag(_) => self.title.set_label(&tr("Tagged Files")),
                _ => {}
            }
        }
        if files::is_trash(&file) {
            self.title.set_label(&tr("Trash"));
        }
        self.window
            .set_title(Some(&format!("{} — Finder Gnome", self.title.text())));
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }
        let mut chain = vec![file];
        while let Some(parent) = chain.last().and_then(|f| f.parent()) {
            chain.push(parent);
        }
        for (index, file) in chain.into_iter().rev().enumerate() {
            if index > 0 {
                self.breadcrumbs.append(&gtk::Label::new(Some("›")));
            }
            let label = if files::is_trash(&file) {
                tr("Trash")
            } else {
                file.basename()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| tr("File System"))
            };
            let button = gtk::Button::with_label(&label);
            button.add_css_class("flat");
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(browser) = weak.upgrade() {
                    browser.navigate(file.clone(), HashSet::new());
                }
            });
            self.breadcrumbs.append(&button);
        }
        let scroll = self.breadcrumb_scroll.clone();
        glib::idle_add_local_once(move || {
            let a = scroll.hadjustment();
            a.set_value(a.upper());
        });
    }

    fn update_status(&self) {
        if let Some(applications) = self.applications.borrow().as_ref() {
            self.status.set_label(&if applications.is_loading() {
                tr("Loading applications…")
            } else {
                count(
                    "{count} application",
                    "{count} applications",
                    applications.count(),
                )
            });
            for (name, action) in self.actions.borrow().iter() {
                action.set_enabled(match name.as_str() {
                    "up" | "reload" | "about" | "new-tab" | "duplicate-tab" | "close-tab"
                    | "preferences" | "search" | "connect-server" | "manage-tags" => true,
                    "next-tab" | "previous-tab" => self.tab_view.n_pages() > 1,
                    "back" => self.history.borrow().can_back(),
                    "forward" => self.history.borrow().can_forward(),
                    "open" | "properties" => applications.has_selection(),
                    _ => false,
                });
            }
            return;
        }
        let pane = self.active_pane();
        let items = pane.as_ref().map(|p| p.selection.n_items()).unwrap_or(0);
        let selected = pane
            .as_ref()
            .map(|p| p.selection.selection().size() as u32)
            .unwrap_or(0);
        let mut message = count("{count} item", "{count} items", items);
        if selected > 0 {
            message = tr("{items}, {selected}")
                .replace("{items}", &message)
                .replace(
                    "{selected}",
                    &count("{count} selected", "{count} selected", selected),
                );
        }
        if pane.as_ref().is_some_and(|p| p.loading.get()) {
            message = tr("Loading… {items}").replace("{items}", &message);
        }
        if let Some(pane) = &pane {
            let notice = pane.notice.borrow();
            if !notice.is_empty() {
                message.push_str(" · ");
                message.push_str(&notice);
            }
        }
        self.status.set_label(&message);
        for (name, action) in self.actions.borrow().iter() {
            let enabled = match name.as_str() {
                "back" => self.history.borrow().can_back(),
                "forward" => self.history.borrow().can_forward(),
                "next-tab" | "previous-tab" => self.tab_view.n_pages() > 1,
                "open-new-tab" => self.selected().iter().any(|i| i.is_dir),
                "rename" => {
                    selected == 1 && self.selected().iter().all(|f| f.can_rename != Some(false))
                }
                "trash" => {
                    selected > 0 && self.selected().iter().all(|f| f.can_trash != Some(false))
                }
                "delete" => {
                    selected > 0 && self.selected().iter().all(|f| f.can_delete != Some(false))
                }
                "properties" => selected <= 1,
                "open-with" => selected == 1 && self.selected().iter().all(|f| !f.is_dir),
                "copy-to" | "compress" => selected > 0,
                "move-to" => {
                    selected > 0 && self.selected().iter().all(|f| f.can_delete != Some(false))
                }
                "extract" => selected > 0 && self.selected().iter().all(file_commands::is_archive),
                "tags" => selected > 0,
                "show-parent" => selected == 1,
                "view-2" => !self.is_collection(),
                "new-folder" | "paste" => {
                    !self.is_collection()
                        && pane
                            .as_ref()
                            .is_none_or(|p| p.writable.get() != Some(false))
                }
                "open" | "copy" | "cut" | "quick-look" => selected > 0,
                "up" => self.current.borrow().parent().is_some(),
                _ => true,
            };
            let enabled = if self.in_trash() {
                match name.as_str() {
                    "restore" | "restore-to" | "delete" | "trash" => selected > 0,
                    "empty-trash" => items > 0 && pane.as_ref().is_some_and(|p| !p.loading.get()),
                    "open" | "open-with" | "open-new-tab" | "quick-look" | "rename" | "copy"
                    | "cut" | "paste" | "copy-to" | "move-to" | "compress" | "extract" | "tags"
                    | "show-parent" | "new-folder" | "search" | "view-2" | "view-3" => false,
                    _ => enabled,
                }
            } else if matches!(name.as_str(), "restore" | "restore-to" | "empty-trash") {
                false
            } else {
                enabled
            };
            action.set_enabled(enabled);
        }
    }

    fn make_pane(self: &Rc<Self>, file: gio::File, saved: HashSet<String>) -> Rc<Pane> {
        let model = DirectoryModel::new(file);
        let show_hidden = Rc::new(Cell::new(self.hidden.get()));
        let hidden = show_hidden.clone();
        let filter = gtk::CustomFilter::new(move |object| hidden.get() || !item(object).hidden);
        let filtered = gtk::FilterListModel::new(Some(model.store.clone()), Some(filter.clone()));
        let options = self.folder_settings.borrow().clone();
        let sorter = gtk::CustomSorter::new(move |a, b| {
            let (a, b) = (item(a), item(b));
            let group = if options.folders_first {
                b.is_dir.cmp(&a.is_dir)
            } else {
                std::cmp::Ordering::Equal
            };
            group
                .then_with(|| {
                    let order = files::compare_field(&a, &b, options.sort_field);
                    if options.descending {
                        order.reverse()
                    } else {
                        order
                    }
                })
                .into()
        });
        let sorted = gtk::SortListModel::new(Some(filtered), Some(sorter));
        let selection = gtk::MultiSelection::new(Some(sorted.clone()));
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        let loading = adw::StatusPage::builder()
            .title(tr("Loading…"))
            .icon_name("folder-symbolic")
            .build();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(
            &adw::StatusPage::builder()
                .title(tr("This folder is empty"))
                .icon_name("folder-symbolic")
                .build(),
            Some("empty"),
        );
        let error = gtk::Label::builder()
            .wrap(true)
            .selectable(true)
            .max_width_chars(45)
            .build();
        let retry = gtk::Button::with_label(&tr("Try Again"));
        retry.set_halign(gtk::Align::Center);
        let error_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        error_box.set_valign(gtk::Align::Center);
        error_box.set_halign(gtk::Align::Center);
        error_box.set_margin_start(18);
        error_box.set_margin_end(18);
        error_box.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        error_box.append(&error);
        error_box.append(&retry);
        stack.add_named(&error_box, Some("error"));
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_hexpand(self.mode.get() != ViewMode::Columns);
        widget.set_vexpand(true);
        if self.mode.get() == ViewMode::Columns {
            widget.set_width_request(self.folder_settings.borrow().column_pane_width);
            widget.add_css_class("column-pane");
        }
        if files::is_trash(&model.file) {
            let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            toolbar.set_margin_start(12);
            toolbar.set_margin_end(12);
            toolbar.set_margin_top(8);
            toolbar.set_margin_bottom(8);
            let label = gtk::Label::builder()
                .label(tr("Restore items or permanently delete them."))
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build();
            toolbar.append(&label);
            toolbar.append(
                &gtk::Button::builder()
                    .label(tr("Restore"))
                    .action_name("win.restore")
                    .build(),
            );
            toolbar.append(
                &gtk::Button::builder()
                    .label(tr("Empty Trash…"))
                    .action_name("win.empty-trash")
                    .build(),
            );
            widget.append(&toolbar);
        }
        widget.append(&stack);
        let pane = Rc::new(Pane {
            model: model.clone(),
            widget,
            stack,
            error,
            selection,
            sorted,
            filter,
            show_hidden,
            restoring: Cell::new(false),
            saved: RefCell::new(saved),
            loading: Cell::new(true),
            last_folder: RefCell::new(None),
            restore_scroll: Cell::new(None),
            preview: RefCell::new(None),
            context_menu: RefCell::new(None),
            location: RefCell::new(Location::directory(&model.file)),
            pending: Cell::new(false),
            connection_cancel: RefCell::new(gio::Cancellable::new()),
            notice: RefCell::new(String::new()),
            writable: Cell::new(None),
        });
        let view = match self.mode.get() {
            ViewMode::Icons => self.icon_view(&pane),
            ViewMode::List => self.list_view(&pane),
            ViewMode::Columns => self.column_view(&pane),
            ViewMode::Gallery => self.gallery_view(&pane),
        };
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(if self.mode.get() == ViewMode::Gallery {
                gtk::PolicyType::Never
            } else {
                gtk::PolicyType::Automatic
            })
            .vexpand(true)
            .child(&view)
            .build();
        pane.stack.add_named(&scroll, Some("files"));
        self.install_file_dnd(&pane, &view);
        let file = pane.model.file.clone();
        let weak_stack = pane.stack.downgrade();
        self.add_drop_target(&pane.stack, move |_, _| {
            let stack = weak_stack.upgrade()?;
            if !files::is_trash(&file) && stack.visible_child_name().as_deref() == Some("files") {
                None
            } else {
                Some(file.clone())
            }
        });
        pane.stack.set_visible_child_name("loading");

        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(&pane);
        pane.selection.connect_selection_changed(move |_, _, _| {
            let (Some(browser), Some(pane)) = (weak.upgrade(), weak_pane.upgrade()) else {
                return;
            };
            if !pane.restoring.get() {
                browser.selection_changed(&pane);
            }
        });
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(&pane);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(move |_| {
            if let (Some(browser), Some(pane)) = (weak.upgrade(), weak_pane.upgrade()) {
                browser.activate_pane(&pane);
            }
        });
        view.add_controller(focus);
        if self.mode.get() == ViewMode::Columns {
            let keys = gtk::EventControllerKey::new();
            let weak = Rc::downgrade(self);
            let weak_pane = Rc::downgrade(&pane);
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                if !modifiers.is_empty() {
                    return glib::Propagation::Proceed;
                }
                if let (Some(browser), Some(pane)) = (weak.upgrade(), weak_pane.upgrade()) {
                    browser.activate_pane(&pane);
                    if key == gdk::Key::Right {
                        browser.open_selected();
                        return glib::Propagation::Stop;
                    }
                    if key == gdk::Key::Left && browser.active.get() > 0 {
                        let parent = browser.panes.borrow()[browser.active.get() - 1].clone();
                        browser.activate_pane(&parent);
                        browser.focus_active();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            });
            view.add_controller(keys);
        }
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(&pane);
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |gesture, _, x, y| {
            if let (Some(browser), Some(pane)) = (weak.upgrade(), weak_pane.upgrade()) {
                browser.activate_pane(&pane);
                if gesture.current_button() == 3 {
                    let widget = gesture.widget().unwrap();
                    let mut picked = widget.pick(x, y, gtk::PickFlags::DEFAULT);
                    let mut position = None;
                    while let Some(child) = picked {
                        if let Some(index) = child
                            .widget_name()
                            .strip_prefix("finder-item-")
                            .and_then(|s| s.parse::<u32>().ok())
                        {
                            position = Some(index);
                            break;
                        }
                        if child == widget {
                            break;
                        }
                        picked = child.parent();
                    }
                    if let Some(position) = position {
                        if !pane.selection.is_selected(position) {
                            pane.selection.select_item(position, true);
                        }
                    } else {
                        pane.selection.unselect_all();
                    }
                    browser.context_menu(&pane, gesture.widget().as_ref().unwrap(), x, y);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        });
        view.add_controller(click);
        // Empty/error states also need a context menu (e.g. Paste, New Folder).
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(&pane);
        let click = gtk::GestureClick::new();
        click.set_button(3);
        click.connect_pressed(move |gesture, _, x, y| {
            if let (Some(browser), Some(pane)) = (weak.upgrade(), weak_pane.upgrade()) {
                if pane.stack.visible_child_name().as_deref() == Some("files") {
                    return;
                }
                browser.activate_pane(&pane);
                pane.selection.unselect_all();
                browser.context_menu(&pane, gesture.widget().as_ref().unwrap(), x, y);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        pane.stack.add_controller(click);

        let weak_pane = Rc::downgrade(&pane);
        let weak = Rc::downgrade(self);
        pane.model.listen(move |event| {
            let (Some(pane), Some(browser)) = (weak_pane.upgrade(), weak.upgrade()) else {
                return;
            };
            match event {
                LoadEvent::Begin => {
                    pane.notice.borrow_mut().clear();
                    let selected = pane.selected();
                    if !selected.is_empty() {
                        *pane.saved.borrow_mut() =
                            selected.iter().map(|i| i.file.uri().to_string()).collect();
                    }
                    pane.restoring.set(true);
                    pane.loading.set(true);
                    pane.stack.set_visible_child_name("loading");
                }
                LoadEvent::Batch => pane.stack.set_visible_child_name("files"),
                LoadEvent::Moved { source, target } => {
                    let catalog = browser.catalog.clone();
                    let weak = Rc::downgrade(&browser);
                    glib::MainContext::default().spawn_local(async move {
                        if let Err(error) = catalog.changed(source, Some(target), true).await {
                            if let Some(browser) = weak.upgrade() {
                                browser.report_catalog_error(&error);
                            }
                        } else if let Some(browser) = weak.upgrade() {
                            browser.refresh_tag_views();
                        }
                    });
                }
                LoadEvent::Notice(notice) => {
                    *pane.notice.borrow_mut() = notice;
                }
                LoadEvent::Finished => {
                    browser.decorate(&pane);
                    pane.loading.set(false);
                    for i in 0..pane.selection.n_items() {
                        if let Some(object) = pane.selection.item(i) {
                            if pane
                                .saved
                                .borrow()
                                .contains(item(&object).file.uri().as_str())
                            {
                                pane.selection.select_item(i, false);
                            }
                        }
                    }
                    pane.restoring.set(false);
                    pane.saved.borrow_mut().clear();
                    if browser.mode.get() == ViewMode::Columns {
                        let replaying = browser.replaying.replace(true);
                        let active = browser.active_pane();
                        browser.selection_changed(&pane);
                        if let Some(active) = active {
                            browser.activate_pane(&active);
                        }
                        browser.replaying.set(replaying);
                    }
                    pane.stack
                        .set_visible_child_name(if pane.selection.n_items() == 0 {
                            "empty"
                        } else {
                            "files"
                        });
                    if let Some(value) = pane.restore_scroll.take() {
                        if let Some(scroll) = pane
                            .stack
                            .child_by_name("files")
                            .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
                        {
                            glib::idle_add_local_once(move || {
                                scroll.vadjustment().set_value(value)
                            });
                        }
                    }
                }
                LoadEvent::Error(error) => {
                    pane.loading.set(false);
                    pane.restoring.set(false);
                    pane.error.set_label(&error);
                    pane.stack.set_visible_child_name("error");
                }
            }
            browser.update_status();
        });
        let weak_pane = Rc::downgrade(&pane);
        let retry_weak = Rc::downgrade(self);
        retry.connect_clicked(move |_| {
            if let Some(pane) = weak_pane.upgrade() {
                if let Some(browser) = retry_weak.upgrade() {
                    browser.load_pane(&pane);
                }
            }
        });
        pane
    }

    fn activate_pane(self: &Rc<Self>, pane: &Rc<Pane>) {
        let index = self.panes.borrow().iter().position(|p| Rc::ptr_eq(p, pane));
        if let Some(index) = index {
            let different = !self.current.borrow().equal(&pane.model.file);
            if different {
                self.record_departure();
            }
            self.active.set(index);
            *self.current.borrow_mut() = pane.model.file.clone();
            self.update_path();
            self.update_status();
            if different {
                self.record_arrival();
            }
        }
    }

    fn selection_changed(self: &Rc<Self>, pane: &Rc<Pane>) {
        if !self.panes.borrow().iter().any(|p| Rc::ptr_eq(p, pane)) {
            return;
        }
        self.activate_pane(pane);
        if self.mode.get() != ViewMode::Columns || pane.loading.get() {
            return;
        }
        let selected = pane.selected();
        let next = if selected.len() == 1 && selected[0].is_dir {
            Some(selected[0].file.clone())
        } else {
            None
        };
        let uri = next.as_ref().map(|f| f.uri().to_string());
        if *pane.last_folder.borrow() == uri {
            return;
        }
        *pane.last_folder.borrow_mut() = uri;
        let index = self.active.get();
        let removed: Vec<_> = self.panes.borrow_mut().drain(index + 1..).collect();
        for old in removed {
            self.content.borrow().remove(&old.widget);
        }
        if let Some(file) = next {
            let child = self.make_pane(file, HashSet::new());
            self.content.borrow().append(&child.widget);
            self.panes.borrow_mut().push(child.clone());
            child.model.reload();
            let scroll = self.columns_scroll.borrow().clone();
            glib::idle_add_local_once(move || {
                let a = scroll.hadjustment();
                a.set_value(a.upper());
            });
        }
    }

    fn activation(self: &Rc<Self>, pane: &Rc<Pane>, position: u32) {
        self.activate_pane(pane);
        if let Some(object) = pane.selection.item(position) {
            self.open_item(item(&object));
        }
    }

    fn icon_view(self: &Rc<Self>, pane: &Rc<Pane>) -> gtk::Widget {
        let factory = self.thumbnail_factory(self.folder_settings.borrow().icon_size);
        let view = gtk::GridView::new(Some(pane.selection.clone()), Some(factory));
        view.set_min_columns(1);
        view.set_max_columns(20);
        view.set_single_click_activate(false);
        view.set_enable_rubberband(true);
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        view.connect_activate(move |_, position| {
            if let (Some(b), Some(p)) = (weak.upgrade(), weak_pane.upgrade()) {
                b.activation(&p, position);
            }
        });
        view.upcast()
    }

    fn thumbnail_factory(&self, size: i32) -> gtk::SignalListItemFactory {
        let factory = name_factory(true);
        let tickets: Rc<RefCell<HashMap<usize, PreviewTicket>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let enabled = self.folder_settings.borrow().thumbnails;
        let service = self.previews.clone();
        let slots = tickets.clone();
        factory.connect_setup(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let image = item
                .child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Image>()
                .unwrap();
            image.set_pixel_size(size);
            let weak_item = item.downgrade();
            let service = service.clone();
            let slots = slots.clone();
            let unmap_slots = slots.clone();
            let key = item.as_ptr() as usize;
            image.connect_unmap(move |_| {
                unmap_slots.borrow_mut().remove(&key);
            });
            image.connect_map(move |image| {
                if enabled {
                    if let Some(item) = weak_item.upgrade() {
                        request_thumbnail(&service, &item, image, size, &slots);
                    }
                }
            });
        });
        let service = self.previews.clone();
        let slots = tickets.clone();
        factory.connect_bind(move |_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let image = item
                .child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Image>()
                .unwrap();
            if enabled && image.is_mapped() {
                request_thumbnail(&service, item, &image, size, &slots);
            }
        });
        factory.connect_unbind(move |_, object| {
            tickets.borrow_mut().remove(&(object.as_ptr() as usize));
        });
        factory
    }

    fn gallery_view(self: &Rc<Self>, pane: &Rc<Pane>) -> gtk::Widget {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let weak = Rc::downgrade(self);
        let preview = PreviewPane::new(self.previews.clone(), &self.window, move |file| {
            if let Some(browser) = weak.upgrade() {
                browser.open_item(file);
            }
        });
        widget.append(&preview.widget);
        let strip = gtk::ListView::new(
            Some(pane.selection.clone()),
            Some(self.thumbnail_factory(64)),
        );
        strip.set_orientation(gtk::Orientation::Horizontal);
        strip.set_single_click_activate(false);
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        strip.connect_activate(move |_, position| {
            if let (Some(b), Some(p)) = (weak.upgrade(), weak_pane.upgrade()) {
                b.activation(&p, position);
            }
        });
        let scroll = gtk::ScrolledWindow::builder()
            .child(&strip)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(130)
            .max_content_height(160)
            .build();
        widget.append(&scroll);
        let weak_preview = Rc::downgrade(&preview);
        let weak_pane = Rc::downgrade(pane);
        pane.selection.connect_selection_changed(move |_, _, _| {
            if let (Some(preview), Some(pane)) = (weak_preview.upgrade(), weak_pane.upgrade()) {
                preview.show_file(pane.selected().first().cloned());
            }
        });
        preview.show_file(None);
        *pane.preview.borrow_mut() = Some(preview);
        widget.upcast()
    }

    fn stop_previews(&self) {
        let quick = self.quick_look.borrow_mut().take();
        if let Some(quick) = quick {
            quick.preview.stop();
            quick.window.close();
        }
        for pane in self.panes.borrow().iter() {
            if let Some(preview) = pane.preview.borrow().as_ref() {
                preview.stop();
            }
        }
    }

    fn toggle_quick_look(self: &Rc<Self>) {
        if self.quick_look.borrow().is_some() {
            self.stop_previews();
            return;
        }
        let selected = self.selected();
        let Some(first) = selected.first() else {
            return;
        };
        let files = if selected.len() > 1 {
            selected.clone()
        } else {
            self.active_pane()
                .map(|p| {
                    (0..p.selection.n_items())
                        .filter_map(|i| p.selection.item(i))
                        .map(|o| item(&o))
                        .collect()
                })
                .unwrap_or_default()
        };
        let index = files
            .iter()
            .position(|f| f.file.equal(&first.file))
            .unwrap_or(0);
        let window = adw::Window::builder()
            .transient_for(&self.window)
            .default_width(900)
            .default_height(650)
            .title(tr("Quick Look"))
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = adw::HeaderBar::new();
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let previous = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(tr("Previous File"))
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text(tr("Next File"))
            .build();
        buttons.append(&previous);
        buttons.append(&next);
        header.pack_start(&buttons);
        body.append(&header);
        let weak = Rc::downgrade(self);
        let preview = PreviewPane::new(self.previews.clone(), &self.window, move |file| {
            if let Some(browser) = weak.upgrade() {
                browser.open_item(file);
            }
        });
        body.append(&preview.widget);
        window.set_content(Some(&body));
        let quick = Rc::new(QuickLook {
            window,
            preview,
            files,
            index: Cell::new(index),
        });
        for (button, delta) in [(previous, -1), (next, 1)] {
            let weak = Rc::downgrade(&quick);
            button.connect_clicked(move |_| {
                if let Some(quick) = weak.upgrade() {
                    quick.step(delta);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&quick);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if !modifiers.is_empty() {
                return glib::Propagation::Proceed;
            }
            if let Some(quick) = weak.upgrade() {
                match key {
                    gdk::Key::Escape => quick.window.close(),
                    gdk::Key::Left => quick.step(-1),
                    gdk::Key::Right => quick.step(1),
                    gdk::Key::Page_Up => quick.preview.change_page(-1),
                    gdk::Key::Page_Down => quick.preview.change_page(1),
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        quick.window.add_controller(keys);
        let weak = Rc::downgrade(self);
        quick.window.connect_close_request(move |_| {
            if let Some(browser) = weak.upgrade() {
                if let Some(quick) = browser.quick_look.borrow_mut().take() {
                    quick.preview.stop();
                }
            }
            glib::Propagation::Proceed
        });
        quick.preview.show_file(quick.files.get(index).cloned());
        quick.preview.request_preview();
        quick.window.present();
        *self.quick_look.borrow_mut() = Some(quick);
    }

    fn column_view(self: &Rc<Self>, pane: &Rc<Pane>) -> gtk::Widget {
        let view = gtk::ListView::new(Some(pane.selection.clone()), Some(name_factory(false)));
        view.set_single_click_activate(false);
        view.set_enable_rubberband(true);
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        view.connect_activate(move |_, position| {
            if let (Some(b), Some(p)) = (weak.upgrade(), weak_pane.upgrade()) {
                b.activation(&p, position);
            }
        });
        view.upcast()
    }

    fn list_view(self: &Rc<Self>, pane: &Rc<Pane>) -> gtk::Widget {
        let view = gtk::ColumnView::new(Some(pane.selection.clone()));
        view.set_show_row_separators(false);
        view.set_enable_rubberband(true);
        let options = self.folder_settings.borrow().clone();
        let mut columns = Vec::new();
        for (field, label) in [
            mark("Name"),
            mark("Size"),
            mark("Kind"),
            mark("Date Modified"),
        ]
        .iter()
        .enumerate()
        {
            let factory = if field == 0 {
                name_factory(false)
            } else {
                detail_factory(field)
            };
            let column = gtk::ColumnViewColumn::new(Some(&tr(label)), Some(factory));
            column.set_resizable(true);
            column.set_expand(field == 0);
            column.set_visible(options.visible_columns[field]);
            column.set_fixed_width(options.column_widths[field]);
            let sorter = gtk::CustomSorter::new(move |a, b| {
                files::compare_field(&item(a), &item(b), field).into()
            });
            column.set_sorter(Some(&sorter));
            view.append_column(&column);
            if field == options.sort_field {
                view.sort_by_column(
                    Some(&column),
                    if options.descending {
                        gtk::SortType::Descending
                    } else {
                        gtk::SortType::Ascending
                    },
                );
            }
            let weak = Rc::downgrade(self);
            let weak_pane = Rc::downgrade(pane);
            column.connect_fixed_width_notify(move |column| {
                if let (Some(b), Some(pane)) = (weak.upgrade(), weak_pane.upgrade()) {
                    if b.panes.borrow().iter().any(|p| Rc::ptr_eq(p, &pane)) {
                        b.folder_settings.borrow_mut().column_widths[field] = column.fixed_width();
                        b.save_folder_settings();
                    }
                }
            });
            columns.push(column);
        }
        let sorter = view.sorter().unwrap();
        let header_sorter = sorter.clone();
        let wrapper = gtk::CustomSorter::new(move |a, b| {
            let group = if options.folders_first {
                item(b).is_dir.cmp(&item(a).is_dir)
            } else {
                std::cmp::Ordering::Equal
            };
            if group != std::cmp::Ordering::Equal {
                group.into()
            } else {
                header_sorter.compare(a, b)
            }
        });
        let order = wrapper.clone();
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        sorter.connect_changed(move |sorter, _| {
            order.changed(gtk::SorterChange::Different);
            if let (Some(b), Some(pane), Some(sorter)) = (
                weak.upgrade(),
                weak_pane.upgrade(),
                sorter.downcast_ref::<gtk::ColumnViewSorter>(),
            ) {
                if b.panes.borrow().iter().any(|p| Rc::ptr_eq(p, &pane)) {
                    if let Some(index) = sorter
                        .primary_sort_column()
                        .and_then(|col| columns.iter().position(|c| *c == col))
                    {
                        b.folder_settings.borrow_mut().sort_field = index;
                        b.folder_settings.borrow_mut().descending =
                            sorter.primary_sort_order() == gtk::SortType::Descending;
                        b.save_folder_settings();
                    }
                }
            }
        });
        pane.sorted.set_sorter(Some(&wrapper));
        let weak = Rc::downgrade(self);
        let weak_pane = Rc::downgrade(pane);
        view.connect_activate(move |_, position| {
            if let (Some(b), Some(p)) = (weak.upgrade(), weak_pane.upgrade()) {
                b.activation(&p, position);
            }
        });
        view.upcast()
    }

    fn context_menu(&self, pane: &Pane, widget: &gtk::Widget, x: f64, y: f64) {
        let Some(point) =
            widget.compute_point(&pane.widget, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        // Context commands operate on files even when the preview text or
        // location entry previously had keyboard focus.
        if self.text_focused() {
            gtk::prelude::GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
        let popover = pane.ensure_context_menu();
        popover.set_pointing_to(Some(&gdk::Rectangle::new(
            point.x() as i32,
            point.y() as i32,
            1,
            1,
        )));
        popover.popup();
    }

    fn open_selected(self: &Rc<Self>) {
        if let Some(applications) = self.applications.borrow().as_ref() {
            applications.launch_selected();
            return;
        }
        let selected = self.selected();
        if selected.len() == 1 {
            self.open_item(selected[0].clone());
        } else {
            for file in selected.into_iter().filter(|i| !i.is_dir) {
                self.open_item(file);
            }
        }
    }

    fn open_item(self: &Rc<Self>, item: FileItem) {
        if files::is_trash(&item.file) {
            self.restore_trash(false);
            return;
        }
        if item.is_dir {
            if self.mode.get() == ViewMode::Columns {
                if let Some(pane) = self.active_pane() {
                    self.selection_changed(&pane);
                    let next = self.panes.borrow().get(self.active.get() + 1).cloned();
                    if let Some(next) = next.filter(|p| p.model.file.equal(&item.file)) {
                        self.activate_pane(&next);
                        self.focus_active();
                        return;
                    }
                }
            }
            self.navigate(item.file, HashSet::new());
            return;
        }
        if item.executable {
            self.error(&tr("Executable files are not launched automatically. Open this file with another application."));
            return;
        }
        let weak = Rc::downgrade(self);
        gio::AppInfo::launch_default_for_uri_async(
            &item.file.uri(),
            Some(&gtk::prelude::WidgetExt::display(&self.window).app_launch_context()),
            gio::Cancellable::NONE,
            move |result| {
                if let (Err(error), Some(browser)) = (result, weak.upgrade()) {
                    browser.error(&error.to_string());
                }
            },
        );
    }

    fn copy(&self, cut: bool) {
        let files = self.selected_files();
        if files.is_empty() {
            return;
        }
        let (gnome, uris) = files::clipboard_payload(&files, cut);
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_bytes(
                "x-special/gnome-copied-files",
                &glib::Bytes::from_owned(gnome.into_bytes()),
            ),
            gdk::ContentProvider::for_bytes(
                "text/uri-list",
                &glib::Bytes::from_owned(uris.into_bytes()),
            ),
        ]);
        if let Err(error) = self.window.clipboard().set_content(Some(&provider)) {
            self.error(&error.to_string());
        }
    }

    fn paste(self: &Rc<Self>) {
        let clipboard = self.window.clipboard();
        let owned_content = clipboard.content();
        let destination = self.current.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = async {
                let (stream, mime) = clipboard
                    .read_future(
                        &["x-special/gnome-copied-files", "text/uri-list"],
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let mut data = Vec::new();
                loop {
                    let chunk = stream
                        .read_bytes_future(8192, glib::Priority::DEFAULT)
                        .await
                        .map_err(|e| e.to_string())?;
                    if chunk.is_empty() {
                        break;
                    }
                    if data.len() + chunk.len() > 8 * 1024 * 1024 {
                        return Err(tr("The clipboard file list is too large."));
                    }
                    data.extend_from_slice(&chunk);
                }
                let text = std::str::from_utf8(&data).map_err(|e| e.to_string())?;
                files::parse_clipboard(text, mime == "x-special/gnome-copied-files")
            }
            .await;
            if let Some(browser) = weak.upgrade() {
                match result {
                    Ok((sources, moving)) => {
                        let clear = if moving {
                            owned_content.map(|provider| (clipboard.clone(), provider))
                        } else {
                            None
                        };
                        browser.enqueue_with_clipboard(
                            Operation::Copy {
                                sources,
                                destination,
                                moving,
                            },
                            clear,
                        );
                    }
                    Err(error) => browser.error(&error),
                }
            }
        });
    }

    fn name_dialog(self: &Rc<Self>, rename: bool) {
        let selected = self.selected();
        if rename && selected.len() != 1 {
            return;
        }
        let target = if rename {
            selected[0].file.clone()
        } else {
            self.current.borrow().clone()
        };
        let dialog = adw::AlertDialog::builder()
            .heading(if rename {
                tr(mark("Rename"))
            } else {
                tr("New Folder")
            })
            .build();
        dialog.add_responses(&[
            ("cancel", &tr("Cancel")),
            (
                "save",
                &if rename {
                    tr(mark("Rename"))
                } else {
                    tr("Create")
                },
            ),
        ]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let entry = gtk::Entry::builder()
            .text(if rename {
                selected[0].name.clone()
            } else {
                tr("Untitled Folder")
            })
            .activates_default(true)
            .build();
        let validation = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .css_classes(["error"])
            .build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&entry);
        body.append(&validation);
        dialog.set_extra_child(Some(&body));
        let weak_dialog = dialog.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(dialog) = weak_dialog.upgrade() {
                let result = files::validate_name(&entry.text());
                dialog.set_response_enabled("save", result.is_ok());
                validation.set_label(&result.err().unwrap_or_default());
            }
        });
        let weak = Rc::downgrade(self);
        let input = entry.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "save" {
                if let Some(browser) = weak.upgrade() {
                    browser.enqueue(if rename {
                        Operation::Rename(target.clone(), input.text().to_string())
                    } else {
                        Operation::Create(target.clone(), input.text().to_string())
                    });
                }
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    fn confirm_delete(self: &Rc<Self>) {
        let files = self.selected_files();
        if files.is_empty() {
            return;
        }
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Delete permanently?"))
            .body(count(
                "{count} item will be permanently deleted. This cannot be undone.",
                "{count} items will be permanently deleted. This cannot be undone.",
                files.len() as u32,
            ))
            .build();
        dialog.add_responses(&[
            ("cancel", &tr("Cancel")),
            ("delete", &tr("Delete Permanently")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "delete" {
                if let Some(browser) = weak.upgrade() {
                    browser.enqueue(if browser.in_trash() {
                        Operation::DeleteTrash(files.clone())
                    } else {
                        Operation::Delete(files.clone())
                    });
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    fn enqueue(self: &Rc<Self>, operation: Operation) {
        self.enqueue_with_clipboard(operation, None);
    }

    fn enqueue_with_clipboard(
        self: &Rc<Self>,
        operation: Operation,
        clear_clipboard: Option<(gdk::Clipboard, gdk::ContentProvider)>,
    ) {
        self.enqueue_complete(operation, clear_clipboard, None);
    }

    fn enqueue_complete(
        self: &Rc<Self>,
        operation: Operation,
        clear_clipboard: Option<(gdk::Clipboard, gdk::ContentProvider)>,
        mut drop_completion: Option<crate::dnd::Completion>,
    ) {
        let cancel = gio::Cancellable::new();
        let (tx, rx) = async_channel::unbounded();
        let task = self.add_task(&operation, &cancel);
        self.submit(operation, cancel.clone(), tx);
        self.jobs.borrow_mut().push(cancel.clone());
        self.operation_revealer.set_reveal_child(true);
        self.operation_cancel.set_sensitive(true);
        self.operation_cancel.set_label(&tr("Cancel All"));
        self.operation_label
            .set_label(&tr("Waiting for file operation…"));
        self.operation_progress.pulse();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                let Some(browser) = weak.upgrade() else {
                    cancel.cancel();
                    break;
                };
                match event {
                    Event::Changed {
                        source,
                        target,
                        moving,
                    } => {
                        let catalog = browser.catalog.clone();
                        let weak = Rc::downgrade(&browser);
                        glib::MainContext::default().spawn_local(async move {
                            if let Err(e) = catalog.changed(source, target, moving).await {
                                if let Some(b) = weak.upgrade() {
                                    b.report_catalog_error(&e);
                                }
                            } else if let Some(b) = weak.upgrade() {
                                b.refresh_tag_views();
                            }
                        });
                    }
                    Event::Failed { operation, error } => {
                        if error.kind == crate::errors::ErrorKind::Offline {
                            task.retry.set_label(&tr("Reconnect and Retry"));
                        }
                        task.failed.borrow_mut().push(operation);
                        task.label.set_tooltip_text(Some(&error.message()));
                    }
                    Event::Progress {
                        name,
                        completed,
                        bytes,
                    } => {
                        task.label
                            .set_label(&format!("{} · {name} · {completed}", task.title));
                        if let Some((current, total)) = bytes.filter(|(_, total)| *total > 0) {
                            task.progress
                                .set_fraction((current as f64 / total as f64).clamp(0.0, 1.0));
                        } else {
                            task.progress.pulse();
                        }
                        browser.operation_label.set_label(
                            &tr("{name} — {completed}").replace("{name}", &name).replace(
                                "{completed}",
                                &count(
                                    "{count} completed",
                                    "{count} completed",
                                    completed.min(u32::MAX as u64) as u32,
                                ),
                            ),
                        );
                        if let Some((current, total)) = bytes.filter(|(_, total)| *total > 0) {
                            browser
                                .operation_progress
                                .set_fraction((current as f64 / total as f64).clamp(0.0, 1.0));
                        } else {
                            browser.operation_progress.pulse();
                        }
                    }
                    Event::Conflict(conflict) => browser.conflict(conflict, cancel.clone()),
                    Event::Finished {
                        completed,
                        skipped,
                        errors,
                        cancelled,
                    } => {
                        if let Some(completion) = drop_completion.as_mut() {
                            completion.finish(!cancelled && errors.is_empty() && skipped == 0);
                        }
                        if !cancelled && skipped == 0 && errors.is_empty() {
                            if let Some((clipboard, provider)) = &clear_clipboard {
                                if clipboard.content().as_ref() == Some(provider) {
                                    let _ = clipboard.set_content(None::<&gdk::ContentProvider>);
                                }
                            }
                        }
                        browser.jobs.borrow_mut().retain(|job| job != &cancel);
                        browser
                            .operation_cancel
                            .set_label(&if browser.jobs.borrow().is_empty() {
                                tr("Dismiss")
                            } else {
                                tr("Cancel All")
                            });
                        let summary = tr("{completed}; {skipped}")
                            .replace(
                                "{completed}",
                                &count(
                                    "{count} completed",
                                    "{count} completed",
                                    completed.min(u32::MAX as u64) as u32,
                                ),
                            )
                            .replace(
                                "{skipped}",
                                &count(
                                    "{count} skipped",
                                    "{count} skipped",
                                    skipped.min(u32::MAX as u64) as u32,
                                ),
                            );
                        let summary = if cancelled {
                            tr("Cancelled. {summary} Completed changes were kept.")
                                .replace("{summary}", &summary)
                        } else {
                            summary
                        };
                        task.done.set(true);
                        task.cancel.set_sensitive(false);
                        task.retry.set_visible(!task.failed.borrow().is_empty());
                        task.progress.set_fraction(1.0);
                        task.label.set_label(&format!("{} · {summary}", task.title));
                        browser.operation_label.set_label(&summary);
                        browser.operation_progress.set_fraction(1.0);
                        if !errors.is_empty() {
                            browser.error(&format!("{summary}\n\n{}", errors.join("\n")));
                        }
                        for pane in browser.panes.borrow().iter() {
                            pane.model.reload();
                        }
                        if browser.closing.get() && browser.jobs.borrow().is_empty() {
                            browser.window.close();
                        }
                        break;
                    }
                }
            }
        });
    }

    fn conflict(self: &Rc<Self>, conflict: operations::Conflict, cancel: gio::Cancellable) {
        let dialog = adw::AlertDialog::builder()
            .heading(tr("An item with this name already exists"))
            .body(
                tr("Source: {source}\nDestination: {destination}")
                    .replace("{source}", &conflict.source)
                    .replace("{destination}", &conflict.destination),
            )
            .build();
        dialog.add_responses(&[
            ("cancel", &tr("Cancel Operation")),
            ("skip", &tr("Skip")),
            ("both", &tr("Keep Both")),
        ]);
        if conflict.both_directories {
            dialog.add_response("merge", &tr("Merge"));
        }
        if conflict.replace_allowed {
            dialog.add_response("replace", &tr("Replace"));
            dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        }
        dialog.set_default_response(Some("skip"));
        dialog.set_close_response("cancel");
        let remember =
            gtk::CheckButton::with_label(&tr("Apply to remaining conflicts of this type"));
        dialog.set_extra_child(Some(&remember));
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |dialog, response| {
            let decision = match response {
                "skip" => Decision::Skip,
                "both" => Decision::KeepBoth,
                "merge" => Decision::Merge,
                "replace" => Decision::Replace,
                _ => Decision::Cancel,
            };
            let _ = conflict.answer.send((decision, remember.is_active()));
            if let Some(browser) = weak.upgrade() {
                browser
                    .conflict_dialogs
                    .borrow_mut()
                    .retain(|(_, d)| d != dialog);
            }
        });
        self.conflict_dialogs
            .borrow_mut()
            .push((cancel, dialog.clone()));
        dialog.present(Some(&self.window));
    }

    fn cancel_job(&self, cancel: &gio::Cancellable) {
        cancel.cancel();
        let dialogs: Vec<_> = self
            .conflict_dialogs
            .borrow()
            .iter()
            .filter(|(c, _)| c == cancel)
            .map(|(_, d)| d.clone())
            .collect();
        for dialog in dialogs {
            // libadwaita 1.5 ignores close requests during the opening
            // transition. Keep closing a cancelled conflict until it is gone.
            let closed = Rc::new(Cell::new(false));
            let did_close = closed.clone();
            dialog.connect_closed(move |_| did_close.set(true));
            dialog.force_close();
            let weak = dialog.downgrade();
            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                let Some(dialog) = weak.upgrade().filter(|_| !closed.get()) else {
                    return glib::ControlFlow::Break;
                };
                dialog.force_close();
                glib::ControlFlow::Continue
            });
        }
    }
    fn cancel_all(&self) {
        for cancel in self.jobs.borrow().iter() {
            self.cancel_job(cancel);
        }
    }

    fn error(&self, message: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Unable to complete the action"))
            .build();
        let details = gtk::Label::builder()
            .label(message)
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .max_width_chars(65)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .child(&details)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(60)
            .max_content_height(280)
            .propagate_natural_height(true)
            .build();
        dialog.set_extra_child(Some(&scroll));
        dialog.add_response("close", &tr("Close"));
        dialog.set_close_response("close");
        dialog.present(Some(&self.window));
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(id) = self.search_timer.get_mut().take() {
            id.remove();
        }
        if let Some(id) = self.save_timer.get_mut().take() {
            id.remove();
        }
        self.storage.flush();
        for job in self.jobs.get_mut().iter() {
            job.cancel();
        }
    }
}

impl QuickLook {
    fn step(&self, delta: i32) {
        let index = self.index.get() as i32 + delta;
        if index >= 0 && (index as usize) < self.files.len() {
            self.index.set(index as usize);
            self.preview
                .show_file(self.files.get(index as usize).cloned());
            self.preview.request_preview();
        }
    }
}

fn request_thumbnail(
    service: &PreviewService,
    list_item: &gtk::ListItem,
    image: &gtk::Image,
    size: i32,
    tickets: &Rc<RefCell<HashMap<usize, PreviewTicket>>>,
) {
    let Some(object) = list_item.item() else {
        return;
    };
    let file = item(&object);
    if !crate::preview::supports_thumbnail(&file) {
        return;
    }
    if !file.file.is_native() {
        return;
    }
    let uri = file.file.uri();
    let modified = (file.modified, file.modified_usec);
    let (ticket, receiver) = service.request(file, size * image.scale_factor(), 0);
    tickets
        .borrow_mut()
        .insert(list_item.as_ptr() as usize, ticket);
    let image = image.downgrade();
    let weak_item = list_item.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Ok(result) = receiver.recv().await else {
            return;
        };
        let (Some(image), Some(list_item)) = (image.upgrade(), weak_item.upgrade()) else {
            return;
        };
        if list_item.item().is_some_and(|o| {
            let file = item(&o);
            file.file.uri() == uri && (file.modified, file.modified_usec) == modified
        }) {
            if let PreviewData::Raster(raster) = result.as_ref() {
                image.set_paintable(Some(&raster.texture()));
            }
        }
    });
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.connection_cancel.get_mut().cancel();
        self.model.cancel();
        if let Some(menu) = self.context_menu.get_mut().take() {
            menu.unparent();
        }
    }
}

impl Pane {
    fn ensure_context_menu(&self) -> gtk::PopoverMenu {
        let mut menu = self.context_menu.borrow_mut();
        let popover = menu.get_or_insert_with(|| {
            let model = if files::is_trash(&self.model.file) {
                trash::menu()
            } else {
                file_menu()
            };
            let popover = gtk::PopoverMenu::from_model(Some(&model));
            popover.set_has_arrow(false);
            // Keep the action hierarchy intact while GtkModelButton closes
            // the menu and then activates its win.* action. Parenting outside
            // the file view also isolates its selection/drag gestures.
            popover.set_parent(&self.widget);
            let weak = popover.downgrade();
            self.widget.connect_unmap(move |_| {
                if let Some(popover) = weak.upgrade() {
                    popover.popdown();
                }
            });
            popover
        });
        popover.clone()
    }

    fn selected(&self) -> Vec<FileItem> {
        let selection = self.selection.selection();
        (0..selection.size())
            .filter_map(|i| self.selection.item(selection.nth(i as u32)))
            .map(|object| item(&object))
            .collect()
    }
}

fn name_factory(icons: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(
            if icons {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            },
            if icons { 6 } else { 8 },
        );
        row.add_css_class(if icons { "file-tile" } else { "file-row" });
        let image = gtk::Image::new();
        image.set_pixel_size(if icons { 56 } else { 20 });
        row.append(&image);
        let label = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(if icons { 15 } else { 40 })
            .xalign(if icons { 0.5 } else { 0.0 })
            .hexpand(true)
            .build();
        if icons {
            label.set_wrap(true);
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            label.set_lines(2);
        }
        row.append(&label);
        if !icons {
            row.append(&gtk::Image::from_icon_name("go-next-symbolic"));
        }
        let badges = gtk::Label::builder()
            .use_markup(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        row.append(&badges);
        list_item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, object| {
        let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let file = item(&list_item.item().unwrap());
        let row = list_item.child().unwrap().downcast::<gtk::Box>().unwrap();
        row.set_widget_name(&format!("finder-item-{}", list_item.position()));
        let image = row.first_child().unwrap().downcast::<gtk::Image>().unwrap();
        image.set_from_gicon(&file.icon);
        let label = image
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        label.set_label(&file.name);
        row.set_tooltip_text(Some(&format!(
            "{}\n{}\n{}",
            file.name,
            file.file.parse_name(),
            file.location_hint
        )));
        let badges = row.last_child().unwrap().downcast::<gtk::Label>().unwrap();
        let mut markup = file
            .tags
            .iter()
            .map(|(name, color)| {
                format!(
                    "<span foreground=\"{}\">●</span> {}",
                    glib::markup_escape_text(color),
                    glib::markup_escape_text(name)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        if !file.location_hint.is_empty() {
            if !markup.is_empty() {
                markup.push('\n');
            }
            markup.push_str(&glib::markup_escape_text(&file.location_hint));
        }
        badges.set_markup(&markup);
        if !icons {
            label.next_sibling().unwrap().set_visible(file.is_dir);
        }
    });
    factory
}

fn detail_factory(field: usize) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let label = gtk::Label::builder()
            .xalign(if field == 1 { 1.0 } else { 0.0 })
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(30)
            .margin_start(8)
            .margin_end(12)
            .build();
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(move |_, object| {
        let list_item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let file = item(&list_item.item().unwrap());
        let label = list_item.child().unwrap().downcast::<gtk::Label>().unwrap();
        label.set_widget_name(&format!("finder-item-{}", list_item.position()));
        label.set_label(&match field {
            1 => file.size_text(),
            2 => file.kind,
            _ => file.date_text(),
        });
    });
    factory
}

fn file_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let open = gio::Menu::new();
    open.append(Some(&tr("Open")), Some("win.open"));
    open.append(Some(&tr("Open With…")), Some("win.open-with"));
    open.append(Some(&tr("Open in New Tab")), Some("win.open-new-tab"));
    open.append(Some(&tr("Quick Look")), Some("win.quick-look"));
    open.append(
        Some(&tr("Show in Enclosing Folder")),
        Some("win.show-parent"),
    );
    open.append(Some(&tr("Properties")), Some("win.properties"));
    open.append(Some(&tr("Tags…")), Some("win.tags"));
    open.append(Some(&tr("New Folder")), Some("win.new-folder"));
    menu.append_section(None, &open);
    let editing = gio::Menu::new();
    for (label, action) in [
        (mark("Copy"), "copy"),
        (mark("Cut"), "cut"),
        (mark("Copy To…"), "copy-to"),
        (mark("Move To…"), "move-to"),
        (mark("Paste"), "paste"),
        (mark("Select All"), "select-all"),
        (mark("Rename"), "rename"),
    ] {
        editing.append(Some(&tr(label)), Some(&format!("win.{action}")));
    }
    menu.append_section(None, &editing);
    let archives = gio::Menu::new();
    archives.append(Some(&tr("Compress…")), Some("win.compress"));
    archives.append(Some(&tr("Extract…")), Some("win.extract"));
    menu.append_section(None, &archives);
    let trash = gio::Menu::new();
    for (label, action) in [
        (mark("Restore"), "restore"),
        (mark("Restore To…"), "restore-to"),
        (mark("Empty Trash…"), "empty-trash"),
    ] {
        let item = gio::MenuItem::new(Some(&tr(label)), Some(&format!("win.{action}")));
        item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        trash.append_item(&item);
    }
    menu.append_section(None, &trash);
    let destructive = gio::Menu::new();
    destructive.append(Some(&tr("Move to Trash")), Some("win.trash"));
    destructive.append(Some(&tr("Delete Permanently…")), Some("win.delete"));
    menu.append_section(None, &destructive);
    menu
}

fn main_menu() -> gio::Menu {
    let menu = file_menu();
    let view = gio::Menu::new();
    view.append(Some(&tr("Connect to Server…")), Some("win.connect-server"));
    view.append(Some(&tr("Search…")), Some("win.search"));
    view.append(Some(&tr("Manage Tags…")), Some("win.manage-tags"));
    view.append(Some(&tr("Show Hidden Files")), Some("win.hidden"));
    view.append(Some(&tr("Reload")), Some("win.reload"));
    view.append(Some(&tr("Enter Location…")), Some("win.location"));
    view.append(Some(&tr("View Options…")), Some("win.view-options"));
    view.append(Some(&tr("Preferences…")), Some("win.preferences"));
    view.append(Some(&tr("About Finder Gnome")), Some("win.about"));
    menu.append_section(None, &view);
    let tabs = gio::Menu::new();
    tabs.append(Some(&tr("New Tab")), Some("win.new-tab"));
    tabs.append(Some(&tr("Duplicate Tab")), Some("win.duplicate-tab"));
    tabs.append(Some(&tr("Close Tab")), Some("win.close-tab"));
    menu.append_submenu(Some(&tr("Tabs")), &tabs);
    menu
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("ui/context_menu_tests.rs");
    include!("ui/stage2_tests.rs");
    include!("ui/stage3_tests.rs");
    include!("ui/trash_tests.rs");
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[track_caller]
    fn until(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let context = glib::MainContext::default();
        loop {
            while context.pending() {
                context.iteration(false);
            }
            if condition() {
                break;
            }
            assert!(Instant::now() < deadline, "GUI condition timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[track_caller]
    fn loaded(browser: &Browser) {
        until(|| browser.panes.borrow().iter().all(|p| !p.loading.get()));
    }

    fn action(browser: &Browser, name: &str) {
        browser.window.lookup_action(name).unwrap().activate(None);
    }

    fn select(browser: &Browser, name: &str) {
        let pane = browser.active_pane().unwrap();
        let index = (0..pane.selection.n_items())
            .find(|i| item(&pane.selection.item(*i).unwrap()).name == name)
            .unwrap();
        pane.selection.select_item(index, true);
    }

    fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget
            .downcast_ref::<gtk::Button>()
            .filter(|b| b.label().as_deref() == Some(label))
        {
            return Some(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(found) = button(&widget, label) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }

    #[track_caller]
    fn respond(browser: &Browser, label: &str) {
        let dialog = browser.window.visible_dialog().unwrap();
        let response = button(dialog.upcast_ref(), &tr(label)).unwrap();
        // libadwaita 1.5 ignores responses during the opening transition. Wait
        // until the response is actually presented, as a real pointer click does.
        let presented = Instant::now();
        until(|| response.is_mapped() && presented.elapsed() > Duration::from_millis(400));
        response.emit_clicked();
        until(|| browser.window.visible_dialog().is_none());
    }

    fn close_dialog(browser: &Browser) {
        let next = Cell::new(Instant::now());
        until(|| {
            let Some(dialog) = browser.window.visible_dialog() else {
                return true;
            };
            if Instant::now() >= next.get() {
                dialog.force_close();
                next.set(Instant::now() + Duration::from_millis(50));
            }
            false
        });
    }

    fn screenshot(browser: &Browser, name: &str) {
        let Some(path) = std::env::var_os("FINDER_GNOME_SCREENSHOTS") else {
            return;
        };
        // Let pending layout/frame work run before taking a widget snapshot.
        let start = Instant::now();
        until(|| start.elapsed() > Duration::from_millis(150));
        let paintable = gtk::WidgetPaintable::new(Some(&browser.window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            browser.window.width() as f64,
            browser.window.height() as f64,
        );
        let node = snapshot.to_node().expect("rendered window");
        let renderer = gtk::gsk::Renderer::for_surface(&browser.window.surface().unwrap()).unwrap();
        let texture = renderer.render_texture(&node, None);
        fs::create_dir_all(&path).unwrap();
        let saved = texture.save_to_png(std::path::PathBuf::from(path).join(format!("{name}.png")));
        renderer.unrealize();
        saved.unwrap();
    }

    #[test]
    #[ignore = "requires a GTK display; run under xvfb-run or in a GNOME session"]
    fn gui_acceptance() {
        adw::init().unwrap();
        gio::resources_register_include!("finder.gresource").unwrap();
        let css = gtk::CssProvider::new();
        css.load_from_resource("/io/github/finder_gnome/style.css");
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let app = adw::Application::builder()
            .application_id("io.github.finder_gnome.Tests")
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("Documents/Nested")).unwrap();
        fs::create_dir(temp.path().join("Pictures")).unwrap();
        fs::create_dir(temp.path().join("Empty")).unwrap();
        fs::write(temp.path().join("Readme.txt"), "Hello").unwrap();
        fs::write(temp.path().join(".hidden"), "hidden").unwrap();
        let file = gio::File::for_path(temp.path());
        let browser = Browser::new(&app, file.clone());
        browser.window.present();
        loaded(&browser);
        assert_eq!(browser.active_pane().unwrap().selection.n_items(), 4);
        select(&browser, "Readme.txt");
        screenshot(&browser, "icons");
        action(&browser, "view-1");
        loaded(&browser);
        assert_eq!(browser.selected()[0].name, "Readme.txt");
        screenshot(&browser, "list");
        action(&browser, "hidden");
        assert_eq!(browser.active_pane().unwrap().selection.n_items(), 5);
        action(&browser, "hidden");
        action(&browser, "view-2");
        loaded(&browser);
        select(&browser, "Documents");
        loaded(&browser);
        assert_eq!(browser.panes.borrow().len(), 2);
        browser.open_selected();
        loaded(&browser);
        assert_eq!(browser.active.get(), 1);
        select(&browser, "Nested");
        loaded(&browser);
        assert_eq!(browser.panes.borrow().len(), 3);
        screenshot(&browser, "columns");
        let parent = browser.panes.borrow()[0].clone();
        browser.activate_pane(&parent);
        select(&browser, "Pictures");
        loaded(&browser);
        assert_eq!(browser.panes.borrow().len(), 2);
        assert!(
            browser.panes.borrow()[1]
                .model
                .file
                .equal(&file.child("Pictures"))
        );

        browser.mode.set(ViewMode::Icons);
        browser.navigate(file.clone(), HashSet::new());
        loaded(&browser);
        select(&browser, "Readme.txt");
        browser.copy(false);
        browser.navigate(file.child("Empty"), HashSet::new());
        loaded(&browser);
        browser.paste();
        until(|| {
            file.child("Empty/Readme.txt").path().unwrap().exists()
                && browser.jobs.borrow().is_empty()
        });
        loaded(&browser);
        assert_eq!(
            fs::read_to_string(temp.path().join("Empty/Readme.txt")).unwrap(),
            "Hello"
        );
        browser.navigate(file.clone(), HashSet::new());
        loaded(&browser);
        browser.name_dialog(false);
        let dialog = browser
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        let input = dialog
            .extra_child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap();
        input.set_text("bad/name");
        assert!(!dialog.is_response_enabled("save"));
        input.set_text("Created");
        assert!(dialog.is_response_enabled("save"));
        screenshot(&browser, "new-folder");
        respond(&browser, "Create");
        until(|| browser.jobs.borrow().is_empty());
        assert!(temp.path().join("Created").is_dir());
        loaded(&browser);
        select(&browser, "Created");
        browser.name_dialog(true);
        let dialog = browser
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        dialog
            .extra_child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap()
            .set_text("Renamed");
        respond(&browser, "Rename");
        until(|| browser.jobs.borrow().is_empty());
        assert!(temp.path().join("Renamed").is_dir());
        loaded(&browser);
        select(&browser, "Renamed");
        browser.confirm_delete();
        respond(&browser, "Cancel");
        assert!(temp.path().join("Renamed").is_dir());

        // Cancelling while a conflict is awaiting input must unblock the worker.
        browser.enqueue(Operation::Copy {
            sources: vec![file.child("Readme.txt")],
            destination: file.child("Empty"),
            moving: false,
        });
        until(|| !browser.conflict_dialogs.borrow().is_empty());
        browser.cancel_all();
        until(|| browser.jobs.borrow().is_empty() && browser.window.visible_dialog().is_none());
        assert_eq!(
            fs::read_to_string(temp.path().join("Empty/Readme.txt")).unwrap(),
            "Hello"
        );

        browser.navigate(file.child("Empty"), HashSet::new());
        loaded(&browser);
        select(&browser, "Readme.txt");
        browser.copy(true);
        browser.navigate(file.child("Pictures"), HashSet::new());
        loaded(&browser);
        browser.paste();
        until(|| {
            temp.path().join("Pictures/Readme.txt").exists() && browser.jobs.borrow().is_empty()
        });
        assert!(!temp.path().join("Empty/Readme.txt").exists());
        assert!(browser.window.clipboard().content().is_none());
        browser.navigate(file.clone(), HashSet::new());
        loaded(&browser);

        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        screenshot(&browser, "dark");
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);

        // External edits are visible through the monitor, without pressing Reload.
        browser.navigate(file.clone(), HashSet::new());
        loaded(&browser);
        fs::write(temp.path().join("external.txt"), "external").unwrap();
        until(|| {
            browser.active_pane().unwrap().model.store.n_items() == 7
                && !browser.active_pane().unwrap().loading.get()
        });

        // A superseded enumeration must never replace the active directory.
        let large = temp.path().join("Large");
        fs::create_dir(&large).unwrap();
        for i in 0..10_000 {
            fs::write(large.join(format!("file{i}")), "").unwrap();
        }
        browser.navigate(gio::File::for_path(&large), HashSet::new());
        browser.navigate(file.child("Pictures"), HashSet::new());
        loaded(&browser);
        assert_eq!(browser.active_pane().unwrap().selection.n_items(), 1);
        browser.navigate(gio::File::for_path(&large), HashSet::new());
        loaded(&browser);
        assert_eq!(browser.active_pane().unwrap().selection.n_items(), 10_000);
        screenshot(&browser, "large-directory");
        browser.navigate(file.child("missing"), HashSet::new());
        loaded(&browser);
        assert_eq!(
            browser
                .active_pane()
                .unwrap()
                .stack
                .visible_child_name()
                .as_deref(),
            Some("error")
        );
        browser.navigate(file.clone(), HashSet::new());
        loaded(&browser);
        // Applications is a virtual location: filesystem actions must not target
        // the previously visited directory while this section is displayed.
        let apps_row = browser
            .places
            .borrow()
            .iter()
            .position(|p| matches!(p, SidebarLocation::Applications))
            .unwrap();
        browser.sidebar.emit_by_name::<()>(
            "row-activated",
            &[&browser.sidebar.row_at_index(apps_row as i32).unwrap()],
        );
        let applications = browser.applications.borrow().clone().unwrap();
        until(|| !applications.is_loading());
        assert!(browser.panes.borrow().is_empty());
        assert_eq!(browser.title.text().as_str(), "Applications");
        for name in [
            "new-folder",
            "paste",
            "rename",
            "trash",
            "delete",
            "view-0",
            "location",
        ] {
            assert!(
                !browser.window.lookup_action(name).unwrap().is_enabled(),
                "{name} must be disabled in Applications"
            );
        }
        screenshot(&browser, "applications");

        // Harmless real desktop launches, without installing a launcher or
        // starting arbitrary applications from the user's machine.
        use std::os::unix::fs::PermissionsExt;
        let script = temp.path().join("launch-fixture");
        fs::write(&script, "#!/bin/sh\nprintf launched > \"${0}.result\"\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut fixtures = Vec::new();
        for (id, name, hidden) in [
            ("alpha", "Alpha Test Application", false),
            ("beta", "Beta Test Application", false),
            ("hidden", "Hidden Test Application", true),
        ] {
            let desktop = temp.path().join(format!("{id}.desktop"));
            fs::write(&desktop, format!("[Desktop Entry]\nType=Application\nName={name}\nExec={}\nNoDisplay={hidden}\nTerminal=false\n", script.display())).unwrap();
            fixtures.push(
                gio::DesktopAppInfo::from_filename(desktop)
                    .unwrap()
                    .upcast::<gio::AppInfo>(),
            );
        }
        applications.replace_apps(fixtures);
        assert_eq!(applications.count(), 2);
        applications.search.set_text("BETA");
        until(|| applications.count() == 1);
        applications.selection.set_selected(0);
        action(&browser, "properties");
        until(|| browser.window.visible_dialog().is_some());
        assert!(
            widget_text(browser.window.visible_dialog().unwrap().upcast_ref())
                .contains("Beta Test Application")
        );
        respond(&browser, "Close");
        browser.open_selected();
        let result = script.with_file_name("launch-fixture.result");
        until(|| result.exists());
        assert_eq!(fs::read_to_string(&result).unwrap(), "launched");
        fs::remove_file(&result).unwrap();
        button(applications.widget.upcast_ref(), "Launch")
            .unwrap()
            .emit_clicked();
        until(|| result.exists());
        applications.search.set_text("no-such-application");
        until(|| applications.count() == 0);
        assert!(!browser.window.lookup_action("open").unwrap().is_enabled());
        applications.search.set_text("");
        until(|| applications.count() == 2);
        applications.selection.set_selected(0);
        fs::remove_file(&script).unwrap();
        browser.open_selected();
        until(|| browser.window.visible_dialog().is_some());
        respond(&browser, "Close");
        action(&browser, "up");
        loaded(&browser);
        assert!(browser.applications.borrow().is_none());
        assert!(browser.current.borrow().equal(&file));
        assert!(
            browser
                .window
                .lookup_action("new-folder")
                .unwrap()
                .is_enabled()
        );
        context_menu_acceptance(&browser);
        stage2_acceptance(&browser);
        stage3_acceptance(&browser);
        browser.window.destroy();
    }
}
