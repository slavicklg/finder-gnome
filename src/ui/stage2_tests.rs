// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

fn stage2_acceptance(browser: &Rc<Browser>) {
    use gtk::gdk_pixbuf;
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("Other")).unwrap();
    fs::write(temp.path().join("preview.txt"), "Stage two text preview").unwrap();
    let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 320, 200).unwrap();
    pixbuf.fill(0x3478f6ff);
    pixbuf
        .savev(temp.path().join("picture.png"), "png", &[])
        .unwrap();
    crate::preview::tests::pdf_fixture(&temp.path().join("document.pdf"));
    let mut wav = Vec::new();
    let samples = 8000u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples * 2).to_le_bytes());
    wav.resize(44 + samples as usize * 2, 0);
    fs::write(temp.path().join("audio.wav"), wav).unwrap();
    let video = temp.path().join("video.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:r=10",
            "-t",
            "0.5",
            "-an",
            "-c:v",
            "mpeg4",
            "-y",
        ])
        .arg(&video)
        .output()
        .expect("install ffmpeg for multimedia acceptance fixtures");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    select(browser, "picture.png");
    action(browser, "view-3");
    loaded(browser);
    assert_eq!(browser.mode.get(), ViewMode::Gallery);
    let preview = browser
        .active_pane()
        .unwrap()
        .preview
        .borrow()
        .clone()
        .unwrap();
    until(|| preview.test_ready());
    assert!(preview.test_raster(), "{}", preview.test_message());
    screenshot(browser, "gallery-image");
    browser.toggle_quick_look();
    let quick = browser.quick_look.borrow().clone().unwrap();
    until(|| quick.preview.test_ready());
    assert!(quick.preview.test_raster());
    quick.window.close();
    until(|| browser.quick_look.borrow().is_none());
    select(browser, "document.pdf");
    until(|| preview.test_pages() == 2 && preview.test_ready());
    preview.change_page(1);
    until(|| preview.test_ready());
    assert!(preview.test_raster(), "{}", preview.test_message());
    screenshot(browser, "gallery-pdf");
    select(browser, "preview.txt");
    until(|| preview.test_ready());
    screenshot(browser, "gallery-text");
    for name in ["audio.wav", "video.mp4"] {
        select(browser, name);
        let media = preview.test_media().expect("media backend");
        until(|| media.is_prepared() || media.error().is_some());
        assert!(media.error().is_none(), "{name}: {:?}", media.error());
        assert!(!media.is_playing(), "preview must not autoplay");
    }
    let first_page = browser.current_tab.borrow().clone();
    let media = preview.test_media().unwrap();
    media.play();
    assert!(media.is_playing());
    browser.open_in_new_tab(root.child("Other"));
    loaded(browser);
    assert!(!media.is_playing());
    assert_eq!(browser.tab_view.n_pages(), 2);
    assert!(!browser.history.borrow().can_back());
    let other_page = browser.current_tab.borrow().clone();
    browser.change_view(|s| {
        s.hidden = true;
        s.icon_size = 96;
        s.mode = ViewMode::List;
        s.visible_columns[2] = false;
    });
    loaded(browser);
    browser.tab_view.set_selected_page(&first_page);
    loaded(browser);
    assert_eq!(browser.mode.get(), ViewMode::Gallery);
    assert_eq!(browser.selected()[0].name, "video.mp4");
    assert!(!browser.hidden.get());
    browser.new_tab(browser.capture_entry());
    loaded(browser);
    assert_eq!(browser.tab_view.n_pages(), 3);
    assert_eq!(browser.selected()[0].name, "video.mp4");
    assert!(!browser.history.borrow().can_back());
    let duplicated = browser.current_tab.borrow().clone();
    browser.tab_view.close_page(&duplicated);
    assert_eq!(browser.tab_view.n_pages(), 2);
    browser.tab_view.set_selected_page(&other_page);
    loaded(browser);
    assert_eq!(browser.mode.get(), ViewMode::List);
    assert!(browser.hidden.get());
    assert_eq!(browser.folder_settings.borrow().icon_size, 96);
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    browser.show_applications();
    until(|| !browser.applications.borrow().as_ref().unwrap().is_loading());
    browser.history_step(false);
    loaded(browser);
    assert!(browser.current.borrow().equal(&root));
    browser.history_step(false);
    loaded(browser);
    assert!(browser.current.borrow().equal(&root.child("Other")));
    assert!(browser.hidden.get());
    browser.history_step(true);
    loaded(browser);
    assert!(browser.current.borrow().equal(&root));
    browser.navigate(root.child("missing"), HashSet::new());
    loaded(browser);
    assert!(!browser.history.borrow().can_forward());
    browser.history_step(false);
    loaded(browser);
    assert!(browser.current.borrow().equal(&root));
    // A same-folder drop is a silent rejection, even when Copy/Move is forced.
    for modifiers in [
        gdk::ModifierType::empty(),
        gdk::ModifierType::CONTROL_MASK,
        gdk::ModifierType::SHIFT_MASK,
    ] {
        browser.receive_drop(
            vec![root.child("preview.txt")],
            root.clone(),
            modifiers,
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
            None,
        );
        assert!(browser.jobs.borrow().is_empty());
        assert!(browser.window.visible_dialog().is_none());
        assert_eq!(
            fs::read_to_string(temp.path().join("preview.txt")).unwrap(),
            "Stage two text preview"
        );
    }
    fs::write(temp.path().join("drag-copy.txt"), "copy payload").unwrap();
    browser.receive_drop(
        vec![root.child("drag-copy.txt")],
        root.child("Other"),
        gdk::ModifierType::CONTROL_MASK,
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
        None,
    );
    until(|| temp.path().join("Other/drag-copy.txt").exists() && browser.jobs.borrow().is_empty());
    assert!(temp.path().join("drag-copy.txt").exists());
    fs::write(temp.path().join("drag-move.txt"), "move payload").unwrap();
    browser.receive_drop(
        vec![root.child("drag-move.txt")],
        root.child("Other"),
        gdk::ModifierType::empty(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
        None,
    );
    until(|| temp.path().join("Other/drag-move.txt").exists() && browser.jobs.borrow().is_empty());
    assert!(!temp.path().join("drag-move.txt").exists());
    native_drag_acceptance(browser, &root);
    screenshot(browser, "tabs");
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    action(browser, "view-2");
    loaded(browser);
    select(browser, "Other");
    until(|| browser.panes.borrow().len() == 2);
    loaded(browser);
    let child = browser.panes.borrow()[1].clone();
    browser.activate_pane(&child);
    let columns = browser.capture_entry();
    browser.show_applications();
    until(|| !browser.applications.borrow().as_ref().unwrap().is_loading());
    browser.history_step(false);
    loaded(browser);
    assert_eq!(browser.mode.get(), ViewMode::Columns);
    assert_eq!(browser.panes.borrow().len(), 2);
    assert_eq!(browser.active.get(), 1);
    assert_eq!(
        browser
            .capture_entry()
            .columns
            .iter()
            .map(|c| &c.uri)
            .collect::<Vec<_>>(),
        columns.columns.iter().map(|c| &c.uri).collect::<Vec<_>>()
    );
    browser.show_applications();
    until(|| !browser.applications.borrow().as_ref().unwrap().is_loading());
    browser.persist_now();
    browser.storage.flush();
    let (session, error) = browser.storage.read_session();
    assert!(error.is_none());
    assert_eq!(session.tabs.len(), 2);
    let (settings, error) = browser.storage.read_settings();
    assert!(error.is_none());
    assert!(settings.for_file(&root.child("Other")).hidden);
    let app = browser
        .window
        .application()
        .unwrap()
        .downcast::<adw::Application>()
        .unwrap();
    let restored = Browser::new(&app, root.clone());
    *restored.settings.borrow_mut() = settings;
    *restored.startup_session.borrow_mut() = Some(session.clone());
    restored.restore_session(None);
    loaded(&restored);
    assert_eq!(restored.tab_view.n_pages() as usize, session.tabs.len());
    for (index, entry) in session.tabs.iter().enumerate() {
        assert_eq!(
            restored
                .entry_for_page(&restored.tab_view.nth_page(index as i32))
                .unwrap()
                .location,
            entry.location
        );
    }
    restored.window.destroy();
    browser.window.present();
    until(|| browser.window.is_mapped());
    browser.tab_view.set_selected_page(&first_page);
    loaded(browser);
    browser.preferences();
    until(|| browser.window.visible_dialog().is_some());
    close_dialog(browser);
    let frame = Instant::now();
    until(|| frame.elapsed() > Duration::from_millis(300));
    browser.view_options();
    until(|| browser.window.visible_dialog().is_some());
    screenshot(browser, "view-options");
    close_dialog(browser);
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    browser.stop_previews();
}

fn native_drag_acceptance(browser: &Rc<Browser>, root: &gio::File) {
    if std::env::var_os("FINDER_GNOME_NATIVE_DND").is_none() {
        return;
    }
    fn find(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.is_mapped() && widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = find(&w, name) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }
    browser.navigate(root.clone(), HashSet::new());
    loaded(browser);
    action(browser, "view-0");
    loaded(browser);
    let path = root.path().unwrap();
    fs::write(path.join("native-drag.txt"), "native pointer drag").unwrap();
    until(|| {
        (0..browser.active_pane().unwrap().selection.n_items()).any(|i| {
            item(&browser.active_pane().unwrap().selection.item(i).unwrap()).name
                == "native-drag.txt"
        })
    });
    let pane = browser.active_pane().unwrap();
    let position = |name: &str| {
        (0..pane.selection.n_items())
            .find(|i| item(&pane.selection.item(*i).unwrap()).name == name)
            .unwrap()
    };
    let source_name = format!("finder-item-{}", position("native-drag.txt"));
    let target_name = format!("finder-item-{}", position("Other"));
    let window: &gtk::Widget = browser.window.upcast_ref();
    until(|| find(window, &source_name).is_some() && find(window, &target_name).is_some());
    let center = |name: &str| {
        let bounds = find(window, name).unwrap().compute_bounds(window).unwrap();
        (
            (bounds.x() + bounds.width() / 2.0) as i32,
            (bounds.y() + bounds.height() / 2.0) as i32,
        )
    };
    let frame = Instant::now();
    until(|| frame.elapsed() > Duration::from_millis(250));
    let (sx, sy) = center(&source_name);
    let (tx, ty) = center(&target_name);
    let id = std::process::Command::new("xdotool")
        .args([
            "search",
            "--onlyvisible",
            "--pid",
            &std::process::id().to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        id.status.success(),
        "xdotool window search: {}",
        String::from_utf8_lossy(&id.stderr)
    );
    let id = String::from_utf8(id.stdout).unwrap();
    let id = id.lines().next().unwrap();
    let stack = pane.stack.compute_bounds(window).unwrap();
    let (bx, by) = (
        (stack.x() + stack.width() - 24.0) as i32,
        (stack.y() + stack.height() - 24.0) as i32,
    );
    let child = RefCell::new(
        std::process::Command::new("xdotool")
            .args([
                "mousemove",
                "--window",
                id,
                &sx.to_string(),
                &sy.to_string(),
                "mousedown",
                "1",
                "sleep",
                "0.2",
                "mousemove",
                "--window",
                id,
                &(sx + 20).to_string(),
                &(sy + 20).to_string(),
                "sleep",
                "0.1",
                "mousemove",
                "--window",
                id,
                &bx.to_string(),
                &by.to_string(),
                "sleep",
                "0.5",
            ])
            .spawn()
            .unwrap(),
    );
    until(|| child.borrow_mut().try_wait().unwrap().is_some());
    fn active_drag(widget: &gtk::Widget) -> Option<gdk::Drag> {
        let controllers = widget.observe_controllers();
        for index in 0..controllers.n_items() {
            if let Some(drag) = controllers
                .item(index)
                .and_then(|o| o.downcast::<gtk::DragSource>().ok())
                .and_then(|s| s.drag())
            {
                return Some(drag);
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(drag) = active_drag(&widget) {
                return Some(drag);
            }
            child = widget.next_sibling();
        }
        None
    }
    let drag = active_drag(window).expect("native drag started");
    assert!(
        drag.selected_action().is_empty(),
        "current folder must not accept a drop"
    );
    fn highlighted(widget: &gtk::Widget) -> bool {
        if widget.has_css_class("drop-target") {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if highlighted(&widget) {
                return true;
            }
            child = widget.next_sibling();
        }
        false
    }
    assert!(
        !highlighted(window),
        "current folder must not be highlighted"
    );
    let child = RefCell::new(
        std::process::Command::new("xdotool")
            .args(["mouseup", "1"])
            .spawn()
            .unwrap(),
    );
    until(|| child.borrow_mut().try_wait().unwrap().is_some());
    let frame = Instant::now();
    until(|| frame.elapsed() > Duration::from_millis(250));
    assert!(path.join("native-drag.txt").exists());
    assert!(browser.jobs.borrow().is_empty());
    assert!(browser.window.visible_dialog().is_none());
    let mut command = std::process::Command::new("xdotool");
    command.args([
        "mousemove",
        "--window",
        id,
        &sx.to_string(),
        &sy.to_string(),
        "mousedown",
        "1",
        "sleep",
        "0.2",
    ]);
    for step in 1..=12 {
        let x = sx + (tx - sx) * step / 12;
        let y = sy + (ty - sy) * step / 12;
        command.args([
            "mousemove",
            "--window",
            id,
            &x.to_string(),
            &y.to_string(),
            "sleep",
            "0.06",
        ]);
    }
    command.args(["sleep", "0.5", "mouseup", "1"]);
    let child = RefCell::new(command.spawn().unwrap());
    until(|| child.borrow_mut().try_wait().unwrap().is_some());
    until(|| path.join("Other/native-drag.txt").exists() && browser.jobs.borrow().is_empty());
    assert!(!path.join("native-drag.txt").exists());
    let source_page = browser.current_tab.borrow().clone();
    let target_page =
        browser
            .tab_view
            .nth_page(if browser.tab_view.page_position(&source_page) == 0 {
                1
            } else {
                0
            });
    browser.tab_view.set_selected_page(&target_page);
    loaded(browser);
    browser.navigate(root.child("Other"), HashSet::new());
    loaded(browser);
    browser.tab_view.set_selected_page(&source_page);
    loaded(browser);
    fs::write(path.join("tab-drag.txt"), "tab hover drop").unwrap();
    until(|| {
        (0..browser.active_pane().unwrap().selection.n_items()).any(|i| {
            item(&browser.active_pane().unwrap().selection.item(i).unwrap()).name == "tab-drag.txt"
        })
    });
    let pane = browser.active_pane().unwrap();
    let index = (0..pane.selection.n_items())
        .find(|i| item(&pane.selection.item(*i).unwrap()).name == "tab-drag.txt")
        .unwrap();
    let name = format!("finder-item-{index}");
    until(|| find(window, &name).is_some());
    let frame = Instant::now();
    until(|| frame.elapsed() > Duration::from_millis(250));
    let (sx, sy) = center(&name);
    fn label(widget: &gtk::Widget) -> Option<gtk::Widget> {
        if widget.is_mapped()
            && widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.text() == "Other")
        {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = label(&w) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }
    until(|| label(window).is_some());
    let bounds = label(window).unwrap().compute_bounds(window).unwrap();
    let tx = (bounds.x() + bounds.width() / 2.0) as i32;
    let ty = (bounds.y() + bounds.height() / 2.0) as i32;
    let mut command = std::process::Command::new("xdotool");
    command.args([
        "mousemove",
        "--window",
        id,
        &sx.to_string(),
        &sy.to_string(),
        "mousedown",
        "1",
        "sleep",
        "0.2",
    ]);
    for step in 1..=12 {
        command.args([
            "mousemove",
            "--window",
            id,
            &(sx + (tx - sx) * step / 12).to_string(),
            &(sy + (ty - sy) * step / 12).to_string(),
            "sleep",
            "0.06",
        ]);
    }
    command.args(["sleep", "1", "mouseup", "1"]);
    let child = RefCell::new(command.spawn().unwrap());
    until(|| child.borrow_mut().try_wait().unwrap().is_some());
    until(|| path.join("Other/tab-drag.txt").exists() && browser.jobs.borrow().is_empty());
    assert!(!path.join("tab-drag.txt").exists());
    browser.tab_view.set_selected_page(&source_page);
    loaded(browser);
}
