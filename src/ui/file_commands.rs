// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! File chooser and desktop integrations. Arguments never pass through a shell.
use super::*;
use std::ffi::{OsStr, OsString};

pub(super) fn is_archive(file: &FileItem) -> bool {
    if file.is_dir {
        return false;
    }
    let name = file.name.to_ascii_lowercase();
    [
        ".zip", ".7z", ".rar", ".tar", ".gz", ".tgz", ".bz2", ".tbz", ".tbz2", ".xz", ".txz",
        ".zst", ".tzst", ".lz", ".lzma", ".cab", ".ar", ".deb", ".rpm", ".cpio",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
        || [
            "application/zip",
            "application/x-7z-compressed",
            "application/vnd.rar",
            "application/x-rar",
            "application/x-tar",
            "application/gzip",
            "application/x-bzip2",
            "application/x-xz",
            "application/zstd",
            "application/x-cpio",
            "application/x-archive",
        ]
        .contains(&file.mime.as_str())
}
fn dismissed(error: &glib::Error) -> bool {
    error.matches(gio::IOErrorEnum::Cancelled)
        || error.matches(gtk::DialogError::Dismissed)
        || error.matches(gtk::DialogError::Cancelled)
}
fn archive_arguments(files: &[gio::File], extract: bool, folder: &gio::File) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("file-roller"),
        OsString::from(if extract { "--extract" } else { "--add" }),
        OsString::from(format!("--default-dir={}", folder.uri())),
        OsString::from("--"),
    ];
    arguments.extend(files.iter().map(|file| OsString::from(file.uri().as_str())));
    arguments
}
impl Browser {
    pub(super) fn install_file_commands(self: &Rc<Self>, app: &adw::Application) {
        self.action(app, "properties", &["<Alt>Return"], |browser| {
            if let Some(applications) = browser.applications.borrow().as_ref() {
                applications.show_properties();
            } else {
                let selected = browser.selected_files();
                if selected.len() <= 1 {
                    let file = selected
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| browser.current.borrow().clone());
                    crate::properties::show_file(&browser.window, file);
                }
            }
        });
        self.action(app, "open-with", &[], |browser| {
            browser.choose_application()
        });
        self.action(app, "copy-to", &[], |browser| browser.transfer_to(false));
        self.action(app, "move-to", &[], |browser| browser.transfer_to(true));
        self.action(app, "compress", &[], |browser| browser.archive_files(false));
        self.action(app, "extract", &[], |browser| browser.archive_files(true));
    }
    fn choose_application(self: &Rc<Self>) {
        let selected = self.selected();
        let Some(file) = selected
            .first()
            .filter(|f| selected.len() == 1 && !f.is_dir)
        else {
            return;
        };
        let launcher = gtk::FileLauncher::new(Some(&file.file));
        launcher.set_always_ask(true);
        let browser = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Err(error) = launcher.launch_future(Some(&browser.window)).await {
                if !dismissed(&error) {
                    browser.error(&error.to_string());
                }
            }
        });
    }
    fn transfer_to(self: &Rc<Self>, moving: bool) {
        let sources = self.selected_files();
        if sources.is_empty() {
            return;
        }
        let dialog = gtk::FileDialog::builder()
            .title(if moving { tr("Move To") } else { tr("Copy To") })
            .accept_label(if moving { tr("Move") } else { tr("Copy") })
            .initial_folder(&*self.current.borrow())
            .modal(true)
            .build();
        let browser = self.clone();
        glib::MainContext::default().spawn_local(async move {
            match dialog.select_folder_future(Some(&browser.window)).await {
                Ok(destination) => browser.enqueue(Operation::Copy {
                    sources,
                    destination,
                    moving,
                }),
                Err(error) if !dismissed(&error) => browser.error(&error.to_string()),
                Err(_) => {}
            }
        });
    }
    fn archive_files(self: &Rc<Self>, extract: bool) {
        let selected = self.selected();
        if selected.is_empty() || (extract && !selected.iter().all(is_archive)) {
            return;
        }
        let files: Vec<_> = selected.into_iter().map(|item| item.file).collect();
        let folder = files
            .first()
            .and_then(|file| file.parent())
            .unwrap_or_else(|| self.current.borrow().clone());
        let arguments = archive_arguments(&files, extract, &folder);
        let arguments: Vec<&OsStr> = arguments.iter().map(|s| s.as_os_str()).collect();
        let process = match gio::Subprocess::newv(&arguments, gio::SubprocessFlags::STDERR_PIPE) {
            Ok(process) => process,
            Err(error) => {
                self.error(&tr("Could not start Archive Manager. Install the “file-roller” package.\n\n{error}").replace("{error}", &error.to_string()));
                return;
            }
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = process.communicate_utf8_future(None).await;
            if let Some(browser) = weak.upgrade() {
                match result {
                    Ok((_, stderr)) if !process.is_successful() => browser.error(
                        &tr("Archive Manager could not complete the operation.\n\n{error}")
                            .replace("{error}", stderr.as_deref().unwrap_or_default()),
                    ),
                    Err(error) => browser.error(&error.to_string()),
                    _ => {}
                }
                for pane in browser.all_panes() {
                    pane.model.reload();
                }
            }
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_command_preserves_names_as_individual_uri_arguments() {
        let folder = gio::File::for_path("/tmp");
        let files = vec![
            folder.child("--flag ; $(touch fake).zip"),
            folder.child("file with spaces"),
        ];
        let arguments = archive_arguments(&files, false, &folder);
        assert_eq!(arguments[1], "--add");
        assert_eq!(arguments[3], "--");
        assert_eq!(arguments.len(), 6);
        assert_eq!(arguments[4], files[0].uri().as_str());
        assert_eq!(
            archive_arguments(&files[..1], true, &folder)[1],
            "--extract"
        );
    }
}
