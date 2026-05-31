use kernel_refactored::*;

#[test]
fn basic_access_ok_valid_range() {
    assert!(check_access(0x1000, 0x100));
    assert!(!check_access(KERN_BASE, 1));
}

#[test]
fn basic_access_ok_overflow() {
    let result = check_access(KERN_BASE - 1, usize::MAX);
    assert!(!result);
}

#[test]
fn basic_zombie_single_child() {
    let tt = TaskTable::new();
    let root = tt.spawn_root();
    let child = tt.fork_task(&root);
    let child_id = child.id();
    let root_id = root.id();

    assert!(child.parent.lock().unwrap().is_some());

    tt.reap(child_id);

    assert!(tt.find(child_id).is_none());
    assert!(tt.find(root_id).is_some());
}
