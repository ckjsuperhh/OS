use kernel_refactored::*;
use std::sync::Arc;
use std::thread;

fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, ms: u64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_millis(ms)).is_ok()
}

#[test]
fn basic_path_resolve_simple() {
    let mt = MountTable::new();
    let r = mt.resolve("/foo/bar");
    assert_eq!(r.unwrap(), "/foo/bar");
}

#[test]
fn basic_mount_then_resolve() {
    let mt = MountTable::new();
    mt.bind("/mnt", "dev0");
    let r = mt.resolve("/mnt/file");
    assert_eq!(r.unwrap(), "dev0:/file");
}

#[test]
fn basic_concurrent_mount_and_lookup() {
    let mt = Arc::new(MountTable::new());
    mt.bind("/mnt", "dev0");

    let mt_move = mt.clone();
    let completed = run_with_timeout(
        move || {
            let mt_r = mt_move.clone();
            let reader = thread::spawn(move || {
                for _ in 0..500 {
                    let _ = mt_r.resolve("/mnt/deep/path");
                }
            });
            let mt_w = mt_move.clone();
            let writer = thread::spawn(move || {
                for i in 0..500 {
                    mt_w.bind(&format!("/other{}", i), "dev1");
                }
            });
            reader.join().unwrap();
            writer.join().unwrap();
        },
        2000,
    );
    assert!(completed);
}
