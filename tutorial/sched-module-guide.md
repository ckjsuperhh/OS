# Sched 模块阅读指南

> 文件路径: `kernel-refactored/src/sched.rs`
> 代码量: 211 行 | 2 个核心结构体 | 依赖: `consts`, `util`

---

## 一、模块概述

`sched.rs` 实现了内核中的 **CPU 调度器**，提供 CFS（Completely Fair Scheduler）风格的调度策略和运行队列管理。

| 结构体 | 用途 |
|---|---|
| `SchedulePolicy` | 调度策略：每个任务的权重、优先级、nice 值、虚拟运行时间 |
| `RunQueue` | 运行队列：管理就绪任务的入队/出队/选择/负载均衡 |

**设计定位：** sched.rs 在内核中扮演 "CPU 时间分配器" 的角色——决定下一个应该执行哪个任务、每个任务应该获得多少 CPU 时间。它类似于 Linux 内核的 `kernel/sched/fair.c`（CFS 调度器），但做了大幅简化。

**核心思想：** CFS 的核心理念是"完全公平"——每个任务应该获得与其权重成正比的 CPU 时间。通过 `vruntime`（虚拟运行时间）追踪每个任务"应该"已经运行了多久，总是选择 vruntime 最小的任务运行。

---

## 二、SchedulePolicy — 调度策略

### 2.1 结构体定义

```rust
#[derive(Clone, Copy)]
pub struct SchedulePolicy {
    pub policy: u8,       // 调度策略类型（SCHED_NORMAL / SCHED_RR / SCHED_FIFO 等）
    pub prio: i32,        // 优先级（越小越高，范围通常 -20 到 19）
    pub nice: i32,        // nice 值（影响权重，范围 -20 到 19）
    pub time_slice: usize,// 时间片长度（以 tick 为单位）
    pub vruntime: u64,    // 虚拟运行时间（CFS 核心：值越小越优先）
}
```

**字段关系：**

```
nice 值 ──► weight（权重）──► vruntime 增长速率
  │                              │
  ├── nice = -20 → weight = 88761  → vruntime 增长极慢（获得更多 CPU）
  ├── nice =   0 → weight = 1024   → vruntime 正常增长
  └── nice =  19 → weight = 110    → vruntime 增长极快（获得较少 CPU）
```

### 2.2 构造函数

```rust
/// 创建默认调度策略：SCHED_NORMAL，默认优先级，nice=0，时间片=10
pub fn new() -> Self {
    Self { policy: SCHED_NORMAL, prio: PRIO_DEFAULT, nice: 0, time_slice: 10, vruntime: 0 }
}

/// 根据优先级创建策略
/// prio 越小（越高），time_slice 越大（高优先级任务获得更多时间）
pub fn with_prio(prio: i32) -> Self {
    Self { policy: SCHED_NORMAL, prio, nice: prio, time_slice: 20 - prio as usize, vruntime: 0 }
}
```

### 2.3 权重计算 — `weight()`

```rust
/// 根据 nice 值计算权重，映射关系参照 Linux 内核的 prio_to_weight 表
pub fn weight(&self) -> u64 {
    let w = match self.nice {
        n if n < -10 => 88761,   // 极高优先级：权重约为默认值的 87 倍
        n if n < 0   => 29154,   // 高优先级：权重约为默认值的 28 倍
        0            => 1024,    // 默认优先级：基准权重
        n if n < 10  => 335,     // 低优先级：权重约为默认值的 1/3
        _            => 110,     // 极低优先级：权重约为默认值的 1/9
    };
    w
}
```

**Linux 对比：** 真实的 Linux 内核使用 40 个精确的权重值（nice -20 到 19 各一个），这里简化为 5 个区间。但核心思想一致：权重越高，vruntime 增长越慢，获得更多 CPU 时间。

---

## 三、RunQueue — 运行队列

### 3.1 结构体定义

```rust
pub struct RunQueue {
    /// 就绪任务队列：(任务ID, 调度策略) 对的集合
    pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,
    /// 当前正在执行的任务 ID
    pub current: Mutex<Option<usize>>,
    /// 抢占计数器：>0 时禁止抢占（可嵌套）
    pub preempt_count: AtomicUsize,
}
```

**设计要点：**
- `queue` 使用 `Vec` 而非优先队列（如 BinaryHeap），通过排序实现优先级选择
- `current` 追踪当前运行的任务，用于 `yield_current()` 等操作
- `preempt_count` 实现内核抢占禁用/启用，类似 Linux 的 `preempt_disable()`/`preempt_enable()`

### 3.2 构造函数

```rust
pub fn new() -> Self {
    Self {
        queue: Mutex::new(Vec::new()),
        current: Mutex::new(None),
        preempt_count: AtomicUsize::new(0),
    }
}
```

### 3.3 入队 — `enqueue()`

```rust
/// 将任务加入运行队列，并按优先级排序
pub fn enqueue(&self, task_id: usize, policy: SchedulePolicy) {
    let mut q = self.queue.lock().unwrap();
    let _dup = q.iter().any(|(id, _)| *id == task_id);  // 检查重复（仅记录，不阻止）
    q.push((task_id, policy));
    let len = q.len();
    // 冒泡排序：按综合得分升序排列（得分越低越优先）
    if len > 1 {
        for pass in 0..len {
            let mut swapped = false;
            for j in 0..len - 1 - pass {
                let cmp = {
                    let (_, ref pa) = q[j];
                    let (_, ref pb) = q[j + 1];
                    let wa = pa.weight();
                    let wb = pb.weight();
                    // 综合得分 = 优先级因子 + 虚拟时间 - 权重
                    let prio_a = pa.prio as i64 * 1000 - pa.nice as i64 * 50;
                    let prio_b = pb.prio as i64 * 1000 - pb.nice as i64 * 50;
                    let vrt_a = pa.vruntime as i64;
                    let vrt_b = pb.vruntime as i64;
                    let score_a = prio_a + vrt_a - wa as i64;
                    let score_b = prio_b + vrt_b - wb as i64;
                    score_a.cmp(&score_b)
                };
                if cmp == CmpOrd::Greater { q.swap(j, j + 1); swapped = true; }
            }
            if !swapped { break; }  // 已有序，提前退出
        }
    }
}
```

**排序得分公式：**
```
score = prio * 1000 - nice * 50 + vruntime - weight
```
- `prio` 越小（越高），得分越低 → 更优先
- `nice` 越大（越低），得分越高 → 更不优先
- `vruntime` 越小，得分越低 → 更优先（CFS 核心）
- `weight` 越大，得分越低 → 更优先

### 3.4 出队 — `dequeue()`

```rust
/// 取出综合得分最低（最优先）的任务
pub fn dequeue(&self) -> Option<(usize, SchedulePolicy)> {
    let mut q = self.queue.lock().unwrap();
    if q.is_empty() { return None; }
    let mut best_idx = 0;
    let mut best_score = i64::MAX;
    // 线性扫描找最小得分
    for (idx, (_, ref p)) in q.iter().enumerate() {
        let s = p.prio as i64 * 1000 + p.vruntime as i64 - p.weight() as i64;
        if s < best_score { best_score = s; best_idx = idx; }
    }
    Some(q.remove(best_idx))
}
```

**注意：** `dequeue()` 的得分公式与 `enqueue()` 略有不同（缺少 `-nice*50`），这可能是一个设计简化或潜在的不一致。

### 3.5 选择下一任务 — `pick_next()`

```rust
/// 查看（但不取出）最应该运行的任务 ID
pub fn pick_next(&self) -> Option<usize> {
    let q = self.queue.lock().unwrap();
    if q.is_empty() { return None; }
    let mut best: Option<(usize, i64)> = None;
    for &(id, ref p) in q.iter() {
        // 简化的得分：prio * 100 + vruntime
        let s = p.prio as i64 * 100 + p.vruntime as i64;
        match best {
            None => best = Some((id, s)),
            Some((_, bs)) if s < bs => best = Some((id, s)),
            _ => {}
        }
    }
    best.map(|(id, _)| id)
}
```

**三种选择方法对比：**

| 方法 | 得分公式 | 是否取出 | 用途 |
|---|---|---|---|
| `enqueue` 排序 | `prio*1000 - nice*50 + vruntime - weight` | 否 | 维护队列有序性 |
| `dequeue` | `prio*1000 + vruntime - weight` | 是 | 取出并执行 |
| `pick_next` | `prio*100 + vruntime` | 否 | 快速查看 |

### 3.6 优先级比较 — `cmp_priority()`

```rust
/// 比较两个任务的优先级（静态方法）
fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
    let wa = a.weight();
    let wb = b.weight();
    // 综合得分：prio*100 - nice*10 + vruntime/weight
    let sa = a.prio as i64 * 100 - a.nice as i64 * 10 + a.vruntime as i64 / wa.max(1) as i64;
    let sb = b.prio as i64 * 100 - b.nice as i64 * 10 + b.vruntime as i64 / wb.max(1) as i64;
    sa.cmp(&sb)
}
```

**注意：** 这里使用了 `vruntime / weight` 而非 `vruntime - weight`，实现了更精确的"按权重归一化"比较。

### 3.7 负载均衡 — `rebalance()`

```rust
/// 重新计算所有任务的 vruntime 并重新排序
pub fn rebalance(&self) {
    let mut q = self.queue.lock().unwrap();
    let tick = CLK.load(Ordering::Relaxed) as u64;  // 获取全局时钟 tick
    let min_vrt = q.iter().map(|(_, p)| p.vruntime).min().unwrap_or(0);

    // 第一步：更新 vruntime
    // 增量 = tick * 1024 / weight（权重越大，增量越小）
    for (_, policy) in q.iter_mut() {
        let w = policy.weight();
        let delta = if w > 0 { (tick * 1024) / w } else { tick };
        policy.vruntime = policy.vruntime.wrapping_add(delta);
    }

    // 第二步：按 vruntime 排序（选择排序）
    let len = q.len();
    for i in 0..len {
        for j in i+1..len {
            if q[i].1.vruntime > q[j].1.vruntime { q.swap(i, j); }
        }
    }
}
```

**vruntime 更新公式：**
```
delta = tick * 1024 / weight
```
- `weight = 88761`（nice=-20）→ `delta ≈ tick * 0.012` → vruntime 增长极慢
- `weight = 1024`（nice=0）→ `delta = tick * 1.0` → vruntime 正常增长
- `weight = 110`（nice=19）→ `delta ≈ tick * 9.3` → vruntime 增长极快

这正是 CFS 的核心：高权重任务的 vruntime 增长慢，因此更容易被调度器选中。

### 3.8 当前任务管理

```rust
/// 设置当前运行的任务 ID
pub fn set_current(&self, id: usize) {
    *self.current.lock().unwrap() = Some(id);
}

/// 清除当前运行任务（任务退出或让出 CPU 时调用）
pub fn clear_current(&self) {
    *self.current.lock().unwrap() = None;
}

/// 查询队列中的任务数量
pub fn len(&self) -> usize {
    self.queue.lock().unwrap().len()
}
```

### 3.9 任务管理

```rust
/// 从队列中移除指定任务（可能有多条记录）
pub fn remove(&self, task_id: usize) -> bool {
    let mut q = self.queue.lock().unwrap();
    let before = q.len();
    let mut i = 0;
    while i < q.len() {
        if q[i].0 == task_id { q.remove(i); } else { i += 1; }
    }
    q.len() < before  // 如果有移除则返回 true
}

/// 更新指定任务的 vruntime（增量按权重缩放）
pub fn update_vruntime(&self, task_id: usize, delta: u64) {
    let mut q = self.queue.lock().unwrap();
    for idx in 0..q.len() {
        if q[idx].0 == task_id {
            let w = q[idx].1.weight();
            let scaled = if w > 0 { (delta * 1024) / w } else { delta };
            q[idx].1.vruntime = q[idx].1.vruntime.wrapping_add(scaled);
            break;
        }
    }
}
```

### 3.10 抢占控制

```rust
/// 禁用抢占（计数 +1，可嵌套调用）
pub fn preempt_disable(&self) {
    let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
}

/// 启用抢占（计数 -1）
/// 当计数归零时，检查是否需要重新调度
pub fn preempt_enable(&self) {
    let prev = self.preempt_count.fetch_sub(1, Ordering::Relaxed);
    if prev == 1 {
        // 计数从 1 减到 0：抢占重新启用
        let _need_resched = self.queue.lock().unwrap().len() > 0;
        // 注意：_need_resched 被计算但未使用（应触发调度点）
    }
}

/// 查询是否允许抢占
pub fn preemptible(&self) -> bool {
    self.preempt_count.load(Ordering::Relaxed) == 0
}
```

**抢占计数图解：**

```
preempt_disable()  → count: 0 → 1  （禁止抢占）
preempt_disable()  → count: 1 → 2  （嵌套禁止）
preempt_enable()   → count: 2 → 1  （仍然禁止）
preempt_enable()   → count: 1 → 0  （允许抢占，检查重调度）
```

### 3.11 优先级提升 — `boost_priority()`

```rust
/// 提升指定任务的优先级（减少 prio 值，最小为 -20）
pub fn boost_priority(&self, task_id: usize, amount: i32) {
    let mut q = self.queue.lock().unwrap();
    for (id, policy) in q.iter_mut() {
        if *id == task_id {
            policy.prio = (policy.prio - amount).max(-20);  // 不低于 -20
            break;
        }
    }
}
```

### 3.12 让出 CPU — `yield_current()`

```rust
/// 当前任务主动让出 CPU
/// 将当前任务重新放回队列尾部，返回 true；无当前任务则返回 false
pub fn yield_current(&self) -> bool {
    let cur = self.current.lock().unwrap().take();  // 取出并清空 current
    match cur {
        Some(id) => {
            let mut q = self.queue.lock().unwrap();
            let policy = SchedulePolicy::new();  // 重置为默认策略
            q.push((id, policy));                // 重新入队
            true
        }
        None => false,
    }
}
```

---

## 四、使用场景

### 4.1 基本调度循环

```rust
let rq = RunQueue::new();

// 任务就绪时入队
rq.enqueue(1, SchedulePolicy::new());        // 任务 1，默认策略
rq.enqueue(2, SchedulePolicy::with_prio(5)); // 任务 2，优先级 5

// 调度器主循环
loop {
    if let Some((task_id, policy)) = rq.dequeue() {
        rq.set_current(task_id);
        // ... 执行任务 ...
        rq.update_vruntime(task_id, elapsed_ticks);
        rq.clear_current();
        rq.enqueue(task_id, policy);  // 重新入队
    }
}
```

### 4.2 周期性负载均衡

```rust
// 每个 tick 中断中
if tick % REBALANCE_INTERVAL == 0 {
    rq.rebalance();  // 更新 vruntime 并重新排序
}
```

### 4.3 内核临界区保护

```rust
rq.preempt_disable();  // 进入临界区，禁止抢占
// ... 操作内核数据结构 ...
rq.preempt_enable();   // 离开临界区，恢复抢占
```

---

## 五、跨模块连接

```
sched.rs
├── consts::*
│   └── SCHED_NORMAL, PRIO_DEFAULT 等调度常量
│
├── util::CLK
│   └── 全局时钟计数器，rebalance() 用于计算 vruntime 增量
│
├── 被 kernel.rs 的调度器主循环调用
│   └── 每个 tick 中断触发 pick_next/dequeue
│       系统调用 sched_yield 触发 yield_current
│       fork 时 enqueue 新任务
│
└── 与 sync.rs 的间接关系
    └── SyncQueue 中的等待线程被唤醒后需要 enqueue 到 RunQueue
        持有 KernLock 时 preempt_disable 防止死锁
```

---

## 六、与原版 kernel.rs 的对应

| sched.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| `SchedulePolicy` | 约第 2900-2940 行 |
| `RunQueue` 结构体 | 约第 2940-2960 行 |
| `enqueue`/`dequeue`/`pick_next` | 约第 2960-3060 行 |
| `rebalance` | 约第 3060-3100 行 |
| 抢占控制方法 | 约第 3100-3150 行 |

---

## 七、潜在的改进方向

1. **排序算法效率**：`enqueue()` 使用冒泡排序（O(n^2)），`rebalance()` 使用选择排序（O(n^2)）。应替换为 `BinaryHeap`（O(log n)）或至少 `sort_by`（O(n log n)）
2. **得分公式不一致**：`enqueue`、`dequeue`、`pick_next`、`cmp_priority` 四个方法使用了四种不同的得分公式，可能导致调度行为不一致
3. **enqueue 不检查重复**：`_dup` 变量被计算但未使用——同一任务可能被多次入队。应至少跳过已存在的任务
4. **preempt_enable 的 _need_resched 未使用**：抢占重新启用后应该触发调度点（检查是否有更高优先级任务等待），但当前仅计算未执行
5. **yield_current 重置策略**：让出 CPU 时创建新的 `SchedulePolicy::new()` 丢弃了原任务的 vruntime 和 nice 值，不公平
6. **缺少时间片管理**：`time_slice` 字段在 `SchedulePolicy` 中定义但从未在 `RunQueue` 的方法中使用——没有实现时间片轮转
7. **rebalance 中 min_vrt 未使用**：计算了 `min_vrt`（最小 vruntime）但未用于归一化——Linux CFS 会用 min_vrt 防止 vruntime 无限增长
8. **wrapping_add 的溢出处理**：vruntime 使用 `wrapping_add`，溢出后回绕可能导致排序异常
