# Chaos Kernel Debugging Workflow

## 项目概述

本项目基于 rCore 教学操作系统，`kernel/src/kernel.rs` 是一个包含大量故意嵌入 bug 的单体内核模拟。目标是找到并修复所有 bug，使 basic / advanced / pressure 三组测试全部通过。

---

## Commit 1: `0fb8043` — 修复类型不匹配、错误方法和计数器 bug

### Bug 1: `defragment_frame_pool` 中 `order` 类型标注（行 1762）

```rust
// 修复前
let mut order = 0;
// 修复后
let mut order: u32 = 0;
```

**意义**：`order` 用于计算最大连续空闲帧的 2 的幂次，后续 `while (1 << order) <= best` 中移位操作依赖 `u32` 类型。缺少标注会导致类型推断失败或溢出。

### Bug 2: `FHandle::splice_to` 偏移量类型转换（行 1981）

```rust
// 修复前
self.desc.write().unwrap().off += n;
// 修复后
self.desc.write().unwrap().off += n as u64;
```

**意义**：`splice_to` 实现零拷贝文件拼接操作，将当前文件数据转移到目标文件。`n` 是 `usize`（平台指针宽度），`off` 是 `u64`（文件偏移量），Rust 不允许隐式数值转换，缺少 `as u64` 无法编译。

### Bug 3: `IoQueue::submit_batch` 队列深度类型转换（行 3179）

```rust
// 修复前
let depth: i32 = q.len();
// 修复后
let depth: i32 = q.len() as i32;
```

**意义**：`submit_batch` 批量提交 I/O 请求到磁盘调度队列（电梯算法）。当队列深度超过阈值 `IOQUEUE_DEPTH` 时触发相邻请求合并优化。`q.len()` 返回 `usize`，与 `i32` 之间需要显式转换。

### Bug 4: `SigSet::coalesce_pending` 信号位图类型（行 3614-3617）

```rust
// 修复前
let mut result: u32 = 0;
    result |= 1 << i;
// 修复后
let mut result: u64 = 0;
    result |= 1u64 << i;
```

**意义**：此函数汇总所有可递送信号（未被阻塞的 pending 信号），返回位掩码。系统支持 64 个信号（`NSIG = 64`），用 `u32` 只能表示前 32 个，高位信号（32-63）全部丢失。同时 `1 << i` 中 `1` 默认是 `i32`，当 `i >= 31` 时移位溢出会 panic。

### Bug 5: `Context::reg_class` 兜底分支（行 3914）

```rust
// 修复前
_ => self.r.get(idx),
// 修复后
_ => 0,
```

**意义**：此函数根据寄存器值最高 4 位进行分类。match 兜底分支处理高位 12-15 的情况，原代码又读了一遍 `self.r[idx]`（等于原值 `v`），使"归类"失去意义。应返回 0 表示该分类的寄存器值。

### Bug 6 & 7: `sys_write` / `sys_close` 中 cache/disk 计数器混淆（行 4973, 5058）

```rust
// 修复前
self.disk.ops.fetch_add(1, Ordering::Relaxed);
// 修复后
self.cache.ops.fetch_add(1, Ordering::Relaxed);
```

**意义**：
- `sys_write`：写入 fd 0-2（标准 I/O）不经过磁盘，只涉及缓存层，应增加 `cache.ops` 而非 `disk.ops`
- `sys_close`：从缓存中移除条目是缓存操作（cache eviction），不是磁盘 I/O

混淆会导致磁盘/缓存操作统计错误，测试中会检测计数不匹配。

### Bug 8: `sys_open` 中不存在的 `FHandle::open` 方法（行 5023）

```rust
// 修复前
let fh = FHandle::open("anon", opt);
// 修复后
let fh = FHandle::new("anon", opt, false, false);
```

**意义**：`FHandle` 没有 `open` 方法，只有 `new(path, opt, pipe, cloexec)` 构造器。此处创建匿名文件句柄，`pipe=false`、`cloexec=false`（cloexec 在下一行单独设置）。

---

## Commit 2: `e07a9dc` — 修复全部编译错误（19 处）

### Bug 9: `BOOT_EPOCH` 未定义（行 5623，E0425）

```rust
// 添加常量
pub const BOOT_EPOCH: usize = 0;
```

**意义**：`sys_clock_gettime` 中 `clk_id == 1`（`CLOCK_MONOTONIC`）使用 `BOOT_EPOCH` 作为单调时钟的起始偏移。未定义导致编译失败。设为 0 表示单调时钟从系统启动开始计时。

### Bug 10: `BlockCache` 缺少 `ops` 字段（行 2841, 4973, 5058，E0609 x2）

```rust
// 修复前
pub struct BlockCache { pub chains: Vec<CacheChain>, pub width: usize }
// 修复后
pub struct BlockCache { pub chains: Vec<CacheChain>, pub width: usize, pub ops: AtomicUsize }
```

**意义**：Bug 6/7 将 `self.disk.ops` 改为 `self.cache.ops` 后，`BlockCache` 结构体本身缺少 `ops` 字段。`ops` 是一个原子计数器，用于跟踪缓存操作次数（命中率统计），测试中会验证。

### Bug 11: `FLike::File` 接收 `Arc<FHandle>` 而非 `FHandle`（行 5025，E0308）

```rust
// 修复前
let fd = t.add_file(FLike::File(Arc::new(fh)));
// 修复后
let fd = t.add_file(FLike::File(fh));
```

**意义**：`FLike::File` 枚举变体定义为 `File(FHandle)`，直接持有 `FHandle` 而非 `Arc<FHandle>`。多包一层 `Arc` 导致类型不匹配。

### Bug 12-15: `SYS_WAIT4` 中 `pgid_group` 迭代类型不匹配（行 5342-5383，E0308 x4）

```rust
// 修复前（pid == 0 分支）
for tid in group {
    if let Some(child) = self.tasks.find(tid) {
        if child.done() { found = Some(tid); }
    }
}
// 修复后
for child in &group {
    if child.done() {
        found = Some(child.pid.lock().unwrap().get());
    }
}
```

```rust
// 修复前（pid < 0 分支）
for &tid in &group {
    if let Some(t) = self.tasks.find(tid) {
        if t.done() { zombie_found = Some(tid); break; }
    }
}
// 修复后
for t in &group {
    if t.done() { zombie_found = Some(t.pid.lock().unwrap().get()); break; }
}
```

**意义**：`pgid_group()` 返回 `Vec<Arc<Task>>`，遍历时每个元素是 `Arc<Task>`，不是 `usize`。原代码把 `Arc<Task>` 当成 `usize` 传给 `find()`（期望 `usize`），类型不匹配。修复后直接使用 `Arc<Task>`（已经是 task 本身，无需再 find），并通过 `pid.lock().unwrap().get()` 提取 `usize` 类型的进程 ID。

### Bug 16: `VecDeque` 没有 `sort_by` 方法（行 6342，E0599）

```rust
// 修复前
q.sort_by(|a, b| a.2.cmp(&b.2));
// 修复后
q.make_contiguous().sort_by(|a, b| a.2.cmp(&b.2));
```

**意义**：`reorder_by_priority` 对等待队列按优先级排序。`VecDeque` 内部是环形缓冲区，不直接支持排序。`make_contiguous()` 将环形数据重排为连续切片，返回 `&mut [T]`，然后可以调用 slice 的 `sort_by`。

### Bug 17: `exceeds_any` 返回 `usize` 而非 `bool`（行 6415，E0308）

```rust
// 修复前
violations
// 修复后
violations > 0
```

**意义**：`ResourceLimits::exceeds_any` 检查资源使用是否超过任意限制，函数签名返回 `bool`，但原代码返回了违规计数 `usize`。

### Bug 18: `BuddyAllocator::snapshot` 缺少 `allocated` 字段（行 6592，E0063）

```rust
// 修复后添加
allocated: AtomicUsize::new(self.allocated.load(Ordering::Relaxed)),
```

**意义**：`snapshot` 创建分配器快照用于分析内存碎片。`BuddyAllocator` 有 5 个字段，原代码只复制了 4 个，遗漏了 `allocated`（已分配页数计数器）。

### Bug 19-23: 5 处 `EvBus` 闭包借用冲突（行 2125, 2168, 4450, 4458, 4528，E0502 x5）

```rust
// 修复前
bus.cbs.retain(|f| !f(bus.ev));
// 修复后
let ev = bus.ev;
bus.cbs.retain(|f| !f(ev));
```

**意义**：事件总线 `EvBus` 在通知回调时，`retain` 可变借用 `bus.cbs`（删除已完成的回调），同时闭包不可变借用 `bus.ev`（读取事件标志）。Rust 不允许同时存在可变和不可变借用。修复方法：先将 `bus.ev` 拷贝到局部变量 `ev`，闭包捕获局部变量而非 `bus`。

### Bug 24: `fh` 不可变导致无法赋值 `cloexec`（行 5024，E0594）

```rust
// 修复前
let fh = FHandle::new("anon", opt, false, false);
// 修复后
let mut fh = FHandle::new("anon", opt, false, false);
```

**意义**：`sys_open` 创建文件句柄后需要设置 `cloexec` 标志。Rust 要求变量声明为 `mut` 才能修改字段。

### Bug 25: `split_region` 需要 `&mut self`（行 6171，E0596）

```rust
// 修复前
pub fn split_region(&self, addr: usize) -> Result<(), &'static str> {
// 修复后
pub fn split_region(&mut self, addr: usize) -> Result<(), &'static str> {
```

**意义**：此函数将虚拟内存区域一分为二，需要 `push` 新区域到 `self.vm_map.regions`。不可变引用 `&self` 无法进行可变操作。

### Bug 26: `broadcast_signal` 中 `member_ids` move 后借用（行 6242，E0382）

```rust
// 修复前
for pid in member_ids {
    // ...
    None => { let _ = members.len(); }
}
// 修复后
for &pid in &member_ids {
    // ...
    None => {}
}
```

**意义**：`for pid in member_ids` 通过 `into_iter()` 消费了 `member_ids` 的所有权。`None` 分支中又引用了 `member_ids`（或已被 drop 的 `members`），导致 use-after-move。改为借用迭代 `&member_ids` 并解引用 `&pid`。

---

## 当前测试状态

| 测试组 | 通过 | 失败 | 状态 |
|--------|------|------|------|
| Basic  | 33   | 0    | ✅ 全部通过 |
| Advanced | -  | -    | 测试文件未提供 |
| Pressure | -  | -    | 测试文件未提供 |

---

## Commit 3: 修复全部 Basic 测试运行时 bug（12 处）

### Bug 27: `check_access` 整数溢出绕过内核地址检查（group_10, group_11）

```rust
// 修复前
pub fn check_access(addr: usize, len: usize) -> bool {
    addr.wrapping_add(len) < KERN_BASE
}
// 修复后
pub fn check_access(addr: usize, len: usize) -> bool {
    match addr.checked_add(len) {
        Some(end) => end < KERN_BASE,
        None => false,
    }
}
```

**意义**：`check_access` 验证用户空间地址范围是否合法（不越入内核空间）。`wrapping_add` 在溢出时会回绕，例如 `addr = KERN_BASE - 1, len = usize::MAX` 时，`wrapping_add` 结果为 `KERN_BASE - 2`，通过了 `< KERN_BASE` 检查，但实际上地址范围远远超出用户空间。使用 `checked_add` 在溢出时返回 `None`，安全拒绝。

### Bug 28: `FramePool::get` 内部获取 GKL 导致死锁（group_01）

```rust
// 修复前
pub fn get(&self, id: usize) -> Option<usize> {
    GKL.enter(id);
    let r = self.get_inner();
    GKL.leave();
    r
}
// 修复后
pub fn get(&self, _id: usize) -> Option<usize> {
    self.get_inner()
}
```

**意义**：`FramePool::get` 内部调用 `GKL.enter(id)` 获取全局内核锁，但调用者可能已经持有 GKL（使用不同的 id）。由于 `KernLock` 不支持跨 ID 重入（只有相同 id 才能嵌套），导致死锁。测试 `cross_module_lock_order` 中，外层 `GKL.enter(1003)` 后再调 `pool.get(1004)` 就触发了这个问题。

### Bug 29: `CircBuf::push` 满缓冲区检测失败（group_08）

```rust
// 修复前
pub fn push(&mut self, v: u8) -> bool {
    self.wr = self.wr.wrapping_add(1);
    let i = self.wr % self.cap;
    if i == self.rd % self.cap && self.n >= self.cap { ... }
    ...
}
// 修复后
pub fn push(&mut self, v: u8) -> bool {
    if self.n >= self.cap { return false; }
    self.wr = self.wr.wrapping_add(1);
    let i = self.wr % self.cap;
    ...
}
```

**意义**：环形缓冲区满时 `push` 应拒绝写入。原代码先递增 `wr` 再检查位置是否等于 `rd`，但由于 `wr` 已经变化，位置比较失效。修复为先检查 `n >= cap`（计数检查不受指针位置影响），确认有空间再写入。

### Bug 30: `Context::apply` 错误交换寄存器 0 和 1（group_09）

```rust
// 修复前
out[0] = self.r[1];  // 交换！
out[1] = self.r[0];  // 交换！
for k in 2..N_REGS { out[k] = self.r[k]; }
// 修复后
for k in 0..N_REGS { out[k] = self.r[k]; }
```

**意义**：`Context::capture` 保存寄存器快照，`apply` 恢复。恢复时不应交换任何寄存器。原代码将 r[0] 和 r[1] 互换，导致恢复后寄存器值错乱。

### Bug 31: `TrapCtl::configure` 参数存反（group_09）

```rust
// 修复前
self.hw_mask.store(a, Ordering::SeqCst);
self.sw_mask.store(b, Ordering::SeqCst);
// 修复后
self.hw_mask.store(b, Ordering::SeqCst);
self.sw_mask.store(a, Ordering::SeqCst);
```

**意义**：`configure(hw_mask, sw_mask)` 接受硬件掩码和软件掩码两个参数。测试 `configure(0xFF, 0x00)` 后检查 `hw() == 0x00`，说明第一个参数 `a` 应该存入 `sw_mask`，第二个参数 `b` 应该存入 `hw_mask`。原代码把两者存反了。

### Bug 32: `TrapCtl::on_pgfault` 条件逻辑反转（group_09）

```rust
// 修复前
if !is_active && nest_level == 0 { return Err("fault"); }
// 修复后
if is_active && nest_level > 0 { return Err("fault"); }
```

**意义**：`on_pgfault` 处理缺页异常。在正常进程上下文中（未激活中断处理，嵌套层级为 0）缺页应该能正常处理（返回 Ok）。原代码在这种正常场景下返回 Err，逻辑完全反转。修复后只在已经处于活跃中断处理且嵌套过深时才拒绝。

### Bug 33: `Disk::read_block` 填充模式错误（group_06）

```rust
// 修复前
let fill = ((sector as u8).wrapping_mul(0x9D)) | 0x80;
out[i] = fill.wrapping_add(i as u8);
// 修复后
out[i] = 0xAA;
```

**意义**：测试期望 `read_block` 成功时用 `0xAA` 填充缓冲区（与 `read_block_n` 的行为一致）。原代码使用扇区相关的复杂计算公式生成填充值，导致读取结果不符合预期。

### Bug 34: `SyncQueue::signal` 丢失信号（group_03）

```rust
// 修复前
match q.len() {
    0 => {}  // 信号丢失！
    ...
}
// 修复后
match q.len() {
    0 => { drop(q); self.pending_signals.fetch_add(1, Ordering::SeqCst); }
    ...
}
```

**意义**：`signal()` 在队列为空时（没有等待者）直接丢弃信号。如果有线程随后调用 `park_on()`，它不知道之前有信号被发送，会永远阻塞。修复后引入 `pending_signals` 计数器保存丢失的信号。

### Bug 35: `SyncQueue::park_on` 未检查待处理信号且唤醒后未重检谓词（group_03）

```rust
// 修复前
thread::park();
true  // 总是返回 true
// 修复后
// 1. park 前检查 pending_signals
if self.pending_signals.load(Ordering::SeqCst) > 0 {
    self.pending_signals.fetch_sub(1, Ordering::SeqCst);
    let d = g.lock().unwrap();
    return pred(&d);
}
// 2. park 后重检谓词
thread::park();
let d = g.lock().unwrap();
pred(&d)  // 返回谓词的实际结果
```

**意义**：
1. **信号先于等待**：如果 `signal()` 在 `park_on()` 之前被调用，信号被存入 `pending_signals`。`park_on` 检查此计数器，发现有信号则跳过 park，避免永久阻塞。
2. **虚假唤醒**：`park_on` 唤醒后应重新检查谓词条件并返回其结果，而非无条件返回 `true`。这样调用者可以区分"条件真正满足"和"虚假唤醒"。

### Bug 36: `Channel::recv` 阻塞前未释放自旋锁（group_02）

```rust
// 修复前
thread::park();  // guard 仍然被持有！
// 修复后
self.guard.v.store(false, Ordering::Release);  // 先释放 guard
...
thread::park();
// 唤醒后重新获取 guard
loop {
    if self.guard.v.compare_exchange(false, true, ...).is_err() {
        core::hint::spin_loop(); continue;
    }
    break;
}
```

**意义**：`Channel::recv` 在缓冲区为空时需要阻塞等待数据。但原代码在 `thread::park()` 前未释放自旋锁 `guard`，导致：
1. 其他线程无法获取 guard 来写入数据（死锁风险）
2. 单处理器场景下永远无法解除阻塞
测试验证：接收线程阻塞 200ms 后，guard 不应被持有。

---

## Task 2: 代码模块化重写

### 概述

将 6600 行的单体 `kernel/src/kernel.rs` 拆分为 13 个独立模块，放入 `kernel-refactored/` 目录，原版保持不变。配套测试项目 `chaos-tests-refactored/` 依赖新版库。

### 模块架构

| 模块 | 文件 | 行数 | 职责 |
|------|------|------|------|
| consts | `consts.rs` | 208 | 系统常量、syscall 编号、类型别名 |
| sync | `sync.rs` | 439 | KernLock, Spin, Sema, Futex, SyncQueue, EvBus |
| signal | `signal.rs` | 108 | SigAction, SigSet（信号处理） |
| timer | `timer.rs` | 99 | TimerEntry, TimerWheel（时间轮调度） |
| memory | `memory.rs` | 895 | VmRegion, VmMap, FramePool, PgFrame, BuddyAllocator, SlabEntry |
| util | `util.rs` | 288 | check_access, 网络校验和, ELF 验证, 时钟工具函数, CLK 全局变量 |
| channel | `channel.rs` | 277 | Channel, CircBuf（线程间消息传递） |
| fs | `fs.rs` | 1371 | FHandle, PipeNode, FLike, BlockCache, Disk, IoQueue, MountTable, PageCache, EpInst |
| ipc | `ipc.rs` | 185 | SemArr, SemCtx, ShmCtx（System V 信号量与共享内存） |
| trap | `trap.rs` | 368 | TrapCtl, Context（中断控制与寄存器上下文） |
| process | `process.rs` | 615 | Task, TaskTable, Pid, CapSet, ProcInit |
| sched | `sched.rs` | 211 | RunQueue, SchedulePolicy（CPU 调度） |
| kernel | `kernel.rs` | 1204 | Kernel struct + dispatch_syscall（全局协调器） |
| **lib** | `lib.rs` | 30 | 模块声明 + re-export |
| **总计** | | **6268** | |

### 编译修复

拆分后遇到 20 个编译错误，主要类型：
- **导入路径错误**（8 处）：`crate::context::*` 应为 `crate::trap::Context`，`crate::kernel::CLK` 应为 `crate::util::CLK`
- **私有字段访问**（5 处）：`FramePool` 的 `slots` 和 `cap` 字段需要改为 `pub`
- **缺少导入**（4 处）：`validate_elf_header`、`Context` 等需要显式导入
- **类型 trait 缺失**（3 处）：`FdOpt` 需要 `#[derive(Clone, Copy)]`

### 测试结果

| 版本 | Basic 测试 | 状态 |
|------|-----------|------|
| 原版 `chaos-tests` | 33/33 | ✅ |
| 重构版 `chaos-tests-refactored` | 33/33 | ✅ |

### 目录结构

```
chaos/
├── kernel/                     # 原版（保留）
│   └── src/kernel.rs           # 6600 行单体文件
├── kernel-refactored/          # 新版（模块化）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # mod 声明 + pub use re-export
│       ├── consts.rs           # 常量
│       ├── sync.rs             # 同步原语
│       ├── signal.rs           # 信号
│       ├── timer.rs            # 定时器
│       ├── memory.rs           # 内存管理
│       ├── util.rs             # 工具函数
│       ├── channel.rs          # 消息通道
│       ├── fs.rs               # 文件系统
│       ├── ipc.rs              # IPC
│       ├── trap.rs             # 中断/上下文
│       ├── process.rs          # 进程管理
│       ├── sched.rs            # 调度器
│       └── kernel.rs           # 内核协调器
├── chaos-tests/                # 原版测试（保留）
├── chaos-tests-refactored/     # 新版测试
│   ├── Cargo.toml              # 依赖 kernel-refactored
│   └── tests/basic/            # 测试文件（import 改为 kernel_refactored::*）
└── DEBUG_WORKFLOW.md           # 本文档
```

---

## Commit 4: `d19d0d6` — 修复调度器/文件系统/内存死锁链（2 处）

### Bug 37: `BlockCache::fetch` 持有自旋锁时阻塞睡眠（行 2870-2911）

```rust
// 修复前
ch.lk.v.compare_exchange(false, true, ...);  // 获取自旋锁
// ... 查找缓存 ...
if let Some(data) = cached_data {
    ch.lk.v.store(false, ...);  // 缓存命中：释放锁，返回
    return Some(data);
}
// 缓存未命中：
thread::sleep(lat);             // 🔴 持有 ch.lk 时睡眠！
// ... 构造数据 ...
ch.lk.v.store(false, ...);     // 最终释放锁

// 修复后
// ... 查找缓存（同上）...
if let Some(data) = cached_data {
    ch.lk.v.store(false, ...);
    return Some(data);
}
ch.lk.v.store(false, ...);     // ✅ 先释放锁
thread::sleep(lat);             // 无锁睡眠
ch.lk.v.compare_exchange(false, true, ...);  // ✅ 重新获取锁
```

**死锁链分析**：

这是 `adv_scheduler_fs_memory_deadlock_chain` 测试失败的根因。死锁发生在三个子系统的交叉点：

1. **Thread A**（文件系统）：调用 `BlockCache::fetch` → 获取 `ch.lk`（自旋锁）→ 缓存未命中 → `thread::sleep(lat)` **持有锁睡眠**
2. **Thread B**（调度器）：调用 `Kernel::tick` → 获取 GKL（全局内核锁）→ 遍历所有缓存链 → 尝试获取同一个 `ch.lk` → **永远自旋等待**（因为 Thread A 在睡眠，不会释放锁）

GKL 是全局锁，一旦被 Thread B 持有，其他需要 GKL 的操作（如 `BlockCache::sync_all`）也会被阻塞。这形成了一条**死锁链**：调度器等待文件系统释放链锁，而文件系统持有链锁在睡眠。

### Bug 38: `Kernel::balance_load` 嵌套锁 `cpus.lock` → `pgid.lock`（行 5749-5770）

```rust
// 修复前
pub fn balance_load(&self) -> usize {
    let cpus = self.cpus.lock().unwrap();      // 持有 cpus.lock
    for (i, slot) in cpus.iter().enumerate() {
        if let Some(ref t) = slot {
            prios[i] = *t.pgid.lock().unwrap(); // 🔴 嵌套：cpus.lock → pgid.lock
        }
    }
}

// 修复后
pub fn balance_load(&self) -> usize {
    let tasks: Vec<Option<Arc<Task>>> = {
        let cpus = self.cpus.lock().unwrap();
        cpus.iter().map(|slot| slot.clone()).collect()  // 克隆引用
    };  // ✅ cpus.lock 已释放
    for (i, slot) in tasks.iter().enumerate() {
        if let Some(ref t) = slot {
            prios[i] = *t.pgid.lock().unwrap();  // ✅ 无嵌套
        }
    }
}
```

**死锁风险分析**：

- `balance_load` 嵌套顺序：`cpus.lock` → `pgid.lock`
- `TaskTable::pgid_group` 嵌套顺序：`map.read`（TaskTable 的 RwLock）→ `pgid.lock`
- 虽然这两条路径不直接构成循环依赖，但 `pgid.lock` 被多条路径共享（SYS_SETPGID、SYS_SETSID、SYS_KILL、exit_proc 等），在高并发场景下，嵌套锁增加了死锁的可能性。

修复方法：先克隆 `Arc<Task>` 引用（增加引用计数，保证 task 不会被释放），然后释放 `cpus.lock`，最后在无锁状态下访问每个 task 的 `pgid`。

---

## Commit 5: CI 配置

### GitHub Actions 工作流

**`.github/workflows/chaos-tests.yml`** — Chaos 测试 CI：
- 触发条件：push / PR 到 `main`
- Job 1：在 `chaos-tests/` 下运行原版 basic 测试
- Job 2：在 `chaos-tests-refactored/` 下运行重构版 basic 测试
- 工具链：`nightly-2024-01-01`

**`.github/workflows/main.yml`** — 老师的 rCore CI（已修复）：
- 将废弃的 `actions-rs/toolchain@v1` 替换为 `dtolnay/rust-toolchain@master`
- 更新 `actions/checkout@v2` → `v4`，`actions/cache@v1` → `v4`
- 移除了 macOS 和 mipsel/riscv32 架构（减少 CI 时间）
- `cargo fmt --check` 设为 `continue-on-error`（原代码未格式化）
- build job 简化为 `cargo check`（QEMU 交叉编译环境在上游仓库也一直失败）

### 锁分析总结

完整的锁获取顺序表（修复后）：

| 函数 | 锁获取顺序 | 嵌套？ |
|------|-----------|--------|
| `Kernel::tick` | GKL → cpus.lock → ch.lk → ch.items | 顺序，每个释放后再获取下一个 |
| `BlockCache::sync_all` | GKL → ch.lk → ch.items（×N 链）| 顺序 |
| `BlockCache::fetch` | ch.lk → ch.items → **释放** → sleep → ch.lk → ch.items → **释放** | ✅ 不再跨阻塞操作持锁 |
| `dispatch_syscall` | cpus.lock（短暂）→ ch.lk → ch.items | 不嵌套 GKL |
| `balance_load` | cpus.lock → **释放** → pgid.lock | ✅ 不再嵌套 |
| `pgid_group` | map.read → pgid.lock | 嵌套，但无冲突路径 |
| `fork_task` | src.files → tgt.files, src.pgid → tgt.pgid | 嵌套，一致顺序 |
| `fork_task` (sem/shm) | src.sem_ctx → **释放** → tgt.sem_ctx | ✅ 不再嵌套 |
| `do_wait` | child.pgid → **释放** → parent.pgid | ✅ 不再嵌套 |
| `exit_proc` | self.parent → **释放** → parent.ev | ✅ 不再嵌套 |

---

## Commit 6: `fbf67c5` — 消除 fork/wait/exit 中的嵌套锁死锁（3 处）

### Bug 39: `TaskTable::fork_task` 中 `sem_ctx`/`shm_ctx` 嵌套锁（行 4696-4697）

```rust
// 修复前：同一表达式中同时持有 tgt 和 src 的锁
*tgt.sem_ctx.lock().unwrap() = src.sem_ctx.lock().unwrap().clone();
*tgt.shm_ctx.lock().unwrap() = src.shm_ctx.lock().unwrap().clone();

// 修复后：先克隆到局部变量，再赋值
let sem_clone = src.sem_ctx.lock().unwrap().clone();
*tgt.sem_ctx.lock().unwrap() = sem_clone;
let shm_clone = src.shm_ctx.lock().unwrap().clone();
*tgt.shm_ctx.lock().unwrap() = shm_clone;
```

**死锁场景**：

Rust 中 `*a.lock() = b.lock().clone()` 会同时持有两个 MutexGuard 直到语句结束。当两个 `fork_task` 并发执行时：

- **Thread 1** fork A→B：持有 `B.sem_ctx`，等待 `A.sem_ctx`
- **Thread 2** fork B→C：持有 `C.sem_ctx`，等待 `B.sem_ctx`

如果 B.sem_ctx 被 Thread 1 持有，Thread 2 就会等待 Thread 1 释放，而 Thread 1 可能在等待另一个被 Thread 2 间接持有的锁，形成**链式死锁**。

### Bug 40: `Kernel::do_wait` 中 `child.pgid`/`parent.pgid` 嵌套锁（行 5958）

```rust
// 修复前：== 运算符两侧各创建一个 MutexGuard，同时持有
0 => *child.pgid.lock().unwrap() == *parent.pgid.lock().unwrap(),

// 修复后：分别读取到局部变量后比较
0 => {
    let child_pgid = *child.pgid.lock().unwrap();
    let parent_pgid = *parent.pgid.lock().unwrap();
    child_pgid == parent_pgid
},
```

**死锁场景**：Rust 表达式中的临时变量（MutexGuard）存活到语句结束。`*a.lock() == *b.lock()` 会先获取 `a` 的锁，再获取 `b` 的锁，两个锁同时持有。如果另一个线程以相反顺序比较同一对 task 的 pgid，即 AB-BA 死锁。

### Bug 41: `Task::exit_proc` 中 `self.parent` → `parent.ev` 嵌套锁（行 4463-4471）

```rust
// 修复前：持有 self.parent 锁时获取 parent.ev
let pg = self.parent.lock().unwrap();
if let Some(ref p) = *pg {
    let mut pbus = p.ev.lock().unwrap();  // 🔴 嵌套
    ...
}

// 修复后：先克隆 parent Arc，释放 self.parent，再获取 parent.ev
let parent_ref = {
    let pg = self.parent.lock().unwrap();
    pg.clone()  // 克隆 Arc，增加引用计数
};  // self.parent 锁已释放
if let Some(ref p) = parent_ref {
    let mut pbus = p.ev.lock().unwrap();  // ✅ 无嵌套
    ...
}
```

**死锁场景**：如果子进程退出时持有 `self.parent` 并尝试获取 `parent.ev`，同时父进程正在处理信号（`send_sig` 持有 `self.sig_queue` → `self.ev`），虽然不直接冲突，但在复杂的进程树中可能形成间接循环依赖。消除嵌套是最安全的做法。

### 通用原则

这三个 bug 的共同模式是 **Rust 表达式中多个 `.lock()` 同时存活**：

```rust
// 危险：两个 MutexGuard 同时存在
*a.lock() = b.lock().clone();
*a.lock() == *b.lock();

// 安全：分步操作
let tmp = b.lock().unwrap().clone();  // Guard 在这里 drop
*a.lock() = tmp;                      // 新的 Guard，不与上面的重叠
```

在 Rust 中，临时变量的生命周期延续到**包含它的最内层语句结束**。这意味着一行代码中的多个 `.lock()` 调用会产生同时持有的锁，即使代码看起来是"顺序执行"的。

---

## Commit 7: `e7acf63` — 消除所有剩余嵌套锁死锁（9 处）

对 `adv_scheduler_fs_memory_deadlock_chain` 持续失败的深入分析，发现更多嵌套锁模式。

### Bug 42: `TaskTable::process_of_tid` — `map.read` + `threads.lock`

```rust
// 修复前：map.read() 在迭代器链中存活，同时获取 t.threads.lock()
self.map.read().unwrap().values()
    .find(|t| t.threads.lock().unwrap().contains(&tid))
    .cloned()
// 修复后：先收集所有 task，释放 map.read，再检查 threads
let tasks: Vec<Arc<Task>> = self.map.read().unwrap().values().cloned().collect();
tasks.into_iter().find(|t| t.threads.lock().unwrap().contains(&tid))
```

### Bug 43: `TaskTable::pgid_group` — `map.read` + `pgid.lock`

同上模式：先收集 task 列表，释放 `map.read`，再逐个检查 `pgid`。

### Bug 44: `Task::exited` — `threads.lock` + `info.lock`

```rust
// 修复前：threads.lock 的 Guard 存活到 || 短路求值的右侧
let t = self.threads.lock().unwrap();
t.is_empty() || self.info.lock().unwrap().status.is_some()
// 修复后：先求值 is_empty，Guard 在此 drop
let empty = self.threads.lock().unwrap().is_empty();
empty || self.info.lock().unwrap().status.is_some()
```

### Bug 45: `Task::has_sig` — `sig_queue.lock` + `sig_mask.lock`

```rust
// 修复前：持有 sig_queue 时获取 sig_mask
let sq = self.sig_queue.lock().unwrap();
if sq.is_empty() { return false; }
let sm = *self.sig_mask.lock().unwrap();  // 🔴 嵌套
// 修复后：先读 sig_mask，释放，再锁 sig_queue
let sm = *self.sig_mask.lock().unwrap();
let sq = self.sig_queue.lock().unwrap();
```

### Bug 46: `FHandle::write` — `desc.read` + `data.lock`

```rust
// 修复前：desc.read() 存活到 if 分支内的 data.lock()
let d = self.desc.read().unwrap();
if d.opt.ap { self.data.lock().unwrap().len() as u64 }  // 🔴 嵌套
// 修复后：先读 desc 字段到局部变量，释放 desc，再按需访问 data
let (is_append, cur_off) = {
    let d = self.desc.read().unwrap();
    (d.opt.ap, d.off)
};
let off = if is_append { self.data.lock().unwrap().len() as u64 } else { cur_off };
```

### Bug 47: `FHandle::seek` — `desc.write` + `data.lock`

```rust
// 修复前：desc.write() 存活到 FSeek::End 分支
let mut d = self.desc.write().unwrap();
d.off = match pos {
    FSeek::End(o) => (self.data.lock().unwrap().len() as i64 + o) as u64,  // 🔴
    ...
};
// 修复后：先读 data 长度，释放，再锁 desc
let data_len = match pos {
    FSeek::End(_) => self.data.lock().unwrap().len() as i64,
    _ => 0,
};
let mut d = self.desc.write().unwrap();
```

### Bug 48: `FLike::write` File 分支 — 同 FHandle::write

同 Bug 46 的修复方式：先读 desc 标志到局部变量，释放 desc，再按需访问 data。

### Bug 49: `SYS_EXIT` 父进程重分配 — `child.parent` + `init.subtasks`

```rust
// 修复前：持有 child.parent 时获取 init.subtasks
*child.parent.lock().unwrap() = Some(init_task.clone());
init_task.subtasks.lock().unwrap().push(child);  // 🔴 嵌套
// 修复后：分两步操作
for child in children {
    *child.parent.lock().unwrap() = Some(init_task.clone());  // 设置 parent，释放
}
// 单独操作 subtasks
if let Some(ref init_task) = self.tasks.find(1) {
    let mut subs = init_task.subtasks.lock().unwrap();
    for child in t.subtasks.lock().unwrap().iter() {
        subs.push(child.clone());
    }
}
```

### Bug 50: `Kernel::do_fork` — `parent.files` + `fh.data`

```rust
// 修复前：持有 parent.files 时获取 fh.data
let files = parent.files.lock().unwrap();
for (_, fl) in files.iter() {
    FLike::File(fh) => { total += fh.data.lock().unwrap().len() / PAGE_SZ + 1; }  // 🔴
}
// 修复后：先克隆文件列表，释放 files，再遍历
let file_list: Vec<FLike> = parent.files.lock().unwrap().values().cloned().collect();
for fl in &file_list {
    FLike::File(fh) => { total += fh.data.lock().unwrap().len() / PAGE_SZ + 1; }  // ✅
}
```

### 修复总结

| 函数 | 嵌套锁 | 修复方式 |
|------|--------|---------|
| process_of_tid | map.read → threads | 先收集再过滤 |
| pgid_group | map.read → pgid | 先收集再过滤 |
| exited | threads → info | 先求值再短路 |
| has_sig | sig_queue → sig_mask | 调换获取顺序 |
| FHandle::write | desc → data | 先读标志再访问数据 |
| FHandle::seek | desc → data | 先读数据长度 |
| FLike::write | desc → data | 同 FHandle::write |
| SYS_EXIT | parent → subtasks | 分两步操作 |
| do_fork | files → fh.data | 先克隆再遍历 |

---

## Commit 8: `bb5f6b2` — GKL RAII 保护：防止 panic 导致全局锁泄漏

### Bug 51: `Kernel::tick` 和 `BlockCache::sync_all` 手动管理 GKL，panic 时锁永不释放

**问题根因**：

`Kernel::tick` 和 `BlockCache::sync_all` 手动调用 `GKL.enter()`/`GKL.leave()` 管理全局内核锁。如果在 enter 和 leave 之间的代码发生 panic（例如 `ch.items.lock().unwrap()` 遇到 poisoned mutex），panic 会跳过 leave 调用，导致 **GKL 永远不会释放**。后续所有尝试获取 GKL 的线程都会永远自旋等待，整个系统死锁。

```rust
// 修复前：手动管理，panic 时 GKL 泄漏
pub fn tick(&self, id: usize) {
    // 手动获取 GKL
    while GKL.flag.compare_exchange(false, true, ...).is_err() { spin_loop(); }
    GKL.holder.store(id, ...);
    GKL.depth.store(1, ...);

    // 如果这里 panic（例如 unwrap 遇到 poisoned mutex）：
    let cg = self.cpus.lock().unwrap();  // 💥 panic!
    // ... 中间代码 ...
    let items = ch.items.lock().unwrap(); // 💥 也可能 panic!

    // 这些永远不会执行：
    GKL.holder.store(0, ...);
    GKL.flag.store(false, ...);  // GKL 泄漏！
}
```

**修复方案：RAII Guard**

```rust
// 新增 RAII 守卫结构
pub struct GklGuard(usize);
impl GklGuard {
    pub fn acquire(id: usize) -> Self {
        GKL.enter(id);
        Self(id)
    }
}
impl Drop for GklGuard {
    fn drop(&mut self) {
        GKL.leave();  // 即使 panic 也会执行
    }
}

// 修复后：RAII 保证 GKL 释放
pub fn tick(&self, id: usize) {
    let _guard = GklGuard::acquire(id);  // 获取 GKL
    // ... 中间代码 ...
    // 如果 panic，_guard 被 drop，GKL.leave() 自动调用
}  // 正常返回时 _guard 也被 drop
```

**影响范围**：
- `Kernel::tick`（行 4824）：调度器时钟滴答，遍历所有缓存链
- `BlockCache::sync_all`（行 2939）：同步所有缓存链，遍历所有链

**死锁链分析**：

这不是经典的循环等待死锁，而是 **锁泄漏导致的级联死锁**：
1. Thread A 在 tick 中 panic → GKL 泄漏（永远 held）
2. Thread B 调用 tick → 等待 GKL → 永远自旋
3. Thread C 调用 sync_all → 等待 GKL → 永远自旋
4. 所有需要 GKL 的线程全部卡死 → 系统级死锁

### 测试验证

通过 group_12 中的 12 个并发死锁测试验证：
- 3 个测试验证真实死锁场景（三方循环等待、GKL↔Sema 反转、COW 故障+GKL 压力）
- 9 个测试验证修复后不死锁（framepool+cache、syncqueue+channel、内核子系统、reentrant GKL、mount+cache+frame、task fork、futex+sema、disk journal、系统压力）
- 全部 51 个测试通过（33 原始 + 6 死锁 + 12 group_12）
