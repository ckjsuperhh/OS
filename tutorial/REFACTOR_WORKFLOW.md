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
| Sync | `sync.rs` | Session 5（BUG-11 KernLock::leave() 移除无用原子读）已完成 |

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
