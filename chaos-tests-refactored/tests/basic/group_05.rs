use kernel_refactored::*;
use std::sync::{Arc, Weak};

#[test]
fn basic_process_create_exit() {
    let tt = TaskTable::new();
    let t = tt.spawn("test");
    assert_eq!(tt.count(), 1);
    let id = t.id();
    tt.reap(id);
    assert_eq!(tt.count(), 0);
}

#[test]
fn basic_weak_ref_after_drop() {
    let task = Task::make(1, "ephemeral");
    let weak: Weak<Task> = Arc::downgrade(&task);
    drop(task);
    assert!(weak.upgrade().is_none());
}

#[test]
fn basic_stale_weak_upgrade() {
    let tt = TaskTable::new();
    let _root = tt.spawn_root();
    let a = tt.spawn("A");
    let id_a = a.id();
    let stale_ref = a.clone();
    tt.reap(id_a);
    let b = tt.spawn("B");
    assert_eq!(stale_ref.info.lock().unwrap().status, Some(0));
    assert_ne!(b.id(), stale_ref.id());
}
