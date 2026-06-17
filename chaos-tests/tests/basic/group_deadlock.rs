use chaos_tests::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, ms: u64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || { f(); let _ = tx.send(()); });
    rx.recv_timeout(Duration::from_millis(ms)).is_ok()
}

/// Test 1: tick (scheduler) + cache chain operations (fs) concurrent
#[test]
fn deadlock_tick_vs_cache_syscall() {
    let kern = Arc::new(Kernel::new(64));
    kern.proc_init();
    let k1 = kern.clone();
    let k2 = kern.clone();

    let done = run_with_timeout(move || {
        let h1 = thread::spawn(move || {
            for _ in 0..50 {
                k1.tick(1);
            }
        });
        let h2 = thread::spawn(move || {
            for i in 0..50usize {
                let _ = k2.dispatch_syscall(SYS_WRITE, 1, 0, 10, 0, 0, 0);
                let _ = k2.dispatch_syscall(SYS_READ, 0, 0, 10, 0, 0, 0);
                let _ = k2.cache.fetch(i % 64, Duration::from_millis(1));
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
    }, 5000);
    if !done {
        // cleanup
        GKL.leave();
    }
    assert!(done, "tick + cache operations deadlocked");
}

/// Test 2: tick + balance_load + fork concurrent
#[test]
fn deadlock_tick_balance_fork() {
    let kern = Arc::new(Kernel::new(64));
    kern.proc_init();
    let root = kern.tasks.root.lock().unwrap().clone().unwrap();
    let root_id = root.id();

    let k1 = kern.clone();
    let k2 = kern.clone();
    let k3 = kern.clone();

    let done = run_with_timeout(move || {
        let h1 = thread::spawn(move || {
            for _ in 0..30 {
                k1.tick(1);
            }
        });
        let h2 = thread::spawn(move || {
            for _ in 0..30 {
                k2.balance_load();
            }
        });
        let h3 = thread::spawn(move || {
            for _ in 0..20 {
                let _ = k3.do_fork(root_id);
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
    }, 5000);
    if !done { GKL.leave(); }
    assert!(done, "tick + balance + fork deadlocked");
}

/// Test 3: concurrent forks creating task chains
#[test]
fn deadlock_concurrent_fork_chains() {
    let kern = Arc::new(Kernel::new(64));
    kern.proc_init();
    let root = kern.tasks.root.lock().unwrap().clone().unwrap();
    let root_id = root.id();

    let k1 = kern.clone();
    let k2 = kern.clone();
    let k3 = kern.clone();

    let done = run_with_timeout(move || {
        let h1 = thread::spawn(move || {
            let mut ids = vec![root_id];
            for _ in 0..10 {
                if let Some(&parent) = ids.last() {
                    if let Ok(child) = k1.do_fork(parent) {
                        ids.push(child);
                    }
                }
            }
        });
        let h2 = thread::spawn(move || {
            let mut ids = vec![root_id];
            for _ in 0..10 {
                if let Some(&parent) = ids.last() {
                    if let Ok(child) = k2.do_fork(parent) {
                        ids.push(child);
                    }
                }
            }
        });
        let h3 = thread::spawn(move || {
            for _ in 0..20 {
                let _ = k3.do_fork(root_id);
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
    }, 5000);
    assert!(done, "concurrent fork chains deadlocked");
}

/// Test 4: memory allocation + cache + syscalls concurrent
#[test]
fn deadlock_memory_cache_syscall() {
    let kern = Arc::new(Kernel::new(64));
    kern.proc_init();

    let k1 = kern.clone();
    let k2 = kern.clone();
    let k3 = kern.clone();

    let done = run_with_timeout(move || {
        let h1 = thread::spawn(move || {
            for _ in 0..30 {
                let pages = k1.alloc_pages(4);
                k1.free_pages(&pages);
            }
        });
        let h2 = thread::spawn(move || {
            for i in 0..30usize {
                let _ = k2.cache.fetch(i % 64, Duration::from_millis(1));
                k2.cache.invalidate(i % 64);
            }
        });
        let h3 = thread::spawn(move || {
            for _ in 0..30 {
                let _ = k3.dispatch_syscall(SYS_WRITE, 1, 0, 10, 0, 0, 0);
                let _ = k3.dispatch_syscall(SYS_OPEN, 0, 2, 0o755, 0, 0, 0);
                let _ = k3.dispatch_syscall(SYS_CLOSE, 3, 0, 0, 0, 0, 0);
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
    }, 5000);
    assert!(done, "memory + cache + syscalls deadlocked");
}

/// Test 5: Full chain — scheduler + fs + memory + process ops
#[test]
fn deadlock_full_chain() {
    let kern = Arc::new(Kernel::new(64));
    kern.proc_init();
    let root = kern.tasks.root.lock().unwrap().clone().unwrap();
    let root_id = root.id();

    let k1 = kern.clone();
    let k2 = kern.clone();
    let k3 = kern.clone();
    let k4 = kern.clone();

    let done = run_with_timeout(move || {
        // Scheduler thread
        let h1 = thread::spawn(move || {
            for _ in 0..20 {
                k1.tick(1);
                k1.schedule_tick(0);
            }
        });
        // FS thread
        let h2 = thread::spawn(move || {
            for i in 0..20usize {
                let _ = k2.cache.fetch(i % 64, Duration::from_millis(1));
                let _ = k2.dispatch_syscall(SYS_WRITE, 1, 0, 10, 0, 0, 0);
                k2.cache.invalidate(i % 64);
            }
        });
        // Memory thread
        let h3 = thread::spawn(move || {
            for _ in 0..20 {
                let pages = k3.alloc_pages(2);
                let _ = k3.memory_pressure();
                k3.free_pages(&pages);
            }
        });
        // Process thread
        let h4 = thread::spawn(move || {
            for _ in 0..10 {
                let _ = k4.do_fork(root_id);
                k4.balance_load();
                let _ = k4.reclaim_zombies();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
        h4.join().unwrap();
    }, 5000);
    if !done { GKL.leave(); }
    assert!(done, "full scheduler+fs+memory+process chain deadlocked");
}

/// Test 6: exit + fork + wait concurrent (process lifecycle)
#[test]
fn deadlock_exit_fork_wait() {
    let kern = Arc::new(Kernel::new(64));
    kern.proc_init();
    let root = kern.tasks.root.lock().unwrap().clone().unwrap();
    let root_id = root.id();

    let k1 = kern.clone();
    let k2 = kern.clone();

    let done = run_with_timeout(move || {
        let h1 = thread::spawn(move || {
            for _ in 0..15 {
                if let Ok(child_id) = k1.do_fork(root_id) {
                    let _ = k1.dispatch_syscall(SYS_EXIT, child_id, 0, 0, 0, 0, 0);
                }
            }
        });
        let h2 = thread::spawn(move || {
            for _ in 0..15 {
                let _ = k2.do_fork(root_id);
                let _ = k2.dispatch_syscall(SYS_WAIT4, root_id, 0xFFFF_FFFF_FFFF_FFFF, 1, 0, 0, 0);
                k2.balance_load();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
    }, 5000);
    if !done { GKL.leave(); }
    assert!(done, "exit + fork + wait deadlocked");
}
