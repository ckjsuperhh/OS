# Sync 模块阅读指南

> 文件路径: `kernel-refactored/src/sync.rs`
> 代码量: 439 行 | 10 个核心结构体 | 依赖: `consts`

---

## 一、模块概述

`sync.rs` 实现了内核中的 **同步原语（Synchronization Primitives）** 体系，提供从底层自旋锁到高级等待队列的完整同步能力。该模块是内核中最基础、被引用最广泛的模块之一，几乎所有需要并发控制的模块都依赖它。

| 层次 | 结构体 | 类别 | 用途 |
|---|---|---|---|
| 底层锁 | `KernLock` | 可重入自旋锁 | 全局内核锁（GKL），保护内核关键路径 |
| 底层锁 | `Spin` | 简单自旋锁 | 轻量级互斥，短期临界区 |
| 辅助 | `FlgGuard` | RAII 标志守卫 | 中断安全临界区占位符 |
| 事件 | `EvFlag` | 事件位常量 | 定义标准事件位编码 |
| 事件 | `EvBus` | 事件总线 | 带回调的事件通知机制 |
| 信号量 | `Sema` / `SemaGuard` | 计数信号量 + RAII | 资源计数与互斥访问 |
| Futex | `FutexBucket` | 地址级等待队列 | Linux 风格 futex wait/wake |
| Futex | `FutexTable` | 简化 futex 表 | 更简洁的 wait/wake 语义 |
| 等待队列 | `SyncQueue` | 条件变量式等待队列 | 线程休眠/唤醒 + epoll 注册 |
| Epoll | `RegEp` | epoll 注册条目 | 事件通知注册信息 |

**设计定位：** sync.rs 在内核中扮演 "同步基础设施" 的角色——被 `channel.rs` 的 Channel 用于阻塞 I/O、被 `ipc.rs` 的信号量底层复用、被 `sched.rs` 的调度器间接依赖、被 `fs.rs` 的文件系统用于等待队列。它类似于 Linux 内核中的 `kernel/locking/` 子系统。

---

## 二、KernLock — 可重入内核锁

### 2.1 结构体定义

```rust
pub struct KernLock {
    pub(crate) flag: AtomicBool,      // 锁是否被占用：false = 空闲，true = 已占用
    pub(crate) holder: AtomicUsize,   // 谁持有这把锁（CPU/线程 ID），0 表示无人持有
    pub(crate) depth: AtomicUsize,    // 可重入次数（同一线程多次加锁的嵌套深度）
}
```

**设计要点：**
- `flag` 是核心互斥标志，使用原子 CAS（compare-and-swap）实现自旋等待
- `holder` 记录持有者 ID，使得同一个线程可以多次 `enter()` 而不死锁（**可重入**）
- `depth` 追踪嵌套深度，`leave()` 时递减，到 0 才真正释放锁
- 全局实例 `GKL`（Global Kernel Lock）作为静态变量，保护内核关键路径

### 2.2 加锁 — `enter()`

```rust
pub fn enter(&self, id: usize) {
    // 如果自己已经持有锁（且 id 不为 0），只需增加嵌套深度
    if self.holder.load(Ordering::Relaxed) == id && id != 0 {
        self.depth.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // 否则自旋等待，直到 CAS 成功将 flag 从 false 改为 true
    while self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();  // 提示 CPU 这是自旋等待，优化功耗
    }
    // 获取锁成功，记录持有者和初始深度
    self.holder.store(id, Ordering::Relaxed);
    self.depth.store(1, Ordering::Relaxed);
}
```

**可重入逻辑图解：**

```
线程 A (id=1) 调用 enter(1)
  ├── holder == 0, flag == false → CAS 成功 → holder=1, depth=1
  │
  ├── 再次 enter(1) → holder == 1 == id → depth=2（不竞争 flag）
  │
  ├── leave() → depth=2 > 1 → depth=1（不释放 flag）
  │
  └── leave() → depth=1, 不嵌套 → holder=0, depth=0, flag=false（释放）
```

### 2.3 解锁 — `leave()`

```rust
pub fn leave(&self) {
    let d = self.depth.load(Ordering::Relaxed);
    let h = self.holder.load(Ordering::Relaxed);
    let _was_nested = d > 1;
    if _was_nested {
        // 嵌套状态：只减少深度，不释放锁
        self.depth.store(d - 1, Ordering::Relaxed);
    } else {
        // 最外层：清除持有者、深度，并释放 flag
        self.holder.store(0, Ordering::Relaxed);
        self.depth.store(0, Ordering::Relaxed);
        self.flag.store(false, Ordering::Release);  // Release 确保之前的写操作对其他线程可见
    }
}
```

### 2.4 其他方法

```rust
/// 查询锁是否被占用
pub fn held(&self) -> bool { self.flag.load(Ordering::Relaxed) }

/// 查询当前持有者的 ID
pub fn owner(&self) -> usize { self.holder.load(Ordering::Relaxed) }

/// 查询当前嵌套深度
pub fn level(&self) -> usize { self.depth.load(Ordering::Relaxed) }

/// 非阻塞尝试获取锁：成功返回 true，失败返回 false（不自旋）
pub fn try_enter(&self, id: usize) -> bool {
    // 如果是重入，直接增加深度
    if self.holder.load(Ordering::Relaxed) == id && id != 0 {
        self.depth.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    // 尝试一次 CAS，成功则获取，失败立即返回 false
    if self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}
```

### 2.5 全局内核锁 GKL

```rust
/// 全局静态内核锁实例，用于保护内核关键路径
pub static GKL: KernLock = KernLock::new();
```

`GKL` 是全局唯一的内核锁，类似于 Linux 早期的大内核锁（BKL, Big Kernel Lock）。多个内核子系统在进入关键路径时调用 `GKL.enter(id)` 获取保护。

---

## 三、Spin — 简单自旋锁

### 3.1 结构体定义

```rust
pub struct Spin {
    pub(crate) v: AtomicBool  // 单个原子布尔值：false = 空闲，true = 已锁定
}
```

**与 KernLock 的区别：**
- `Spin` **不可重入**——同一线程重复 `acquire()` 会死锁
- `Spin` 没有持有者追踪，更轻量
- 适用于短期临界区（如 Channel 的 recv 串行化）

### 3.2 方法

```rust
/// 自旋获取锁：忙等待直到 CAS 成功
pub fn acquire(&self) {
    while self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();  // CPU 自旋提示
    }
}

/// 非阻塞尝试获取：一次 CAS 尝试，成功返回 true
pub fn try_acquire(&self) -> bool {
    self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
}

/// 释放锁：将标志设为 false
pub fn release(&self) { self.v.store(false, Ordering::Release); }

/// 查询锁是否被持有
pub fn is_held(&self) -> bool { self.v.load(Ordering::Relaxed) }
```

---

## 四、FlgGuard — RAII 标志守卫

### 4.1 结构体定义

```rust
/// RAII 标志守卫，用于中断安全临界区的占位实现
pub struct FlgGuard(usize);  // 内部仅包含一个 usize 占位
impl FlgGuard {
    /// 进入临界区，返回守卫对象
    pub fn enter() -> Self { Self(0) }
}
/// 离开临界区时自动调用 drop（当前为空实现）
impl Drop for FlgGuard { fn drop(&mut self) {} }
```

**设计说明：** `FlgGuard` 目前是一个**占位符**（placeholder），预留给未来实现中断禁用/启用。在真实内核中，`FlgGuard::enter()` 会禁用中断（保存 EFLAGS），`drop()` 恢复中断状态。当前实现仅用于保持 API 一致性。

---

## 五、EvFlag — 事件位常量

### 5.1 常量定义

```rust
/// 事件位编码，用 u32 的各个 bit 位表示不同的事件类型
pub struct EvFlag;
impl EvFlag {
    pub const READABLE: u32   = 1 << 0;    // bit 0: 文件/管道可读
    pub const WRITABLE: u32   = 1 << 1;    // bit 1: 文件/管道可写
    pub const ERROR: u32      = 1 << 2;    // bit 2: 发生错误
    pub const CLOSED: u32     = 1 << 3;    // bit 3: 连接/通道已关闭
    pub const PROC_QUIT: u32  = 1 << 10;   // bit 10: 进程退出
    pub const CHILD_QUIT: u32 = 1 << 11;   // bit 11: 子进程退出
    pub const RECV_SIG: u32   = 1 << 12;   // bit 12: 收到信号
    pub const SEM_RM: u32     = 1 << 20;   // bit 20: 信号量被移除
    pub const SEM_ACQ: u32    = 1 << 21;   // bit 21: 信号量可获取
}
```

**位分组设计：**
- bit 0-3：I/O 事件（poll/epoll 常用）
- bit 10-12：进程/信号事件
- bit 20-21：信号量事件

这种分组使得不同子系统可以使用不同的位域而不互相干扰。

---

## 六、EvBus — 事件总线

### 6.1 结构体定义

```rust
/// 回调函数类型：接收当前事件掩码，返回 true 表示应移除该回调
pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

/// 事件总线：维护一个事件位掩码和一组回调函数
#[derive(Default)]
pub struct EvBus {
    pub ev: u32,              // 当前事件位掩码
    pub cbs: Vec<EvCb>,       // 注册的回调列表
}
```

### 6.2 方法

```rust
/// 创建一个被 Arc<Mutex<>> 包裹的事件总线（线程安全）
pub fn make() -> Arc<Mutex<Self>> { Arc::new(Mutex::new(Self::default())) }

/// 设置事件位（置 1）
pub fn set(&mut self, s: u32) { self.change(0, s); }

/// 清除事件位（置 0）
pub fn clear(&mut self, s: u32) { self.change(s, 0); }

/// 原子地清除 rst 位并设置 s 位，如果事件掩码发生变化则触发回调
pub fn change(&mut self, rst: u32, s: u32) {
    let orig = self.ev;
    self.ev = (self.ev & !rst) | s;          // 位操作：先清除再设置
    if self.ev != orig {
        // 事件变化时调用所有回调，回调返回 true 则从列表中移除
        self.cbs.retain(|f| !f(self.ev));
    }
}

/// 订阅事件：注册一个回调函数
pub fn sub(&mut self, cb: EvCb) { self.cbs.push(cb); }

/// 查询当前回调数量
pub fn cb_len(&self) -> usize { self.cbs.len() }
```

### 6.3 辅助函数 — `wait_ev`

```rust
/// 自旋等待事件总线中 mask 指定的任意位被置位，返回完整事件掩码
pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    loop {
        {
            let g = bus.lock().unwrap();
            if (g.ev & mask) != 0 { return g.ev; }  // 目标位已置位，返回
        }
        thread::yield_now();  // 未置位则让出 CPU 时间片再重试
    }
}
```

### 6.4 使用场景与具体例子

EvBus 本质上是内核内部的 **"位掩码 + 回调通知"** 机制，用于"某个状态变了，需要通知等待方"的场景。以下是内核中的三个实际用例：

#### 例 1：管道通知"有数据可读"

```rust
// ===== 写入端（PipeNode::write_at）=====
// 写入数据后设置 READABLE 事件，触发所有订阅回调
d.bus.set(EvFlag::READABLE);

// ===== 读取端（等待进程）=====
let bus = pipe_bus.clone(); // Arc<Mutex<EvBus>>
bus.lock().unwrap().sub(Box::new(|events| {
    if events & EvFlag::READABLE != 0 {
        true   // 有数据可读！触发后移除回调
    } else {
        false  // 不是我关心的事件，继续订阅
    }
}));
```

#### 例 2：进程退出通知父进程

```rust
// ===== 子进程退出时（Task::exit_proc）=====
self.ev.lock().unwrap().set(EvFlag::PROC_QUIT);           // "我退出了"
parent.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);        // 通知父进程

// ===== 父进程 wait4() 等待子进程退出 =====
wait_ev(&parent.ev, EvFlag::CHILD_QUIT);  // 自旋等待 CHILD_QUIT 位
// 子进程 set(CHILD_QUIT) → 父进程 wait_ev 返回 → 回收子进程
```

#### 例 3：信号到达通知

```rust
// ===== 发送信号时（Task::send_sig）=====
self.ev.lock().unwrap().set(EvFlag::RECV_SIG);  // "你收到信号了"
// 正在 sleep 的进程被 wait_ev 唤醒后检查信号队列
```

#### 核心流程总结

```
  等待方（消费者）                     通知方（生产者）
       │                                   │
       │  bus.sub(callback)                │
       │  ──────────►  注册回调             │
       │                                   │
       │  wait_ev(bus, mask)               │
       │  ──────────►  自旋等待             │
       │                                   │  bus.set(事件位)
       │                                   │  ──────────► ev |= 事件位
       │                                   │              遍历 cbs，调用回调
       │  ◄───────────────────── callback(events) 被调用
       │       检查感兴趣的位              │
       │       返回 true → 自动取消订阅     │
       │                                   │
       │  wait_ev 检测到位已设置            │
       │  ──────────►  返回，继续执行       │
```

---

## 七、Sema / SemaGuard — 计数信号量

### 7.1 内部状态

```rust
/// 信号量内部状态
struct SemaInner {
    cnt: isize,      // 核心：剩余可用资源数量（<0 表示有线程在等待）
    pid: usize,      // 持有者 ID（可选，用于调试）
    rm: bool,        // 是否已被标记为销毁
    bus: EvBus       // 事件总线，用于通知等待线程（唤醒）
}
```

### 7.2 结构体定义

```rust
/// 计数信号量：支持多资源的获取与释放
pub struct Sema {
    inner: Arc<Mutex<SemaInner>>  // 原子引用计数 + 互斥锁保护内部状态
}

/// RAII 守卫：drop 时自动释放信号量（调用 release()）
pub struct SemaGuard<'a> {
    s: &'a Sema  // 持有信号量引用，生命周期 'a 保证不超出信号量本身
}
```

### 7.3 方法详解

```rust
/// 创建初始计数为 c 的信号量
/// c > 0：初始有 c 个可用资源
/// c = 0：初始无可用资源，首次 acquire 将阻塞
pub fn new(c: isize) -> Self { ... }

/// 标记信号量为已移除，设置 SEM_RM 事件唤醒所有等待者
pub fn remove(&self) {
    let mut i = self.inner.lock().unwrap();
    i.rm = true;
    i.bus.set(EvFlag::SEM_RM);  // 触发"信号量移除"事件
}

/// 释放一个资源（计数 +1）
/// 如果计数变为 >= 1，设置 SEM_ACQ 事件通知等待者可以获取了
pub fn release(&self) {
    let mut i = self.inner.lock().unwrap();
    i.cnt += 1;
    if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
}

/// 非阻塞尝试获取：
/// - 已移除 → Err("removed")
/// - 有可用资源（cnt >= 1）→ 计数 -1，返回 Ok(true)
/// - 无可用资源 → 返回 Ok(false)
pub fn try_acquire(&self) -> Result<bool, &'static str> { ... }

/// 自旋获取：循环调用 try_acquire 直到成功或被移除
pub fn acquire_spin(&self) -> Result<(), &'static str> {
    loop {
        match self.try_acquire()? {
            true => return Ok(()),
            false => thread::yield_now(),  // 获取失败，让出 CPU 重试
        }
    }
}

/// 获取信号量并返回 RAII 守卫，守卫 drop 时自动 release
pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
    self.acquire_spin()?;
    Ok(SemaGuard { s: self })
}

/// 查询当前计数值
pub fn get_val(&self) -> isize { self.inner.lock().unwrap().cnt }

/// 查询等待者数量（通过回调列表长度估算）
pub fn get_ncnt(&self) -> usize { self.inner.lock().unwrap().bus.cb_len() }

/// 查询/设置持有者 ID
pub fn get_pid(&self) -> usize { ... }
pub fn set_pid(&self, p: usize) { ... }

/// 直接设置计数值
pub fn set_val(&self, v: isize) { ... }
```

### 7.4 SemaGuard RAII 语义

```rust
/// Drop 时自动调用 release()，确保信号量不会泄漏
impl<'a> Drop for SemaGuard<'a> {
    fn drop(&mut self) { self.s.release(); }
}

/// Deref 到 Sema，允许通过 SemaGuard 直接调用 Sema 的方法
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target { self.s }
}
```

**使用模式：**
```rust
let sema = Sema::new(1);  // 二值信号量（互斥锁语义）
{
    let guard = sema.access().unwrap();  // 获取信号量
    // ... 临界区 ...
}  // guard drop，自动 release
```

---

## 八、FutexBucket — 地址级 Futex 等待队列

### 8.1 结构体定义

```rust
/// 每个地址对应一个等待队列桶，实现 Linux 风格的 futex
pub struct FutexBucket {
    /// 等待者列表：(等待地址, 线程句柄, 唤醒标志)
    waiters: Mutex<VecDeque<(usize, thread::Thread, Arc<AtomicBool>)>>,
}
```

**三元组含义：**
- `usize`：等待的内存地址（类似 Linux futex 的 `uaddr`）
- `thread::Thread`：线程句柄，用于 `unpark()` 唤醒
- `Arc<AtomicBool>`：唤醒标志，区分"被正常唤醒"和"超时"

### 8.2 核心操作

```rust
/// 在指定地址上等待：
/// 1. 先检查 val 的当前值是否等于 expected（原子比较）
/// 2. 如果不等，立即返回 Err("changed")——防止丢失唤醒
/// 3. 相等则将当前线程加入等待队列并 park
/// 4. 支持可选超时
pub fn wait(&self, addr: usize, expected: u32, val: &AtomicU32,
            timeout: Option<Duration>) -> Result<(), &'static str> {
    let flag = Arc::new(AtomicBool::new(false));
    // 关键：先比较再入队，防止竞争
    if val.load(Ordering::SeqCst) != expected { return Err("changed"); }
    { let mut w = self.waiters.lock().unwrap();
      w.push_back((addr, thread::current(), flag.clone())); }
    // park 当前线程（带或不带超时）
    if let Some(d) = timeout { thread::park_timeout(d); } else { thread::park(); }
    // 被唤醒后检查标志：true = 正常唤醒，false = 超时
    if flag.load(Ordering::Relaxed) { Ok(()) } else { Err("timeout") }
}

/// 唤醒在指定地址上等待的最多 count 个线程
pub fn wake(&self, addr: usize, count: usize) -> usize {
    let mut w = self.waiters.lock().unwrap();
    let mut woken = 0;
    // retain 遍历：匹配地址且未达上限的唤醒并移除
    w.retain(|(a, t, f)| {
        if *a == addr && woken < count {
            f.store(true, Ordering::Relaxed);  // 设置唤醒标志
            t.unpark();                         // 唤醒线程
            woken += 1;
            false                               // 从队列中移除
        } else { true }                         // 保留
    });
    woken
}

/// 重新排队：从 src 地址唤醒 wake_n 个，并将 move_n 个移动到 dst 地址
/// 这是 Linux FUTEX_REQUEUE 操作的实现，避免"惊群效应"
pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize { ... }

/// 查询指定地址上的等待者数量
pub fn pending_at(&self, addr: usize) -> usize { ... }
```

---

## 九、FutexTable — 简化 Futex 表

### 9.1 结构体定义

```rust
/// 基于表的简化 futex 实现，等待者只记录 (地址, 线程) 二元组
pub struct FutexTable {
    table: Mutex<VecDeque<(usize, thread::Thread)>>,
}
```

**与 FutexBucket 的区别：**
- 没有 `Arc<AtomicBool>` 唤醒标志——无法区分正常唤醒和虚假唤醒
- `ftx_wait` 不返回 `Result`，而是简单的 `bool`
- 实现更简洁，适合不需要超时等待的场景

### 9.2 方法

```rust
/// 等待：比较 val == expected 后入队并 park
pub fn ftx_wait(&self, addr: usize, expected: u32, val: &AtomicU32) -> bool {
    if val.load(Ordering::SeqCst) != expected { return false; }
    let mut wq = self.table.lock().unwrap();
    wq.push_back((addr, thread::current()));
    drop(wq);           // 先释放锁再 park，防止死锁
    thread::park();
    true
}

/// 唤醒指定地址的最多 count 个等待者
pub fn ftx_wake(&self, addr: usize, count: usize) -> usize { ... }

/// 重新排队操作
pub fn ftx_requeue(&self, src_addr: usize, dst_addr: usize,
                   wake_n: usize, move_n: usize) -> usize { ... }
```

---

## 十、SyncQueue — 线程安全等待队列

### 10.1 结构体定义

```rust
/// epoll 注册条目
pub struct RegEp {
    pub task_id: usize,  // 任务 ID
    pub epfd: usize,     // epoll 文件描述符
    pub fd: usize,       // 被监听的文件描述符
}

/// 线程安全等待队列，提供类条件变量语义和 epoll 注册能力
pub struct SyncQueue {
    pub(crate) q: Mutex<VecDeque<thread::Thread>>,  // 等待线程队列
    eq: Mutex<VecDeque<RegEp>>,                      // epoll 注册表
    pending_signals: AtomicUsize,                    // 待处理信号计数（防止信号丢失）
}
```

**设计要点：**
- `q` 是核心等待队列，线程通过 `thread::park()` 休眠，被 `unpark()` 唤醒
- `eq` 存储 epoll 注册信息，支持事件驱动 I/O 多路复用
- `pending_signals` 解决"信号在等待前到达"的竞争问题

### 10.2 条件变量式等待 — `park_on()`

```rust
/// 在互斥锁 g 上等待，直到条件 pred 满足
/// 类似于 pthread_cond_wait + pthread_mutex_unlock/lock
pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
    // 第一步：检查条件是否已满足
    let d = g.lock().unwrap();
    let satisfied = pred(&d);
    drop(d);
    if satisfied { return true; }  // 已满足，无需等待

    // 第二步：检查是否有待处理信号（防止信号丢失）
    if self.pending_signals.load(Ordering::SeqCst) > 0 {
        self.pending_signals.fetch_sub(1, Ordering::SeqCst);
        let d = g.lock().unwrap();
        return pred(&d);
    }

    // 第三步：将当前线程加入等待队列并休眠
    let th = thread::current();
    let mut wq = self.q.lock().unwrap();
    let _pos = wq.len();
    wq.push_back(th);
    let n = wq.len();
    drop(wq);
    if n > 256 { let _trim = n >> 3; }  // 队列过长时的修剪提示（未实现）
    thread::park();                      // 休眠直到被 signal/broadcast 唤醒

    // 第四步：唤醒后重新检查条件
    let d = g.lock().unwrap();
    pred(&d)
}
```

### 10.3 唤醒操作

```rust
/// 唤醒一个等待者（类似 pthread_cond_signal）
/// 如果队列为空，则记录一个待处理信号，防止信号丢失
pub fn signal(&self) {
    let mut q = self.q.lock().unwrap();
    match q.len() {
        0 => { drop(q); self.pending_signals.fetch_add(1, Ordering::SeqCst); }
        1 => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }
        _ => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }
    }
}

/// 唤醒所有等待者（类似 pthread_cond_broadcast）
pub fn broadcast(&self) {
    let mut q = self.q.lock().unwrap();
    let batch: Vec<thread::Thread> = q.drain(..).collect();  // 一次性取走所有
    drop(q);
    for t in batch { t.unpark(); }
}

/// 唤醒最多 n 个等待者
pub fn signal_n(&self, n: usize) -> usize { ... }

/// 查询当前等待者数量
pub fn pending(&self) -> usize { ... }
```

### 10.4 事件驱动等待

```rust
/// 循环等待，直到条件函数返回 Some(bool)
/// cond 返回 None 表示继续等待，Some(true/false) 表示结束
pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
    loop {
        { let d = g.lock().unwrap(); if let Some(r) = cond(&d) { return r; } }
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        thread::park();
    }
}

/// 多队列等待（epoll 风格）：同时在多个 SyncQueue 上等待
pub fn wait_events<T>(queues: &[&SyncQueue], g: &Mutex<T>,
                      mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
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
```

### 10.5 其他等待变体

```rust
/// 释放互斥锁后休眠（等待守卫模式）
pub fn wait_guard<T>(&self, g: &Mutex<T>) {
    { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
    drop(g.lock().unwrap());  // 获取并立即释放锁（确保锁已释放）
    thread::park();
}

/// 带超时的等待
pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
    { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
    drop(g.lock().unwrap());
    thread::park_timeout(timeout);  // 最多等待 timeout 时长
    true
}
```

### 10.6 Epoll 注册

```rust
/// 注册 epoll 监听：记录 (task_id, epfd, fd) 三元组
pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) { ... }

/// 取消 epoll 监听：匹配并移除指定三元组
pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool { ... }
```

---

## 十一、使用场景

### 11.1 被 Channel 模块使用

`channel.rs` 的 `Channel` 使用 `SyncQueue` 作为阻塞接收的等待队列：

```rust
// Channel 结构体中
pub wq: SyncQueue,  // 读者在此休眠等待

// recv() 中缓冲区为空时
let mut wq = self.wq.q.lock().unwrap();
wq.push_back(thread::current());
drop(wq);
thread::park();

// send() 成功后唤醒一个读者
let mut wq = self.wq.q.lock().unwrap();
if let Some(t) = wq.pop_front() { t.unpark(); }
```

### 11.2 被 IPC 模块使用

`ipc.rs` 的 `Sema`（信号量）内部使用 `EvBus` 进行事件通知：

```rust
struct SemaInner {
    bus: EvBus  // 信号量状态变化时通过事件总线唤醒等待线程
}
```

### 11.3 内核全局锁

```rust
// 进入内核关键路径
GKL.enter(thread_id);
// ... 操作内核数据结构 ...
GKL.leave();
```

### 11.4 Futex 系统调用

```rust
let futex = FutexBucket::new();
let val = AtomicU32::new(0);

// 线程 A：等待 val 变为 0
futex.wait(addr, 0, &val, None);

// 线程 B：设置 val 为 1 并唤醒一个等待者
val.store(1, Ordering::SeqCst);
futex.wake(addr, 1);
```

---

## 十二、跨模块连接

```
sync.rs
├── KernLock (GKL)
│   └── 被内核各子系统用于保护全局关键路径
│
├── Spin
│   └── 被 Channel.guard 使用，串行化 recv 操作
│
├── EvBus + EvFlag
│   └── 被 Sema 内部使用，信号量事件通知
│       被 fs.rs 的文件节点使用，poll/epoll 事件
│
├── Sema / SemaGuard
│   └── 被 ipc.rs 的 SemArr 复用，作为 SysV 信号量底层实现
│
├── FutexBucket / FutexTable
│   └── 实现 futex() 系统调用，用户态锁的底层支持
│
└── SyncQueue
    └── 被 Channel.wq 使用，阻塞 I/O 等待
        被 fs.rs 的管道/TTY 使用
        支持 epoll 注册，事件驱动 I/O
```

---

## 十三、与原版 kernel.rs 的对应

| sync.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| `KernLock` + `GKL` | 约第 100-170 行 |
| `Spin` | 约第 172-190 行 |
| `FlgGuard` | 约第 192-200 行 |
| `EvFlag` + `EvBus` | 约第 200-250 行 |
| `Sema` / `SemaGuard` | 约第 250-330 行 |
| `FutexBucket` / `FutexTable` | 约第 330-430 行 |
| `SyncQueue` | 约第 430-550 行 |

---

## 十四、潜在的改进方向

1. **KernLock 的内存序问题**：`holder` 和 `depth` 使用 `Relaxed` 序，在多核场景下可能与其他原子变量的操作产生不一致。应至少使用 `Acquire`/`Release`
2. **Sema 缺少真正的阻塞等待**：当前 `acquire_spin()` 使用忙等待（yield + 重试），而不是基于 `SyncQueue` 的真正休眠。在高竞争场景下效率较低
3. **FutexBucket.wait() 的虚假唤醒**：`thread::park()` 可能被虚假唤醒，但 `wait()` 没有循环检查条件，可能导致返回 `Ok(())` 但实际未被 `wake()` 调用
4. **SyncQueue.park_on() 的修剪逻辑未实现**：`n > 256` 时的 `_trim` 变量被计算但未使用，队列可能无限增长
5. **EvBus 回调在锁内执行**：`change()` 中 `self.cbs.retain(|f| !f(self.ev))` 在持有 EvBus 锁时执行回调，如果回调内部尝试获取同一把锁将死锁
6. **FutexTable.ftx_wake() 的 off-by-one**：`wk <= limit` 条件导致实际可能唤醒 count+1 个线程（当 wk == limit 时仍然进入循环体）

---

## Debug 修正记录

| Bug ID | 位置 | 问题描述 | 修复方式 | 日期 |
|--------|------|----------|----------|------|
| BUG-11 | `KernLock::leave()` | `let h = self.holder.load(Ordering::Relaxed)` 读取后变量 h 从未被引用，属于无用原子读 | 删除该行 | 2026-06-29 |
