// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

fn history_key(browser: &Browser, key: &str) -> bool {
    let Ok(output) = std::process::Command::new("xdotool")
        .args([
            "search",
            "--onlyvisible",
            "--pid",
            &std::process::id().to_string(),
        ])
        .output()
    else {
        return false;
    };
    assert!(output.status.success());
    let ids = String::from_utf8(output.stdout).unwrap();
    let surface = browser.window.surface().unwrap();
    let id = ids
        .lines()
        .find(|id| {
            let output = std::process::Command::new("xdotool")
                .args(["getwindowgeometry", "--shell", id])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            text.lines()
                .any(|line| line == format!("WIDTH={}", surface.width()))
                && text
                    .lines()
                    .any(|line| line == format!("HEIGHT={}", surface.height()))
        })
        .unwrap();
    let child = RefCell::new(
        std::process::Command::new("xdotool")
            .args(["windowfocus", "--sync", id, "key", "--clearmodifiers", key])
            .spawn()
            .unwrap(),
    );
    until(|| child.borrow_mut().try_wait().unwrap().is_some());
    true
}

fn undo_acceptance(browser: &Rc<Browser>) {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    browser.focus_active();
    browser.enqueue(Operation::Create(root.clone(), "undo-folder".into()));
    until(|| !browser.undo_history.borrow().busy);
    loaded(browser);
    assert!(browser.window.lookup_action("undo").unwrap().is_enabled());
    if !history_key(browser, "ctrl+z") {
        action(browser, "undo");
    }
    until(|| !temp.path().join("undo-folder").exists() && !browser.undo_history.borrow().busy);
    if !history_key(browser, "ctrl+shift+z") {
        action(browser, "redo");
    }
    until(|| temp.path().join("undo-folder").is_dir() && !browser.undo_history.borrow().busy);
    fs::write(temp.path().join("tagged"), "data").unwrap();
    browser.active_pane().unwrap().model.reload();
    loaded(browser);
    browser.edit_tags(
        tr("Change Tags"),
        crate::catalog::Edit::Assign {
            files: vec![crate::catalog::Identity::uri(&root.child("tagged"))],
            tags: vec![1],
            enabled: true,
        },
    );
    until(|| !browser.undo_history.borrow().busy);
    loaded(browser);
    until(|| {
        browser
            .active_pane()
            .unwrap()
            .model
            .store
            .iter::<glib::Object>()
            .filter_map(Result::ok)
            .any(|o| item(&o).name == "tagged" && !item(&o).tags.is_empty())
    });
    select(browser, "tagged");
    assert!(!browser.selected()[0].tags.is_empty());
    action(browser, "undo");
    until(|| !browser.undo_history.borrow().busy);
    loaded(browser);
    until(|| {
        browser
            .active_pane()
            .unwrap()
            .model
            .store
            .iter::<glib::Object>()
            .filter_map(Result::ok)
            .any(|o| item(&o).name == "tagged" && item(&o).tags.is_empty())
    });
    select(browser, "tagged");
    assert!(browser.selected()[0].tags.is_empty());
    action(browser, "redo");
    until(|| !browser.undo_history.borrow().busy);
    loaded(browser);
    until(|| {
        browser
            .active_pane()
            .unwrap()
            .model
            .store
            .iter::<glib::Object>()
            .filter_map(Result::ok)
            .any(|o| item(&o).name == "tagged" && !item(&o).tags.is_empty())
    });
    select(browser, "tagged");
    assert!(!browser.selected()[0].tags.is_empty());
    // A tab change uses the same operation history.
    browser.new_tab(browser.capture_entry());
    loaded(browser);
    browser.focus_active();
    action(browser, "undo");
    until(|| !browser.undo_history.borrow().busy);
    loaded(browser);
    until(|| {
        browser
            .active_pane()
            .unwrap()
            .model
            .store
            .iter::<glib::Object>()
            .filter_map(Result::ok)
            .any(|o| item(&o).name == "tagged" && item(&o).tags.is_empty())
    });
    select(browser, "tagged");
    assert!(browser.selected()[0].tags.is_empty());
    browser.enqueue(Operation::Create(root.clone(), "new-branch".into()));
    until(|| !browser.undo_history.borrow().busy);
    assert!(!browser.window.lookup_action("redo").unwrap().is_enabled());
    // Queue file and tag work back-to-back; both must be undoable in order.
    browser.enqueue(Operation::Rename(root.child("tagged"), "renamed".into()));
    browser.edit_tags(
        tr("Change Tags"),
        crate::catalog::Edit::Assign {
            files: vec![crate::catalog::Identity::uri(&root.child("renamed"))],
            tags: vec![2],
            enabled: true,
        },
    );
    until(|| !browser.undo_history.borrow().busy);
    loaded(browser);
    action(browser, "undo");
    until(|| !browser.undo_history.borrow().busy);
    action(browser, "undo");
    until(|| !browser.undo_history.borrow().busy);
    assert!(temp.path().join("tagged").exists());
    // Entry editing must never consume an operation-history entry.
    action(browser, "location");
    let before = browser.undo_menu.n_items();
    browser.location.set_text("edited location");
    if history_key(browser, "End") {
        history_key(browser, "x");
        until(|| browser.location.text() == "edited locationx");
        history_key(browser, "ctrl+z");
        until(|| browser.location.text() == "edited location");
        history_key(browser, "ctrl+y");
        until(|| browser.location.text() == "edited locationx");
    }
    browser.undo(false);
    assert_eq!(browser.undo_menu.n_items(), before);
    assert!(temp.path().join("new-branch").exists());
    browser.focus_active();
    browser.enqueue(Operation::Delete(vec![root.child("tagged")]));
    until(|| !browser.undo_history.borrow().busy);
    if browser.window.visible_dialog().is_some() {
        close_dialog(browser);
    }
    assert!(!browser.window.lookup_action("undo").unwrap().is_enabled());
    assert!(!browser.window.lookup_action("redo").unwrap().is_enabled());
}

#[test]
#[ignore = "requires a GTK display; run tools/fixtures/gui.sh undo_gui"]
fn undo_gui() {
    adw::init().unwrap();
    gio::resources_register_include!("finder.gresource").unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.finder_gnome.UndoTests")
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let browser = Browser::new(&app, gio::File::for_path(temp.path()));
    browser.window.present();
    loaded(&browser);
    undo_acceptance(&browser);
    browser.window.close();
}

#[test]
#[ignore = "requires isolated HOME/XDG and GVfs; run FINDER_GNOME_GUI_VFS=gvfs tools/fixtures/gui.sh undo_trash_gui"]
fn undo_trash_gui() {
    // Never enumerate/delete an entire Trash in this fixture; mutate only its own
    // unique files, and require the private XDG root supplied by gui.sh.
    let data = std::path::PathBuf::from(
        std::env::var_os("XDG_DATA_HOME").expect("isolated XDG_DATA_HOME"),
    );
    assert!(data.starts_with("/tmp"));
    adw::init().unwrap();
    gio::resources_register_include!("finder.gresource").unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.finder_gnome.UndoTrashTests")
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let temp = tempfile::tempdir_in(&data).unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir_all(temp.path().join("folder/nested")).unwrap();
    fs::write(temp.path().join("folder/nested/file"), "content").unwrap();
    let browser = Browser::new(&app, root.clone());
    browser.window.present();
    loaded(&browser);
    browser.focus_active();
    browser.enqueue(Operation::Trash(vec![root.child("folder")]));
    until(|| !browser.undo_history.borrow().busy);
    assert!(!temp.path().join("folder").exists());
    assert!(
        browser.window.lookup_action("undo").unwrap().is_enabled(),
        "Trash could not be journalled"
    );
    action(&browser, "undo");
    until(|| !browser.undo_history.borrow().busy);
    assert!(temp.path().join("folder/nested/file").exists());
    action(&browser, "redo");
    until(|| !browser.undo_history.borrow().busy);
    assert!(!temp.path().join("folder").exists());
    action(&browser, "undo");
    until(|| !browser.undo_history.borrow().busy);
    assert!(temp.path().join("folder/nested/file").exists());
    browser.window.close();
}
