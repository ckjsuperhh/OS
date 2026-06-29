# Timer 模块阅读指南

> 文件路径: `kernel-refactored/src/timer.rs`
> 代码量: 99 行 | 2 个核心结构体 | 依赖: `consts`（TIMER_WHEEL_SIZE）, `util`（CLK）

---

## 一、模块概述

`timer.rs` 实现了内核中的 **定时器管理系统**，基于经典的 **时间轮（Timer Wheel）** 算法提供定时任务的注册、到期检测和取消功能。

| 结构体 | 用途 | 类比 |
|---|---|---|
| `TimerEntry` | 单个定时器条目：到期时间、间隔、回调、状态 | Linux `timer_list` |
| `TimerWheel` | 时间轮调度器：基于槽位的定时器集合管理 | Linux `tvec_base` |

**设计定位：** Timer 模块在内核中扮演"闹钟"的角色——内核子系统（如进程调度器的时间片耗尽、网络重传、`alarm()` 系统调用）通过 `add_timer()` 注册定时任务，时钟中断驱动 `advance()` 逐槽检查到期定时器并触发回调。

```
  时钟中断 (TIMER_TICK_HZ = 100Hz, 每 10ms 一次)
       │
       ▼
  timer_wheel.advance()
       │
       ▼
  current_slot = (current_slot + 1) % 256
       │
       ▼
  检查当前槽中的所有 TimerEntry
       │
       ├── expired() && active ──► 触发回调（加入 fired 列表）
       │       │
       │       └── repeat? ──► reset() + 重新插入新槽
       │
       ├── active 但未到期 ──► 保留在槽中
       │
       └── !active ──► 丢弃（已取消的定时器）
```

---

## 二、TimerEntry — 定时器条目

### 2.1 结构体定义

```rust
pub struct TimerEntry {
    /// 到期时间戳（以 tick 为单位的绝对时间）
    /// 当全局时钟 CLK 超过此值时，定时器到期
    pub deadline: usize,
    /// 重复间隔（tick 数）。0 表示一次性定时器，>0 表示周期性
    pub interval: usize,
    /// 回调函数 ID——内核维护一个回调表，通过 ID 索引实际函数
    pub callback_id: usize,
    /// 定时器是否处于激活状态（false 表示已取消或已过期）
    pub active: bool,
    /// 是否循环触发（由 interval > 0 自动推导）
    pub repeat: bool,
}
```

**字段设计要点：**
- `callback_id` 而非函数指针：使用 ID 而非 `fn()` 指针，避免了 Rust 中函数指针的生命周期问题和 `Send`/`Sync` 约束
- `repeat` 是 `interval > 0` 的冗余标记：空间换时间，避免每次判断 `interval > 0`
- `deadline` 是绝对时间而非相对时间：避免了每次 tick 都要递减的开销

### 2.2 构造函数

```rust
/// 创建一个新的定时器条目
/// deadline: 到期的绝对时间戳
/// interval: 重复间隔（0 = 一次性，>0 = 周期性）
/// cb_id: 回调函数 ID
pub fn new(deadline: usize, interval: usize, cb_id: usize) -> Self {
    Self {
        deadline,
        interval,
        callback_id: cb_id,
        active: true,                    // 创建即激活
        repeat: interval > 0,            // 自动推导是否为周期定时器
    }
}
```

### 2.3 到期检测 — `expired()`

```rust
/// 检查定时器是否已到期
/// 通过原子加载全局时钟 CLK，与 deadline 比较
pub fn expired(&self) -> bool {
    CLK.load(Ordering::Relaxed) > self.deadline
}
```

**`Ordering::Relaxed` 的合理性：** 时钟值是单调递增的，即使读到稍微过时的值也不会导致逻辑错误（最多延迟一个 tick 检测到到期），因此使用最宽松的内存序。

### 2.4 重置/续期 — `reset()`

```rust
/// 重置定时器
/// 周期定时器：将 deadline 延长一个 interval
/// 一次性定时器：标记为非激活
pub fn reset(&mut self) {
    if self.repeat {
        self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
    } else {
        self.active = false;
    }
}
```

**注意：** 周期定时器的新 deadline 基于"当前时间"而非"旧 deadline"计算。这意味着如果因为处理延迟导致 `reset()` 调用晚于预期，新的 deadline 不会累积误差——但可能跳过某些预期的触发。

### 2.5 剩余时间 — `remaining()`

```rust
/// 返回距离到期的剩余 tick 数
/// 如果已到期返回 0
pub fn remaining(&self) -> usize {
    let now = CLK.load(Ordering::Relaxed);
    if now >= self.deadline { 0 } else { self.deadline - now }
}
```

### 2.6 取消 — `cancel()`

```rust
/// 取消定时器（标记为非激活）
/// 不会从时间轮中移除，而是在 advance() 时被跳过
pub fn cancel(&mut self) { self.active = false; }
```

---

## 三、TimerWheel — 时间轮调度器

### 3.1 结构体定义

```rust
pub struct TimerWheel {
    /// 时间轮槽数组，每个槽是一个 TimerEntry 向量
    /// 槽数由 TIMER_WHEEL_SIZE 决定（默认 256）
    pub slots: Vec<Vec<TimerEntry>>,
    /// 当前指针位置（每次 advance 前进一格）
    pub current_slot: usize,
}
```

**时间轮原理：**

时间轮是一种高效的定时器管理数据结构，核心思想是用一个环形数组（轮）来组织定时器：

```
  TIMER_WHEEL_SIZE = 256

      slot[0]    slot[1]    slot[2]   ...  slot[255]
    ┌────────┐ ┌────────┐ ┌────────┐     ┌────────┐
    │ timer1 │ │        │ │ timer3 │     │ timer5 │
    │ timer2 │ │        │ │        │     │        │
    └────────┘ └────────┘ └────────┘     └────────┘
                    ^
               current_slot
               (指针逐格前进)
```

**插入算法：** `slot = deadline % TIMER_WHEEL_SIZE`

- 因为 256 是 2 的幂，取模等价于位与（`& 0xFF`），非常快
- 两个 deadline 相差 256 的倍数的定时器会落入同一槽——通过 `expired()` 的精确比较来区分

### 3.2 构造函数

```rust
/// 创建一个空的时间轮，256 个槽全部为空
pub fn new() -> Self {
    let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
    for _ in 0..TIMER_WHEEL_SIZE {
        slots.push(Vec::new());
    }
    Self { slots, current_slot: 0 }
}
```

### 3.3 添加定时器 — `add_timer()`

```rust
/// 将定时器条目插入时间轮
/// 根据 deadline 取模确定目标槽位
pub fn add_timer(&mut self, entry: TimerEntry) {
    let slot = entry.deadline % TIMER_WHEEL_SIZE;
    self.slots[slot].push(entry);
}
```

**复杂度：** O(1)——只需一次取模和一次 push。

### 3.4 推进时间轮 — `advance()`

这是 TimerWheel 最核心（也最复杂）的方法，在每次时钟中断时被调用。

```rust
/// 推进时间轮一格，返回本轮到期的所有定时器
pub fn advance(&mut self) -> Vec<TimerEntry> {
    // 1. 指针前移一格（环形）
    self.current_slot = (self.current_slot + 1) % TIMER_WHEEL_SIZE;

    // 2. 收集当前槽中到期的定时器
    let mut fired = Vec::new();       // 到期的定时器
    let slot = &mut self.slots[self.current_slot];
    let mut remaining = Vec::new();   // 未到期但仍活跃的定时器

    // 3. 遍历当前槽中的所有条目
    for entry in slot.drain(..) {
        if entry.active && entry.expired() {
            fired.push(entry);          // 到期且活跃 → 触发
        } else if entry.active {
            remaining.push(entry);      // 活跃但未到期 → 保留
        }
        // !active 的条目直接丢弃
    }

    // 4. 将未到期条目放回槽中
    *slot = remaining;

    // 5. 处理周期定时器：重置并重新插入
    for t in fired.iter_mut() {
        if t.repeat {
            t.reset();    // deadline += interval
            let new_slot = t.deadline % TIMER_WHEEL_SIZE;
            let clone = TimerEntry::new(t.deadline, t.interval, t.callback_id);
            self.slots[new_slot].push(clone);
        }
    }

    fired  // 返回所有本次到期的定时器
}
```

**流程图：**

```
advance() 被调用（每 10ms 一次）
    │
    ▼
current_slot = (current_slot + 1) % 256
    │
    ▼
drain 当前槽中的所有 TimerEntry
    │
    ├── active && expired() ──► 加入 fired 列表
    │       │
    │       └── repeat? ──► reset() → 计算新槽位 → 插入 clone
    │
    ├── active && !expired() ──► 加入 remaining → 放回槽中
    │
    └── !active ──► 丢弃
    │
    ▼
返回 fired 列表（调用者遍历执行回调）
```

**注意 `clone` 的创建方式：** 代码对周期定时器创建了一个全新的 `TimerEntry` 而非移动原始条目，因为原始条目需要保留在 `fired` 列表中返回给调用者。

### 3.5 取消定时器 — `cancel()`

```rust
/// 按 callback_id 取消第一个匹配的活跃定时器
/// 返回 true 表示找到并取消了，false 表示未找到
pub fn cancel(&mut self, cb_id: usize) -> bool {
    for slot in self.slots.iter_mut() {
        for entry in slot.iter_mut() {
            if entry.callback_id == cb_id && entry.active {
                entry.active = false;  // 标记为非活跃（延迟删除）
                return true;
            }
        }
    }
    false
}
```

**复杂度：** O(N) 最坏情况——需要遍历所有槽中的所有条目。这是简单时间轮的一个缺点。生产级内核使用多级时间轮（如 Linux 的 4 级 `tvec`），可以在 O(1) 内取消。

### 3.6 活跃定时器计数 — `active_count()`

```rust
/// 统计所有槽中活跃的定时器总数
pub fn active_count(&self) -> usize {
    self.slots.iter()
        .flat_map(|s| s.iter())    // 展平所有槽
        .filter(|e| e.active)       // 只计数活跃的
        .count()
}
```

---

## 四、使用场景

### 4.1 alarm() 系统调用

```rust
// 用户进程调用 alarm(seconds) 设置闹钟
fn sys_alarm(seconds: u32) -> u32 {
    let deadline = CLK.load(Ordering::Relaxed) + seconds * TIMER_TICK_HZ;
    let entry = TimerEntry::new(deadline, 0, SIGALRM as usize);
    timer_wheel.add_timer(entry);
    0
}
// 到期后 advance() 返回该条目，内核向进程投递 SIGALRM
```

### 4.2 进程时间片管理

```rust
// 调度器为每个进程设置时间片到期定时器
let deadline = CLK.load(Ordering::Relaxed) + TIME_SLICE;
let entry = TimerEntry::new(deadline, 0, TIMESLICE_CB_ID);
timer_wheel.add_timer(entry);
// 到期后触发调度器切换到下一个进程
```

### 4.3 周期性任务

```rust
// 注册一个每 100 tick (1 秒) 触发的周期定时器
let entry = TimerEntry::new(
    CLK.load(Ordering::Relaxed) + 100,  // 首次到期
    100,                                 // 每 100 tick 重复
    PERIODIC_CB_ID
);
timer_wheel.add_timer(entry);
```

### 4.4 时钟中断处理流程

```rust
// 每次时钟中断（100Hz）
fn timer_interrupt_handler() {
    CLK.fetch_add(1, Ordering::Relaxed);  // 全局时钟 +1
    let fired = timer_wheel.advance();     // 推进时间轮
    for entry in fired {
        dispatch_callback(entry.callback_id);  // 执行回调
    }
}
```

---

## 五、跨模块连接

```
timer.rs
├── consts.rs  — TIMER_WHEEL_SIZE (256), TIMER_TICK_HZ (100), BOOT_EPOCH (0)
├── util.rs    — CLK（全局原子时钟计数器，advance 和 expired 通过它判断到期）
├── signal.rs  — 定时器到期可能触发 SIGALRM
├── proc.rs    — 调度器使用时间轮管理进程时间片
│               每个进程可能持有自己的定时器列表
└── 中断处理    — 时钟中断驱动 advance()
```

---

## 六、与原版 kernel.rs 的对应

| timer.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| `TimerEntry` 结构体 | 约在定时器相关函数定义前 |
| `TimerWheel` 结构体 | 全局变量区或内核初始化区 |
| `advance()` | 时钟中断处理函数中 |
| `add_timer()` / `cancel()` | 系统调用 `alarm()` / `setitimer()` 实现中 |

---

## 七、潜在的改进方向

1. **单级时间轮的精度问题**：当 `TIMER_WHEEL_SIZE = 256` 时，超过 256 tick 的定时器会与较近的定时器落入同一槽。`advance()` 中的 `expired()` 检查能正确区分，但会导致某些槽积累过多条目。生产级实现使用多级时间轮（如 Linux 的 `tv1`~`tv4`，4 级级联）
2. **`advance()` 中的 clone 开销**：对周期定时器创建新 `TimerEntry` 是必要的（原条目要返回给调用者），但可以考虑用 `Cow` 或 Arc 共享
3. **`cancel()` 的 O(N) 复杂度**：可通过维护 `callback_id → slot_index` 的哈希映射优化为 O(1)
4. **缺少线程安全**：`TimerWheel` 没有使用任何同步原语。如果时钟中断和其他线程（如 `sys_alarm`）并发访问会竞态。应使用 `Mutex` 或 `Spin` 保护
5. **`active_count()` 的 O(N) 复杂度**：每次遍历所有槽所有条目。可维护一个 `active_count: usize` 字段，在 add/cancel/fire 时增量更新
6. **`advance()` 只前进一格**：如果某次时钟中断处理耗时过长导致跳过了多个 tick，当前实现会丢失中间槽的检查。可改为循环前进直到追上实际时钟
