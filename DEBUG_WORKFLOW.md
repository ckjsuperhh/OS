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
