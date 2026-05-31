use kernel_refactored::*;
use std::sync::Arc;
use std::thread;

#[test]
fn basic_refcount_increment_decrement() {
    let f = PgFrame::new();
    f.up();
    f.down();
    assert_eq!(f.count(), 0);
}

#[test]
fn basic_refcount_concurrent_increment() {
    let f = Arc::new(PgFrame::with_rc(0));
    let handles: Vec<_> = (0..64)
        .map(|_| {
            let f = f.clone();
            thread::spawn(move || {
                f.up();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(f.count(), 64);
}

#[test]
fn basic_cow_single_thread() {
    let pool = FramePool::new(16);
    let src = PgFrame::with_rc(2);
    let sp = SharedPage::new(0);
    let initial_free = pool.free_count();
    let result = sp.fault(&pool, &src);
    assert!(result.is_ok());
    assert_eq!(pool.free_count(), initial_free - 1);
    assert_eq!(src.count(), 1);
}
