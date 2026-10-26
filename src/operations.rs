// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT

//! Serialized, cancellable GIO operations. No GTK objects cross the worker boundary.
use gio::prelude::*;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crate::{files::validate_name, i18n::tr};

#[derive(Clone, Debug)]
pub enum Operation {
    Retry {
        operation: Box<Operation>,
        completed: Vec<(String, String)>,
    },
    Create(gio::File, String),
    Rename(gio::File, String),
    Copy {
        sources: Vec<gio::File>,
        destination: gio::File,
        moving: bool,
    },
    Restore {
        sources: Vec<gio::File>,
        destination: Option<gio::File>,
    },
    DeleteTrash(Vec<gio::File>),
    Trash(Vec<gio::File>),
    Delete(Vec<gio::File>),
}

impl Operation {
    pub fn title(&self) -> String {
        match self {
            Self::Retry { operation, .. } => operation.title(),
            Self::Create(..) => tr("New Folder"),
            Self::Rename(..) => tr("Rename"),
            Self::Copy { moving: true, .. } => tr("Move"),
            Self::Copy { .. } => tr("Copy"),
            Self::Restore { .. } => tr("Restore from Trash"),
            Self::DeleteTrash(..) => tr("Delete from Trash"),
            Self::Trash(..) => tr("Move to Trash"),
            Self::Delete(..) => tr("Delete Permanently"),
        }
    }
    pub fn locations(&self) -> Vec<gio::File> {
        match self {
            Self::Retry { operation, .. } => operation.locations(),
            Self::Create(f, _) => vec![f.clone()],
            Self::Rename(f, _) => vec![f.parent().unwrap_or_else(|| f.clone())],
            Self::Copy {
                sources,
                destination,
                ..
            } => sources
                .iter()
                .filter_map(|f| f.parent())
                .chain(std::iter::once(destination.clone()))
                .collect(),
            Self::Restore {
                sources,
                destination,
            } => sources.iter().cloned().chain(destination.clone()).collect(),
            Self::DeleteTrash(v) => v.clone(),
            Self::Trash(v) | Self::Delete(v) => v.iter().filter_map(|f| f.parent()).collect(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Replace,
    Merge,
    Skip,
    KeepBoth,
    Cancel,
}

pub struct Conflict {
    pub source: String,
    pub destination: String,
    pub both_directories: bool,
    pub replace_allowed: bool,
    pub answer: mpsc::SyncSender<(Decision, bool)>,
}

pub enum Event {
    Changed {
        source: String,
        target: Option<String>,
        moving: bool,
    },
    Failed {
        operation: Operation,
        error: crate::errors::AppError,
    },
    Progress {
        name: String,
        completed: u64,
        bytes: Option<(i64, i64)>,
    },
    Conflict(Conflict),
    Finished {
        completed: u64,
        skipped: u64,
        errors: Vec<String>,
        cancelled: bool,
    },
}

pub struct Job {
    pub operation: Operation,
    pub cancel: gio::Cancellable,
    pub events: async_channel::Sender<Event>,
}

pub fn start_worker() -> mpsc::Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("file-operations".into())
        .spawn(move || {
            for job in rx {
                execute(job);
            }
        })
        .expect("cannot start file operations worker");
    tx
}

#[derive(Debug)]
pub enum OperationError {
    Gio(glib::Error),
    Message(String),
}
impl From<String> for OperationError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}
impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gio(e) => write!(f, "{e}"),
            Self::Message(e) => write!(f, "{e}"),
        }
    }
}
type Result<T> = std::result::Result<T, OperationError>;

struct Context {
    cancel: gio::Cancellable,
    events: async_channel::Sender<Event>,
    completed: u64,
    skipped: u64,
    errors: Vec<String>,
    remembered: Option<(bool, bool, Decision)>,
    last_progress: Instant,
    completed_targets: Vec<(String, String)>,
    skip_targets: Vec<(String, String)>,
}

impl Context {
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(tr("Operation cancelled.").into())
        } else {
            Ok(())
        }
    }

    fn progress(&mut self, file: &gio::File, bytes: Option<(i64, i64)>, force: bool) {
        if force || self.last_progress.elapsed() >= Duration::from_millis(100) {
            self.last_progress = Instant::now();
            let _ = self.events.send_blocking(Event::Progress {
                name: file.parse_name().to_string(),
                completed: self.completed,
                bytes,
            });
        }
    }

    fn done(&mut self, file: &gio::File) {
        self.completed += 1;
        self.progress(file, None, false);
    }

    fn resolve(
        &mut self,
        source: &gio::File,
        target: &gio::File,
        directory: bool,
    ) -> Result<Option<(gio::File, bool, bool)>> {
        let Some(info) = optional_info(target, &self.cancel)? else {
            return Ok(Some((target.clone(), false, false)));
        };
        if source.equal(target) {
            return Err(tr("The source and destination are the same.").into());
        }
        let both_dirs = directory && info.file_type() == gio::FileType::Directory;
        let replace_allowed = !directory && info.file_type() != gio::FileType::Directory;
        let decision = if let Some((dirs, replace, decision)) = self
            .remembered
            .filter(|(d, r, _)| *d == both_dirs && *r == replace_allowed)
        {
            let _ = (dirs, replace);
            decision
        } else {
            let (tx, rx) = mpsc::sync_channel(1);
            self.events
                .send_blocking(Event::Conflict(Conflict {
                    source: source.parse_name().to_string(),
                    destination: target.parse_name().to_string(),
                    both_directories: both_dirs,
                    replace_allowed,
                    answer: tx,
                }))
                .map_err(|_| tr("The operation window was closed."))?;
            let (decision, remember) = loop {
                self.check()?;
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(answer) => break answer,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => {
                        self.cancel.cancel();
                        return self.check().map(|_| None);
                    }
                }
            };
            if remember {
                self.remembered = Some((both_dirs, replace_allowed, decision));
            }
            decision
        };
        match decision {
            Decision::Cancel => {
                self.cancel.cancel();
                self.check().map(|_| None)
            }
            Decision::Skip => {
                self.skipped += 1;
                Ok(None)
            }
            Decision::Merge if both_dirs => Ok(Some((target.clone(), true, false))),
            Decision::Replace if replace_allowed => Ok(Some((target.clone(), false, true))),
            Decision::KeepBoth => Ok(Some((
                unique_destination(target, &self.cancel)?,
                false,
                false,
            ))),
            _ => Err(tr("This conflict cannot be resolved with that action.").into()),
        }
    }

    fn changed(&self, source: &gio::File, target: Option<&gio::File>, moving: bool) {
        let _ = self.events.send_blocking(Event::Changed {
            source: source.uri().to_string(),
            target: target.map(|f| f.uri().to_string()),
            moving,
        });
    }

    fn failed(&self, operation: Operation, file: &gio::File, message: &OperationError) {
        let error = if let OperationError::Gio(error) = message {
            crate::errors::AppError::gio(&operation.title(), file, error)
        } else {
            crate::errors::AppError {
                kind: crate::errors::ErrorKind::Other,
                operation: tr("File operation"),
                uri: file.uri().to_string(),
                details: message.to_string(),
            }
        };
        let operation = if self.completed_targets.is_empty() {
            operation
        } else {
            Operation::Retry {
                operation: Box::new(operation),
                completed: self.completed_targets.clone(),
            }
        };
        let _ = self
            .events
            .send_blocking(Event::Failed { operation, error });
    }

    fn copy_tree(
        &mut self,
        source: &gio::File,
        target: &gio::File,
        moving: bool,
        depth: usize,
    ) -> Result<bool> {
        self.check()?;
        if self
            .skip_targets
            .iter()
            .any(|(src, dst)| src == source.uri().as_str() && dst == target.uri().as_str())
        {
            return Ok(true);
        }
        if depth > 256 {
            return Err(tr("The folder tree is too deep.").into());
        }
        let info = source
            .query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                Some(&self.cancel),
            )
            .map_err(error)?;
        let directory = info.file_type() == gio::FileType::Directory;
        let Some((target, merge, replace)) = self.resolve(source, target, directory)? else {
            return Ok(false);
        };
        self.progress(source, None, depth == 0);
        // Atomic rename on the same filesystem; explicitly disallow the GIO fallback
        // because recursive cross-filesystem moves must preserve unprocessed sources.
        if moving && !merge {
            match source.move_(
                &target,
                gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE
                    | if replace {
                        gio::FileCopyFlags::OVERWRITE
                    } else {
                        gio::FileCopyFlags::NONE
                    },
                Some(&self.cancel),
                None,
            ) {
                Ok(()) => {
                    self.changed(source, Some(&target), true);
                    self.completed_targets
                        .push((source.uri().to_string(), target.uri().to_string()));
                    self.done(source);
                    return Ok(true);
                }
                Err(e)
                    if e.matches(gio::IOErrorEnum::NotSupported)
                        || e.matches(gio::IOErrorEnum::WouldRecurse)
                        || e.matches(gio::IOErrorEnum::WouldMerge) => {}
                Err(e) => return Err(error(e)),
            }
        }
        if directory {
            if !merge {
                target.make_directory(Some(&self.cancel)).map_err(error)?;
            }
            let enumerator = source
                .enumerate_children(
                    "standard::name",
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    Some(&self.cancel),
                )
                .map_err(error)?;
            let mut complete = true;
            loop {
                self.check()?;
                let Some(child) = enumerator.next_file(Some(&self.cancel)).map_err(error)? else {
                    break;
                };
                let src = source.child(child.name());
                match self.copy_tree(&src, &target.child(child.name()), moving, depth + 1) {
                    Ok(done) => complete &= done,
                    Err(e) => {
                        self.check()?;
                        complete = false;
                        self.failed(
                            Operation::Copy {
                                sources: vec![src.clone()],
                                destination: target.clone(),
                                moving,
                            },
                            &src,
                            &e,
                        );
                        self.errors.push(format!("{}: {e}", src.parse_name()));
                    }
                }
            }
            if let (_, Some(e)) = enumerator.close(Some(&self.cancel)) {
                return Err(error(e));
            }
            if moving && complete {
                source.delete(Some(&self.cancel)).map_err(error)?;
            }
            // Children already emitted their own changes, including partial merges.
            if complete {
                self.changed(source, Some(&target), moving);
                self.completed_targets
                    .push((source.uri().to_string(), target.uri().to_string()));
            }
            self.done(source);
            Ok(complete)
        } else {
            self.copy_leaf(source, &target, replace)?;
            self.changed(source, Some(&target), false);
            if moving {
                source.delete(Some(&self.cancel)).map_err(error)?;
            }
            if moving {
                self.changed(source, Some(&target), true);
            }
            self.completed_targets
                .push((source.uri().to_string(), target.uri().to_string()));
            self.done(source);
            Ok(true)
        }
    }

    fn copy_leaf(&mut self, source: &gio::File, target: &gio::File, replace: bool) -> Result<()> {
        // Copy into an exclusive sibling first: cancellation never truncates an
        // existing destination, and partially copied files are cleaned up.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = target
            .parent()
            .ok_or_else(|| tr("The destination has no parent folder."))?;
        let temporary = parent.child(format!(
            ".finder-gnome-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let cancel = self.cancel.clone();
        let result = source.copy(
            &temporary,
            gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA,
            Some(&cancel),
            Some(&mut |current, total| self.progress(source, Some((current, total)), false)),
        );
        if let Err(e) = result {
            let _ = temporary.delete(gio::Cancellable::NONE);
            return Err(error(e));
        }
        let result = temporary.move_(
            target,
            gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE
                | if replace {
                    gio::FileCopyFlags::OVERWRITE
                } else {
                    gio::FileCopyFlags::NONE
                },
            Some(&self.cancel),
            None,
        );
        if result.is_err() {
            let _ = temporary.delete(gio::Cancellable::NONE);
        }
        result.map_err(error)
    }

    fn delete_tree(&mut self, file: &gio::File, depth: usize) -> Result<()> {
        self.check()?;
        if depth > 256 {
            return Err(tr("The folder tree is too deep.").into());
        }
        let info = file
            .query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                Some(&self.cancel),
            )
            .map_err(error)?;
        if info.file_type() == gio::FileType::Directory {
            let enumerator = file
                .enumerate_children(
                    "standard::name",
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    Some(&self.cancel),
                )
                .map_err(error)?;
            while let Some(child) = enumerator.next_file(Some(&self.cancel)).map_err(error)? {
                self.delete_tree(&file.child(child.name()), depth + 1)?;
            }
            if let (_, Some(e)) = enumerator.close(Some(&self.cancel)) {
                return Err(error(e));
            }
        }
        self.progress(file, None, false);
        file.delete(Some(&self.cancel)).map_err(error)?;
        self.changed(file, None, true);
        self.done(file);
        Ok(())
    }
}

fn error(e: glib::Error) -> OperationError {
    OperationError::Gio(e)
}

fn optional_info(file: &gio::File, cancel: &gio::Cancellable) -> Result<Option<gio::FileInfo>> {
    match file.query_info(
        "standard::type",
        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
        Some(cancel),
    ) {
        Ok(info) => Ok(Some(info)),
        Err(e) if e.matches(gio::IOErrorEnum::NotFound) => Ok(None),
        Err(e) => Err(error(e)),
    }
}

fn unique_destination(file: &gio::File, cancel: &gio::Cancellable) -> Result<gio::File> {
    let parent = file
        .parent()
        .ok_or_else(|| tr("The destination has no parent folder."))?;
    let name = file.basename().ok_or_else(|| tr("The file has no name."))?;
    // Preserve the raw OS filename, including non-UTF-8 bytes.
    for n in 2.. {
        let mut candidate = name.clone().into_os_string();
        candidate.push(format!(" ({n})"));
        let candidate = parent.child(PathBuf::from(candidate));
        if optional_info(&candidate, cancel)?.is_none() {
            return Ok(candidate);
        }
    }
    unreachable!()
}

pub fn normalized_sources(sources: Vec<gio::File>) -> Result<Vec<gio::File>> {
    let mut result: Vec<gio::File> = Vec::new();
    for source in sources {
        if !crate::files::supported(&source) {
            return Err(tr("Unsupported file location.").into());
        }
        if crate::files::is_root(&source) {
            return Err(tr("Operations on the filesystem root are not allowed.").into());
        }
        if let Some(path) = source.path() {
            let parent = path.parent().and_then(|p| std::fs::canonicalize(p).ok());
            if path == Path::new("/")
                || (parent.as_deref() == Some(Path::new("/")) && path.file_name().is_none())
            {
                return Err(tr("Operations on the filesystem root are not allowed.").into());
            }
        }
        if result
            .iter()
            .any(|f| source.equal(f) || source.has_prefix(f))
        {
            continue;
        }
        result.retain(|f| !f.has_prefix(&source));
        result.push(source);
    }
    Ok(result)
}

fn validate_destination(
    source: &gio::File,
    destination: &gio::File,
    cancel: Option<&gio::Cancellable>,
) -> Result<()> {
    if !crate::files::supported(source) || !crate::files::supported(destination) {
        return Err(tr("Unsupported file location.").into());
    }
    if destination.equal(source) || destination.has_prefix(source) {
        return Err(tr("A folder cannot be copied or moved into itself.").into());
    }
    if !source.is_native() || !destination.is_native() {
        let info = source
            .query_info(
                "standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                cancel,
            )
            .map_err(error)?;
        if info.file_type() == gio::FileType::Directory
            && source.uri_scheme() == destination.uri_scheme()
        {
            // GVfs does not expose realpath on every backend. Avoid ambiguous
            // directory aliases rather than following one into the source tree.
            for origin in [source, destination] {
                let mut ancestor = Some(origin.clone());
                while let Some(file) = ancestor {
                    if crate::files::is_root(&file) {
                        break;
                    }
                    let metadata = file
                        .query_info(
                            "standard::is-symlink",
                            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                            cancel,
                        )
                        .map_err(error)?;
                    if metadata.has_attribute("standard::is-symlink") && metadata.is_symlink() {
                        return Err(tr("Choose a directory path without symbolic-link aliases for this transfer.").into());
                    }
                    ancestor = file.parent();
                }
            }
        }
        return Ok(());
    }
    let src = source
        .path()
        .ok_or_else(|| tr("Only local files are supported in this version."))?;
    let dst = destination
        .path()
        .ok_or_else(|| tr("Only local files are supported in this version."))?;
    // Canonicalize the parent rather than following the source's final symlink.
    let src_parent = src
        .parent()
        .ok_or_else(|| tr("Operations on the filesystem root are not allowed."))?;
    let src = std::fs::canonicalize(src_parent)
        .map_err(|e| e.to_string())?
        .join(src.file_name().ok_or_else(|| tr("The file has no name."))?);
    let dst = std::fs::canonicalize(dst).map_err(|e| e.to_string())?;
    if dst == src || dst.starts_with(&src) {
        return Err(tr("A folder cannot be copied or moved into itself.").into());
    }
    Ok(())
}

fn validate_trash_entry(file: &gio::File) -> Result<()> {
    if !crate::files::is_trash(file)
        || !file
            .parent()
            .is_some_and(|parent| parent.equal(&gio::File::for_uri("trash:///")))
    {
        return Err(
            tr("Only top-level Trash items can be restored or permanently deleted here.").into(),
        );
    }
    Ok(())
}

fn execute(job: Job) {
    let mut operation = job.operation;
    let mut skip_targets = Vec::new();
    while let Operation::Retry {
        operation: inner,
        completed,
    } = operation
    {
        skip_targets.extend(completed);
        operation = *inner;
    }
    let mut ctx = Context {
        cancel: job.cancel,
        events: job.events,
        completed: 0,
        skipped: 0,
        errors: Vec::new(),
        remembered: None,
        last_progress: Instant::now(),
        completed_targets: skip_targets.clone(),
        skip_targets,
    };
    let original = operation.clone();
    let result = (|| -> Result<()> {
        ctx.check()?;
        let deleting = matches!(&operation, Operation::Delete(_));
        match operation {
            Operation::Retry { .. } => unreachable!("retry unwrapped"),
            Operation::Create(parent, name) => {
                validate_name(&name)?;
                parent
                    .child(name)
                    .make_directory(Some(&ctx.cancel))
                    .map_err(error)?;
                ctx.done(&parent);
            }
            Operation::Rename(file, name) => {
                validate_name(&name)?;
                normalized_sources(vec![file.clone()])?;
                let parent = file
                    .parent()
                    .ok_or_else(|| tr("The file has no parent folder."))?;
                if optional_info(&parent.child(&name), &ctx.cancel)?.is_some() {
                    return Err(tr("An item with that name already exists.").into());
                }
                let target = file
                    .set_display_name(&name, Some(&ctx.cancel))
                    .map_err(error)?;
                ctx.changed(&file, Some(&target), true);
                ctx.done(&file);
            }
            Operation::Copy {
                sources,
                destination,
                moving,
            } => {
                let sources = normalized_sources(sources)?;
                // Validate every source before any mutation.
                for source in &sources {
                    validate_destination(source, &destination, Some(&ctx.cancel))?;
                }
                for source in sources {
                    ctx.check()?;
                    let name = source
                        .basename()
                        .ok_or_else(|| tr("The file has no name."))?;
                    if let Err(e) = ctx.copy_tree(&source, &destination.child(name), moving, 0) {
                        ctx.check()?;
                        ctx.failed(
                            Operation::Copy {
                                sources: vec![source.clone()],
                                destination: destination.clone(),
                                moving,
                            },
                            &source,
                            &e,
                        );
                        ctx.errors.push(format!("{}: {e}", source.parse_name()));
                    }
                }
            }
            Operation::Restore {
                sources,
                destination,
            } => {
                for source in sources {
                    ctx.check()?;
                    let result = (|| -> Result<()> {
                        validate_trash_entry(&source)?;
                        let info = source
                            .query_info(
                                "trash::orig-path",
                                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                                Some(&ctx.cancel),
                            )
                            .map_err(error)?;
                        let original = info
                            .attribute_file_path("trash::orig-path")
                            .filter(|path| path.is_absolute())
                            .ok_or_else(|| {
                                tr("The original location is unavailable. Use Restore To instead.")
                            })?;
                        let original = gio::File::for_path(original);
                        let target = if let Some(folder) = &destination {
                            if !crate::files::supported(folder) {
                                return Err(tr("Choose a local, SMB or SFTP folder.").into());
                            }
                            folder.child(
                                original
                                    .basename()
                                    .ok_or_else(|| tr("The original file has no name."))?,
                            )
                        } else {
                            original
                        };
                        // Never overwrite a file created since the item was trashed.
                        // No recursive fallback: a failed restore leaves the trash entry intact.
                        source
                            .move_(
                                &target,
                                gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE,
                                Some(&ctx.cancel),
                                None,
                            )
                            .map_err(error)?;
                        ctx.changed(&source, Some(&target), true);
                        ctx.done(&source);
                        Ok(())
                    })();
                    if let Err(e) = result {
                        ctx.check()?;
                        ctx.failed(
                            Operation::Restore {
                                sources: vec![source.clone()],
                                destination: destination.clone(),
                            },
                            &source,
                            &e,
                        );
                        ctx.errors.push(format!("{}: {e}", source.parse_name()));
                    }
                }
            }
            Operation::DeleteTrash(sources) => {
                for source in sources {
                    ctx.check()?;
                    let result = validate_trash_entry(&source)
                        .and_then(|()| source.delete(Some(&ctx.cancel)).map_err(error));
                    if let Err(e) = result {
                        ctx.check()?;
                        ctx.failed(Operation::DeleteTrash(vec![source.clone()]), &source, &e);
                        ctx.errors.push(format!("{}: {e}", source.parse_name()));
                    } else {
                        ctx.changed(&source, None, true);
                        ctx.done(&source);
                    }
                }
            }
            Operation::Trash(sources) | Operation::Delete(sources) => {
                for file in normalized_sources(sources)? {
                    ctx.check()?;
                    let result = if deleting {
                        ctx.delete_tree(&file, 0)
                    } else {
                        file.trash(Some(&ctx.cancel)).map_err(error).map(|_| {
                            ctx.changed(&file, None, true);
                            ctx.done(&file);
                        })
                    };
                    if let Err(e) = result {
                        ctx.check()?;
                        ctx.failed(
                            if deleting {
                                Operation::Delete(vec![file.clone()])
                            } else {
                                Operation::Trash(vec![file.clone()])
                            },
                            &file,
                            &e,
                        );
                        ctx.errors.push(format!("{}: {e}", file.parse_name()));
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        if !ctx.cancel.is_cancelled() {
            let file = match &original {
                Operation::Retry { .. } => unreachable!("retry unwrapped"),
                Operation::Create(f, _) | Operation::Rename(f, _) => f.clone(),
                Operation::Copy { destination, .. } => destination.clone(),
                Operation::Restore { sources: v, .. }
                | Operation::DeleteTrash(v)
                | Operation::Trash(v)
                | Operation::Delete(v) => v
                    .first()
                    .cloned()
                    .unwrap_or_else(|| gio::File::for_path("/")),
            };
            ctx.failed(original, &file, &e);
            ctx.errors.push(e.to_string());
        }
    }
    let _ = ctx.events.send_blocking(Event::Finished {
        completed: ctx.completed,
        skipped: ctx.skipped,
        errors: ctx.errors,
        cancelled: ctx.cancel.is_cancelled(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(operation: Operation, decision: Decision) -> (u64, u64, Vec<String>, bool) {
        let (tx, rx) = async_channel::unbounded();
        let cancel = gio::Cancellable::new();
        std::thread::spawn(move || {
            execute(Job {
                operation,
                cancel,
                events: tx,
            })
        });
        loop {
            match rx.recv_blocking().unwrap() {
                Event::Conflict(c) => c.answer.send((decision, true)).unwrap(),
                Event::Finished {
                    completed,
                    skipped,
                    errors,
                    cancelled,
                } => return (completed, skipped, errors, cancelled),
                _ => {}
            }
        }
    }
    fn copy(src: &Path, dst: &Path, moving: bool) -> Operation {
        Operation::Copy {
            sources: vec![gio::File::for_path(src)],
            destination: gio::File::for_path(dst),
            moving,
        }
    }

    #[test]
    fn trash_mutations_reject_root_nested_and_non_trash_files() {
        assert!(validate_trash_entry(&gio::File::for_uri("trash:///entry")).is_ok());
        for uri in [
            "trash:///",
            "trash:///entry/child",
            "file:///tmp/a",
            "sftp://server/path",
        ] {
            assert!(validate_trash_entry(&gio::File::for_uri(uri)).is_err());
        }
    }
    #[test]
    fn recursive_copy_preserves_links_and_sources() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(src.join("nested/file"), "contents").unwrap();
        std::os::unix::fs::symlink("missing", src.join("broken")).unwrap();
        assert!(run(copy(&src, &dst, false), Decision::Cancel).2.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join("source/nested/file")).unwrap(),
            "contents"
        );
        assert_eq!(
            fs::read_link(dst.join("source/broken")).unwrap(),
            PathBuf::from("missing")
        );
        assert!(src.join("nested/file").exists());
    }
    #[test]
    fn conflicts_skip_replace_keep_both_and_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let dst = temp.path().join("dest");
        fs::create_dir(&dst).unwrap();
        let src = temp.path().join("file");
        fs::write(&src, "new").unwrap();
        fs::write(dst.join("file"), "old").unwrap();
        assert_eq!(run(copy(&src, &dst, false), Decision::Skip).1, 1);
        assert_eq!(fs::read_to_string(dst.join("file")).unwrap(), "old");
        assert!(
            run(copy(&src, &dst, false), Decision::KeepBoth)
                .2
                .is_empty()
        );
        assert_eq!(fs::read_to_string(dst.join("file (2)")).unwrap(), "new");
        assert!(run(copy(&src, &dst, false), Decision::Replace).2.is_empty());
        assert_eq!(fs::read_to_string(dst.join("file")).unwrap(), "new");
        assert!(run(copy(&src, &dst, true), Decision::Cancel).3);
        assert!(src.exists());
    }
    #[test]
    fn rejects_self_descendants_and_symlink_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("source");
        fs::create_dir_all(src.join("child")).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&src, &alias).unwrap();
        assert!(
            !run(copy(&src, &alias, true), Decision::Replace)
                .2
                .is_empty()
        );
        assert!(src.join("child").exists());
        assert!(
            !run(copy(&src, &src.join("child"), false), Decision::Replace)
                .2
                .is_empty()
        );
    }
    #[test]
    fn deletion_does_not_follow_symlinks_and_sources_are_normalized() {
        let temp = tempfile::tempdir().unwrap();
        let tree = temp.path().join("tree");
        let safe = temp.path().join("safe");
        fs::create_dir(&tree).unwrap();
        fs::write(&safe, "keep").unwrap();
        std::os::unix::fs::symlink(&safe, tree.join("link")).unwrap();
        let sources = normalized_sources(vec![
            gio::File::for_path(tree.join("link")),
            gio::File::for_path(&tree),
        ])
        .unwrap();
        assert_eq!(sources.len(), 1);
        assert!(
            run(Operation::Delete(sources), Decision::Cancel)
                .2
                .is_empty()
        );
        assert!(safe.exists());
        assert!(!tree.exists());
        assert!(normalized_sources(vec![gio::File::for_path("/")]).is_err());
    }
    #[test]
    fn failed_move_keeps_source_and_merge_skip_keeps_unmoved_children() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("source");
        fs::write(&src, "keep").unwrap();
        assert!(
            !run(
                copy(&src, &temp.path().join("missing"), true),
                Decision::Replace
            )
            .2
            .is_empty()
        );
        assert!(src.exists());
    }

    #[test]
    fn replacement_does_not_follow_destination_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let dst = temp.path().join("dest");
        fs::create_dir(&dst).unwrap();
        let source = temp.path().join("file");
        let protected = temp.path().join("protected");
        fs::write(&source, "replacement").unwrap();
        fs::write(&protected, "keep").unwrap();
        std::os::unix::fs::symlink(&protected, dst.join("file")).unwrap();
        assert!(
            run(copy(&source, &dst, false), Decision::Replace)
                .2
                .is_empty()
        );
        assert_eq!(fs::read_to_string(&protected).unwrap(), "keep");
        assert_eq!(fs::read_to_string(dst.join("file")).unwrap(), "replacement");
        assert!(!fs::symlink_metadata(dst.join("file")).unwrap().is_symlink());
    }

    #[test]
    fn cancelled_queued_job_makes_no_changes() {
        let temp = tempfile::tempdir().unwrap();
        let cancel = gio::Cancellable::new();
        cancel.cancel();
        let (tx, rx) = async_channel::unbounded();
        execute(Job {
            operation: Operation::Create(gio::File::for_path(temp.path()), "never-created".into()),
            cancel,
            events: tx,
        });
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished {
                cancelled: true,
                completed: 0,
                ..
            }
        ));
        assert!(!temp.path().join("never-created").exists());
    }

    #[test]
    fn merge_move_preserves_skipped_sources() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let dst = temp.path().join("dest");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(dst.join("source")).unwrap();
        fs::write(source.join("duplicate"), "new").unwrap();
        fs::write(source.join("fresh"), "fresh").unwrap();
        fs::write(dst.join("source/duplicate"), "old").unwrap();
        let (tx, rx) = async_channel::unbounded();
        let cancel = gio::Cancellable::new();
        let operation = copy(&source, &dst, true);
        std::thread::spawn(move || {
            execute(Job {
                operation,
                cancel,
                events: tx,
            })
        });
        loop {
            match rx.recv_blocking().unwrap() {
                Event::Conflict(conflict) => {
                    conflict
                        .answer
                        .send((
                            if conflict.both_directories {
                                Decision::Merge
                            } else {
                                Decision::Skip
                            },
                            false,
                        ))
                        .unwrap();
                }
                Event::Finished {
                    errors, skipped, ..
                } => {
                    assert!(errors.is_empty(), "{errors:?}");
                    assert_eq!(skipped, 1);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(fs::read_to_string(source.join("duplicate")).unwrap(), "new");
        assert!(!source.join("fresh").exists());
        assert_eq!(
            fs::read_to_string(dst.join("source/duplicate")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(dst.join("source/fresh")).unwrap(),
            "fresh"
        );
    }

    #[test]
    fn non_utf8_names_survive_recursive_copy() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        let name = std::ffi::OsString::from_vec(vec![b'f', 0xff]);
        fs::write(src.join(&name), "raw name").unwrap();
        assert!(run(copy(&src, &dst, false), Decision::Cancel).2.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join("source").join(name)).unwrap(),
            "raw name"
        );
    }

    #[test]
    #[ignore = "requires a writable /dev/shm on a different filesystem"]
    fn cross_filesystem_move() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir_in("/dev/shm").unwrap();
        assert_ne!(
            fs::metadata(temp.path()).unwrap().dev(),
            fs::metadata(second.path()).unwrap().dev()
        );
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested/data"), "cross device").unwrap();
        std::os::unix::fs::symlink("missing", source.join("broken")).unwrap();
        let result = run(copy(&source, second.path(), true), Decision::Cancel);
        assert!(result.2.is_empty(), "{:?}", result.2);
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(second.path().join("source/nested/data")).unwrap(),
            "cross device"
        );
        assert_eq!(
            fs::read_link(second.path().join("source/broken")).unwrap(),
            PathBuf::from("missing")
        );
    }

    #[test]
    fn trash_uses_isolated_xdg_directory() {
        // GLib caches XDG paths, so use a fresh process instead of mutating the
        // environment of parallel tests. Every input and trash location is temporary.
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "operations::tests::isolated_trash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("FINDER_GNOME_TRASH_TEST", temp.path())
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("GIO_USE_VFS", "local")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "subprocess helper; run trash_uses_isolated_xdg_directory instead"]
    fn isolated_trash_child() {
        let Some(root) = std::env::var_os("FINDER_GNOME_TRASH_TEST") else {
            return;
        };
        let root = PathBuf::from(root);
        let source = root.join("trash-me.txt");
        let probe = root.join("probe.txt");
        fs::write(&probe, "probe").unwrap();
        let unsupported = match gio::File::for_path(&probe).trash(gio::Cancellable::NONE) {
            Ok(()) => false,
            Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => true,
            Err(e) => panic!("unexpected trash probe failure: {e}"),
        };
        fs::write(&source, "trash contents").unwrap();
        let result = run(
            Operation::Trash(vec![gio::File::for_path(&source)]),
            Decision::Cancel,
        );
        if unsupported {
            assert!(!result.2.is_empty());
            assert!(
                source.exists(),
                "unsupported trash must never fall back to deletion"
            );
            return;
        }
        assert!(result.2.is_empty(), "{:?}", result.2);
        assert!(!source.exists());
        let trash = root.join("data/Trash");
        assert!(fs::read_dir(trash.join("files")).unwrap().any(|entry| fs::read_to_string(entry.unwrap().path()).unwrap() == "trash contents"));
        assert!(fs::read_dir(trash.join("info")).unwrap().next().is_some());
    }
    #[test]
    fn retry_does_not_touch_successful_copy_children() {
        let temp = tempfile::tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("destination");
        fs::create_dir(&src).unwrap();
        fs::create_dir_all(dst.join("source")).unwrap();
        fs::write(src.join("a"), "source original").unwrap();
        fs::write(src.join("b"), "remaining").unwrap();
        fs::write(dst.join("source/a"), "completed target unchanged").unwrap();
        let operation = Operation::Retry {
            operation: Box::new(Operation::Copy {
                sources: vec![gio::File::for_path(&src)],
                destination: gio::File::for_path(&dst),
                moving: false,
            }),
            completed: vec![(
                gio::File::for_path(src.join("a")).uri().to_string(),
                gio::File::for_path(dst.join("source/a")).uri().to_string(),
            )],
        };
        let (_, _, errors, _) = run(operation, Decision::Merge);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            fs::read_to_string(dst.join("source/a")).unwrap(),
            "completed target unchanged"
        );
        assert_eq!(
            fs::read_to_string(dst.join("source/b")).unwrap(),
            "remaining"
        );
    }
}
