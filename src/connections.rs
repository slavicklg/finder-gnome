// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! GVfs owns protocols, credentials, SSH trust and system mount integration.
use crate::{
    errors::{AppError, ErrorKind},
    files,
    i18n::tr,
};
use adw::prelude::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
type Waiters = HashMap<String, Vec<async_channel::Sender<Result<(), AppError>>>>;
#[derive(Default)]
pub struct Connections {
    pending: RefCell<Waiters>,
}
impl Connections {
    pub async fn ensure(
        self: &Rc<Self>,
        file: &gio::File,
        parent: &adw::ApplicationWindow,
        cancel: &gio::Cancellable,
    ) -> Result<(), AppError> {
        if files::is_trash(file) {
            return Ok(());
        }
        if !files::supported(file) {
            return Err(AppError {
                kind: ErrorKind::Unsupported,
                operation: tr("Connect"),
                uri: String::new(),
                details: tr("Unsupported or unsafe folder address."),
            });
        }
        if file.is_native() {
            return Ok(());
        }
        let parsed = glib::Uri::parse(&file.uri(), glib::UriFlags::HAS_PASSWORD).unwrap();
        let share = if parsed.scheme() == "smb" {
            parsed
                .path()
                .split('/')
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let uri = format!(
            "{}://{}@{}:{}/{}",
            parsed.scheme(),
            parsed.user().unwrap_or_default(),
            parsed.host().unwrap_or_default().to_ascii_lowercase(),
            if parsed.port() < 0 {
                if parsed.scheme() == "sftp" { 22 } else { 445 }
            } else {
                parsed.port()
            },
            share
        );
        let (tx, rx) = async_channel::bounded(1);
        {
            let mut pending = self.pending.borrow_mut();
            if let Some(waiters) = pending.get_mut(&uri) {
                waiters.push(tx);
            } else {
                pending.insert(uri.clone(), vec![tx]);
            }
        }
        // The first waiter starts the operation; every waiter gets the same result.
        if self
            .pending
            .borrow()
            .get(&uri)
            .is_some_and(|v| v.len() == 1)
        {
            let service = self.clone();
            let file = file.clone();
            let parent = parent.clone();
            glib::MainContext::default().spawn_local(async move {
                let abandoned = gio::Cancellable::new();
                let monitor = abandoned.clone();
                let pending = service.clone();
                let key = uri.clone();
                let timer =
                    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                        if pending
                            .pending
                            .borrow()
                            .get(&key)
                            .is_none_or(|waiters| waiters.iter().all(|w| w.is_closed()))
                        {
                            monitor.cancel();
                        }
                        glib::ControlFlow::Continue
                    });
                let result = gio::CancellableFuture::new(
                    async {
                        match file
                            .query_info_future(
                                "standard::type",
                                gio::FileQueryInfoFlags::NONE,
                                glib::Priority::DEFAULT,
                            )
                            .await
                        {
                            Ok(_) => Ok(()),
                            Err(e) if e.matches(gio::IOErrorEnum::NotMounted) => {
                                let operation = gtk::MountOperation::new(Some(&parent));
                                #[cfg(test)]
                                if file.uri().contains("127.0.0.1") {
                                    if let Ok(password) =
                                        std::env::var("FINDER_GNOME_TEST_PASSWORD")
                                    {
                                        operation.connect_ask_password(move |op, _, _, _, _| {
                                            op.stop_signal_emission_by_name("ask-password");
                                            op.set_username(Some("finder"));
                                            op.set_domain(Some("WORKGROUP"));
                                            op.set_password(Some(&password));
                                            op.set_password_save(gio::PasswordSave::Never);
                                            op.reply(gio::MountOperationResult::Handled);
                                        });
                                        operation.connect_local("ask-question", false, |values| {
                                            let op =
                                                values[0].get::<gtk::MountOperation>().unwrap();
                                            op.stop_signal_emission_by_name("ask-question");
                                            op.set_choice(0);
                                            op.reply(gio::MountOperationResult::Handled);
                                            None
                                        });
                                    }
                                }
                                match file
                                    .mount_enclosing_volume_future(
                                        gio::MountMountFlags::NONE,
                                        Some(&operation),
                                    )
                                    .await
                                {
                                    Ok(()) => Ok(()),
                                    Err(e) if e.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
                                    Err(e) => Err(AppError::gio(&tr("Connect"), &file, &e)),
                                }
                            }
                            Err(e)
                                if e.matches(gio::IOErrorEnum::NotFound)
                                    || e.matches(gio::IOErrorEnum::PermissionDenied) =>
                            {
                                Ok(())
                            }
                            Err(e) => Err(AppError::gio(&tr("Connect"), &file, &e)),
                        }
                    },
                    abandoned,
                )
                .await
                .unwrap_or_else(|_| {
                    Err(AppError::gio(
                        &tr("Connect"),
                        &file,
                        &glib::Error::new(gio::IOErrorEnum::Cancelled, "Cancelled"),
                    ))
                });
                timer.remove();
                let waiters = service
                    .pending
                    .borrow_mut()
                    .remove(&uri)
                    .unwrap_or_default();
                for waiter in waiters {
                    let _ = waiter.try_send(result.clone());
                }
            });
        }
        match gio::CancellableFuture::new(rx.recv(), cancel.clone()).await {
            Ok(Ok(result)) => result,
            _ => Err(AppError {
                kind: ErrorKind::Cancelled,
                operation: tr("Connect"),
                uri: file.uri().to_string(),
                details: tr("Connection cancelled."),
            }),
        }
    }
}
