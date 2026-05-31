use kernel_refactored::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[test]
fn basic_spinlock_protect_data() {
    let spin = Arc::new(Spin::new());
    let data = Arc::new(AtomicUsize::new(0));

    let s = spin.clone();
    let d = data.clone();
    let h = std::thread::spawn(move || {
        s.acquire();
        d.fetch_add(42, Ordering::SeqCst);
        s.release();
    });

    h.join().unwrap();
    assert_eq!(data.load(Ordering::SeqCst), 42);
}

#[test]
fn basic_sleep_under_spinlock_uniprocessor() {
    let ch = Arc::new(Channel::new(4));
    let ch2 = ch.clone();

    std::thread::spawn(move || {
        ch2.recv();
    });

    std::thread::sleep(Duration::from_millis(200));

    assert!(!ch.guard.is_held());
}

#[test]
fn basic_spinlock_held_duration() {
    let spin = Arc::new(Spin::new());
    let s1 = spin.clone();
    let s2 = spin.clone();

    let a = std::thread::spawn(move || {
        s1.acquire();
        std::thread::sleep(Duration::from_millis(100));
        s1.release();
    });

    std::thread::sleep(Duration::from_millis(10));

    let start = Instant::now();
    s2.acquire();
    let elapsed = start.elapsed();
    s2.release();

    a.join().unwrap();

    assert!(elapsed >= Duration::from_millis(50));
}
