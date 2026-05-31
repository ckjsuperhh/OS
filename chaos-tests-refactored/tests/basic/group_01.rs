use kernel_refactored::*;
use std::sync::Arc;

fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, ms: u64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { f(); let _ = tx.send(()); });
    rx.recv_timeout(std::time::Duration::from_millis(ms)).is_ok()
}

#[test]
fn basic_bkl_single_acquire_release() {
    GKL.enter(1001);
    assert!(GKL.held());
    assert_eq!(GKL.owner(), 1001);
    GKL.leave();
    assert!(!GKL.held());
}

#[test]
fn basic_bkl_double_acquire_single_release() {
    GKL.enter(1002);
    GKL.enter(1002);
    assert_eq!(GKL.level(), 2);
    GKL.leave();
    assert!(GKL.held());
    assert_eq!(GKL.level(), 1);
    GKL.leave();
}

#[test]
fn basic_cross_module_lock_order() {
    let pool = Arc::new(FramePool::new(16));
    let p = pool.clone();
    let done = run_with_timeout(move || {
        GKL.enter(1003);
        p.get(1004);
        GKL.leave();
    }, 2000);
    if !done {
        GKL.leave();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(done);
}
