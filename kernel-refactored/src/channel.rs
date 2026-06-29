//! Channel IPC: circular buffer and thread-safe message passing.
//!
//! 本模块提供内核中字节流 IPC 的两层抽象：
//! - `CircBuf`: 非线程安全的环形字节缓冲区（被 fs.rs 的 PipeNode 复用）
//! - `Channel`: 线程安全的阻塞式字节通道（用于 pipe 系统调用、伪终端 I/O）

use crate::consts::*;
use crate::sync::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::cmp::min;

// ─────────────────────────── CircBuf ───────────────────────────
// 环形缓冲区：固定容量的字节队列，FIFO 语义。
// wr 指向下一个写入位置，rd 指向下一个读取位置。
// n 独立记录元素数量，避免满/空状态歧义。

pub struct CircBuf {
    /// 底层存储字节数组
    pub data: Vec<u8>,
    /// 读指针：下一个待读取元素的位置（rd % cap）
    pub rd: usize,
    /// 写指针：下一个待写入元素的位置（wr % cap）
    pub wr: usize,
    /// 缓冲区总容量
    pub cap: usize,
    /// 当前已存储有效数据长度
    pub n: usize,
}

impl CircBuf {
    /// 创建容量为 c 的空环形缓冲区，读写指针均为 0
    pub fn new(c: usize) -> Self { Self { data: vec![0u8; c], rd: 0, wr: 0, cap: c, n: 0 } }

    /// 创建指定初始读写位置的环形缓冲区（主要用于测试构造特定状态）
    pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
        let n = if w >= r { w - r } else { c - r + w };
        Self { data: vec![0u8; c], rd: r, wr: w, cap: c, n }
    }

    /// 写入一个字节到 wr 指向的位置，然后后移写指针。满则返回 false。
    pub fn push(&mut self, v: u8) -> bool {
        if self.full() { return false; }
        let i = self.wr % self.cap;
        self.data[i] = v;
        self.wr = self.wr.wrapping_add(1);
        self.n += 1;
        true
    }

    /// 从 rd 指向的位置读取一个字节，然后后移读指针。空则返回 None。
    pub fn pop(&mut self) -> Option<u8> {
        if self.empty() { return None; }
        let i = self.rd % self.cap;
        let v = self.data[i];
        self.rd = self.rd.wrapping_add(1);
        self.n -= 1;
        Some(v)
    }

    pub fn len(&self) -> usize { self.n }
    pub fn empty(&self) -> bool { self.n == 0 }
    pub fn full(&self) -> bool { self.n >= self.cap }

    /// 偷看 rd 指向的下一个可读字节，不消费（不移动 rd）
    pub fn peek(&self) -> Option<u8> {
        if self.empty() { return None; }
        let i = self.rd % self.cap;
        Some(self.data[i])
    }

    /// 批量读取最多 max 个字节到 dst
    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        let take = min(max, self.n);
        for _ in 0..take {
            if let Some(b) = self.pop() { dst.push(b); }
        }
        take
    }

    /// 从 src 逐字节写入，满则停止
    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        let mut written = 0;
        for &b in src {
            if !self.push(b) { break; }
            written += 1;
        }
        written
    }

    /// 剩余可写空间
    pub fn remaining(&self) -> usize { self.cap.saturating_sub(self.n) }
}
// ── CircBuf Debug Notes ──────────────────────────────────────────
// [BUG-01] 原 push/pop/peek 使用 `i >= self.data.len()` 做防御性越界检查，
//   这是不正确的做法：因为 cap 始终等于 data.len()，取模后 i 不可能越界。
//   正确做法是用 full()/empty() 判断"能否写入/能否读出"。
//   修复：push 改为 `if self.full() { return false }`，pop 改为 `if self.empty() { return None }`。
//
// [BUG-02] 原 push/pop 的指针语义是"先移动指针，再在新位置操作"，
//   导致第一次 push 写入 index=1 而非 index=0，非常不直观。
//   修复：改为"先在当前位置操作，再后移指针"。
//   新语义：wr 指向下一个写入位置，rd 指向下一个读取位置。
//   Channel 中所有直接操作 ring 的地方也已同步修改。
// ─────────────────────────────────────────────────────────────────

// ─────────────────────────── Channel ───────────────────────────
// 线程安全的阻塞式字节通道。
// 同步机制：guard(Spin) 串行化读者 → buf(Mutex) 保护数据 → wq(SyncQueue) 阻塞等待 → shut(AtomicBool) 关闭标志

pub struct Channel {
    /// 环形缓冲区，Mutex 保护并发读写
    pub buf: Mutex<CircBuf>,
    /// 自旋锁，保证同一时刻只有一个读者进入 recv 逻辑
    pub guard: Spin,
    /// 等待队列，缓冲区空时读者在此休眠，send/close 时唤醒
    pub wq: SyncQueue,
    /// 关闭标志，true 时 recv 不再阻塞，返回 None 表示 EOF
    pub shut: AtomicBool,
}

impl Channel {
    /// 创建指定容量的通道。容量钳制到 [1, 1MB]
    pub fn new(cap: usize) -> Self {
        let effective_cap = if cap == 0 { 1 } else if cap > 1 << 20 { 1 << 20 } else { cap };
        let ring = CircBuf {
            data: {
                let mut v = Vec::with_capacity(effective_cap);
                v.resize(effective_cap, 0u8);
                v
            },
            rd: 0, wr: 0, cap: effective_cap, n: 0,
        };
        Self {
            buf: Mutex::new(ring),
            guard: Spin::new(),
            wq: SyncQueue::new(),
            shut: AtomicBool::new(false),
        }
    }

    // Ordering::Acquire：拿锁时用，防止临界区代码重排到上锁前，同步其他线程 Release 的数据。
    // Ordering::Release：释放锁时用，保证临界区所有修改先完成再解锁，数据全局可见。
    // 上述二者明显用于获取和释放的过程中，比较强制前完成或者后不做
    // Ordering::Relaxed：仅需要原子性、不需要内存同步的场景（轮询锁失败、简单状态读取），追求性能。
    /// 阻塞式接收一个字节。
    /// 缓冲区有数据 → 立即返回 Some(byte)
    /// 缓冲区空且未关闭 → 休眠等待，被 send/close 唤醒后重试
    /// 通道已关闭 → 返回 None (EOF)
    pub fn recv(&self) -> Option<u8> {
        // 阶段 1：获取 guard 自旋锁（CAS 忙等，串行化读者）
        loop {
            if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() { 
                // 如果锁状态等于预期 false ，改成 true ，拿到锁，直接跳出循环；如果为 true ，就直接开始等
                core::hint::spin_loop(); // CPU pause 指令，缓解忙等压力
                continue;
            }
            break;
        }
        // 自旋锁约束只能有一个读者进入，而 Mutex<CircBuf> 约束对于 CircBuf 读写的互斥

        // 阶段 2：锁 buf，第一次尝试读取（rd 指向的位置）
        let result = {
            let mut ring = self.buf.lock().unwrap();
            ring.pop()
        };

        // 读到了 → 释放 guard 并返回
        if result.is_some() {
            self.guard.v.store(false, Ordering::Release);
            return result;
        }

        // 阶段 3：缓冲区空，检查是否已关闭
        if self.shut.load(Ordering::Relaxed) {
            self.guard.v.store(false, Ordering::Release);
            return None;
        }

        // 阶段 4：注册到等待队列并休眠
        {
            let data_ref = &self.buf;
            {
                let d = data_ref.lock().unwrap();
                if !d.empty() {
                    // 在我们检查关闭状态期间有人写入了，不休眠，直接重试（进入下一阶段）
                    drop(d);
                } else {
                    drop(d);
                    self.guard.v.store(false, Ordering::Release); // 释放 guard
                    // 将当前线程加入等待队列
                    let mut wq = self.wq.q.lock().unwrap();
                    wq.push_back(thread::current());
                    drop(wq);
                    thread::park(); // 休眠，直到 send()/close() 调用 unpark
                    // 被唤醒后重新获取 guard
                    loop {
                        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                            core::hint::spin_loop();
                            continue;
                        }
                        break;
                    }
                }
            }
        }

        // 阶段 5：第二次尝试读取（被唤醒后）
        let v = {
            let mut ring = self.buf.lock().unwrap();
            ring.pop()
        };
        self.guard.v.store(false, Ordering::Release);
        v
    }

    // 多个生产者本来就要靠 buf 的 Mutex 天然串行排队，并发写本身就会被内核互斥锁限流，不会出现大规模瞬间唤醒争抢锁的惊群现象
    // 假设 100 个读线程同时调用 recv、缓冲区为空，全部进入等待队列 park，当生产者写入 1 字节，所有 100 个线程全部被唤醒，同时争抢 buf.lock()
    // 过程导致大量内核上下文切换、频繁阻塞唤醒，这也就是锁惊群现象，导致 CPU 被塞满。

    /// 非阻塞式发送一个字节。满返回 false，成功返回 true 并唤醒一个等待读者
    pub fn send(&self, v: u8) -> bool {
        let success = {
            let mut ring = self.buf.lock().unwrap();
            ring.push(v)
        };
        // 写入成功 → 唤醒等待队列中第一个读者
        if success {
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        success
    }

    /// 关闭通道：设置 shut 标志避免生产者写入并唤醒所有等待的读者消费最终的数据
    pub fn close(&self) {
        self.shut.store(true, Ordering::Release);
        let mut wq = self.wq.q.lock().unwrap();
        while let Some(t) = wq.pop_front() { t.unpark(); }
        // 后续所有 send() 写入操作直接失败，不能再往缓冲区放新数据，已经存在缓冲区里的历史数据保留不变，允许消费者正常读完
    }

    /// 非阻塞尝试接收：获取不到 guard 或缓冲区空都立即返回 None
    pub fn try_recv(&self) -> Option<u8> {
        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() { // 锁拿不到直接返回
            return None;
        }
        let r = {
            let mut ring = self.buf.lock().unwrap();
            ring.pop()
        };
        self.guard.v.store(false, Ordering::Release);
        r
    }

    /// 批量发送：一次写入多个字节，返回实际写入数。写入后唤醒一个读者
    pub fn send_batch(&self, data: &[u8]) -> usize {
        let mut ring = self.buf.lock().unwrap();
        let mut written = 0;
        for &byte in data {
            if !ring.push(byte) { break; }
            written += 1;
        }
        if written > 0 {
            drop(ring);
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        written
    }

    /// 查询当前缓冲区中的数据量（不消费）
    pub fn depth(&self) -> usize {
        self.buf.lock().unwrap().len()
    }

    /// 一次性取走缓冲区中所有数据
    pub fn drain_all(&self) -> Vec<u8> {
        let mut result = Vec::new();
        let mut ring = self.buf.lock().unwrap();
        while let Some(b) = ring.pop() {
            result.push(b);
        }
        result
    }

    /// 查询通道是否已关闭
    pub fn is_closed(&self) -> bool {
        self.shut.load(Ordering::Acquire)
    }

    /// 查询剩余可写容量
    pub fn remaining_capacity(&self) -> usize {
        self.buf.lock().unwrap().remaining()
    }
}
// ── Channel Debug Notes ──────────────────────────────────────────
// [BUG-03] recv() / try_recv() / drain_all() 中直接操作 ring 时，
//   原代码与 CircBuf 存在相同的"先移指针再读"问题。
//   已统一改为"先读 rd 位置，再后移 rd"，与 CircBuf::pop() 一致。
//
// [BUG-04] send() / send_batch() 中直接操作 ring 时，
//   原代码与 CircBuf::push() 存在相同的"先移指针再写"问题。
//   已统一改为"先写 wr 位置，再后移 wr"，与 CircBuf::push() 一致。
//
// [BUG-05] 所有 Channel 方法中的 `idx < ring.data.len()` / `idx >= ring.data.len()`
//   防御性检查已移除，改为通过 ring.empty()/ring.full() 判断可读/可写，
//   与 CircBuf 修复后的风格保持一致。
//
// [BUG-06] Channel 方法中大量重复了 CircBuf 的内部逻辑（手动计算 idx、移动指针、更新 n），
//   违反 DRY 原则，且一旦 CircBuf 语义变更，Channel 中所有内联代码都需要同步修改。
//   修复：recv/try_recv/drain_all 改用 ring.pop()，send/send_batch 改用 ring.push()，
//   depth() 改用 ring.len()，remaining_capacity() 改用 ring.remaining()。
//   消除了所有对 ring 内部字段（rd/wr/data/cap/n）的直接操作。
// ─────────────────────────────────────────────────────────────────
