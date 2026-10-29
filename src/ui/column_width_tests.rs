// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

#[test]
#[ignore = "requires a GTK display; run under xvfb-run or in a GNOME session"]
fn column_width_acceptance() {
    fn label_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Label> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            if label.text() == name {
                return Some(label.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(label) = label_named(&widget, name) {
                return Some(label);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn assert_footer_above_scrollbar(browser: &Browser, pane: &Pane) {
        let scroll = browser.columns_scroll.borrow().clone();
        let scrollbar = scroll.hscrollbar();
        let button = pane.fit_width.as_ref().unwrap();
        until(|| {
            scrollbar.is_mapped()
                && scroll.hadjustment().upper() > scroll.hadjustment().page_size()
                && button
                    .compute_bounds(&browser.window)
                    .zip(scrollbar.compute_bounds(&browser.window))
                    .is_some_and(|(button, bar)| {
                        button.height() > 0.0
                            && bar.height() > 0.0
                            && button.y() + button.height() <= bar.y()
                    })
        });
        let bounds = button.compute_bounds(&browser.window).unwrap();
        // Check the bottom of the button, where an overlay scrollbar used to
        // intercept pointer input. Programmatic emit_clicked() cannot catch this.
        let mut picked = browser.window.pick(
            f64::from(bounds.x() + bounds.width() / 2.0),
            f64::from(bounds.y() + bounds.height() - 1.0),
            gtk::PickFlags::DEFAULT,
        );
        while let Some(widget) = picked {
            if widget == *button.upcast_ref::<gtk::Widget>() {
                return;
            }
            picked = widget.parent();
        }
        panic!("the fit-width button must receive pointer input above the scrollbar");
    }

    fn drag_column(browser: &Browser, pane: &Pane, delta: i32) {
        let handle = pane.widget.last_child().unwrap();
        assert!(handle.has_css_class("column-resize-handle"));
        until(|| handle.width() == COLUMN_RESIZE_HANDLE_WIDTH);
        let bounds = handle.compute_bounds(&browser.window).unwrap();
        let x = bounds.x() + bounds.width() / 2.0;
        let y = bounds.y() + bounds.height() / 2.0;
        assert_eq!(
            browser
                .window
                .pick(f64::from(x), f64::from(y), gtk::PickFlags::DEFAULT),
            Some(handle.clone())
        );
        let button = pane
            .fit_width
            .as_ref()
            .unwrap()
            .compute_bounds(&browser.window)
            .unwrap();
        assert!(button.x() + button.width() <= bounds.x());
        if std::env::var("GDK_BACKEND").as_deref() == Ok("x11") {
            // XFetchName uses the legacy STRING property; keep the fixture title ASCII.
            browser.window.set_title(Some("Finder Column Drag Test"));
            let scale = browser.window.scale_factor() as f64;
            let (surface_x, surface_y) = browser.window.surface_transform();
            let process = RefCell::new(
                std::process::Command::new("python3")
                    .arg("-c")
                    .arg(include_str!("../../tools/fixtures/column-drag.py"))
                    .arg(browser.window.title().unwrap())
                    .arg((((f64::from(x) + surface_x) * scale).round() as i32).to_string())
                    .arg((((f64::from(y) + surface_y) * scale).round() as i32).to_string())
                    .arg((delta * browser.window.scale_factor()).to_string())
                    .spawn()
                    .unwrap(),
            );
            let status = Cell::new(None);
            until(|| {
                status.set(process.borrow_mut().try_wait().unwrap());
                status.get().is_some()
            });
            assert!(status.get().unwrap().success());
        } else {
            pane.resize_column(pane.widget.width_request(), f64::from(delta));
        }
    }

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
        .application_id("io.github.finder_gnome.ColumnWidthTests")
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("Child")).unwrap();
    fs::write(temp.path().join("Child/short.txt"), "").unwrap();
    for i in 0..1000 {
        fs::write(temp.path().join(format!("file-{i:03}.txt")), "").unwrap();
    }
    let long_name = format!("zz-{}-Длинное文件.txt", "W".repeat(100));
    let hidden_name = format!(".{}", "W".repeat(230));
    fs::write(temp.path().join(&long_name), "").unwrap();
    fs::write(temp.path().join(&hidden_name), "").unwrap();
    let root = gio::File::for_path(temp.path());
    let browser = Browser::new(&app, root.clone());
    browser.window.present();
    loaded(&browser);
    action(&browser, "view-2");
    loaded(&browser);
    select(&browser, "Child");
    loaded(&browser);
    let pane = browser.panes.borrow()[0].clone();
    let neighbor = browser.panes.borrow()[1].clone();
    let neighbor_width = neighbor.widget.width_request();
    let configured_width = browser.folder_settings.borrow().column_pane_width;
    let button = pane.fit_width.as_ref().unwrap();
    until(|| button.is_mapped());
    assert!(button.is_sensitive());
    drag_column(&browser, &pane, 24);
    assert_eq!(pane.widget.width_request(), configured_width + 24);
    assert_eq!(neighbor.widget.width_request(), neighbor_width);
    assert_eq!(pane.selected()[0].name, "Child");
    assert_eq!(browser.panes.borrow().len(), 2);
    until(|| {
        pane.widget
            .compute_bounds(&pane.widget)
            .unwrap()
            .width()
            .round() as i32
            == configured_width + 24
    });
    drag_column(&browser, &pane, -150);
    assert_eq!(pane.widget.width_request(), MIN_COLUMN_WIDTH);
    pane.resize_column(configured_width, 0.0);
    neighbor.widget.set_width_request(1200);
    assert_footer_above_scrollbar(&browser, &pane);
    screenshot(&browser, "columns-fit-width-scrollbar");
    neighbor.widget.set_width_request(neighbor_width);
    assert!(label_named(pane.widget.upcast_ref(), &long_name).is_none());
    screenshot(&browser, "columns-fit-width-controls");
    button.emit_clicked();
    let fitted = pane.widget.width_request();
    assert!(
        fitted > 600,
        "long names must fit beyond the settings limit"
    );
    assert_eq!(neighbor.widget.width_request(), neighbor_width);
    assert_eq!(pane.selected()[0].name, "Child");
    assert_eq!(browser.panes.borrow().len(), 2);
    assert_eq!(
        browser.folder_settings.borrow().column_pane_width,
        configured_width
    );

    let scroll = pane
        .stack
        .child_by_name("files")
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    until(|| scroll.vadjustment().upper() > scroll.vadjustment().page_size());
    scroll.vadjustment().set_value(scroll.vadjustment().upper());
    until(|| {
        label_named(pane.widget.upcast_ref(), &long_name)
            .is_some_and(|label| label.width() > 0 && !label.layout().is_ellipsized())
    });
    screenshot(&browser, "columns-fit-width");
    assert!(
        button.is_mapped(),
        "footer must stay visible after scrolling"
    );

    action(&browser, "hidden");
    button.emit_clicked();
    assert!(pane.widget.width_request() > fitted);
    action(&browser, "hidden");
    button.emit_clicked();
    assert_eq!(pane.widget.width_request(), fitted);

    // Recompute after external removal and shrink back to the minimum.
    fs::remove_file(temp.path().join(&long_name)).unwrap();
    pane.model.reload();
    assert!(!button.is_sensitive(), "disabled while loading");
    loaded(&browser);
    button.emit_clicked();
    assert_eq!(pane.widget.width_request(), 180);
    until(|| (179..=200).contains(&pane.widget.width()));

    browser.navigate(root.clone(), HashSet::new());
    loaded(&browser);
    let reopened = browser.active_pane().unwrap();
    assert_eq!(reopened.widget.width_request(), configured_width);
    reopened.fit_width.as_ref().unwrap().emit_clicked();
    assert_eq!(reopened.widget.width_request(), 180);
    browser.navigate(root.child("Child"), HashSet::new());
    loaded(&browser);
    action(&browser, "view-2");
    loaded(&browser);
    let empty = browser.active_pane().unwrap();
    assert!(
        empty
            .widget
            .last_child()
            .unwrap()
            .has_css_class("column-resize-handle")
    );
    fs::remove_file(temp.path().join("Child/short.txt")).unwrap();
    empty.model.reload();
    loaded(&browser);
    assert!(!empty.fit_width.as_ref().unwrap().is_sensitive());
    browser.navigate(root.child("missing"), HashSet::new());
    loaded(&browser);
    action(&browser, "view-2");
    loaded(&browser);
    assert!(
        !browser
            .active_pane()
            .unwrap()
            .fit_width
            .as_ref()
            .unwrap()
            .is_sensitive()
    );
    fs::create_dir_all(temp.path().join("Child/Next/Next/Next")).unwrap();
    browser.open_in_new_tab(root.clone());
    loaded(&browser);
    action(&browser, "view-2");
    loaded(&browser);
    select(&browser, "Child");
    loaded(&browser);
    let tab_pane = browser.panes.borrow()[0].clone();
    browser.panes.borrow()[1].widget.set_width_request(1200);
    assert_footer_above_scrollbar(&browser, &tab_pane);
    for pane in browser.panes.borrow().iter() {
        pane.widget.set_width_request(360);
    }
    for _ in 0..3 {
        let parent = browser.panes.borrow().last().unwrap().clone();
        browser.activate_pane(&parent);
        // Start at the left to catch scrolling against a stale adjustment.
        browser.columns_scroll.borrow().hadjustment().set_value(0.0);
        select(&browser, "Next");
        loaded(&browser);
        let child = browser.panes.borrow().last().unwrap().clone();
        until(|| {
            let bounds = child
                .widget
                .compute_bounds(&*browser.content.borrow())
                .unwrap();
            let adjustment = browser.columns_scroll.borrow().hadjustment();
            let left = f64::from(bounds.x());
            let right = left + f64::from(bounds.width());
            child.widget.width() > 0
                && adjustment.value() > 0.0
                && left >= adjustment.value() - 1.0
                && right <= adjustment.value() + adjustment.page_size() + 1.0
        });
        assert_eq!(parent.selected()[0].name, "Next");
        assert!(Rc::ptr_eq(&browser.active_pane().unwrap(), &parent));
    }
    screenshot(&browser, "columns-auto-scroll");
    action(&browser, "view-1");
    loaded(&browser);
    assert!(browser.active_pane().unwrap().fit_width.is_none());
    browser.window.close();
}
