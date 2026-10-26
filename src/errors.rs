// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Structured, credential-safe errors shared by asynchronous services.
use crate::i18n::tr;
use gio::prelude::*;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Cancelled,
    Offline,
    Authentication,
    Permission,
    NoSpace,
    Unsupported,
    Missing,
    Other,
}
#[derive(Clone, Debug)]
pub struct AppError {
    pub kind: ErrorKind,
    pub operation: String,
    pub uri: String,
    pub details: String,
}
impl AppError {
    pub fn gio(operation: &str, file: &gio::File, error: &glib::Error) -> Self {
        use gio::IOErrorEnum as E;
        let kind = if error.matches(E::Cancelled) {
            ErrorKind::Cancelled
        } else if error.matches(E::NotMounted)
            || error.matches(E::HostUnreachable)
            || error.matches(E::HostNotFound)
            || error.matches(E::NetworkUnreachable)
            || error.matches(E::BrokenPipe)
            || error.matches(E::Closed)
            || error.matches(E::ConnectionRefused)
            || error.matches(E::TimedOut)
        {
            ErrorKind::Offline
        } else if error.matches(E::PermissionDenied) || error.matches(E::ReadOnly) {
            ErrorKind::Permission
        } else if error.matches(E::NotSupported) {
            ErrorKind::Unsupported
        } else if error.matches(E::NoSpace) {
            ErrorKind::NoSpace
        } else if error.matches(E::NotFound) {
            ErrorKind::Missing
        } else if error.matches(E::FailedHandled) {
            ErrorKind::Authentication
        } else {
            ErrorKind::Other
        };
        Self {
            kind,
            operation: operation.into(),
            uri: file.uri().to_string(),
            details: error.to_string(),
        }
    }
    pub fn message(&self) -> String {
        let title = match self.kind {
            ErrorKind::Cancelled => tr("Operation cancelled."),
            ErrorKind::Offline => tr("The location is disconnected. Connect again and retry."),
            ErrorKind::Authentication => tr("Authentication was not completed."),
            ErrorKind::Permission => tr("You do not have permission for this action."),
            ErrorKind::NoSpace => tr("There is not enough free space."),
            ErrorKind::Unsupported => tr("This location does not support this action."),
            ErrorKind::Missing => tr("The item is no longer available."),
            ErrorKind::Other => tr("Unable to complete the action"),
        };
        format!(
            "{title}\n{}\n{}\n{}",
            self.operation, self.uri, self.details
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_io_failures_without_losing_context() {
        let file = gio::File::for_uri("sftp://user@server/folder");
        for (code, kind) in [
            (gio::IOErrorEnum::PermissionDenied, ErrorKind::Permission),
            (gio::IOErrorEnum::NoSpace, ErrorKind::NoSpace),
            (gio::IOErrorEnum::NotMounted, ErrorKind::Offline),
            (gio::IOErrorEnum::Cancelled, ErrorKind::Cancelled),
            (gio::IOErrorEnum::NotSupported, ErrorKind::Unsupported),
        ] {
            let error = AppError::gio("Copy", &file, &glib::Error::new(code, "test error"));
            assert_eq!(error.kind, kind);
            assert_eq!(error.uri, file.uri());
            assert!(error.message().contains("Copy"));
        }
    }
}
