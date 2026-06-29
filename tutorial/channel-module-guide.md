# Channel 模块阅读指南

> 文件路径: `kernel-refactored/src/channel.rs`
> 代码量: 277 行 | 2 个核心结构体 | 依赖: `consts`, `sync`

---

## 一、模块概述

`channel.rs` 实现了内核中的 **字节流 IPC（进程间通信）** 机制，提供两个层次的能力：

| 层次 | 结构体 | 线程安全 | 用途 |
|---|---|---|---|
| 底层 | `CircBuf` | 否（裸操作） | 纯环形字节缓冲区，被 `fs.rs` 中的 Pipe 等复用 |
| 上层 | `Channel` | 是（Spin + Mutex + SyncQueue） | 完整的阻塞式单生产者-单消费者字节通道 |

**设计定位：** Channel 在内核中扮演 "管道" 的角色——用于 `pipe()` 系统调用的数据传输、伪终端 I/O 缓冲、以及任何需要在线程/进程间传递字节流的场景。它类似于 Go 的 `chan byte` 或 Unix pipe，但有固定容量。

---

## 二、CircBuf — 环形缓冲区

### 2.1 结构体定义

```rust
pub struct CircBuf {
    /// 底层存储字节数组
    pub data: Vec<u8>,
    /// 读索引位置（下一次 pop 将在此位置读取）
    pub rd: usize,
    /// 写索引位置（下一次 push 将在此位置写入）
    pub wr: usize,
    /// 缓冲区总容量
    pub cap: usize,
    /// 当前已存储有效数据长度
    pub n: usize,
}
```

**设计要点：**
- `rd` 和 `wr` 采用 **单调递增 + 取模** 的方式追踪位置，而非在到达边界时回绕到 0
- `n` 独立记录元素数量，避免了 "满" 和 "空" 状态无法区分的经典环形缓冲区问题
- `cap` 和 `data.len()` 在当前实现中总是相等（构造时 `vec![0u8; c]`）

### 2.2 构造函数

```rust
/// 创建一个容量为 c 的空环形缓冲区，读写指针均为 0
pub fn new(c: usize) -> Self {
    Self { data: vec![0u8; c], rd: 0, wr: 0, cap: c, n: 0 }
}

/// 创建一个指定初始读写位置的环形缓冲区
/// 自动计算已有数据量 n：如果 w >= r 则 n = w - r，否则 n = c - r + w（跨越边界）
pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
    let n = if w >= r { w - r } else { c - r + w };
    Self { data: vec![0u8; c], rd: r, wr: w, cap: c, n }
}
```

**`with_pos` 的用途：** 主要用于测试中构造特定初始状态的缓冲区（如测试 wrap-around 行为）。

### 2.3 核心读写操作

```rust
/// 向环形缓冲区写入一个字节。
/// 成功返回 true；缓冲区已满时返回 false。
pub fn push(&mut self, v: u8) -> bool {
    if self.full() { return false; }              // 满则拒绝
    let i = self.wr % self.cap;                   // 取模得到实际数组索引
    self.data[i] = v;                             // 写入数据
    self.wr = self.wr.wrapping_add(1);           // 写指针前移（使用 wrapping_add 防溢出）
    self.n += 1;                                  // 元素计数 +1
    true
}

/// 从环形缓冲区读取一个字节。
/// 有数据返回 Some(byte)；空则返回 None。
pub fn pop(&mut self) -> Option<u8> {
    if self.empty() { return None; }              // 空则返回 None
    let i = self.rd % self.cap;                   // 取模得到实际索引
    let v = self.data[i];                         // 读取数据
    self.rd = self.rd.wrapping_add(1);           // 读指针前移
    self.n -= 1;                                  // 元素计数 -1
    Some(v)
}
```

**索引计算图解：**

```
        cap = 8, rd = 2, wr = 5, n = 3
        ┌───┬───┬───┬───┬───┬───┬───┬───┐
  idx   │ 0 │ 1 │ 2 │ 3 │ 4 │ 5 │ 6 │ 7 │
        ├───┼───┼───┼───┼───┼───┼───┼───┤
  data  │   │   │ A │ B │ C │   │   │   │
        └───┴───┴───┴───┴───┴───┴───┴───┘
                  ^           ^
                 rd=2        wr=5
              (next pop)  (next push)
```

### 2.4 辅助方法

```rust
pub fn len(&self) -> usize { self.n }               // 当前数据量
pub fn empty(&self) -> bool { self.n == 0 }         // 是否为空
pub fn full(&self) -> bool { self.n >= self.cap }   // 是否已满
pub fn remaining(&self) -> usize {                   // 剩余可写空间
    self.cap.saturating_sub(self.n)
}

/// 偷看下一个可读字节，但不消费（不移动 rd）
pub fn peek(&self) -> Option<u8> {
    if self.empty() { return None; }
    let i = self.rd % self.cap;
    Some(self.data[i])
}

/// 批量读取：最多取 max 个字节到 dst 中，返回实际读取数
pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
    let take = min(max, self.n);
    for _ in 0..take {
        if let Some(b) = self.pop() { dst.push(b); }
    }
    take
}

/// 批量写入：从 src 切片逐字节 push，满则停止，返回实际写入数
pub fn fill_from(&mut self, src: &[u8]) -> usize {
    let mut written = 0;
    for &b in src {
        if !self.push(b) { break; }
        written += 1;
    }
    written
}
```

### 2.5 CircBuf 在内核中的复用

`CircBuf` 被 `fs.rs` 的 `PipeNode` 导入使用（`use crate::channel::CircBuf;`），作为管道文件描述符的底层存储。但注意 `PipeNode` 实际使用了 `VecDeque<u8>`（`PipeBuf`），`CircBuf` 的导入是备用路径。

---

## 三、Channel — 线程安全字节通道

### 3.1 结构体定义

```rust
pub struct Channel {
    /// 环形缓冲区，用 Mutex 保护（允许并发读写但互斥访问 buf）
    pub buf: Mutex<CircBuf>,
    /// 自旋锁守卫，用于 recv 端的互斥（同一时刻只有一个线程在读）
    pub guard: Spin,
    /// 等待队列，当缓冲区为空时，读者在此休眠等待
    pub wq: SyncQueue,
    /// 关闭标志，一旦设为 true，recv 将不再阻塞而是返回 None
    pub shut: AtomicBool,
}
```

**四层同步机制的设计意图：**

| 组件 | 保护对象 | 类型 | 为什么用这种锁 |
|---|---|---|---|
| `buf: Mutex` | 环形缓冲区的读写操作 | 互斥锁（可休眠） | 保护数据结构一致性，操作时间短 |
| `guard: Spin` | recv 端的串行化 | 自旋锁 | 确保同一时刻只有一个读者进入 recv 逻辑 |
| `wq: SyncQueue` | 休眠的读者线程 | 等待队列 | 空缓冲时让出 CPU，有新数据时被唤醒 |
| `shut: AtomicBool` | 通道关闭状态 | 原子变量 | 轻量级标志，无需加锁即可检查 |

### 3.2 构造函数

```rust
pub fn new(cap: usize) -> Self {
    // 容量钳制：最小 1 字节，最大 1MB (2^20)
    let effective_cap = if cap == 0 { 1 }
                        else if cap > 1 << 20 { 1 << 20 }
                        else { cap };
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
```

**常量 `RBUF_CAP = 256`**：在 `consts.rs` 中定义，是管道 I/O 测试中 Channel 的默认容量。

### 3.3 阻塞接收 — `recv()`

这是 Channel 最复杂的方法（约 70 行），实现了 **"先尝试读 → 读不到则休眠等待 → 被唤醒后再读"** 的经典阻塞 I/O 模式。

```rust
pub fn recv(&self) -> Option<u8> {
    // === 阶段 1：获取自旋锁（串行化读者） ===
    loop {
        if self.guard.v.compare_exchange(
            false, true, Ordering::Acquire, Ordering::Relaxed
        ).is_err() {
            core::hint::spin_loop();  // 忙等待提示 CPU
            continue;
        }
        break;
    }

    // === 阶段 2：第一次尝试读取 ===
    let result = {
        let mut ring = self.buf.lock().unwrap();
        if !ring.empty() {
            // 有数据：取出字节，然后移动 rd 指针
            let idx = ring.rd % ring.cap;
            let byte = ring.data[idx];
            ring.rd = ring.rd.wrapping_add(1);
            ring.n -= 1;
            Some(byte)
        } else {
            None  // 缓冲区空
        }
    };

    // 读到了，释放自旋锁并返回
    if result.is_some() {
        self.guard.v.store(false, Ordering::Release);
        return result;
    }

    // === 阶段 3：缓冲区空，检查是否已关闭 ===
    if self.shut.load(Ordering::Relaxed) {
        self.guard.v.store(false, Ordering::Release);
        return None;  // 通道已关闭，返回 None 表示 EOF
    }

    // === 阶段 4：注册到等待队列并休眠 ===
    {
        let data_ref = &self.buf;
        let d = data_ref.lock().unwrap();
        if d.n > 0 {
            drop(d);  // 在我们检查关闭状态期间有人写入了，重试
        } else {
            drop(d);  // 释放 buf 锁
            self.guard.v.store(false, Ordering::Release);  // 释放自旋锁

            // 将当前线程加入等待队列
            let mut wq = self.wq.q.lock().unwrap();
            wq.push_back(thread::current());
            drop(wq);

            // 休眠当前线程，直到被 send() 或 close() 唤醒
            thread::park();

            // 被唤醒后重新获取自旋锁
            loop {
                if self.guard.v.compare_exchange(
                    false, true, Ordering::Acquire, Ordering::Relaxed
                ).is_err() {
                    core::hint::spin_loop();
                    continue;
                }
                break;
            }
        }
    }

    // === 阶段 5：第二次尝试读取（被唤醒后） ===
    let v = {
        let mut ring = self.buf.lock().unwrap();
        if !ring.empty() {
            let idx = ring.rd % ring.cap;
            let byte = ring.data[idx];
            ring.rd = ring.rd.wrapping_add(1);
            ring.n -= 1;
            Some(byte)
        } else {
            None  // 虚假唤醒或 close() 唤醒
        }
    };
    self.guard.v.store(false, Ordering::Release);
    v
}
```

**流程图：**

```
recv() 被调用
    │
    ▼
[获取 guard 自旋锁] ──忙等──► 其他读者正在读
    │
    ▼
[锁 buf，尝试读取]
    │
    ├── 有数据 ──► 释放 guard ──► return Some(byte)
    │
    ├── 已关闭 ──► 释放 guard ──► return None (EOF)
    │
    └── 空且未关闭
         │
         ▼
    [释放 buf + guard]
    [推入等待队列 wq]
    [thread::park() 休眠]
         │
         ▼ (被 send/close 的 unpark 唤醒)
    [重新获取 guard]
    [锁 buf，再次尝试读]
         │
         ├── 有数据 ──► return Some(byte)
         └── 仍空 ────► return None (虚假唤醒)
```

### 3.4 发送 — `send()`

```rust
pub fn send(&self, v: u8) -> bool {
    // 锁住 buf 写入
    let success = {
        let mut ring = self.buf.lock().unwrap();
        if ring.full() { false }          // 满了，写入失败
        else {
            let idx = ring.wr % ring.cap;
            ring.data[idx] = v;
            ring.wr = ring.wr.wrapping_add(1);
            ring.n += 1;
            true
        }
    };
    // 写入成功 → 唤醒一个等待中的读者
    if success {
        let mut wq = self.wq.q.lock().unwrap();
        if let Some(t) = wq.pop_front() { t.unpark(); }
    }
    success
}
```

**注意：** `send()` 是**非阻塞**的——缓冲区满时立即返回 `false`，不会等待。调用者需要自行实现重试（如 `while !ch.send(v) { yield_now(); }`）。

### 3.5 关闭通道 — `close()`

```rust
pub fn close(&self) {
    self.shut.store(true, Ordering::Release);   // 标记关闭
    // 唤醒所有等待中的读者，让它们看到 shut=true 并返回 None
    let mut wq = self.wq.q.lock().unwrap();
    while let Some(t) = wq.pop_front() { t.unpark(); }
}
```

### 3.6 其他方法

```rust
/// 非阻塞尝试接收：获取不到自旋锁或缓冲区为空都立即返回 None
pub fn try_recv(&self) -> Option<u8> { ... }

/// 批量发送：一次写入多个字节，返回实际写入数
/// 写入后唤醒一个等待的读者
pub fn send_batch(&self, data: &[u8]) -> usize { ... }

/// 查询当前缓冲区中的数据量（不消费）
pub fn depth(&self) -> usize { ... }

/// 一次性取走缓冲区中所有数据
pub fn drain_all(&self) -> Vec<u8> { ... }

/// 查询通道是否已关闭
pub fn is_closed(&self) -> bool { ... }

/// 查询剩余可写容量
pub fn remaining_capacity(&self) -> usize { ... }
```

---

## 四、使用场景

### 4.1 管道 IPC（pipe 系统调用）

Channel 是 `pipe()` 系统调用的数据传输后端。内核中调用 `do_pipe()` 创建管道时，会分配一个 Channel 连接读写两端：

```
  进程 A (写端)                    进程 B (读端)
       │                                │
       ▼                                ▼
  ch.send(byte) ──► Channel ──► ch.recv()
                     │
              CircBuf (环形缓冲)
              SyncQueue (等待队列)
```

### 4.2 伪终端 / TTY I/O

终端输入输出也使用类似的环形缓冲 + 阻塞等待模式。用户按键写入 Channel，shell 进程阻塞 `recv()` 等待输入。

### 4.3 生产者-消费者线程通信

测试 `group_11::basic_pipe_ipc_workload` 展示了典型用法：

```rust
let ch = Arc::new(Channel::new(RBUF_CAP));  // 256 字节通道

// 生产者线程：发送 0..200 字节
let producer = std::thread::spawn(move || {
    for i in 0..200u8 {
        while !ch_prod.send(i) {          // 满则重试
            std::thread::yield_now();
        }
    }
    ch_prod.close();                       // 发完关闭
});

// 消费者（主线程）：阻塞接收直到 EOF
loop {
    match ch.recv() {
        Some(v) => received.lock().unwrap().push(v),
        None => break,  // close() 后 recv 返回 None
    }
}
```

### 4.4 测试 SpinLock 不持有保证

测试 `group_02::basic_sleep_under_spinlock_uniprocessor` 验证了一个重要性质：**Channel 的 recv() 在阻塞等待时不持有任何自旋锁**。这确保了单处理器场景下不会因为持有自旋锁时休眠而导致死锁。

---

## 五、同步原语协作关系

```
Channel
├── guard (Spin)
│   └── 保证同一时刻只有一个线程执行 recv 的读逻辑
│       避免多个读者竞争 rd 指针导致数据乱序
│
├── buf (Mutex<CircBuf>)
│   └── 保护环形缓冲区的读写操作原子性
│       写者 send() 和读者 recv() 通过此锁互斥
│
├── wq (SyncQueue)
│   └── 当 buf 为空时，读者线程 park 在此队列中
│       send() 成功写入后 unpark 队首线程
│       close() 时 unpark 所有线程
│
└── shut (AtomicBool)
    └── 关闭标志，recv() 检查此标志决定是否继续等待
        使用原子操作避免额外加锁
```

**锁获取顺序（避免死锁）：**
```
recv: guard(Spin) → buf(Mutex) → wq(Mutex)
send: buf(Mutex) → wq(Mutex)
```

---

## 六、与原版 kernel.rs 的对应

| channel.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| `CircBuf` 结构体 | 约第 2360-2385 行（独立结构体） |
| `Channel` 结构体 | 约第 2386-2585 行 |
| 使用 `CircBuf` 的 `PipeNode` | `fs.rs` 中（从 `kernel.rs` 约第 1824 行起） |

---

## 七、潜在的改进方向

1. **`recv()` 的重复代码**：阶段 2 和阶段 5 的读取逻辑完全相同（~15 行），可以提取为私有方法 `try_read_locked()`
2. **`send()` 非阻塞设计**：当前满时返回 false，调用者需手动重试。可考虑添加 `send_blocking()` 方法
3. **`send_batch()` 只唤醒一个读者**：如果多个读者在等待，批量写入可能应该唤醒多个
4. **`wrapping_add` 的溢出风险**：在极长时间运行的系统中，`rd`/`wr` 会持续递增直到 `usize::MAX`，`wrapping_add` 会回绕到 0 后取模仍然正确，但代码可读性不如直接用 `data.len()` 内的相对索引

---

## 八、Debug 修正记录

以下记录了开发过程中发现并修复的 6 个 Bug，均与指针语义、边界检查和 API 委托有关。

### BUG-01：`push` 先移动指针再写入导致 off-by-one

**现象：** `push` 后 `wr` 指向的不是下一个写入位置，而是最后写入位置的前一个。调用方读取 `wr` 计算可写空间时得到错误结果。

**根因：** 旧代码先执行 `wr = wr.wrapping_add(1)` 再在 `wr % cap` 处写入，使得 `wr` 的语义变成"最后写入的位置"而非"下一个写入位置"。

**修复：** 改为先在 `wr % cap` 处写入数据，再执行 `wr = wr.wrapping_add(1)`。修复后 `wr` 始终指向下一个写入位置。

### BUG-02：`pop` 先移动指针再读取导致 off-by-one

**现象：** `pop` 返回的不是队列头部最旧的数据，而是跳过了一字节。第一次 `pop` 丢失了 `data[0]`。

**根因：** 与 BUG-01 对称——旧代码先 `rd = rd.wrapping_add(1)` 再读取 `rd % cap`，导致 `rd` 语义不一致。

**修复：** 改为先读取 `rd % cap` 处的数据，再执行 `rd = rd.wrapping_add(1)`。修复后 `rd` 始终指向下一个读取位置。

### BUG-03：`push`/`pop` 中多余的 `data.len()` 边界检查

**现象：** 在 `wrapping_add` 导致 `usize` 回绕的极端情况下，`i >= self.data.len()` 检查会误判为越界并回退指针，使操作失败。

**根因：** 由于 `cap == data.len()` 始终成立，`idx = ptr % cap` 的结果一定在 `[0, cap)` 范围内，不可能越界。该检查是多余的，且在 `wrapping_add` 回绕时会因 `ptr` 变为极小值导致 `idx` 计算正常，但回退操作破坏了状态。

**修复：** 移除 `i >= self.data.len()` 检查及对应的指针回退逻辑。改用 `self.full()` / `self.empty()` 作为唯一的前置校验。

### BUG-04：`peek` 使用 `rd + 1` 而非 `rd` 导致读到错误位置

**现象：** `peek` 返回的不是下一个将被 `pop` 读取的字节，而是其后一个字节。

**根因：** 旧代码中 `rd` 的语义是"最后读取位置的前一个"，所以 `peek` 用 `rd.wrapping_add(1) % cap` 来预览下一个。修复 BUG-02 后 `rd` 已经直接指向下一个读取位置，`peek` 不再需要 `+1`。

**修复：** 将 `peek` 改为 `self.rd % self.cap` 直接读取，同时移除多余的 `data.len()` 检查。

### BUG-05：`Channel::recv`/`send`/`try_recv`/`send_batch`/`drain_all` 未同步新指针语义

**现象：** `CircBuf` 的 `push`/`pop` 修复后，`Channel` 的方法中仍然直接操作 `rd`/`wr` 指针（先加一再操作），导致 Channel 层面仍存在 off-by-one 问题。

**根因：** `Channel` 的方法绕过了 `CircBuf` 的 `push`/`pop` API，直接内联了环形缓冲区的指针操作逻辑。修复 `CircBuf` 时遗漏了这些调用点。

**修复：** 将 `recv`、`send`、`try_recv`、`send_batch`、`drain_all` 中所有直接操作 `rd`/`wr` 的代码统一改为新语义：先在当前指针位置操作，再推进指针。同时移除所有 `data.len()` 边界检查，改用 `full()`/`empty()` 校验。

### BUG-06：Channel 方法内联 CircBuf 逻辑 → 改用 API 委托

**现象：** Channel 的 recv/send/try_recv/send_batch/drain_all/depth/remaining_capacity 都手动操作 ring 内部字段（rd/wr/data/cap/n），与 CircBuf 的 push/pop/len/remaining 逻辑完全重复，违反 DRY 原则。

**根因：** Channel 的方法绕过了 CircBuf 的公开 API，直接内联了环形缓冲区的索引计算、指针移动和计数更新。一旦 CircBuf 内部语义变更（如 BUG-02 的指针修复），Channel 中所有内联代码都必须同步修改，极易遗漏。

**修复：** 全部改为调用 CircBuf 公开方法：
- `recv()` 阶段 2/5 的手动读取 → `ring.pop()`
- `try_recv()` 的手动读取 → `ring.pop()`
- `send()` 的手动写入 → `ring.push(v)`
- `send_batch()` 的手动写入循环 → `ring.push(byte)` 循环
- `drain_all()` 的手动循环 → `while let Some(b) = ring.pop()`
- `depth()` 的直接读 n → `ring.len()`
- `remaining_capacity()` 的直接算 `cap - n` → `ring.remaining()`

**效果：** 消除 DRY 违规，Channel 不再直接触碰 ring 内部实现。代码量减少约 30 行。
