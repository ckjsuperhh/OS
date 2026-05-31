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
| Basic  | 21   | 12   | 待修复 |
| Advanced | -  | -    | 待运行 |
| Pressure | -  | -    | 待运行 |

### 待修复的 Basic 测试失败（12 个）

1. `group_01::basic_cross_module_lock_order` — 跨模块锁顺序
2. `group_02::basic_sleep_under_spinlock_uniprocessor` — 自旋锁下睡眠
3. `group_03::basic_condvar_signal_before_wait` — 条件变量信号先于等待
4. `group_03::basic_spurious_wakeup_no_recheck` — 虚假唤醒未重检
5. `group_06::basic_block_read_success` — 块设备读成功
6. `group_08::basic_ring_full_reject` — 环形缓冲区满拒绝
7. `group_09::basic_save_restore_context` — 上下文保存/恢复
8. `group_09::basic_interrupt_mask_set` — 中断掩码设置
9. `group_09::basic_page_fault_in_process_context` — 进程上下文缺页
10. `group_10::basic_access_ok_overflow` — 访问检查溢出
11. `group_11::basic_mmap_file_io_workload` — mmap 文件 I/O
12. `group_11::basic_fork_exec_workload` — fork/exec
