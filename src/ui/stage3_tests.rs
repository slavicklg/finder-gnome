// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

fn stage3_dialog_details(browser: &Browser) -> Option<String> {
    fn first_label(widget: &gtk::Widget) -> Option<String> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            return Some(label.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(text) = first_label(&widget) {
                return Some(text);
            }
            child = widget.next_sibling();
        }
        None
    }
    let dialog = browser
        .window
        .visible_dialog()?
        .downcast::<adw::AlertDialog>()
        .ok()?;
    first_label(&dialog.extra_child()?)
}

fn stage3_acceptance(browser: &Rc<Browser>) {
    fn value<T: 'static>(future: impl std::future::Future<Output = T> + 'static) -> T {
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        glib::MainContext::default().spawn_local(async move {
            *output.borrow_mut() = Some(future.await);
        });
        until(|| result.borrow().is_some());
        result.borrow_mut().take().unwrap()
    }
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("Nested")).unwrap();
    fs::create_dir(temp.path().join(".Hidden")).unwrap();
    fs::write(temp.path().join("needle.txt"), "search fixture").unwrap();
    fs::write(temp.path().join("Nested/NEEDLE.txt"), "nested").unwrap();
    fs::write(temp.path().join(".Hidden/needle.txt"), "hidden").unwrap();
    std::os::unix::fs::symlink(temp.path(), temp.path().join("Nested/loop")).unwrap();
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    action(browser, "view-2");
    loaded(browser);
    browser.open_collection(
        Location::Search {
            root: root.uri().to_string(),
            query: "needle".into(),
        },
        HashSet::new(),
    );
    loaded(browser);
    assert_eq!(browser.mode.get(), ViewMode::List);
    assert!(browser.is_collection());
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 2);
    assert!(!browser.window.lookup_action("view-2").unwrap().is_enabled());
    assert!(!browser.window.lookup_action("paste").unwrap().is_enabled());
    screenshot(browser, "search-results");
    action(browser, "hidden");
    loaded(browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 3);
    action(browser, "hidden");
    loaded(browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 2);
    browser.open_collection(
        Location::Search {
            root: root.uri().to_string(),
            query: "cancel me".into(),
        },
        HashSet::new(),
    );
    browser.active_pane().unwrap().model.stop();
    loaded(browser);
    assert!(
        browser
            .active_pane()
            .unwrap()
            .notice
            .borrow()
            .contains("stopped")
    );
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    let file = root.child("needle.txt");
    let info = file
        .query_info(
            crate::files::ATTRIBUTES,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .unwrap();
    let identity = crate::catalog::Identity {
        uri: file.uri().to_string(),
        stable: FileItem::from_info(&root, &info).identity,
    };
    let catalog = browser.catalog.clone();
    value(async move { catalog.assign(vec![identity], vec![1, 5], true).await }).unwrap();
    browser.active_pane().unwrap().model.reload();
    loaded(browser);
    until(|| {
        (0..browser.active_pane().unwrap().selection.n_items()).any(|i| {
            let f = item(&browser.active_pane().unwrap().selection.item(i).unwrap());
            f.name == "needle.txt" && f.tags.len() == 2
        })
    });
    select(browser, "needle.txt");
    action(browser, "tags");
    until(|| browser.window.visible_dialog().is_some());
    close_dialog(browser);
    let frame = Instant::now();
    until(|| frame.elapsed() > Duration::from_millis(350));
    browser.enqueue(Operation::Rename(file.clone(), "renamed.txt".into()));
    until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
    let catalog = browser.catalog.clone();
    let uri = root.child("renamed.txt").uri().to_string();
    assert_eq!(
        value(async move { catalog.labels(uri).await })
            .unwrap()
            .len(),
        2
    );
    browser.open_collection(Location::Tag(1), HashSet::new());
    until(|| {
        browser
            .active_pane()
            .is_some_and(|p| matches!(*p.model.source.borrow(), crate::directory::Source::Files(_)))
    });
    loaded(browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 1);
    assert_eq!(browser.selected().len(), 0);
    screenshot(browser, "tagged-files");
    let catalog = browser.catalog.clone();
    let uri = root.child("gone.txt").uri().to_string();
    value(async move {
        catalog
            .assign(
                vec![crate::catalog::Identity { uri, stable: None }],
                vec![1],
                true,
            )
            .await
    })
    .unwrap();
    browser.open_collection(Location::Tag(1), HashSet::new());
    until(|| {
        browser
            .active_pane()
            .is_some_and(|p| matches!(*p.model.source.borrow(), crate::directory::Source::Files(_)))
    });
    loaded(browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 2);
    let remote = gio::File::for_uri("sftp://user@example.invalid/picture.png");
    let info = gio::FileInfo::new();
    info.set_name("picture.png");
    info.set_display_name("picture.png");
    info.set_content_type("image/png");
    let preview = crate::preview::PreviewPane::new(browser.previews.clone(), |_| {});
    preview.show_file(Some(FileItem::from_info(&remote.parent().unwrap(), &info)));
    assert!(preview.test_message().contains("download on request"));
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    action(browser, "manage-tags");
    until(|| browser.window.visible_dialog().is_some());
    close_dialog(browser);
    fs::write(temp.path().join("Nested/renamed.txt"), "keep destination").unwrap();
    // gone.txt deliberately has pending local tags. Completion of this copy
    // reports that fallback even when the copy itself is cancelled.
    let catalog = browser.catalog.clone();
    let expect_local_notice = !browser.local_tags_notice.get()
        && value(async move { catalog.pending_count().await }).unwrap() > 0;
    let task_count = browser.task_rows.borrow().len();
    browser.enqueue(Operation::Copy {
        sources: vec![root.child("renamed.txt")],
        destination: root.child("Nested"),
        moving: false,
    });
    until(|| browser.task_rows.borrow().len() > task_count);
    let task = browser.task_rows.borrow().last().unwrap().clone();
    until(|| {
        browser
            .conflict_dialogs
            .borrow()
            .iter()
            .any(|(token, _)| token == &task.token)
    });
    task.cancel.emit_clicked();
    until_described(
        || {
            task.done.get()
                && browser.jobs.borrow().is_empty()
                && !browser.undo_history.borrow().busy
        },
        || {
            format!(
                "cancelled copy: done={}, jobs={}, busy={}, conflicts={}, dialog={:?}",
                task.done.get(),
                browser.jobs.borrow().len(),
                browser.undo_history.borrow().busy,
                browser.conflict_dialogs.borrow().len(),
                stage3_dialog_details(browser)
            )
        },
    );
    assert!(task.token.is_cancelled());
    if expect_local_notice {
        let expected = tr(
            "Some tags are stored locally because file attributes could not be written. They will be retried when the files are available.",
        );
        until_described(
            || stage3_dialog_details(browser).as_deref() == Some(expected.as_str()),
            || {
                format!(
                    "expected local tag notice after cancellation, got {:?}",
                    stage3_dialog_details(browser)
                )
            },
        );
        assert!(browser.local_tags_notice.get());
        respond(browser, "Close");
    }
    until_described(
        || {
            browser.conflict_dialogs.borrow().is_empty()
                && browser.window.visible_dialog().is_none()
        },
        || {
            format!(
                "dialogs left after cancellation: conflicts={}, dialog={:?}",
                browser.conflict_dialogs.borrow().len(),
                stage3_dialog_details(browser)
            )
        },
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("Nested/renamed.txt")).unwrap(),
        "keep destination"
    );
    browser.enqueue(Operation::Create(
        root.child("missing-parent"),
        "retry-created".into(),
    ));
    until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
    until(|| browser.window.visible_dialog().is_some());
    close_dialog(browser);
    let failed = browser.task_rows.borrow().last().unwrap().clone();
    assert!(
        failed.retry.get_visible(),
        "{}; failures={}; cancelled={}",
        failed.label.text(),
        failed.failed.borrow().len(),
        failed.token.is_cancelled()
    );
    fs::create_dir(temp.path().join("missing-parent")).unwrap();
    failed.retry.emit_clicked();
    until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
    assert!(temp.path().join("missing-parent/retry-created").is_dir());
    file_commands_acceptance(browser);
    stage3_network_acceptance(browser);
}

fn stage3_network_acceptance(browser: &Rc<Browser>) {
    for variable in ["FINDER_GNOME_TEST_SFTP", "FINDER_GNOME_TEST_SMB"] {
        let Ok(uri) = std::env::var(variable) else {
            continue;
        };
        let root = gio::File::for_uri(&uri);
        browser.navigate(root.clone(), HashSet::new());
        loaded(browser);
        let pane = browser.active_pane().unwrap();
        assert_eq!(
            pane.stack.visible_child_name().as_deref(),
            Some("files"),
            "{variable}: {}",
            pane.error.text()
        );
        assert!(
            (0..pane.selection.n_items())
                .any(|i| item(&pane.selection.item(i).unwrap()).name == "origin.txt")
        );
        let temp = tempfile::tempdir().unwrap();
        let local = gio::File::for_path(temp.path());
        browser.enqueue(Operation::Copy {
            sources: vec![root.child("origin.txt")],
            destination: local.clone(),
            moving: false,
        });
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(
            browser.window.visible_dialog().is_none(),
            "download failed: {:?}",
            browser
                .task_rows
                .borrow()
                .last()
                .unwrap()
                .label
                .tooltip_text()
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("origin.txt")).unwrap(),
            "remote fixture\n"
        );
        fs::write(temp.path().join("upload.txt"), "upload fixture").unwrap();
        browser.enqueue(Operation::Copy {
            sources: vec![local.child("upload.txt")],
            destination: root.clone(),
            moving: false,
        });
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none(), "upload failed");
        browser.enqueue(Operation::Rename(
            root.child("upload.txt"),
            "renamed-remote.txt".into(),
        ));
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none());
        browser.enqueue(Operation::Copy {
            sources: vec![root.child("renamed-remote.txt")],
            destination: root.child("Folder"),
            moving: true,
        });
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none());
        browser.enqueue(Operation::Copy {
            sources: vec![root.child("Folder/renamed-remote.txt")],
            destination: local.clone(),
            moving: false,
        });
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none());
        assert_eq!(
            fs::read_to_string(temp.path().join("renamed-remote.txt")).unwrap(),
            "upload fixture"
        );
        browser.enqueue(Operation::Delete(vec![
            root.child("Folder/renamed-remote.txt"),
        ]));
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none());
        let image = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 32, 24)
            .unwrap();
        image.fill(0x3484e4ff);
        image
            .savev(temp.path().join("preview.png"), "png", &[])
            .unwrap();
        browser.enqueue(Operation::Copy {
            sources: vec![local.child("preview.png")],
            destination: root.clone(),
            moving: false,
        });
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none());
        browser.navigate(root.clone(), HashSet::new());
        action(browser, "view-3");
        loaded(browser);
        select(browser, "preview.png");
        let pane = browser.active_pane().unwrap();
        let preview = pane.preview.borrow().clone().unwrap();
        assert!(preview.test_message().contains("download on request"));
        assert!(!preview.test_raster());
        browser.toggle_quick_look();
        let quick = browser.quick_look.borrow().clone().unwrap();
        until(|| quick.preview.test_ready());
        assert!(
            quick.preview.test_raster(),
            "remote image: {}",
            quick.preview.test_message()
        );
        quick.window.close();
        until(|| browser.quick_look.borrow().is_none());
        browser.open_collection(
            Location::Search {
                root: root.uri().to_string(),
                query: "origin".into(),
            },
            HashSet::new(),
        );
        loaded(browser);
        assert_eq!(browser.active_pane().unwrap().selection.n_items(), 1);
        screenshot(
            browser,
            if variable.ends_with("SMB") {
                "smb-search"
            } else {
                "sftp-search"
            },
        );
        browser.enqueue(Operation::Delete(vec![root.child("preview.png")]));
        until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
        assert!(browser.window.visible_dialog().is_none());
        if variable.ends_with("SMB") {
            let readonly = gio::File::for_uri(&uri.replace("/share", "/readonly"));
            browser.navigate(readonly.clone(), HashSet::new());
            loaded(browser);
            browser.enqueue(Operation::Copy {
                sources: vec![local.child("origin.txt")],
                destination: readonly,
                moving: false,
            });
            until(|| browser.jobs.borrow().is_empty() && !browser.undo_history.borrow().busy);
            until(|| browser.window.visible_dialog().is_some());
            assert!(
                !browser
                    .task_rows
                    .borrow()
                    .last()
                    .unwrap()
                    .failed
                    .borrow()
                    .is_empty()
            );
            close_dialog(browser);
            assert!(temp.path().join("origin.txt").exists());
        }
        browser.navigate(local.clone(), HashSet::new());
        loaded(browser);
    }
}

fn file_commands_acceptance(browser: &Rc<Browser>) {
    let previous = browser.sidebar_paned.position();
    browser.sidebar_paned.set_position(275);
    browser.persist_now();
    until(|| browser.storage.read_settings().0.sidebar_width == 275);
    assert_eq!(browser.settings.borrow().sidebar_width, 275);
    screenshot(browser, "resized-sidebar");
    browser.sidebar_paned.set_position(previous);
    let confirmed = Rc::new(Cell::new(0));
    let count = confirmed.clone();
    browser.confirm_disconnect("Fixture Disk", true, move || count.set(count.get() + 1));
    until(|| browser.window.visible_dialog().is_some());
    respond(browser, "Cancel");
    assert_eq!(confirmed.get(), 0);
    let count = confirmed.clone();
    browser.confirm_disconnect("Fixture Disk", true, move || count.set(count.get() + 1));
    until(|| browser.window.visible_dialog().is_some());
    respond(browser, "Eject");
    assert_eq!(confirmed.get(), 1);
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("sample.ZIP"), "fixture").unwrap();
    fs::write(temp.path().join("sample.txt"), "fixture").unwrap();
    browser.navigate(gio::File::for_path(temp.path()), HashSet::new());
    loaded(browser);
    select(browser, "sample.txt");
    for name in ["open-with", "copy-to", "move-to", "compress"] {
        assert!(browser.window.lookup_action(name).unwrap().is_enabled());
    }
    assert!(
        !browser
            .window
            .lookup_action("extract")
            .unwrap()
            .is_enabled()
    );
    action(browser, "properties");
    until(|| {
        browser.window.visible_dialog().is_some_and(|dialog| {
            let text = widget_text(dialog.upcast_ref());
            text.contains("sample.txt")
                && text.contains("7 bytes")
                && text.contains(&temp.path().to_string_lossy().to_string())
        })
    });
    use std::os::unix::fs::PermissionsExt;
    fn permission_check(widget: &gtk::Widget, name: &str) -> Option<gtk::CheckButton> {
        if widget.widget_name() == name {
            return widget.clone().downcast().ok();
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(check) = permission_check(&widget, name) {
                return Some(check);
            }
            child = widget.next_sibling();
        }
        None
    }
    let dialog = browser.window.visible_dialog().unwrap();
    let check = permission_check(dialog.upcast_ref(), "permission-100").unwrap();
    let original = fs::metadata(temp.path().join("sample.txt"))
        .unwrap()
        .permissions()
        .mode();
    check.set_active(original & 0o100 == 0);
    button(dialog.upcast_ref(), "Apply Permissions")
        .unwrap()
        .emit_clicked();
    until(|| widget_text(dialog.upcast_ref()).contains("Permissions updated:"));
    assert_eq!(
        fs::metadata(temp.path().join("sample.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        (original ^ 0o100) & 0o777
    );
    screenshot(browser, "properties");
    respond(browser, "Close");
    select(browser, "sample.ZIP");
    assert!(
        browser
            .window
            .lookup_action("extract")
            .unwrap()
            .is_enabled()
    );
    browser.active_pane().unwrap().selection.unselect_all();
    for name in ["open-with", "copy-to", "move-to", "compress", "extract"] {
        assert!(!browser.window.lookup_action(name).unwrap().is_enabled());
    }
    action(browser, "properties");
    until(|| {
        browser.window.visible_dialog().is_some_and(|dialog| {
            let text = widget_text(dialog.upcast_ref());
            text.contains("Complete: 14 bytes") && text.contains("2 files")
        })
    });
    screenshot(browser, "folder-properties");
    respond(browser, "Close");
}

fn widget_text(widget: &gtk::Widget) -> String {
    let mut text = widget
        .downcast_ref::<gtk::Label>()
        .map(|label| label.text().to_string())
        .unwrap_or_default();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        text.push_str(&widget_text(&widget));
        text.push('\n');
        child = widget.next_sibling();
    }
    text
}

#[test]
#[ignore = "requires a GTK display; run tools/fixtures/gui.sh stage3_gui"]
fn stage3_gui() {
    adw::init().unwrap();
    gio::resources_register_include!("finder.gresource").unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.finder_gnome.Stage3Tests")
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let browser = Browser::new(&app, gio::File::for_path(temp.path()));
    browser.window.present();
    loaded(&browser);
    stage3_acceptance(&browser);
    browser.window.close();
}
