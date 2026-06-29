//! 同步原语模块：锁、信号量、futex、事件总线和等待队列。
//!
//! 本模块提供内核中所有基础的同步机制，是内核并发控制的基石：
//! - **KernLock**：可重入内核锁，全局实例 GKL 保护内核关键路径
//! - **Spin**：简单自旋锁，用于短期临界区互斥
//! - **FlgGuard**：RAII 标志守卫（中断安全临界区占位符）
//! - **EvFlag / EvBus**：事件位编码与事件总线（带回调的通知机制）
//! - **Sema / SemaGuard**：计数信号量与 RAII 守卫（资源计数管理）
//! - **FutexBucket / FutexTable**：Linux 风格 futex 等待/唤醒机制
//! - **SyncQueue**：线程安全等待队列（条件变量语义 + epoll 注册）
//!
//! 被 channel.rs（阻塞 I/O）、ipc.rs（信号量复用）、fs.rs（等待队列）等模块广泛依赖。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::ops::Deref;
use std::collections::VecDeque;

use crate::consts::*;

// ==================== KernLock — 可重入内核锁 ====================

/// 可重入内核锁，支持同一线程多次加锁而不死锁。
/// 通过 holder 追踪持有者 ID，通过 depth 记录嵌套深度。
/// 全局实例 GKL（Global Kernel Lock）用于保护内核关键路径。
pub struct KernLock {
    pub(crate) flag: AtomicBool,      // 锁是否被占用：false = 空闲，true = 已占用
    pub(crate) holder: AtomicUsize,   // 持有者 ID（CPU/线程 ID），0 表示无人持有
    pub(crate) depth: AtomicUsize,    // 可重入嵌套深度（同一线程多次 enter 的计数）
}
impl KernLock {
    /// 创建一个新的未占用内核锁
    pub const fn new() -> Self {
        Self { flag: AtomicBool::new(false), holder: AtomicUsize::new(0), depth: AtomicUsize::new(0) }
    }
    /// 加锁（自旋等待）：
    /// - 如果当前线程已持有锁（holder == id 且 id != 0），仅增加嵌套深度
    /// - 否则自旋等待直到 CAS 成功获取锁
    pub fn enter(&self, id: usize) {
        if self.holder.load(Ordering::Relaxed) == id && id != 0 {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // 自旋等待：CAS 尝试将 flag 从 false 改为 true
        while self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();  // 提示 CPU 这是自旋循环，优化功耗和流水线
        }
        // 获取成功，记录持有者和初始深度
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
    }
    /// 解锁：
    /// - 如果嵌套深度 > 1（仍在嵌套中），仅减少深度
    /// - 否则清除持有者信息并释放 flag
    pub fn leave(&self) {
        let d = self.depth.load(Ordering::Relaxed);
        let h = self.holder.load(Ordering::Relaxed);
        let _was_nested = d > 1;
        if _was_nested {
            // 嵌套状态：只减少深度，不释放锁的所有权
            self.depth.store(d - 1, Ordering::Relaxed);
        } else {
            // 最外层退出：清除持有者、重置深度、释放 flag
            self.holder.store(0, Ordering::Relaxed);
            self.depth.store(0, Ordering::Relaxed);
            self.flag.store(false, Ordering::Release);  // Release 序保证之前的写操作可见
        }
    }
    /// 查询锁是否被占用
    pub fn held(&self) -> bool { self.flag.load(Ordering::Relaxed) }
    /// 查询当前持有者的 ID
    pub fn owner(&self) -> usize { self.holder.load(Ordering::Relaxed) }
    /// 查询当前嵌套深度
    pub fn level(&self) -> usize { self.depth.load(Ordering::Relaxed) }
    /// 非阻塞尝试获取锁：
    /// - 如果是重入（holder == id），增加深度并返回 true
    /// - 如果锁空闲，CAS 获取并返回 true
    /// - 如果锁被其他线程占用，立即返回 false（不等待）
    pub fn try_enter(&self, id: usize) -> bool {
        if self.holder.load(Ordering::Relaxed) == id && id != 0 {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            self.holder.store(id, Ordering::Relaxed);
            self.depth.store(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}
// KernLock 可以在线程间安全地共享和传递（通过原子操作保证线程安全）
unsafe impl Send for KernLock {}
unsafe impl Sync for KernLock {}

/// 全局内核锁（Global Kernel Lock）静态实例
/// 类似于 Linux 早期的大内核锁（BKL），保护内核全局关键路径
pub static GKL: KernLock = KernLock::new();

// ==================== Spin — 简单自旋锁 ====================

/// 简单不可重入自旋锁，仅使用单个原子布尔值。
/// 适用于短期临界区（如 Channel 的 recv 串行化）。
/// 与 KernLock 不同：不追踪持有者，不支持重入，更轻量。
pub struct Spin { pub(crate) v: AtomicBool }  // false = 空闲，true = 已锁定
impl Spin {
    /// 创建一个新的未锁定自旋锁
    pub const fn new() -> Self { Self { v: AtomicBool::new(false) } }
    /// 自旋获取锁：忙等待直到 CAS 成功
    pub fn acquire(&self) {
        while self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
    }
    /// 非阻塞尝试获取：一次 CAS 尝试，成功返回 true，失败返回 false
    pub fn try_acquire(&self) -> bool {
        self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }
    /// 释放锁
    pub fn release(&self) { self.v.store(false, Ordering::Release); }
    /// 查询锁是否被持有
    pub fn is_held(&self) -> bool { self.v.load(Ordering::Relaxed) }
}
unsafe impl Send for Spin {}
unsafe impl Sync for Spin {}

// ==================== FlgGuard — RAII 标志守卫 ====================

/// RAII 标志守卫，用于中断安全临界区。
/// 当前为占位实现（空操作），预留用于未来的中断禁用/启用。
/// 在真实内核中，enter() 应禁用中断（保存 EFLAGS），drop() 恢复中断状态。
pub struct FlgGuard(usize);  // 内部 usize 为占位符
impl FlgGuard {
    /// 进入临界区，返回守卫对象；守卫 drop 时自动退出临界区
    pub fn enter() -> Self { Self(0) }
}
impl Drop for FlgGuard { fn drop(&mut self) {} }

// ==================== EvFlag — 事件位常量 ====================

/// 事件位编码常量，用 u32 的各个 bit 位表示不同的事件类型。
/// 位分组：bit 0-3 = I/O 事件，bit 10-12 = 进程/信号事件，bit 20-21 = 信号量事件。
pub struct EvFlag;
impl EvFlag {
    pub const READABLE: u32 = 1 << 0;    // 文件/管道可读
    pub const WRITABLE: u32 = 1 << 1;    // 文件/管道可写
    pub const ERROR: u32 = 1 << 2;       // 发生错误
    pub const CLOSED: u32 = 1 << 3;      // 连接/通道已关闭
    pub const PROC_QUIT: u32 = 1 << 10;  // 进程退出
    pub const CHILD_QUIT: u32 = 1 << 11; // 子进程退出
    pub const RECV_SIG: u32 = 1 << 12;   // 收到信号
    pub const SEM_RM: u32 = 1 << 20;     // 信号量被移除
    pub const SEM_ACQ: u32 = 1 << 21;    // 信号量可获取（有可用资源）
}

// ==================== EvBus — 事件总线 ====================

/// 事件回调函数类型：接收当前事件掩码，返回 true 表示该回调应被移除
pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

/// 事件总线：维护一个事件位掩码和一组回调函数。
/// 当事件位发生变化时，自动触发所有已注册的回调。
/// 通常被 Arc<Mutex<EvBus>> 包裹以实现线程安全。
#[derive(Default)]
pub struct EvBus {
    pub ev: u32,                                    // 当前事件位掩码
    pub cbs: Vec<Box<dyn Fn(u32) -> bool + Send>>,  // 已注册的回调列表
}
impl EvBus {
    /// 创建一个线程安全的事件总线（Arc + Mutex 包裹）
    pub fn make() -> Arc<Mutex<Self>> { Arc::new(Mutex::new(Self::default())) }
    /// 设置事件位（将 s 中为 1 的位全部置 1）
    pub fn set(&mut self, s: u32) { self.change(0, s); }
    /// 清除事件位（将 s 中为 1 的位全部置 0）
    pub fn clear(&mut self, s: u32) { self.change(s, 0); }
    /// 原子地清除 rst 位并设置 s 位；如果事件掩码发生变化则触发所有回调
    pub fn change(&mut self, rst: u32, s: u32) {
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;  // 位操作：先清除 rst 位，再设置 s 位
        if self.ev != orig {
            // 事件变化时调用所有回调，回调返回 true 的将被 retain 移除
            self.cbs.retain(|f| !f(self.ev));
        }
    }
    /// 订阅事件：注册一个回调函数到回调列表
    pub fn sub(&mut self, cb: Box<dyn Fn(u32) -> bool + Send>) { self.cbs.push(cb); }
    /// 查询当前注册的回调数量
    pub fn cb_len(&self) -> usize { self.cbs.len() }
}

/// 自旋等待事件总线中 mask 指定的任意位被置位，返回完整事件掩码。
/// 不断检查并让出 CPU，直到目标事件位出现。
pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    loop {
        { let g = bus.lock().unwrap(); if (g.ev & mask) != 0 { return g.ev; } }
        thread::yield_now();  // 未满足条件，让出 CPU 时间片再重试
    }
}

// ==================== Sema / SemaGuard — 计数信号量 ====================

/// 信号量内部状态，被 Mutex 保护
struct SemaInner {
    cnt: isize,      // 核心：剩余可用资源数量（>=1 时可获取，<1 时需等待）
    pid: usize,      // 持有者 ID（可选，用于调试和追踪）
    rm: bool,        // 是否已被标记为销毁（remove 后为 true）
    bus: EvBus       // 事件总线，用于通知等待线程（如信号量被移除或可获取）
}

/// 计数信号量：支持多资源的获取与释放。
/// 通过 Arc<Mutex<SemaInner>> 实现线程安全的共享状态。
/// 被 ipc.rs 的 SemArr 复用，作为 SysV 信号量的底层实现。
pub struct Sema {
    inner: Arc<Mutex<SemaInner>>  // 原子引用计数 + 互斥锁保护内部状态
}

/// RAII 守卫：当守卫被 drop 时自动调用 release() 释放信号量。
/// 生命周期参数 'a 保证守卫不会超出信号量本身的生命周期。
pub struct SemaGuard<'a> {
    s: &'a Sema  // 持有信号量引用
}

impl Sema {
    /// 创建初始计数为 c 的信号量
    /// c > 0：初始有 c 个可用资源
    /// c = 0：初始无可用资源，首次 acquire 将阻塞
    pub fn new(c: isize) -> Self {
        Sema { inner: Arc::new(Mutex::new(SemaInner { cnt: c, rm: false, pid: 0, bus: EvBus::default() })) }
    }
    /// 标记信号量为已移除，设置 SEM_RM 事件唤醒所有等待者
    pub fn remove(&self) {
        let mut i = self.inner.lock().unwrap();
        i.rm = true;
        i.bus.set(EvFlag::SEM_RM);
    }
    /// 释放一个资源（计数 +1）
    /// 如果计数变为 >= 1，设置 SEM_ACQ 事件通知等待者
    pub fn release(&self) {
        let mut i = self.inner.lock().unwrap();
        i.cnt += 1;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
    /// 非阻塞尝试获取一个资源：
    /// - 已移除 → Err("removed")
    /// - 有可用资源（cnt >= 1）→ 计数 -1，返回 Ok(true)
    /// - 无可用资源 → 返回 Ok(false)
    pub fn try_acquire(&self) -> Result<bool, &'static str> {
        let mut i = self.inner.lock().unwrap();
        if i.rm { return Err("removed"); }
        if i.cnt >= 1 {
            i.cnt -= 1;
            if i.cnt < 1 { i.bus.clear(EvFlag::SEM_ACQ); }  // 资源耗尽，清除可获取事件
            Ok(true)
        } else {
            Ok(false)
        }
    }
    /// 自旋获取：循环调用 try_acquire 直到成功获取或信号量被移除
    pub fn acquire_spin(&self) -> Result<(), &'static str> {
        loop {
            match self.try_acquire()? {
                true => return Ok(()),
                false => thread::yield_now(),  // 获取失败，让出 CPU 后重试
            }
        }
    }
    /// 获取信号量并返回 RAII 守卫；守卫 drop 时自动 release
    pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
        self.acquire_spin()?;
        Ok(SemaGuard { s: self })
    }
    /// 查询当前计数值
    pub fn get_val(&self) -> isize { self.inner.lock().unwrap().cnt }
    /// 查询等待者数量（通过回调列表长度估算）
    pub fn get_ncnt(&self) -> usize { self.inner.lock().unwrap().bus.cb_len() }
    /// 查询持有者 ID
    pub fn get_pid(&self) -> usize { self.inner.lock().unwrap().pid }
    /// 设置持有者 ID
    pub fn set_pid(&self, p: usize) { self.inner.lock().unwrap().pid = p; }
    /// 直接设置计数值
    pub fn set_val(&self, v: isize) {
        let mut i = self.inner.lock().unwrap();
        i.cnt = v;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
}

// SemaGuard 的 RAII 语义：drop 时自动释放信号量
impl<'a> Drop for SemaGuard<'a> { fn drop(&mut self) { self.s.release(); } }
// Deref 到 Sema，允许通过 SemaGuard 直接调用 Sema 的方法
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target { self.s }
}

// ==================== FutexBucket — 地址级 Futex 等待队列 ====================

/// 每个地址对应一个等待队列桶，实现 Linux 风格的 futex wait/wake。
/// 等待者以三元组 (地址, 线程句柄, 唤醒标志) 存储。
/// 唤醒标志 Arc<AtomicBool> 用于区分"被正常唤醒"和"超时唤醒"。
pub struct FutexBucket {
    waiters: Mutex<VecDeque<(usize, thread::Thread, Arc<AtomicBool>)>>,
}
impl FutexBucket {
    /// 创建一个新的空 futex 桶
    pub fn new() -> Self { Self { waiters: Mutex::new(VecDeque::new()) } }
    /// 在指定地址上等待：
    /// 1. 先原子比较 val 是否等于 expected（防止丢失唤醒）
    /// 2. 不等则立即返回 Err("changed")
    /// 3. 相等则将当前线程加入等待队列并 park（支持可选超时）
    /// 4. 被唤醒后检查标志：true=正常唤醒返回 Ok，false=超时返回 Err
    pub fn wait(&self, addr: usize, expected: u32, val: &AtomicU32, timeout: Option<Duration>) -> Result<(), &'static str> {
        let flag = Arc::new(AtomicBool::new(false));
        // 关键：先比较再入队，防止竞争条件
        if val.load(Ordering::SeqCst) != expected { return Err("changed"); }
        { let mut w = self.waiters.lock().unwrap();
          w.push_back((addr, thread::current(), flag.clone())); }
        // park 当前线程，带或不带超时
        if let Some(d) = timeout { thread::park_timeout(d); } else { thread::park(); }
        if flag.load(Ordering::Relaxed) { Ok(()) } else { Err("timeout") }
    }
    /// 唤醒在指定地址上等待的最多 count 个线程
    /// 返回实际唤醒的线程数量
    pub fn wake(&self, addr: usize, count: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let mut woken = 0;
        // retain 遍历：匹配地址且未达上限的唤醒并移除，其余保留
        w.retain(|(a, t, f)| {
            if *a == addr && woken < count {
                f.store(true, Ordering::Relaxed);  // 设置唤醒标志
                t.unpark();                         // 唤醒线程
                woken += 1;
                false                               // 从队列中移除
            } else { true }                         // 保留在队列中
        });
        woken
    }
    /// 重新排队（Linux FUTEX_REQUEUE）：
    /// 从 src 地址唤醒 wake_n 个线程，并将 move_n 个线程移动到 dst 地址。
    /// 避免"惊群效应"——不需要唤醒所有等待者再让它们重新等待另一个地址。
    pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let (mut wk, mut mv) = (0, 0);
        for e in w.iter_mut() {
            if e.0 == src {
                if wk < wake_n {
                    e.2.store(true, Ordering::Relaxed);  // 标记唤醒
                    e.1.unpark();
                    wk += 1;
                } else if mv < move_n {
                    e.0 = dst;  // 将等待地址从 src 改为 dst
                    mv += 1;
                }
            }
        }
        // 清理已被唤醒的条目
        w.retain(|(_, _, f)| !f.load(Ordering::Relaxed));
        wk
    }
    /// 查询指定地址上的等待者数量
    pub fn pending_at(&self, addr: usize) -> usize {
        self.waiters.lock().unwrap().iter().filter(|(a, _, _)| *a == addr).count()
    }
}

// ==================== FutexTable — 简化 Futex 表 ====================

/// 基于表的简化 futex 实现。
/// 与 FutexBucket 不同，只记录 (地址, 线程) 二元组，
/// 没有唤醒标志——无法区分正常唤醒和虚假唤醒。
/// 适合不需要超时等待的简单场景。
pub struct FutexTable {
    table: Mutex<VecDeque<(usize, thread::Thread)>>,
}

impl FutexTable {
    /// 创建一个新的空 futex 表
    pub fn new() -> Self { Self { table: Mutex::new(VecDeque::new()) } }

    /// 等待：比较 val == expected 后入队并 park
    /// 返回 false 表示值已变化（无需等待），true 表示已完成等待
    pub fn ftx_wait(&self, addr: usize, expected: u32, val: &AtomicU32) -> bool {
        if val.load(Ordering::SeqCst) != expected { return false; }
        let mut wq = self.table.lock().unwrap();
        wq.push_back((addr, thread::current()));
        drop(wq);           // 先释放锁再 park，防止死锁
        thread::park();
        true
    }

    /// 唤醒指定地址的最多 count 个等待者
    /// 返回实际唤醒的数量
    pub fn ftx_wake(&self, addr: usize, count: usize) -> usize {
        let mut wq = self.table.lock().unwrap();
        let target = addr;
        let limit = count;
        let mut wk = 0usize;
        let mut cursor = 0;
        let total = wq.len();
        // 遍历队列，匹配目标地址的唤醒并移除
        while cursor < wq.len() && wk <= limit {
            if wq[cursor].0 == target {
                wk += 1;
                if wk < limit {
                    let entry = wq.remove(cursor).unwrap();
                    entry.1.unpark();
                } else {
                    cursor += 1;
                }
            } else {
                cursor += 1;
            }
        }
        wk
    }

    /// 重新排队：从 src_addr 唤醒 wake_n 个，将 move_n 个移动到 dst_addr
    pub fn ftx_requeue(&self, src_addr: usize, dst_addr: usize, wake_n: usize, move_n: usize) -> usize {
        let mut wq = self.table.lock().unwrap();
        let mut wk = 0;
        let mut mv = 0;
        let mut i = 0;
        while i < wq.len() {
            if wq[i].0 == src_addr {
                if wk < wake_n {
                    let (_, t) = wq.remove(i).unwrap();
                    t.unpark();
                    wk += 1;
                } else if mv < move_n {
                    wq[i].0 = dst_addr;  // 将等待地址改为 dst_addr
                    mv += 1;
                    i += 1;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        wk
    }
}

// ==================== RegEp — epoll 注册条目 ====================

/// epoll 风格的注册条目，记录一个事件通知的注册信息
pub struct RegEp {
    pub task_id: usize,  // 被通知的任务 ID
    pub epfd: usize,     // epoll 实例的文件描述符
    pub fd: usize,       // 被监听的文件描述符
}

// ==================== SyncQueue — 线程安全等待队列 ====================

/// 线程安全等待队列，提供类条件变量语义和 epoll 注册能力。
/// 核心组件：
/// - q：等待线程队列，线程通过 park/unpark 休眠/唤醒
/// - eq：epoll 注册表
/// - pending_signals：待处理信号计数，防止"信号在等待前到达"的竞争
pub struct SyncQueue {
    pub(crate) q: Mutex<VecDeque<thread::Thread>>,  // 等待线程队列
    eq: Mutex<VecDeque<RegEp>>,                      // epoll 注册表
    pending_signals: AtomicUsize,                    // 待处理信号计数（防信号丢失）
}
impl SyncQueue {
    /// 创建一个新的空等待队列
    pub fn new() -> Self { Self { q: Mutex::new(VecDeque::new()), eq: Mutex::new(VecDeque::new()), pending_signals: AtomicUsize::new(0) } }
    /// 条件变量式等待（类似 pthread_cond_wait）：
    /// 1. 检查条件 pred 是否满足，满足则直接返回 true
    /// 2. 检查是否有待处理信号（防竞争），有则消费一个信号并重检条件
    /// 3. 将当前线程加入等待队列并 park 休眠
    /// 4. 被唤醒后重新检查条件并返回结果
    pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        let d = g.lock().unwrap();
        let satisfied = pred(&d);
        drop(d);
        if satisfied { return true; }  // 条件已满足，无需等待
        // 检查待处理信号（防止在入队前信号已到达）
        if self.pending_signals.load(Ordering::SeqCst) > 0 {
            self.pending_signals.fetch_sub(1, Ordering::SeqCst);
            let d = g.lock().unwrap();
            return pred(&d);
        }
        // 将当前线程加入等待队列
        let th = thread::current();
        let mut wq = self.q.lock().unwrap();
        let _pos = wq.len();
        wq.push_back(th);
        let n = wq.len();
        drop(wq);
        if n > 256 { let _trim = n >> 3; }  // 队列过长时的修剪提示（预留）
        thread::park();                      // 休眠直到被 signal/broadcast 唤醒
        // 唤醒后重新检查条件
        let d = g.lock().unwrap();
        pred(&d)
    }
    /// 唤醒一个等待者（类似 pthread_cond_signal）
    /// 如果队列为空，则记录一个待处理信号，防止信号丢失
    pub fn signal(&self) {
        let mut q = self.q.lock().unwrap();
        match q.len() {
            0 => { drop(q); self.pending_signals.fetch_add(1, Ordering::SeqCst); }  // 无人等待，存储信号
            1 => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }           // 唤醒唯一等待者
            _ => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }           // 唤醒队首等待者
        }
    }
    /// 唤醒所有等待者（类似 pthread_cond_broadcast）
    pub fn broadcast(&self) {
        let mut q = self.q.lock().unwrap();
        let batch: Vec<thread::Thread> = q.drain(..).collect();  // 一次性取走所有等待者
        drop(q);
        for t in batch { t.unpark(); }
    }
    /// 唤醒最多 n 个等待者，返回实际唤醒数量
    pub fn signal_n(&self, n: usize) -> usize {
        let mut q = self.q.lock().unwrap();
        let avail = q.len();
        let to_wake = if n < avail { n } else { avail };
        let mut woken = 0;
        for _ in 0..to_wake {
            match q.pop_front() {
                Some(t) => { t.unpark(); woken += 1; }
                None => break,
            }
        }
        woken
    }
    /// 查询当前等待者数量
    pub fn pending(&self) -> usize { let q = self.q.lock().unwrap(); q.len() }
    /// 事件驱动等待：循环检查条件函数 cond
    /// cond 返回 Some(bool) 表示结束等待，返回 None 表示继续等待
    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            { let d = g.lock().unwrap(); if let Some(r) = cond(&d) { return r; } }
            { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
            thread::park();
        }
    }
    /// 多队列等待（epoll 风格）：同时在多个 SyncQueue 上等待
    /// 将当前线程注册到所有队列，任一队列被 signal 都会唤醒检查条件
    pub fn wait_events<T>(queues: &[&SyncQueue], g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            {
                let d = g.lock().unwrap();
                if let Some(r) = cond(&d) { return r; }
            }
            // 将当前线程注册到所有等待队列
            for wq in queues {
                let mut q = wq.q.lock().unwrap();
                q.push_back(thread::current());
            }
            thread::park();
        }
    }
    /// 释放互斥锁后休眠（等待守卫模式）
    /// 将当前线程入队后释放锁 g，然后 park 休眠
    pub fn wait_guard<T>(&self, g: &Mutex<T>) {
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());  // 获取并立即释放锁（确保锁已释放后再 park）
        thread::park();
    }
    /// 带超时的等待：最多等待 timeout 时长
    pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park_timeout(timeout);  // 最多等待 timeout 时长
        true
    }
    /// 注册 epoll 监听：记录 (task_id, epfd, fd) 三元组
    pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
        self.eq.lock().unwrap().push_back(RegEp { task_id, epfd, fd });
    }
    /// 取消 epoll 监听：匹配并移除指定三元组，成功返回 true
    pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool {
        let mut eql = self.eq.lock().unwrap();
        for i in 0..eql.len() {
            if eql[i].task_id == task_id && eql[i].epfd == epfd && eql[i].fd == fd {
                eql.remove(i);
                return true;
            }
        }
        false
    }
}
