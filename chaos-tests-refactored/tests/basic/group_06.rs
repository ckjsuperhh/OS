use kernel_refactored::*;

#[test]
fn basic_block_read_success() {
    let d = Disk::new("ok");
    let mut buf = [0u8; 512];
    let result = d.read_block(0, &mut buf);
    assert!(result.is_ok());
    assert!(buf.iter().all(|&b| b == 0xAA));
}

#[test]
fn basic_block_read_single_retry() {
    let d = Disk::failing("retry1", 1);
    let mut buf = [0u8; 512];
    let result = d.read_block_n(0, &mut buf, 100);
    assert!(result.is_ok());
    assert_eq!(d.total_ops(), 2);
}

#[test]
fn basic_block_read_infinite_retry() {
    let d = Disk::failing("inf", usize::MAX);
    let mut buf = [0u8; 512];
    let result = d.read_block_n(0, &mut buf, 10);
    assert_eq!(result, Err("limit"));
    assert_eq!(d.total_ops(), 10);

    let d2 = Disk::failing("inf2", usize::MAX);
    let finished = run_with_timeout(
        move || {
            let mut b = [0u8; 512];
            let _ = d2.read_block(0, &mut b);
        },
        200,
    );
    assert!(!finished);
}

fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, ms: u64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_millis(ms)).is_ok()
}
