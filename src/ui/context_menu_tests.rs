// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

fn context_menu_acceptance(browser: &Rc<Browser>) {
    let native = std::env::var_os("FINDER_GNOME_NATIVE_DND").is_some();
    fn find(widget: &gtk::Widget, matches: &impl Fn(&gtk::Widget) -> bool) -> Option<gtk::Widget> {
        if widget.is_mapped() && matches(widget) {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(found) = find(&w, matches) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }
    fn label(widget: &gtk::Widget, text: &str) -> gtk::Widget {
        find(widget, &|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.text() == text)
        })
        .unwrap()
    }
    fn settle() {
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(250));
    }
    #[track_caller]
    fn pointer(_browser: &Browser, widget: &gtk::Widget, button: &str, twice: bool) {
        let native = widget.native().unwrap();
        let native_widget = native.clone().dynamic_cast::<gtk::Widget>().unwrap();
        let bounds = widget.compute_bounds(&native_widget).unwrap();
        let (dx, dy) = native.surface_transform();
        let (x, y) = if button == "background" {
            (
                bounds.x() + bounds.width() - 12.0,
                bounds.y() + bounds.height() - 12.0,
            )
        } else {
            (
                bounds.x() + bounds.width() / 2.0,
                bounds.y() + bounds.height() / 2.0,
            )
        };
        let id = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--pid",
                &std::process::id().to_string(),
            ])
            .output()
            .unwrap();
        assert!(id.status.success());
        let id = String::from_utf8(id.stdout).unwrap();
        let surface = native.surface().unwrap();
        let id = id
            .lines()
            .find(|id| {
                let geometry = std::process::Command::new("xdotool")
                    .args(["getwindowgeometry", "--shell", id])
                    .output()
                    .unwrap();
                let geometry = String::from_utf8_lossy(&geometry.stdout);
                geometry
                    .lines()
                    .any(|line| line == format!("WIDTH={}", surface.width()))
                    && geometry
                        .lines()
                        .any(|line| line == format!("HEIGHT={}", surface.height()))
            })
            .expect("X11 window matching the widget's native surface");
        let mut command = std::process::Command::new("xdotool");
        command.args([
            "mousemove",
            "--window",
            id,
            &((f64::from(x) - dx) as i32).to_string(),
            &((f64::from(y) - dy) as i32).to_string(),
            "click",
        ]);
        if twice {
            command.args(["--repeat", "2", "--delay", "100"]);
        }
        command.arg(if button == "background" { "3" } else { button });
        let child = RefCell::new(command.spawn().unwrap());
        until(|| child.borrow_mut().try_wait().unwrap().is_some());
    }
    fn popup(browser: &Rc<Browser>, name: Option<&str>, native: bool) -> gtk::PopoverMenu {
        loaded(browser);
        settle();
        let pane = browser.active_pane().unwrap();
        if native {
            if let Some(name) = name {
                let position = (0..pane.selection.n_items())
                    .find(|i| item(&pane.selection.item(*i).unwrap()).name == name)
                    .unwrap();
                let row = find(pane.widget.upcast_ref(), &|w| {
                    w.widget_name() == format!("finder-item-{position}")
                })
                .unwrap();
                pointer(browser, &label(&row, name), "3", false);
            } else {
                pointer(browser, pane.stack.upcast_ref(), "background", false);
            }
        } else {
            if let Some(name) = name {
                select(browser, name);
            } else {
                pane.selection.unselect_all();
            }
            // A headless Wayland compositor has no input serial for a grab.
            // Native X11 tests above retain ordinary autohide/grab behavior.
            pane.ensure_context_menu().set_autohide(false);
            browser.context_menu(&pane, pane.widget.upcast_ref(), 20.0, 20.0);
        }
        until(|| {
            pane.context_menu
                .borrow()
                .as_ref()
                .is_some_and(|p| p.is_mapped())
        });
        pane.context_menu.borrow().as_ref().unwrap().clone()
    }
    #[track_caller]
    fn command_ready(browser: &Browser, button: &gtk::Widget, text: &str) {
        until_described(
            || !browser.active_pane().unwrap().loading.get() && button.is_sensitive(),
            || {
                format!(
                    "Menu command {text} did not become ready; selected={:?}, delete-enabled={}, loading={}, sensitivity={:?}",
                    browser
                        .selected()
                        .iter()
                        .map(|item| (&item.name, item.can_delete))
                        .collect::<Vec<_>>(),
                    browser.window.lookup_action("delete").unwrap().is_enabled(),
                    browser.active_pane().unwrap().loading.get(),
                    {
                        let mut path = Vec::new();
                        let mut widget = Some(button.clone());
                        while let Some(current) = widget {
                            path.push((
                                current.type_().name().to_string(),
                                current.is_sensitive(),
                                current.get_sensitive(),
                            ));
                            widget = current.parent();
                        }
                        path
                    }
                )
            },
        );
    }
    #[track_caller]
    fn choose(browser: &Browser, menu: &gtk::PopoverMenu, text: &str, native: bool) {
        settle();
        let label = label(menu.upcast_ref(), text);
        if native {
            let button = label.parent().expect("context menu model button");
            command_ready(browser, &button, text);
            // Pointer activation does not require keyboard focus. In particular,
            // do not use grab_focus() as a prerequisite for scrolling a clipped
            // GTK 4.14 model button into view.
            let mut ancestor = button.parent();
            while let Some(widget) = ancestor {
                if widget == *menu.upcast_ref::<gtk::Widget>() {
                    break;
                }
                if let Some(viewport) = widget.downcast_ref::<gtk::Viewport>() {
                    let content = viewport.child().unwrap();
                    let bounds = button.compute_bounds(&content).unwrap();
                    viewport.vadjustment().unwrap().clamp_page(
                        f64::from(bounds.y()),
                        f64::from(bounds.y() + bounds.height()),
                    );
                }
                ancestor = widget.parent();
            }
            settle();
            let native_widget = button
                .native()
                .unwrap()
                .dynamic_cast::<gtk::Widget>()
                .unwrap();
            until(|| {
                if browser.active_pane().unwrap().loading.get() || !button.is_sensitive() {
                    return false;
                }
                let bounds = button.compute_bounds(&native_widget).unwrap();
                native_widget
                    .pick(
                        f64::from(bounds.x() + bounds.width() / 2.0),
                        f64::from(bounds.y() + bounds.height() / 2.0),
                        gtk::PickFlags::DEFAULT,
                    )
                    .is_some_and(|hit| hit == button || hit.is_ancestor(&button))
            });
            pointer(browser, &button, "1", false);
        } else {
            // Activate the actual menu widget, exercising its close/action order.
            let mut current = Some(label);
            let mut activated = false;
            while let Some(widget) = current {
                if widget == *menu.upcast_ref::<gtk::Widget>() {
                    break;
                }
                if widget.activate() {
                    activated = true;
                    break;
                }
                current = widget.parent();
            }
            assert!(activated, "menu command {text} has an activation handler");
        }
        until(|| !menu.is_visible());
    }
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::write(temp.path().join("context.txt"), "context menu fixture").unwrap();
    fs::write(temp.path().join("second.txt"), "second selected file").unwrap();
    fs::create_dir(temp.path().join("Folder")).unwrap();
    let mut name = "context.txt";
    for mode in 0..4 {
        browser.navigate(root.clone(), HashSet::new());
        action(browser, &format!("view-{mode}"));
        loaded(browser);
        if mode == 3 {
            select(browser, name);
            until(|| {
                find(browser.active_pane().unwrap().widget.upcast_ref(), &|w| {
                    w.is::<gtk::TextView>()
                })
                .is_some()
            });
            find(browser.active_pane().unwrap().widget.upcast_ref(), &|w| {
                w.is::<gtk::TextView>()
            })
            .unwrap()
            .grab_focus();
            assert!(browser.text_focused());
        }
        let menu = popup(browser, Some(name), native);
        assert_eq!(browser.selected()[0].name, name);
        choose(browser, &menu, "Rename", native);
        until(|| browser.window.visible_dialog().is_some());
        if mode == 0 {
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
                .set_text("context-renamed.txt");
            respond(browser, "Rename");
            until(|| {
                browser.jobs.borrow().is_empty() && temp.path().join("context-renamed.txt").exists()
            });
            name = "context-renamed.txt";
            until(|| {
                (0..browser.active_pane().unwrap().selection.n_items()).any(|i| {
                    item(&browser.active_pane().unwrap().selection.item(i).unwrap()).name == name
                })
            });
        } else {
            respond(browser, "Cancel");
        }
        assert_eq!(
            menu,
            popup(browser, Some(name), native),
            "reuse the pane menu after closing"
        );
        choose(browser, &menu, "Copy", native);
        let menu = popup(browser, Some("Folder"), native);
        choose(browser, &menu, "Open", native);
        loaded(browser);
        assert!(browser.current.borrow().equal(&root.child("Folder")));
    }
    // Clipboard transfer via menus into an empty folder.
    let menu = popup(browser, None, native);
    choose(browser, &menu, "Paste", native);
    until(|| browser.jobs.borrow().is_empty() && temp.path().join("Folder").join(name).exists());
    browser.navigate(root.clone(), HashSet::new());
    action(browser, "view-0");
    loaded(browser);
    let menu = popup(browser, None, native);
    choose(browser, &menu, "New Folder", native);
    until(|| browser.window.visible_dialog().is_some());
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
        .set_text("MenuCreated");
    respond(browser, "Create");
    until(|| browser.jobs.borrow().is_empty() && temp.path().join("MenuCreated").is_dir());
    let menu = popup(browser, Some(name), native);
    if native {
        // Regression: a mouse click must work even when this menu command cannot
        // receive keyboard focus. Keep the real native click and dialog checks.
        let command = label(menu.upcast_ref(), "Delete Permanently…")
            .parent()
            .unwrap();
        command.set_focusable(false);
        assert!(!command.grab_focus());
        // A directory monitor may refresh the model while the menu is open.
        let pane = browser.active_pane().unwrap();
        pane.model.reload();
        assert!(pane.loading.get());
        assert!(browser.selected().is_empty());
        // A loading batch updates actions before Finished restores selection.
        browser.update_status();
        assert!(!browser.window.lookup_action("delete").unwrap().is_enabled());
        command_ready(browser, &command, "Delete Permanently…");
        assert!(!pane.loading.get());
        assert!(browser.window.lookup_action("delete").unwrap().is_enabled());
        assert_eq!(
            browser
                .selected()
                .iter()
                .map(|item| item.name.clone())
                .collect::<Vec<_>>(),
            [name]
        );
    }
    choose(browser, &menu, "Delete Permanently…", native);
    until(|| browser.window.visible_dialog().is_some());
    respond(browser, "Cancel");
    assert!(temp.path().join(name).exists());
    if native {
        let pane = browser.active_pane().unwrap();
        select(browser, name);
        let second = (0..pane.selection.n_items())
            .find(|i| item(&pane.selection.item(*i).unwrap()).name == "second.txt")
            .unwrap();
        pane.selection.select_item(second, false);
        let menu = popup(browser, Some(name), native);
        assert_eq!(browser.selected().len(), 2);
        assert!(!browser.window.lookup_action("rename").unwrap().is_enabled());
        choose(browser, &menu, "Copy", native);
        let menu = popup(browser, None, native);
        assert!(browser.selected().is_empty());
        // Escape and an outside click must dismiss a reusable menu.
        let child = RefCell::new(
            std::process::Command::new("xdotool")
                .args(["key", "Escape"])
                .spawn()
                .unwrap(),
        );
        until(|| child.borrow_mut().try_wait().unwrap().is_some());
        until(|| !menu.is_visible());
        let menu = popup(browser, Some("second.txt"), native);
        assert_eq!(browser.selected().len(), 1);
        pointer(browser, browser.title.upcast_ref(), "1", false);
        until(|| !menu.is_visible());
        pointer(
            browser,
            &label(pane.widget.upcast_ref(), "Folder"),
            "1",
            true,
        );
        loaded(browser);
        until(|| browser.current.borrow().equal(&root.child("Folder")));
        browser.navigate(root.clone(), HashSet::new());
        loaded(browser);
    }
    let menu = popup(browser, Some(name), native);
    choose(browser, &menu, "Quick Look", native);
    until(|| browser.quick_look.borrow().is_some());
    browser.stop_previews();
}
