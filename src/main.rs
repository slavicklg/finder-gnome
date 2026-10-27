// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

mod applications;
mod catalog;
mod connections;
mod directory;
mod dnd;
mod errors;
mod files;
mod i18n;
mod operations;
mod preview;
mod properties;
mod state;
mod ui;

use adw::prelude::*;
use std::{cell::RefCell, rc::Rc};

thread_local! { static BROWSER: RefCell<Option<Rc<ui::Browser>>> = const { RefCell::new(None) }; }

fn main() -> glib::ExitCode {
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new("--finder-gnome-render-svg"))
    {
        std::process::exit(preview::svg::helper_main());
    }
    i18n::init();
    gio::resources_register_include!("finder.gresource").expect("embedded application resources");
    let app = adw::Application::builder()
        .application_id("io.github.finder_gnome")
        .resource_base_path("/io/github/finder_gnome")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(|_| {
        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/io/github/finder_gnome/style.css");
        gtk::Window::set_default_icon_name("io.github.finder_gnome");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display)
                .add_resource_path("/io/github/finder_gnome/icons");
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    });
    app.connect_activate(|app| present(app, None));
    app.connect_open(|app, files, _| {
        for file in files {
            present(app, Some(file.clone()));
        }
    });
    app.connect_shutdown(|_| {
        BROWSER.with(|b| b.borrow_mut().take());
    });
    app.run()
}

fn present(app: &adw::Application, file: Option<gio::File>) {
    BROWSER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(browser) = slot.as_ref() {
            if let Some(file) = file {
                browser.open_in_new_tab(file);
            }
            browser.window.present();
        } else {
            let browser = ui::Browser::new(
                app,
                file.clone()
                    .unwrap_or_else(|| gio::File::for_path(glib::home_dir())),
            );
            browser.restore_session(file);
            browser.window.present();
            *slot = Some(browser);
        }
    });
}
