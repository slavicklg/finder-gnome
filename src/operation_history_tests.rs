// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT
use super::*;
use std::fs;

#[derive(Default)]
struct Outcome {
    steps: Vec<Step>,
    remaining: Vec<Step>,
    irreversible: bool,
    errors: Vec<String>,
}
fn run(operation: Operation, decision: Decision) -> Outcome {
    let (events, rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        execute(Job {
            fallback_tags: Default::default(),
            operation,
            cancel: gio::Cancellable::new(),
            events,
        })
    });
    let mut result = Outcome::default();
    loop {
        match rx.recv_blocking().unwrap() {
            Event::Conflict(conflict) => conflict.answer.send((decision, true)).unwrap(),
            Event::History {
                steps,
                remaining,
                irreversible,
                ..
            } => {
                result.steps = steps;
                result.remaining = remaining;
                result.irreversible = irreversible;
            }
            Event::Finished { errors, .. } => {
                result.errors = errors;
                return result;
            }
            _ => {}
        }
    }
}
fn replay(steps: Vec<Step>) -> Outcome {
    run(
        Operation::Replay {
            title: "test".into(),
            steps,
        },
        Decision::Cancel,
    )
}
fn copy(source: &gio::File, destination: &gio::File, moving: bool) -> Operation {
    Operation::Copy {
        sources: vec![source.clone()],
        destination: destination.clone(),
        moving,
    }
}
#[test]
fn folder_copy_undo_redo_cycles_and_external_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("source")).unwrap();
    fs::create_dir(temp.path().join("destination")).unwrap();
    fs::write(temp.path().join("source/file"), "original").unwrap();
    let out = run(
        copy(&root.child("source"), &root.child("destination"), false),
        Decision::Cancel,
    );
    assert!(out.errors.is_empty());
    assert!(!out.steps.is_empty());
    let undone = replay(out.steps);
    assert!(undone.errors.is_empty(), "{:?}", undone.errors);
    assert!(!temp.path().join("destination/source").exists());
    let redone = replay(undone.steps);
    assert!(redone.errors.is_empty(), "{:?}", redone.errors);
    fs::write(
        temp.path().join("destination/source/file"),
        "externally changed",
    )
    .unwrap();
    let blocked = replay(redone.steps);
    assert!(!blocked.errors.is_empty());
    assert!(!blocked.remaining.is_empty());
    assert_eq!(
        fs::read_to_string(temp.path().join("destination/source/file")).unwrap(),
        "externally changed"
    );
}
#[test]
fn undo_creation_keeps_new_children_and_recovers_on_retry() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    let out = run(Operation::Create(root, "created".into()), Decision::Cancel);
    fs::write(temp.path().join("created/new"), "new").unwrap();
    let blocked = replay(out.steps);
    assert!(!blocked.errors.is_empty());
    assert!(blocked.steps.is_empty());
    fs::remove_file(temp.path().join("created/new")).unwrap();
    let undone = replay(blocked.remaining);
    assert!(undone.errors.is_empty());
    assert!(!temp.path().join("created").exists());
    assert!(replay(undone.steps).errors.is_empty());
    assert!(temp.path().join("created").is_dir());
}
#[test]
fn merged_move_undo_preserves_existing_destination_and_restores_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir_all(temp.path().join("destination/source")).unwrap();
    fs::create_dir(temp.path().join("source")).unwrap();
    fs::write(temp.path().join("source/moved"), "move").unwrap();
    fs::write(temp.path().join("destination/source/existing"), "keep").unwrap();
    let out = run(
        copy(&root.child("source"), &root.child("destination"), true),
        Decision::Merge,
    );
    assert!(out.errors.is_empty());
    assert!(!temp.path().join("source").exists());
    let undone = replay(out.steps);
    assert!(undone.errors.is_empty(), "{:?}", undone.errors);
    assert!(temp.path().join("source/moved").exists());
    assert!(temp.path().join("destination/source/existing").exists());
    let redone = replay(undone.steps);
    assert!(redone.errors.is_empty(), "{:?}", redone.errors);
    assert!(temp.path().join("destination/source/moved").exists());
    assert!(!temp.path().join("source").exists());
}
#[test]
fn rename_redo_and_occupied_destination_are_safe() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::write(temp.path().join("a"), "a").unwrap();
    let out = run(
        Operation::Rename(root.child("a"), "b".into()),
        Decision::Cancel,
    );
    fs::write(temp.path().join("a"), "new a").unwrap();
    let blocked = replay(out.steps);
    assert!(!blocked.errors.is_empty());
    assert_eq!(fs::read_to_string(temp.path().join("a")).unwrap(), "new a");
    fs::remove_file(temp.path().join("a")).unwrap();
    let undone = replay(blocked.remaining);
    assert!(undone.errors.is_empty());
    assert!(replay(undone.steps).errors.is_empty());
    assert!(temp.path().join("b").exists());
}
#[test]
fn replacement_and_permanent_delete_mark_history_barriers() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("dest")).unwrap();
    fs::write(temp.path().join("a"), "new").unwrap();
    fs::write(temp.path().join("dest/a"), "old").unwrap();
    let out = run(
        copy(&root.child("a"), &root.child("dest"), false),
        Decision::Replace,
    );
    assert!(out.irreversible);
    let out = run(Operation::Delete(vec![root.child("a")]), Decision::Cancel);
    assert!(out.irreversible);
}
#[test]
fn keep_both_undo_uses_actual_destination() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("dest")).unwrap();
    fs::write(temp.path().join("a"), "new").unwrap();
    fs::write(temp.path().join("dest/a"), "old").unwrap();
    let out = run(
        copy(&root.child("a"), &root.child("dest"), false),
        Decision::KeepBoth,
    );
    let undone = replay(out.steps);
    assert!(undone.errors.is_empty());
    assert_eq!(fs::read_dir(temp.path().join("dest")).unwrap().count(), 1);
    assert_eq!(
        fs::read_to_string(temp.path().join("dest/a")).unwrap(),
        "old"
    );
}
#[test]
#[ignore = "requires /dev/shm on a different writable filesystem"]
fn cross_filesystem_history_roundtrip() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir_in("/dev/shm").unwrap();
    fs::create_dir(source.path().join("folder")).unwrap();
    fs::write(source.path().join("folder/file"), "content").unwrap();
    let out = run(
        copy(
            &gio::File::for_path(source.path().join("folder")),
            &gio::File::for_path(target.path()),
            true,
        ),
        Decision::Cancel,
    );
    assert!(out.errors.is_empty());
    let undone = replay(out.steps);
    assert!(undone.errors.is_empty(), "{:?}", undone.errors);
    assert!(source.path().join("folder/file").exists());
    let redone = replay(undone.steps);
    assert!(redone.errors.is_empty(), "{:?}", redone.errors);
    assert!(target.path().join("folder/file").exists());
}

#[test]
fn cancelled_batch_keeps_only_completed_steps_in_history() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("dest")).unwrap();
    fs::write(temp.path().join("a"), "a").unwrap();
    fs::write(temp.path().join("b"), "b").unwrap();
    fs::write(temp.path().join("dest/b"), "existing").unwrap();
    let out = run(
        Operation::Copy {
            sources: vec![root.child("a"), root.child("b")],
            destination: root.child("dest"),
            moving: false,
        },
        Decision::Cancel,
    );
    assert_eq!(out.steps.len(), 1);
    assert!(temp.path().join("dest/a").exists());
    let undone = replay(out.steps);
    assert!(undone.errors.is_empty());
    assert!(!temp.path().join("dest/a").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("dest/b")).unwrap(),
        "existing"
    );
}
#[test]
fn partial_undo_keeps_failed_remainder_and_redoes_only_completed_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = gio::File::for_path(temp.path());
    fs::create_dir(temp.path().join("dest")).unwrap();
    fs::write(temp.path().join("a"), "a").unwrap();
    fs::write(temp.path().join("b"), "b").unwrap();
    let out = run(
        Operation::Copy {
            sources: vec![root.child("a"), root.child("b")],
            destination: root.child("dest"),
            moving: false,
        },
        Decision::Cancel,
    );
    fs::write(temp.path().join("dest/a"), "external").unwrap();
    let partial = replay(out.steps);
    assert_eq!(partial.remaining.len(), 1);
    assert_eq!(partial.steps.len(), 1);
    assert!(!temp.path().join("dest/b").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("dest/a")).unwrap(),
        "external"
    );
    let redone = replay(partial.steps);
    assert!(redone.errors.is_empty());
    assert!(temp.path().join("dest/b").exists());
}

#[test]
fn catalog_move_notifications_are_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let first = gio::File::for_path(temp.path().join("first"));
    let target = gio::File::for_path(temp.path().join("target"));
    fs::write(first.path().unwrap(), "data").unwrap();
    let catalog = crate::catalog::Catalog::new(temp.path().join("catalog.sqlite"));
    glib::MainContext::new().block_on(async {
        catalog
            .assign(
                vec![crate::catalog::Identity::uri(&first)],
                vec![1, 5],
                true,
            )
            .await
            .unwrap();
        fs::rename(first.path().unwrap(), target.path().unwrap()).unwrap();
        for _ in 0..3 {
            catalog
                .changed(
                    first.uri().to_string(),
                    Some(target.uri().to_string()),
                    true,
                )
                .await
                .unwrap();
        }
        assert_eq!(
            catalog
                .labels(target.uri().to_string())
                .await
                .unwrap()
                .len(),
            2
        );
        catalog
            .observe(vec![crate::catalog::Identity::uri(&target)])
            .await
            .unwrap();
        assert_eq!(
            catalog
                .labels(target.uri().to_string())
                .await
                .unwrap()
                .len(),
            2
        );
    });
}
