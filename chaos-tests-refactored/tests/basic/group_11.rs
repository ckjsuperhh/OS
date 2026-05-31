use kernel_refactored::*;
use std::sync::Arc;

fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, ms: u64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_millis(ms)).is_ok()
}

#[test]
fn basic_fork_exec_workload() {
    let kern = Kernel::new(64);
    kern.proc_init();
    let root = kern.tasks.root.lock().unwrap().clone().unwrap();

    let child = kern.tasks.fork_task(&root);

    let mut frames = Vec::new();
    for _ in 0..4 {
        if let Some(f) = kern.pool.get_inner() {
            frames.push(f);
        }
    }
    assert_eq!(frames.len(), 4);

    let src = PgFrame::with_rc(2);
    let sp = SharedPage::new(frames[0]);
    let cow_result = sp.fault(&kern.pool, &src);
    assert!(cow_result.is_ok());

    let parent_guard = child.parent.lock().unwrap();
    assert!(parent_guard.is_some());
    assert_eq!(parent_guard.as_ref().unwrap().id(), root.id());
    drop(parent_guard);

    // 4 direct allocations + 1 from CoW fault
    assert_eq!(kern.pool.free_count(), 59);
    assert!(!GKL.held());
}

#[test]
fn basic_pipe_ipc_workload() {
    let ch = Arc::new(Channel::new(RBUF_CAP));
    let received = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));

    let ok = run_with_timeout(
        {
            let ch = ch.clone();
            let received = received.clone();
            move || {
                let ch_prod = ch.clone();
                let producer = std::thread::spawn(move || {
                    for i in 0..200u8 {
                        while !ch_prod.send(i) {
                            std::thread::yield_now();
                        }
                    }
                    ch_prod.close();
                });

                loop {
                    match ch.recv() {
                        Some(v) => received.lock().unwrap().push(v),
                        None => break,
                    }
                }
                producer.join().unwrap();
            }
        },
        3000,
    );

    assert!(ok);
    let data = received.lock().unwrap();
    assert_eq!(data.len(), 200);
    for (i, &v) in data.iter().enumerate() {
        assert_eq!(v, i as u8);
    }
}

#[test]
fn basic_mmap_file_io_workload() {
    let pool = FramePool::new(32);

    assert!(check_access(0x1000, 0x2000));

    let f = pool.get_inner().unwrap();
    let src = PgFrame::with_rc(2);
    let sp = SharedPage::new(f);
    let nf = sp.fault(&pool, &src);
    assert!(nf.is_ok());

    // 1 direct + 1 from fault
    assert_eq!(pool.free_count(), 30);

    // overflow wraps 0x1000 + usize::MAX well below KERN_BASE
    assert!(!check_access(0x1000, usize::MAX));
}
