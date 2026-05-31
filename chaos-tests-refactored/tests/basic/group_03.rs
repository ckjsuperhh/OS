use kernel_refactored::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, ms: u64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { f(); let _ = tx.send(()); });
    rx.recv_timeout(std::time::Duration::from_millis(ms)).is_ok()
}

#[test]
fn basic_condvar_signal_before_wait() {
    let q = Arc::new(SyncQueue::new());
    let m = Arc::new(Mutex::new(false));

    q.signal();

    let q2 = q.clone();
    let m2 = m.clone();
    let done = run_with_timeout(move || {
        q2.park_on(&m2, |v| *v);
    }, 2000);

    assert!(done);
}

#[test]
fn basic_spurious_wakeup_no_recheck() {
    let q = Arc::new(SyncQueue::new());
    let m = Arc::new(Mutex::new(false));

    let q_c = q.clone();
    let m_c = m.clone();

    let consumer = std::thread::spawn(move || -> bool {
        q_c.park_on(&m_c, |v| *v)
    });

    std::thread::sleep(Duration::from_millis(50));

    q.broadcast();

    let returned = consumer.join().unwrap();
    let actual = *m.lock().unwrap();

    assert_eq!(returned, actual);
}

#[test]
fn basic_producer_consumer_single() {
    let q = Arc::new(SyncQueue::new());
    let m: Arc<Mutex<Option<u8>>> = Arc::new(Mutex::new(None));

    let q_c = q.clone();
    let m_c = m.clone();

    let done = run_with_timeout(move || {
        let q_consumer = q_c.clone();
        let m_consumer = m_c.clone();

        let consumer = std::thread::spawn(move || {
            q_consumer.park_on(&m_consumer, |v| v.is_some());
        });

        std::thread::yield_now();
        *m_c.lock().unwrap() = Some(42);
        q_c.signal();

        consumer.join().unwrap();
    }, 2000);

    assert!(done);
}
