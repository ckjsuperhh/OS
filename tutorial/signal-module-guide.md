# Signal 模块阅读指南

> 文件路径: `kernel-refactored/src/signal.rs`
> 代码量: 109 行 | 2 个核心结构体 | 依赖: `consts`（NSIG, SIG_DFL, SIG_IGN, SIGKILL, SIGSTOP）

---

## 一、模块概述

`signal.rs` 实现了内核中的 **信号处理机制**，提供 POSIX 风格的信号注册、屏蔽、投递和查询能力。它包含两个核心结构体：

| 结构体 | 用途 | 类比 |
|---|---|---|
| `SigAction` | 描述单个信号的处理方式（处理函数、标志、临时屏蔽） | Linux `struct sigaction` |
| `SigSet` | 管理信号集合状态：待处理信号、屏蔽信号、每个信号的处理动作 | Linux `signal_struct` + `pending` |

**设计定位：** Signal 模块在内核中扮演"中断通知"的角色——当某个事件发生时（子进程退出、用户按 Ctrl+C、定时器到期），内核通过 `sig_raise()` 向目标进程投递信号，进程在返回用户态前检查 `deliverable()` 并按注册的 `SigAction` 执行对应操作。

```
  事件发生（如子进程退出）
       │
       ▼
  sig_raise(SIGCHLD)  ──►  信号进入 pending 集合
       │
       ▼
  deliverable()  ──►  检查 pending & ~blocked，找到可投递信号
       │
       ▼
  get_action(signo)  ──►  获取该信号的 SigAction
       │
       ▼
  执行 handler（SIG_DFL=默认动作 / 自定义函数 / SIG_IGN=忽略）
```

---

## 二、SigAction — 信号处理动作

### 2.1 结构体定义

```rust
pub struct SigAction {
    /// 信号处理函数的指针（虚拟地址）
    /// SIG_DFL (0) = 默认动作，SIG_IGN (1) = 忽略，其他值 = 用户自定义函数地址
    pub handler: usize,
    /// 处理行为标志位（如 SA_RESTART 重启系统调用、SA_NOCLDSTOP 等）
    pub flags: u32,
    /// 处理该信号期间临时屏蔽的其他信号位掩码
    /// 防止信号处理函数被其他信号打断（嵌套）
    pub mask: u64,
}
```

**字段详解：**

- `handler` 是一个用户态函数地址。特殊值 `SIG_DFL`（0）表示使用内核默认处理（如 SIGKILL 默认终止进程），`SIG_IGN`（1）表示忽略该信号
- `flags` 控制信号处理的细节行为，当前实现中尚未解析具体标志位，但预留了接口
- `mask` 是信号处理期间的临时屏蔽掩码。例如处理 SIGUSR1 时屏蔽 SIGUSR2，防止嵌套中断

### 2.2 与 Linux 的对比

| 字段 | Linux `sigaction` | 本实现 |
|---|---|---|
| 处理函数 | `sa_handler` / `sa_sigaction` (union) | `handler: usize` |
| 标志 | `sa_flags: int` | `flags: u32` |
| 屏蔽掩码 | `sa_mask: sigset_t` (128 位) | `mask: u64` (64 位) |

**简化点：** 本实现使用 `u64` 位掩码表示信号集（支持 64 个信号），而 Linux 使用 128 位的 `sigset_t`。对于教学内核来说 64 位已经足够。

---

## 三、SigSet — 信号集合状态

### 3.1 结构体定义

```rust
pub struct SigSet {
    /// 待处理信号位掩码：每一位代表一个信号，1 表示该信号已投递但未处理
    pub pending: u64,
    /// 屏蔽信号位掩码：每一位代表一个信号，1 表示该信号被暂时屏蔽
    pub blocked: u64,
    /// 每个信号对应的处理动作数组（索引 0 不使用，1~63 对应信号 1~63）
    pub actions: Vec<SigAction>,
}
```

**位掩码设计：** 使用 `u64` 的每个 bit 表示一个信号的状态。信号编号从 1 开始（0 不使用），因此：
- `pending & (1 << 9)` 检查 SIGKILL 是否在 pending 中
- `blocked & (1 << 17)` 检查 SIGCHLD 是否被屏蔽

```
  pending:  0b...0000_0010_0000_0000  → SIGCHLD (17) 待处理
  blocked:  0b...0000_0100_0000_0000  → SIGSTOP (19) 被屏蔽（但这会被 sig_block 阻止）
```

### 3.2 构造函数

```rust
pub fn new() -> Self {
    // 为 NSIG+1 个信号（0~64）分配默认动作：SIG_DFL（默认处理）
    let mut actions = Vec::with_capacity(NSIG as usize + 1);
    for _ in 0..=NSIG {
        actions.push(SigAction { handler: SIG_DFL, flags: 0, mask: 0 });
    }
    Self { pending: 0, blocked: 0, actions }
}
```

**初始化策略：** 所有信号初始为默认处理（`SIG_DFL`），pending 和 blocked 掩码全 0（无待处理信号，无屏蔽信号）。

### 3.3 信号查询 — `sig_pending()`

```rust
/// 检查指定信号是否在 pending 集合中
pub fn sig_pending(&self, signo: u32) -> bool {
    (self.pending & (1u64 << signo)) != 0  // 位与操作检测第 signo 位
}
```

### 3.4 信号投递 — `sig_raise()`

```rust
/// 向进程投递一个信号（加入 pending 集合）
pub fn sig_raise(&mut self, signo: u32) {
    if signo < NSIG {
        self.pending |= 1u64 << signo;  // 将第 signo 位置 1
    }
}
```

**注意：** 信号投递只是将位掩码中对应位置 1，不会立即执行处理。实际的信号处理发生在进程返回用户态时（通过 `deliverable()` 检查）。这是 POSIX 信号的标准行为——信号是异步投递、同步处理的。

### 3.5 合并待处理信号 — `coalesce_pending()`

```rust
/// 计算所有可投递的待处理信号（pending 中未被 blocked 屏蔽的）
/// 返回一个位掩码，每一位代表一个可投递信号
pub fn coalesce_pending(&mut self) -> u64 {
    // active = pending 中不在 blocked 中的信号
    let active = self.pending & !self.blocked;
    let mut result: u64 = 0;
    // 逐位检查信号 1 到 NSIG-1
    for i in 1..NSIG {
        if (active & (1u64 << i)) != 0 {
            result |= 1u64 << i;
        }
    }
    result
}
```

**逻辑说明：** 此方法等价于 `self.pending & !self.blocked`（但排除了信号 0 和信号 64）。遍历循环在当前实现中是冗余的——`active` 已经是正确结果，循环只是做了位复制。这是一个潜在优化点。

### 3.6 信号清除 — `sig_clear()`

```rust
/// 从 pending 集合中清除指定信号（标记为已处理）
pub fn sig_clear(&mut self, signo: u32) {
    if signo < NSIG {
        self.pending &= !(1u64 << signo);  // 将第 signo 位清 0
    }
}
```

### 3.7 信号屏蔽操作

```rust
/// 添加屏蔽信号（按位或合并到 blocked）
/// SIGKILL 和 SIGSTOP 永远不能被屏蔽——这是 POSIX 强制规定
pub fn sig_block(&mut self, mask: u64) {
    self.blocked |= mask;
    // 强制清除 SIGKILL 和 SIGSTOP 的屏蔽位
    self.blocked &= !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
}

/// 解除屏蔽信号（按位与取反）
pub fn sig_unblock(&mut self, mask: u64) {
    self.blocked &= !mask;
}

/// 直接设置屏蔽掩码（替换整个 blocked）
/// 同样强制排除 SIGKILL 和 SIGSTOP
pub fn sig_setmask(&mut self, mask: u64) {
    self.blocked = mask & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
}
```

**SIGKILL/SIGSTOP 不可屏蔽的原因：** 这两个信号是系统管理员的"最后手段"——SIGKILL 用于强制终止失控进程，SIGSTOP 用于强制暂停进程。如果允许屏蔽，恶意程序可以让自己无法被杀死。

### 3.8 可投递信号查询 — `deliverable()`

```rust
/// 返回下一个可投递的信号编号（pending 且未 blocked 的最低编号信号）
/// 如果没有可投递信号，返回 None
pub fn deliverable(&self) -> Option<u32> {
    let actionable = self.pending & !self.blocked;  // 可投递 = 待处理 & ~屏蔽
    if actionable == 0 { return None; }
    // 从信号 1 开始遍历，返回第一个可投递的信号
    for i in 1..NSIG {
        if (actionable & (1u64 << i)) != 0 {
            return Some(i);
        }
    }
    None
}
```

**优先级：** 信号编号越低优先级越高（遍历从 1 开始）。SIGKILL (9) 会在 SIGUSR1 (10) 之前被投递，这是符合直觉的设计——致命信号优先处理。

**流程图：**

```
deliverable() 被调用
    │
    ▼
actionable = pending & ~blocked
    │
    ├── actionable == 0 ──► return None（无可投递信号）
    │
    └── actionable != 0
         │
         ▼
    遍历 i = 1, 2, 3, ... NSIG-1
         │
         ├── bit i 为 1 ──► return Some(i)（找到最低编号信号）
         └── bit i 为 0 ──► 继续下一个
```

### 3.9 信号动作管理

```rust
/// 设置指定信号的处理动作
/// SIGKILL 和 SIGSTOP 的处理动作不可修改（POSIX 规定）
pub fn set_action(&mut self, signo: u32, action: SigAction) {
    if signo < NSIG as u32 && signo != SIGKILL && signo != SIGSTOP {
        self.actions[signo as usize] = action;
    }
}

/// 获取指定信号的处理动作
/// 超出范围时返回 actions[0]（安全的默认值）
pub fn get_action(&self, signo: u32) -> &SigAction {
    if (signo as usize) < self.actions.len() {
        &self.actions[signo as usize]
    } else {
        &self.actions[0]
    }
}

/// 检查指定信号是否被设置为忽略
pub fn is_ignored(&self, signo: u32) -> bool {
    if (signo as usize) < self.actions.len() {
        self.actions[signo as usize].handler == SIG_IGN
    } else {
        false
    }
}
```

### 3.10 exec 后的重置 — `clear_non_caught()`

```rust
/// 将所有自定义处理函数重置为默认动作
/// 在 exec() 系统调用后调用——新程序不继承旧的信号处理函数
pub fn clear_non_caught(&mut self) {
    for i in 1..self.actions.len() {
        // 只重置自定义处理函数（非 SIG_DFL 且非 SIG_IGN）
        // SIG_IGN 在 exec 后保留（POSIX 规定）
        if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
            self.actions[i].handler = SIG_DFL;
        }
    }
}
```

**为什么保留 SIG_IGN：** POSIX 标准规定 `exec()` 后，`SIG_IGN` 状态的信号保持忽略，而自定义处理函数因地址空间已变更必须重置为默认（旧的函数指针在新程序中无效）。

---

## 四、使用场景

### 4.1 系统调用 kill()

```rust
// proc.rs 中的 sys_kill 实现
fn sys_kill(pid: i32, sig: u32) -> i32 {
    // 找到目标进程，调用 sig_raise
    target.sigset.sig_raise(sig);
    0
}
```

### 4.2 子进程退出时发送 SIGCHLD

```rust
// 当子进程 exit() 时，向父进程发送 SIGCHLD
parent.sigset.sig_raise(SIGCHLD);
// 如果父进程正在 wait4()，SIGCHLD 会唤醒它
```

### 4.3 返回用户态前的信号检查

```rust
// 每次从系统调用/中断返回用户态前
if let Some(signo) = sigset.deliverable() {
    let action = sigset.get_action(signo);
    match action.handler {
        SIG_DFL => handle_default(signo),  // 默认动作（终止/忽略/停止）
        SIG_IGN => { /* 忽略 */ }
        addr => {
            // 保存当前上下文，跳转到用户态处理函数
            sigset.sig_clear(signo);
            jump_to_handler(addr, signo);
        }
    }
}
```

### 4.4 sigaction 系统调用

```rust
// sys_sigaction: 注册/查询信号处理函数
fn sys_sigaction(signo: u32, new: Option<&SigAction>, old: Option<&mut SigAction>) {
    if let Some(old_out) = old {
        *old_out = sigset.get_action(signo).clone();
    }
    if let Some(new_act) = new {
        sigset.set_action(signo, new_act.clone());
    }
}
```

---

## 五、跨模块连接

```
signal.rs
├── consts.rs — NSIG (64), SIG_DFL (0), SIG_IGN (1), SIGKILL (9), SIGSTOP (19)
│               SIGCHLD (17), SIGUSR1 (10), SIGUSR2 (12), SIGALRM (14)
├── proc.rs   — 每个进程持有一个 SigSet 实例
│               sys_kill() 调用 sig_raise()
│               sys_sigaction() 调用 set_action() / get_action()
│               sys_sigprocmask() 调用 sig_block() / sig_unblock() / sig_setmask()
├── timer.rs  — 定时器到期可能触发 SIGALRM
└── fs.rs     — 终端 Ctrl+C 触发 SIGINT（通过 LM_ISIG 标志控制）
```

---

## 六、与原版 kernel.rs 的对应

| signal.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| `SigAction` 结构体 | 约在进程结构体定义附近 |
| `SigSet` 结构体 | 内嵌在 `Process` / `Proc` 结构体中 |
| `sig_raise()` / `sig_clear()` | 信号投递逻辑中 |
| `deliverable()` | 系统调用返回路径中 |
| `clear_non_caught()` | `exec()` 实现中 |

---

## 七、潜在的改进方向

1. **`coalesce_pending()` 的冗余循环**：`active` 已经是 `pending & !blocked`，后面的逐位循环只是复制了一遍（跳过了 bit 0 和 bit 63），可以简化为 `return active & !1;`
2. **`deliverable()` 可用位运算优化**：使用 `actionable.trailing_zeros()` 直接获取最低位信号编号，避免循环
3. **缺少 `SigAction` 的 `Clone` 实现**：`sys_sigaction` 需要返回旧的 `SigAction`，当前可能需要手动复制字段
4. **缺少信号队列**：当前实现中同一信号只保留一个 pending 位，多次 `sig_raise` 同一信号会"合并"为一个。POSIX 的 `sigqueue()` 要求实时信号 (32-64) 排队不丢失
5. **`get_action()` 的越界处理**：当 `signo` 超出范围时返回 `actions[0]`，但 `actions[0]` 是一个空白的默认动作，语义不够清晰——可以考虑 `panic!` 或返回 `Option`
6. **线程安全**：`SigSet` 没有任何同步保护，在多核环境下可能存在竞态。如果每个进程只有一个线程访问则无碍，但 `kill()` 可能从其他进程/线程调用
