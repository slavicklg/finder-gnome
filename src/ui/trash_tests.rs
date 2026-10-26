// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

#[test]
#[ignore = "requires an isolated container with GVfs; run tools/fixtures/trash.sh"]
fn trash_acceptance() {
    assert!(
        std::path::Path::new("/.dockerenv").exists(),
        "Never test Empty Trash on the desktop host"
    );
    assert_eq!(
        std::env::var("FINDER_GNOME_ISOLATED_TRASH").as_deref(),
        Ok("1")
    );
    adw::init().unwrap();
    gio::resources_register_include!("finder.gresource").unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.finder_gnome.TrashTests")
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    let browser = Browser::new(&app, root.clone());
    browser.window.present();
    loaded(&browser);
    fs::write(temp.path().join("restore.txt"), "original").unwrap();
    fs::create_dir_all(temp.path().join("Folder/Nested")).unwrap();
    fs::write(temp.path().join("Folder/Nested/data"), "nested").unwrap();
    std::os::unix::fs::symlink(temp.path(), temp.path().join("Folder/link")).unwrap();
    browser.enqueue(Operation::Trash(vec![
        root.child("restore.txt"),
        root.child("Folder"),
    ]));
    until(|| browser.jobs.borrow().is_empty());
    assert!(!temp.path().join("restore.txt").exists());
    let trash_row = browser
        .places
        .borrow()
        .iter()
        .position(|place| matches!(place, SidebarLocation::Folder(file) if files::is_trash(file)))
        .unwrap();
    browser.sidebar.emit_by_name::<()>(
        "row-activated",
        &[&browser.sidebar.row_at_index(trash_row as i32).unwrap()],
    );
    loaded(&browser);
    assert!(browser.in_trash());
    assert_eq!(browser.title.text(), "Trash");
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 2);
    assert!(
        browser
            .window
            .lookup_action("empty-trash")
            .unwrap()
            .is_enabled()
    );
    for name in [
        "paste",
        "new-folder",
        "view-2",
        "view-3",
        "compress",
        "search",
    ] {
        assert!(
            !browser.window.lookup_action(name).unwrap().is_enabled(),
            "{name}"
        );
    }
    action(&browser, "view-1");
    loaded(&browser);
    select(&browser, "restore.txt");
    assert!(browser.selected()[0].location_hint.contains("restore.txt"));
    screenshot(&browser, "trash");
    // Existing original is never overwritten; the trashed copy remains retryable.
    fs::write(temp.path().join("restore.txt"), "replacement").unwrap();
    action(&browser, "restore");
    until(|| browser.jobs.borrow().is_empty());
    until(|| browser.window.visible_dialog().is_some());
    close_dialog(&browser);
    assert_eq!(
        fs::read_to_string(temp.path().join("restore.txt")).unwrap(),
        "replacement"
    );
    loaded(&browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 2);
    fs::remove_file(temp.path().join("restore.txt")).unwrap();
    select(&browser, "restore.txt");
    action(&browser, "restore");
    until(|| browser.jobs.borrow().is_empty());
    assert_eq!(
        fs::read_to_string(temp.path().join("restore.txt")).unwrap(),
        "original"
    );
    loaded(&browser);
    select(&browser, "Folder");
    action(&browser, "restore");
    until(|| browser.jobs.borrow().is_empty());
    assert_eq!(
        fs::read_to_string(temp.path().join("Folder/Nested/data")).unwrap(),
        "nested"
    );
    assert!(temp.path().join("Folder/link").is_symlink());
    browser.enqueue(Operation::Trash(vec![
        root.child("restore.txt"),
        root.child("Folder"),
    ]));
    until(|| browser.jobs.borrow().is_empty());
    loaded(&browser);
    select(&browser, "restore.txt");
    fs::create_dir(temp.path().join("elsewhere")).unwrap();
    browser.enqueue(Operation::Restore {
        sources: browser.selected_files(),
        destination: Some(root.child("elsewhere")),
    });
    until(|| browser.jobs.borrow().is_empty());
    assert_eq!(
        fs::read_to_string(temp.path().join("elsewhere/restore.txt")).unwrap(),
        "original"
    );
    loaded(&browser);
    action(&browser, "empty-trash");
    until(|| browser.window.visible_dialog().is_some());
    respond(&browser, "Cancel");
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 1);
    action(&browser, "empty-trash");
    until(|| browser.window.visible_dialog().is_some());
    respond(&browser, "Empty Trash");
    until(|| browser.jobs.borrow().is_empty());
    loaded(&browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 0);
    assert_eq!(
        fs::read_to_string(temp.path().join("elsewhere/restore.txt")).unwrap(),
        "original",
        "Deleting a trashed symlink must not follow its target"
    );
    browser.receive_drop(
        vec![root.child("elsewhere/restore.txt")],
        gio::File::for_uri("trash:///"),
        gdk::ModifierType::empty(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
        None,
    );
    until(|| {
        !temp.path().join("elsewhere/restore.txt").exists() && browser.jobs.borrow().is_empty()
    });
    browser.active_pane().unwrap().model.reload();
    loaded(&browser);
    select(&browser, "restore.txt");
    action(&browser, "delete");
    until(|| browser.window.visible_dialog().is_some());
    respond(&browser, "Cancel");
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 1);
    action(&browser, "delete");
    until(|| browser.window.visible_dialog().is_some());
    respond(&browser, "Delete Permanently");
    until(|| browser.jobs.borrow().is_empty());
    loaded(&browser);
    assert_eq!(browser.active_pane().unwrap().selection.n_items(), 0);
    browser.window.close();
}
