# Chaos Kernel 重构优化工作流

## 概述

本文档记录在对重构后的内核模块进行代码审阅（code review / code reading）过程中发现的优化点和潜在问题，属于**代码质量改进**范畴。

这与仓库根目录下的 [`DEBUG_WORKFLOW.md`](../DEBUG_WORKFLOW.md) 是**不同的工作流**：

| | `DEBUG_WORKFLOW.md` | `REFACTOR_WORKFLOW.md`（本文档） |
|---|---|---|
| **目标** | 修复 kernel.rs 中故意嵌入的 bug，使测试通过 | 改善重构后代码的可读性、正确性和一致性 |
| **触发方式** | 测试失败驱动 | 代码审阅驱动 |
| **阶段** | 初始调试阶段 | 重构后的审阅阶段 |

---

## 当前状态

| 模块 | 文件 | 状态 |
|---|---|---|
| Channel | `channel.rs` | CircBuf 和 Channel 已完成重构；Session 2（BUG-06 API 委托）已完成；Session 3（BUG-07 关闭检查）已完成 |
| Signal | `signal.rs` | Session 4（BUG-08/09/10 信号集合操作优化）已完成 |
| Sync | `sync.rs` | Session 5（BUG-11 KernLock::leave() 移除无用原子读）已完成；Session 6（BUG-13 FutexBucket/FutexTable 哈希桶改造、BUG-14 SyncQueue 三处冗余与语义修复）已完成 |
| Ipc | `ipc.rs` | Session 7（BUG-15 SemArr otime/ctime 改为真实 Unix 时间戳）已完成 |
| Trap | `trap.rs` | Session 8（BUG-16 死代码清理 + 删除 handle_irq + dispatch 对标标准两层 trap 范式重写）已完成 |
| Util | `util.rs` | Session 9（BUG-17 check_access_rw 三层校验实现 + cfu 死代码清理）已完成 |

---

## 已完成的优化

### Session 1: channel.rs — CircBuf 与 Channel 指针语义修正

**日期**: 2026-06-29
**模块**: `channel.rs`
**触发**: 代码审阅过程中发现 CircBuf 的指针操作不直观，存在多余的防御性检查，且 Channel 中内联的 ring 操作与 CircBuf 未保持一致。

---

#### BUG-01: push/pop/peek 使用了不必要的 `data.len()` 越界检查

- **问题**: 原代码在计算 `i = ptr % cap` 后，用 `i >= self.data.len()` 做防御性检查。但由于 `cap` 始终等于 `data.len()`，取模后的 `i` 不可能越界，这个检查完全多余且掩盖了真正的逻辑判断。
- **修复**: 改为用 `self.full()`（push）和 `self.empty()`（pop/peek）判断是否可以操作。
- **影响**: CircBuf 所有读写方法 + Channel 中所有直接操作 ring 的方法。

#### BUG-02: push/pop 的"先移指针再操作"语义不直观

- **问题**: 原 push 先 `wr += 1` 再在 `wr` 处写入，导致第一次写入落在 index=1 而非 index=0。pop 同理，先 `rd += 1` 再读取。
- **修复**: 改为"先操作当前指针位置，再后移指针"。新语义：`wr` 指向下一个写入位置，`rd` 指向下一个读取位置。
- **影响**: `CircBuf::push`/`pop` + `Channel::recv`/`send`/`try_recv`/`send_batch`/`drain_all` 中所有直接操作 ring 的代码。

#### BUG-03: Channel recv/try_recv/drain_all 的直接 ring 操作未同步

- **问题**: Channel 方法中内联了与 CircBuf 相同的"先移指针再读"逻辑。
- **修复**: 统一改为"先读 rd 位置，再后移 rd"。

#### BUG-04: Channel send/send_batch 的直接 ring 操作未同步

- **问题**: 同上，写操作也是"先移指针再写"。
- **修复**: 统一改为"先写 wr 位置，再后移 wr"。

#### BUG-05: Channel 方法中的 `data.len()` 防御性检查未同步移除

- **问题**: Channel 方法中也有与 CircBuf 相同的多余 `data.len()` 检查。
- **修复**: 统一移除，改用 `ring.empty()` / `ring.full()` 判断。

#### 验证结果

所有 33 个 basic 测试通过，关键测试组：

- `group_08` (CircBuf): `basic_ring_write_read`, `basic_ring_full_reject`, `basic_ring_wrap_around` ✓
- `group_02` (Channel+Spin): `basic_sleep_under_spinlock_uniprocessor` ✓
- `group_11` (Channel IPC): `basic_pipe_ipc_workload` ✓

---

### Session 2: channel.rs — Channel 方法改用 CircBuf API（消除内联重复逻辑）

**Date**: 2026-06-29
**Module**: channel.rs
**Trigger**: 代码审阅中发现 Channel 方法直接内联了 CircBuf 的内部逻辑（手动算索引、移指针、改 n），违反 DRY 原则

#### BUG-06: Channel 方法应委托给 CircBuf 的公开 API，而非重复其内部实现

- **问题**: Channel 的 recv/try_recv/send/send_batch/drain_all/depth/remaining_capacity 全部手动操作 ring 的内部字段（rd/wr/data/cap/n），与 CircBuf::push/pop/len/remaining 逻辑完全重复。一旦 CircBuf 的指针语义变更（如 BUG-02 的修复），Channel 中所有内联代码都必须同步修改，极易遗漏。
- **修复**:
  - `recv()` 阶段 2/5 的手动读取 → `ring.pop()`
  - `try_recv()` 的手动读取 → `ring.pop()`
  - `send()` 的手动写入 → `ring.push(v)`
  - `send_batch()` 的手动写入循环 → `ring.push(byte)` 循环
  - `drain_all()` 的手动循环 → `while let Some(b) = ring.pop()`
  - `depth()` 的直接读 n → `ring.len()`
  - `remaining_capacity()` 的直接算 `cap - n` → `ring.remaining()`
- **效果**: Channel 不再直接访问 ring 的任何内部字段（rd/wr/data/cap/n），所有缓冲区操作完全委托给 CircBuf 的公开 API。代码量减少约 30 行。

#### 验证结果
- 所有 33 个 basic 测试通过

---

### Session 3: channel.rs — send/send_batch 增加通道关闭状态前置检查

**Date**: 2026-06-29
**Module**: channel.rs
**Trigger**: 代码审阅中发现 send() 和 send_batch() 在通道已被 close() 关闭后仍尝试写入缓冲区，违反关闭语义

---

#### BUG-07: send() / send_batch() 未检查通道关闭状态

- **问题**: `send()` 和 `send_batch()` 在通道已被 `close()` 关闭后仍会尝试写入缓冲区。虽然不会导致数据损坏，但违反了"关闭即停止写入"的语义契约。生产者应该在关闭后立即得到失败反馈，而不是继续写入。
- **修复**: `send()` 入口增加 `if self.is_closed() { return false }`，`send_batch()` 入口增加 `if self.is_closed() { return 0 }`。
- **影响**: Channel::send, Channel::send_batch

#### 验证结果
- 所有 33 个 basic 测试通过

---

### Session 4: signal.rs — 信号集合操作优化

**Date**: 2026-06-29
**Module**: signal.rs
**Trigger**: 代码审阅中发现 coalesce_pending/deliverable 存在冗余循环，边界检查风格不一致

---

#### BUG-08: coalesce_pending() 冗余循环

- **问题**: 循环逐位复制 `pending & !blocked` 的结果，完全多余
- **修复**: 直接返回 `self.pending & !self.blocked & !1`
- **影响**: SigSet::coalesce_pending

#### BUG-09: deliverable() 循环查找 → trailing_zeros()

- **问题**: for 循环从 1 到 NSIG 逐位查找第一个可投递信号，O(n)
- **修复**: 使用 `u64::trailing_zeros()` 在 O(1) 内定位最低置位
- **影响**: SigSet::deliverable

#### BUG-10: actions.len() → NSIG 常量统一

- **问题**: get_action/is_ignored/clear_non_caught 用 .len() 做边界检查，与其他方法用 NSIG 不一致
- **修复**: 统一改用 `signo < NSIG` 或 `1..NSIG as usize`
- **影响**: SigSet::get_action, SigSet::is_ignored, SigSet::clear_non_caught

#### 验证结果

- 所有 33 个 basic 测试通过

---

### Session 5: sync.rs — KernLock::leave() 移除无用原子读

**Date**: 2026-06-29
**Module**: sync.rs

#### BUG-11: KernLock::leave() 中 holder 读取未使用
- 问题：`let h = self.holder.load(Ordering::Relaxed)` 读取后变量 h 从未被引用
- 修复：删除该行

#### 验证结果

- 所有 33 个 basic 测试通过

---

## Session 6 — FutexBucket / FutexTable 哈希桶改造

**Date**: 2026-07-01
**Module**: sync.rs / process.rs

#### BUG-13: FutexBucket / FutexTable 单一大队列 → 哈希多桶（已修复）

- **问题**：`FutexBucket` 名为"地址级等待队列"，实际是单一大 `VecDeque`，
  所有地址的 waiters 混在一起。`wake/requeue/pending_at` 全部线性扫描（O(n)），
  且一把 `Mutex` 被所有地址共享，不同地址的 wait/wake 互相阻塞。
  `process.rs` 的 `BTreeMap<usize, Arc<FutexBucket>>` 外层已按地址分桶，
  导致桶内 `addr` 字段实际冗余（逻辑错位）。
- **修复（Linux 风格 futex_hash_bucket）**：
  1. `FutexBucket` 内部维护 `NUM_FBUCKETS=256` 个独立桶，每桶一把 `Mutex`
  2. `FutexTable` 内部维护 `NUM_FTBUCKETS=128` 个独立桶（轻量场景）
  3. 哈希函数：`hash(addr) = ((addr >> 2) ^ (addr >> 13)) & (N-1)`，位混洗风格
  4. 查桶 O(1)，不同地址走不同 `Mutex`，锁争用下降 N 倍
  5. 新增 `enqueue(addr, ...)` 辅助方法：供外部直接入队（已构造 flag 的场景）
  6. 新增 `lock_ordered(si, di)`：跨桶 requeue 按索引小→大顺序加锁避免死锁
  7. `requeue(src, dst, ...)` 支持两种路径：
     - 同桶（src 与 dst 哈希到同一桶）：退化为单桶扫描，src 条目就地改写为 dst
     - 跨桶：src 桶 `retain` 取出 `wake_n + move_n` 个，wake 后把 move_n 个追加到 dst 桶
  8. `process.rs` 的 `Task.futexes` 从 `Mutex<BTreeMap<usize, Arc<FutexBucket>>>`
     简化为 `Arc<FutexBucket>`（单个哈希表），`get_futex()` 直接返回 `Arc` 克隆，O(1)
  9. 顺手修复了原 `FutexTable::ftx_wake` 的 off-by-one：`wk <= limit` 改为 `wk < count`
- **原 API 签名保持向后兼容**：`wait/wake/requeue/pending_at` 和
  `ftx_wait/ftx_wake/ftx_requeue` 接口不变；仅内部实现从单队列换成哈希桶。
- **验证**：`cargo test --workspace --test basic` → 33/33 通过。
- 详细分析见 `sync.rs` 底部 `Sync Debug Notes` 的 `[BUG-13]` 注释块。

#### BUG-14: SyncQueue 三处冗余 / 语义问题（已修复）

1. **`signal()` 的冗余分支**：`match q.len()` 的 `1 =>` 与 `_ =>` 完全相同
   （都是 `pop_front().unwrap()` + `unpark`）。修复：合并为 `if q.is_empty() { ... } else { ... }`。
2. **`signal_n()` 的冗余 None 分支**：`to_wake = n.min(q.len())` 已界定循环次数，
   但循环体内仍用 `match pop_front() { Some/None }`，None 分支不可能走到。
   修复：改为 `q.pop_front().unwrap()`。
3. **`wait_timeout()` 返回值永远是 true**：调用方无法区分"被 signal 唤醒"与"超时到期"，
   且超时后线程不会把自己从队列里摘掉，导致队列泄漏（虚假唤醒/超时唤醒的残留条目）。
   该函数在 `kernel-refactored` 和 `chaos-tests-refactored` 中**均无调用方**。
   修复：`park_timeout` 后检查线程是否仍在队列里，在队列里则自己 `remove` 并返回 `false`（超时）；
   不在则返回 `true`（被 signal/broadcast 唤醒）。
4. **`park_on()` 的 `if n > 256 { let _trim = n >> 3; }` 是无效预留**：`_trim` 变量立刻被丢弃，
   虚假唤醒 / 超时唤醒未出队会导致队列无限增长。
   修复：`q.len() > 256` 时 `pop_front` 出最老的 `n>>3` 个等待者并 `unpark` 它们
   （stale waiter cleanup）。被剔除的线程在用户态重检 pred：pred 为 true 则消费数据，
   为 false 则再次 park_on 重新入队。这样既限制了队列长度，又不破坏 park_on 的返回语义。
   注意必须 `unpark` 被剔除线程，否则它们会永远挂在 `park()` 上。
   （早期曾尝试"不入队 + pending_signals+=1 + 直接返回 true"的方案 A，但该方案让调用方
   拿到 true 时 pred 实际并未满足，属于虚假成功，已被否决。）
- **验证**：33/33 测试全过。
- 详见 `sync.rs` 中 `impl SyncQueue` 后的 `[BUG-14]` 注释块。

---

## Session 7 — SemArr 时间戳改为真实时钟

**Date**: 2026-07-01
**Module**: ipc.rs

#### BUG-15: SemArr otime_now/ctime_now 占位 0 → 真实 Unix 时间戳（已修复）

- **问题**：`SemArr::otime_now()` 与 `ctime_now()` 原本写入 0 作为占位，
  导致用户态通过 `semctl(IPC_STAT)` 读到的"最后 semop 时间 / 最后属性修改时间"
  永远是 0，无法反映真实操作历史。
- **修复**：
  1. 新增 `now_secs()` 辅助函数：
     `SystemTime::now().duration_since(UNIX_EPOCH).as_secs() as usize`
  2. `otime_now` / `ctime_now` 改为写入 `now_secs()` 的真实 Unix 时间戳（秒）
  3. 失败时（系统时钟早于 UNIX_EPOCH，正常不该发生）兜底回 0
- **验证**：33/33 测试全过。
- 详见 `ipc.rs` 中 `impl SemArr` 后的 `[BUG-15]` 注释块。

---

## Session 8 — TrapCtl 死代码清理与 dispatch 语义修复

**Date**: 2026-07-01
**Module**: trap.rs

#### BUG-16: TrapCtl 死代码清理 + dispatch 语义重写（已修复）

- **问题**：trap.rs 中存在大量死代码和 dispatch 空壳：
  1. `set_ip()` / `set_sp()` 保存旧值 `_old` 但从未使用
  2. `apply()` 中 `_checksum` 校验和计算后丢弃
  3. `configure()` 中 `combined` 和 `_parity` 奇偶校验位纯浪费 CPU
  4. `hw()` / `sw()` 中 `_check` 重复 load 同一原子变量
  5. `handle_irq()` 中 `_nest_before` / `_supp` / `_suppressed_tick` 全为死变量
  6. `on_pgfault()` 中 `_page` / `_offset` 计算后未使用
  7. `clone_with_ret()` 的 while 循环可简化为 for
  8. **dispatch() 是 no-op**：保存帧 → nest +1/-1 → 原样返回 ctx
  9. **dispatch_vector() 中 vector 14 不可达**：被 `8..=15` match 臂覆盖
  10. **handle_irq() 与 dispatch() 重复**：帧保存两次、nest ±1 成对抵消
  11. **缺页双重故障检测写了两遍**
  12. **dispatch 无 ISR 回调机制**：所有向量分支都是空操作
  13. **save/restore 循环不完整**：保存了上下文但恢复时直接返回入参

- **修复**（对标 x86/RISC-V 标准两层 trap 分发范式）：
  1. 删除所有 `_` 前缀死变量和无副作用的计算块
  2. 手动 Context 构造全部改用 `ctx.clone()`
  3. TrapCtl 新增 **handlers** 字段：16 路 ISR 回调表
     `Mutex<Box<[Option<Box<dyn Fn(&mut Context)+Send>>; 16]>>`
     内核通过 `register_handler(vector, callback)` 注入真实处理逻辑
  4. `dispatch(vector, ctx)` 重写为标准统一 trap handler：
     ① 保存 ctx 到帧槽（move，不 clone）
     ② active=true, nest+1
     ③ 查 handlers[vector]，若已注册则调用回调（可修改帧槽中 Context）
     ④ 从帧槽读出（可能被修改的）上下文——完成 save/restore 循环
     ⑤ nest-1, active=false → 返回恢复后的上下文
  5. **删除 handle_irq()**：逻辑内联到 dispatch 的回调机制中
  6. `on_pgfault()` 改为返回 `(page, offset)`，不再做死计算
  7. `dispatch_vector()` 重排 match：14 放在 8..=15 之前，缺页不可屏蔽

- **验证**：33/33 测试全过。
- 详见 `trap.rs` 中 `impl TrapCtl` 后的 `[BUG-16]` 注释块。

---

## Session 9 — check_access_rw 三层校验实现 + 死代码清理

**Date**: 2026-07-01
**Module**: util.rs, trap.rs

#### BUG-17: check_access_rw 三层校验实现 + cfu/validate_access 死代码清理（已修复）

- **问题**：`check_access_rw` 原意是三层增强地址校验，但第 2、3 层算了却没用：
  1. `_span_check`：计算了页面数是否超过 KHEAP_SZ 限制，但结果未使用
  2. `_alignment_ok`：计算了写操作地址对齐，但结果未使用
  3. `crosses_kern`：中间布尔变量，可内联
  4. 最终返回 `boundary < KERN_BASE` 与 `check_access` 完全等价
  5. `cfu()` 中 `_alignment` 对齐检查算了但没用
  6. `validate_access()` mode 1 中 `_pages` 页统计算了但没用

- **修复**：
  1. `check_access_rw` 重写为三层真实校验：
     - 第 1 层：`checked_add` 溢出 + `KERN_BASE` 边界
     - 第 2 层：`n_pages > KHEAP_SZ/PAGE_SZ` → 拒绝
     - 第 3 层：`writable` 时 `addr % sizeof(usize) != 0` 且 `len >= align` → 拒绝
  2. `cfu()` 删除死变量 `_alignment`
  3. `validate_access()` mode 1 删除死变量 `_pages`

- **验证**：33/33 测试全过。
- 详见 `util.rs` 中 `ctu()` 后的 `[BUG-17]` 注释块。

---

## 待处理 / TODO

- 继续审阅其他模块，发现不直观或有潜在 bug 的代码时记录在此。

---

## 约定

1. 每个 bug 用 `BUG-XX` 编号（与本工作流内的修改对应）。
2. 源码注释中以 `[BUG-XX]` 标记对应修改，方便追溯。
3. 修复后必须运行以下命令验证：
   ```bash
   cargo test --workspace --test basic
   ```
4. 提交前确认所有测试通过，不引入回归。
