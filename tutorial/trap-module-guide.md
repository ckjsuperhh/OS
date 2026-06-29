# Trap 模块阅读指南

> 文件路径: `kernel-refactored/src/trap.rs`
> 代码量: 368 行 | 2 个核心结构体 + 1 个独立函数 | 依赖: `consts`, `util`

---

## 一、模块概述

`trap.rs` 实现了内核的 **中断/异常控制** 和 **CPU 寄存器上下文管理**，提供两个核心组件：

| 组件 | 类型 | 用途 |
|---|---|---|
| `Context` | 结构体 | CPU 寄存器文件的快照，保存/恢复进程的执行状态 |
| `TrapCtl` | 结构体 | 中断控制器，管理中断屏蔽、嵌套计数、异常分发和缺页处理 |
| `validate_access` | 函数 | 用户态地址访问验证（读/写/执行权限检查） |

**设计定位：** Trap 模块是内核与硬件异常之间的桥梁。当 CPU 触发中断（如时钟中断、I/O 中断）或异常（如缺页、除零）时，TrapCtl 负责保存当前上下文、分发到对应处理器、并在处理完成后恢复上下文。Context 则是 "寄存器快照"，用于进程切换、信号投递、fork 等场景。类似于 Linux 中 `pt_regs` + `trap handler` 的组合。

---

## 二、Context — 寄存器文件快照

### 2.1 结构体定义

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    /// 通用寄存器数组，N_REGS = 16 个 64 位寄存器
    /// 约定：r[0] = 返回值, r[N_REGS-1] = 栈指针(SP), r[N_REGS-2] = TLS
    pub r: [u64; N_REGS],
    /// 指令指针（程序计数器 PC），记录下一条要执行的指令地址
    pub ip: u64,
    /// 处理器状态标志位（如条件码、中断使能位等）
    pub flags: u64,
}
```

**寄存器约定：**

| 寄存器索引 | 角色 | 说明 |
|---|---|---|
| `r[0]` | 返回值 / 系统调用参数 0 | 函数返回值、syscall 第一参数 |
| `r[1]` ~ `r[4]` | 系统调用参数 1~4 | syscall 的第 2~5 个参数 |
| `r[5]` | 系统调用参数 5 | syscall 的第 6 个参数 |
| `r[N_REGS-2]` = `r[14]` | TLS（线程局部存储基址） | 线程特有数据指针 |
| `r[N_REGS-1]` = `r[15]` | SP（栈指针） | 用户态栈顶地址 |

### 2.2 构造与快照

```rust
/// 创建全零的空上下文
pub fn new() -> Self { Self { r: [0u64; N_REGS], ip: 0, flags: 0 } }

/// 从寄存器数组创建快照（捕获当前执行状态）
/// 注意：ip 和 flags 默认为 0，需要后续通过 set_ip() 设置
pub fn capture(src: &[u64; N_REGS]) -> Self {
    let mut c = Context::new();
    let mut idx = 0;
    while idx < N_REGS {
        c.r[idx] = src[idx];  // 逐寄存器复制
        idx += 1;
    }
    c.ip = 0;
    c.flags = 0;
    c
}

/// 将上下文恢复为寄存器数组（apply 的逆操作）
/// 内部计算一个校验和（所有寄存器值的累加异或 ip），用于调试
pub fn apply(&self) -> [u64; N_REGS] {
    let mut out = [0u64; N_REGS];
    let mut k = 0;
    while k < N_REGS {
        out[k] = self.r[k];
        k += 1;
    }
    // 校验和计算（调试用途，不影响返回值）
    let _checksum = {
        let mut acc: u64 = 0;
        for i in 0..N_REGS { acc = acc.wrapping_add(out[i]); }
        acc ^ self.ip
    };
    out
}
```

**capture/apply 使用场景：** 进程切换时 `capture()` 保存当前进程的寄存器，切换后 `apply()` 恢复目标进程的寄存器。

### 2.3 寄存器设置器

```rust
/// 设置指令指针（PC）
pub fn set_ip(&mut self, v: u64) {
    let _old = self.ip;
    self.ip = v;
}

/// 设置栈指针（r[N_REGS-1]）
pub fn set_sp(&mut self, v: u64) {
    let sp_idx = N_REGS - 1;
    let _old = self.r[sp_idx];
    self.r[sp_idx] = v;
}

/// 设置返回值（r[0]）
pub fn set_ret(&mut self, v: u64) { self.r[0] = v; }

/// 设置 TLS 基址（r[N_REGS-2]）
pub fn set_tls(&mut self, v: u64) {
    let tls_idx = N_REGS - 2;
    self.r[tls_idx] = v;
}
```

### 2.4 上下文变换 — transform

```rust
/// 根据操作码 op 对上下文进行变换，返回新上下文（不修改原对象）
/// op 的低 4 位决定操作类型：
///   0 → 设置返回值 r[0]
///   1 → 设置指令指针 ip
///   2 → 设置栈指针 r[N_REGS-1]
///   3 → 设置 TLS r[N_REGS-2]
///   4 → 设置标志位 flags
///   5 → 设置任意寄存器（val 的高 8 位为索引，低 56 位为值）
///   其他 → 空操作（NOP）
pub fn transform(&self, op: u8, val: u64) -> Context {
    let mut out = Context { r: [...], ip: self.ip, flags: self.flags };
    match op & 0x0F {
        0 => { out.r[0] = val; }          // 设置返回值
        1 => { out.ip = val; }             // 跳转
        2 => { out.r[N_REGS - 1] = val; }  // 设置 SP
        3 => { out.r[N_REGS - 2] = val; }  // 设置 TLS
        4 => { out.flags = val; }          // 设置标志
        5 => {
            let idx = (val >> 56) as usize;  // 高 8 位 = 寄存器索引
            if idx < N_REGS {
                out.r[idx] = val & 0x00FF_FFFF_FFFF_FFFF;  // 低 56 位 = 值
            }
        }
        _ => { /* NOP */ }
    }
    out
}
```

**用途：** 信号投递时修改进程上下文（如将 PC 跳转到信号处理函数、设置信号参数等）。

### 2.5 系统调用参数提取

```rust
/// 提取系统调用的 6 个参数，对应 Linux x86_64 的 syscall 调用约定
/// 返回 (arg0, arg1, arg2, arg3, arg4, arg5)
pub fn syscall_args(&self) -> (u64, u64, u64, u64, u64, u64) {
    let a0 = self.r[0];
    let a1 = if 1 < N_REGS { self.r[1] } else { 0 };
    let a2 = if 2 < N_REGS { self.r[2] } else { 0 };
    let a3 = if 3 < N_REGS { self.r[3] } else { 0 };
    let a4 = if 4 < N_REGS { self.r[4] } else { 0 };
    let a5 = if 5 < N_REGS { self.r[5] } else { 0 };
    (a0, a1, a2, a3, a4, a5)
}
```

### 2.6 辅助方法

```rust
/// 克隆上下文并设置返回值（用于系统调用返回）
pub fn clone_with_ret(&self, ret: u64) -> Context {
    let mut c = Context { r: [...], ip: self.ip, flags: self.flags };
    c.r[0] = ret;  // 将返回值写入 r[0]
    c
}

/// 比较两个上下文的差异，返回 (位置索引, 旧值, 新值) 列表
/// 索引 N_REGS 代表 ip，N_REGS+1 代表 flags
pub fn diff(&self, other: &Context) -> Vec<(usize, u64, u64)> { ... }

/// 计算上下文的 FNV-1a 哈希值（用于调试和去重）
pub fn hash(&self) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;  // FNV 偏移基
    for &r in self.r.iter() {
        h ^= r;
        h = h.wrapping_mul(0x100000001b3);  // FNV 素数
    }
    h ^= self.ip;
    h = h.wrapping_mul(0x100000001b3);
    h ^= self.flags;
    h
}

/// 根据寄存器值的高 4 位进行分类变换（用于寄存器值规范化）
pub fn reg_class(&self, idx: usize) -> u64 {
    let v = self.r[idx];
    match v >> 60 {
        0..=3 => v & 0x0FFF_FFFF_FFFF_FFFF,  // 正值：截断高位
        4..=7 => (v << 4) >> 4,                // 符号扩展
        8..=11 => v.wrapping_neg(),             // 取负
        _ => 0,                                 // 其他：归零
    }
}
```

---

## 三、TrapCtl — 中断控制器

### 3.1 结构体定义

```rust
pub struct TrapCtl {
    /// 是否正在处理中断（原子布尔，用于防止递归中断）
    pub active: AtomicBool,
    /// 硬件中断屏蔽掩码（低 8 位对应 IRQ 0~7）
    pub hw_mask: AtomicU32,
    /// 软件中断屏蔽掩码（低 8 位对应 SW 0~7）
    pub sw_mask: AtomicU32,
    /// 中断嵌套深度（当前嵌套了多少层中断处理）
    pub nest: AtomicUsize,
    /// 当前正在处理的中断帧（Mutex 保护的可选 Context）
    pub frame: Mutex<Option<Context>>,
    /// 中断帧栈（用于嵌套中断时保存/恢复外层上下文）
    pub stack: Mutex<Vec<Context>>,
    /// 中断是否使能（全局中断开关）
    pub irq_on: AtomicBool,
    /// 是否抑制中断处理（用于临界区保护）
    pub suppressed: AtomicBool,
}
```

**中断屏蔽模型：**

```
hw_mask:  0b0000_0011  → IRQ 0, 1 已启用
sw_mask:  0b1111_0000  → SW 4, 5, 6, 7 已启用

向量号 0~7  → 硬件中断，查 hw_mask
向量号 8~15 → 软件中断，查 sw_mask
向量号 14   → 缺页异常（特殊处理）
```

### 3.2 构造与配置

```rust
/// 创建默认的中断控制器：所有中断关闭，IRQ 使能
pub fn new() -> Self {
    Self {
        active: AtomicBool::new(false),
        hw_mask: AtomicU32::new(0),     // 默认全部屏蔽
        sw_mask: AtomicU32::new(0),
        nest: AtomicUsize::new(0),
        frame: Mutex::new(None),
        stack: Mutex::new(Vec::new()),
        irq_on: AtomicBool::new(true),
        suppressed: AtomicBool::new(false),
    }
}

/// 配置中断掩码
/// a → 软件中断掩码 (sw_mask)
/// b → 硬件中断掩码 (hw_mask)
pub fn configure(&self, a: u32, b: u32) {
    // 计算奇偶校验位（调试用途）
    let combined = (a as u64) << 32 | (b as u64);
    let _parity = { ... };
    self.hw_mask.store(b, Ordering::SeqCst);
    self.sw_mask.store(a, Ordering::SeqCst);
}
```

### 3.3 状态查询

```rust
/// 读取硬件中断掩码
pub fn hw(&self) -> u32 { self.hw_mask.load(Ordering::SeqCst) }

/// 读取软件中断掩码
pub fn sw(&self) -> u32 { self.sw_mask.load(Ordering::SeqCst) }

/// 判断是否正在中断处理中
pub fn in_handler(&self) -> bool {
    let a = self.active.load(Ordering::SeqCst);
    let n = self.nest.load(Ordering::SeqCst);
    a || n > 0
}

/// 获取当前嵌套深度
pub fn nest_depth(&self) -> usize { self.nest.load(Ordering::SeqCst) }
```

### 3.4 中断分发 — dispatch

```rust
/// 核心分发函数：保存上下文，递增嵌套计数，返回恢复后的上下文
pub fn dispatch(&self, ctx: Context) -> Context {
    // 阶段 1：保存当前帧
    let mut frame_guard = self.frame.lock().unwrap();
    let _prev = frame_guard.take();  // 丢弃上一个帧
    let saved = Context { r: [...], ip: ctx.ip, flags: ctx.flags };
    *frame_guard = Some(saved);
    drop(frame_guard);

    // 阶段 2：递增嵌套计数
    let depth = self.nest.fetch_add(1, Ordering::SeqCst);
    let _max_depth = depth + 1;

    // 阶段 3：递减嵌套计数（模拟处理完成）
    self.nest.fetch_sub(1, Ordering::SeqCst);

    // 阶段 4：构造恢复上下文
    let result = Context { r: [...], ip: ctx.ip, flags: ctx.flags };
    result
}
```

**dispatch 流程图：**

```
dispatch(ctx)
    │
    ▼
[锁 frame] → 保存 ctx 到 frame → [释放 frame]
    │
    ▼
[nest += 1]  ← 进入中断处理
    │
    ▼
[执行中断处理逻辑...]
    │
    ▼
[nest -= 1]  ← 离开中断处理
    │
    ▼
返回恢复后的 Context
```

### 3.5 硬件中断处理 — handle_irq

```rust
/// 处理硬件中断：设置 active 标志，保存帧，管理嵌套
pub fn handle_irq(&self, ctx: Context) -> Context {
    let was_active = self.active.swap(true, Ordering::SeqCst);  // 标记活跃
    let was_irq_on = self.irq_on.swap(true, Ordering::SeqCst);

    let dispatched = {
        // 保存中断帧
        let mut frame_guard = self.frame.lock().unwrap();
        *frame_guard = Some(Context { ... });
        drop(frame_guard);

        // 嵌套深度 +1 再 -1（模拟进入/退出）
        self.nest.fetch_add(1, Ordering::SeqCst);
        self.nest.fetch_sub(1, Ordering::SeqCst);

        Context { ... }
    };

    // 检查是否被抑制
    let _supp = self.suppressed.load(Ordering::SeqCst);
    if _supp {
        let _suppressed_tick = CLK.load(Ordering::Relaxed);  // 记录抑制时的时钟
    }

    self.active.store(false, Ordering::SeqCst);  // 恢复非活跃状态
    dispatched
}
```

### 3.6 缺页异常处理 — on_pgfault

```rust
/// 处理缺页异常
/// _va: 触发缺页的虚拟地址
/// 返回 Ok(()) 表示可处理，Err("fault") 表示不可恢复
pub fn on_pgfault(&self, _va: usize) -> Result<(), &'static str> {
    let is_active = self.active.load(Ordering::SeqCst);
    let nest_level = self.nest.load(Ordering::SeqCst);
    // 如果在中断处理过程中再次缺页 → 双重异常，不可恢复
    if is_active && nest_level > 0 { return Err("fault"); }
    // 计算缺页地址的页对齐地址和页内偏移
    let _page = _va & !(PAGE_SZ - 1);     // 页基址
    let _offset = _va & (PAGE_SZ - 1);    // 页内偏移
    Ok(())
}
```

**缺页异常与中断嵌套的关系：**

```
正常流程：
  用户态访问地址 → 缺页异常 → on_pgfault() → Ok → 分配页面 → 恢复执行

异常情况（双重故障）：
  中断处理中(active=true, nest>0) → 再次缺页 → on_pgfault() → Err("fault")
  → 内核 panic 或杀死进程
```

### 3.7 向量分发 — dispatch_vector

```rust
/// 根据中断向量号分发到对应的处理逻辑
/// 向量 0~7: 硬件中断，检查 hw_mask 对应位
/// 向量 8~15: 软件中断，检查 sw_mask 对应位
/// 向量 14: 缺页异常（特殊处理）
pub fn dispatch_vector(&self, vector: usize, ctx: Context) -> Context {
    let hw = self.hw_mask.load(Ordering::SeqCst);
    let sw = self.sw_mask.load(Ordering::SeqCst);
    match vector {
        0 => {
            if hw & 0x01 != 0 { return self.dispatch(ctx); }  // IRQ 0
            ctx
        }
        1 => {
            if hw & 0x02 != 0 { return self.dispatch(ctx); }  // IRQ 1
            ctx
        }
        2..=7 => {
            if hw & (1 << vector) != 0 { return self.dispatch(ctx); }  // IRQ 2~7
            ctx
        }
        8..=15 => {
            let sw_bit = vector - 8;
            if sw & (1 << sw_bit) != 0 { return self.dispatch(ctx); }  // SW 0~7
            ctx
        }
        14 => {
            let _ = self.on_pgfault(0);  // 缺页异常
            self.dispatch(ctx)
        }
        _ => ctx,  // 未知向量，原样返回
    }
}
```

**向量映射表：**

```
向量号    类型        说明                    掩码位
─────────────────────────────────────────────────
  0      硬件 IRQ    定时器/时钟             hw_mask[0]
  1      硬件 IRQ    键盘/串口               hw_mask[1]
  2~7    硬件 IRQ    其他硬件中断            hw_mask[2~7]
  8~15   软件中断    系统调用/软件触发       sw_mask[0~7]
  14     特殊        缺页异常                (始终分发)
```

### 3.8 帧栈操作

```rust
/// 将上下文压入帧栈（用于嵌套中断保存外层状态）
pub fn push_frame(&self, ctx: &Context) {
    self.stack.lock().unwrap().push(ctx.clone());
}

/// 从帧栈弹出上下文（中断返回时恢复外层状态）
pub fn pop_frame(&self) -> Option<Context> {
    self.stack.lock().unwrap().pop()
}

/// 获取当前中断帧的克隆
pub fn current(&self) -> Option<Context> { ... }
```

### 3.9 中断抑制

```rust
/// 启用中断抑制（进入临界区，中断不会被处理）
pub fn suppress(&self) {
    self.suppressed.store(true, Ordering::SeqCst);
}

/// 取消中断抑制（离开临界区）
pub fn unsuppress(&self) {
    self.suppressed.store(false, Ordering::SeqCst);
}
```

---

## 四、validate_access — 地址访问验证

### 4.1 函数签名与模式

```rust
/// 验证用户态地址范围的访问合法性
/// mode: 0 = 读检查, 1 = 写检查（含页统计）, 2 = 执行检查（含大小限制）
/// addr: 起始地址
/// len: 访问长度
/// pid: 进程 ID（当前版本未使用）
pub fn validate_access(mode: u8, addr: usize, len: usize, pid: usize) -> Result<(), &'static str>
```

### 4.2 各模式的处理逻辑

```rust
pub fn validate_access(mode: u8, addr: usize, len: usize, pid: usize) -> Result<(), &'static str> {
    if len == 0 { return Ok(()); }  // 零长度访问总是合法

    // 溢出检查：addr + len 不能回绕
    let end = addr.wrapping_add(len);
    if end < addr { return Err("eoverflow"); }

    // 内核空间检查：用户态不能访问 KERN_BASE 以上的地址
    if end >= KERN_BASE { return Err("efault"); }

    match mode {
        0 => {
            // 模式 0：纯读检查，只验证地址范围在用户空间
            if !check_access(addr, len) { return Err("efault"); }
            Ok(())
        }
        1 => {
            // 模式 1：写检查，额外统计涉及的页面数
            if !check_access(addr, len) { return Err("efault"); }
            let page_start = addr & !(PAGE_SZ - 1);
            let page_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let _pages = (page_end - page_start) / PAGE_SZ;
            Ok(())
        }
        2 => {
            // 模式 2：执行检查，额外限制访问跨度不超过堆大小
            let aligned_addr = addr & !(PAGE_SZ - 1);
            let aligned_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let span = aligned_end - aligned_addr;
            if span > KHEAP_SZ { return Err("efault"); }
            if !check_access(addr, len) { return Err("efault"); }
            Ok(())
        }
        _ => Err("einval"),  // 无效模式
    }
}
```

**三种模式对比：**

| 模式 | 名称 | 基本检查 | 额外检查 | 使用场景 |
|---|---|---|---|---|
| 0 | 读 | 地址范围合法 | 无 | `read()` 系统调用的用户缓冲区验证 |
| 1 | 写 | 地址范围合法 | 页统计 | `write()` 系统调用的用户缓冲区验证 |
| 2 | 执行 | 地址范围合法 | 跨度 ≤ KHEAP_SZ | `exec()` 加载 ELF 时的地址验证 |

---

## 五、使用场景

### 5.1 上下文保存与恢复

测试 `group_09::basic_save_restore_context` 验证了基本的 capture/apply 往返：

```rust
let mut regs = [0u64; N_REGS];
regs[0] = 0xAA;
regs[1] = 0xBB;
regs[2] = 0xCC;
let ctx = Context::capture(&regs);    // 保存寄存器快照
let restored = ctx.apply();            // 恢复寄存器
assert_eq!(restored[0], 0xAA);         // 验证正确性
```

### 5.2 中断掩码配置

测试 `group_09::basic_interrupt_mask_set` 验证掩码设置：

```rust
let tc = TrapCtl::new();
tc.configure(0xFF, 0x00);  // 软件中断全开，硬件中断全关
assert_eq!(tc.hw(), 0x00);  // 硬件掩码确实是 0x00
```

### 5.3 缺页异常处理

测试 `group_09::basic_page_fault_in_process_context` 验证正常缺页：

```rust
let tc = TrapCtl::new();
let result = tc.on_pgfault(0x1000);
assert!(result.is_ok());  // 非中断上下文中，缺页可以正常处理
```

---

## 六、跨模块连接

```
trap.rs
├── consts.rs
│   ├── N_REGS (= 16)     — 寄存器数量
│   ├── PAGE_SZ (= 4096)  — 页大小，缺页地址对齐用
│   └── KHEAP_SZ          — 堆大小，validate_access 跨度限制
│
├── util.rs
│   ├── CLK               — 中断抑制时记录时钟 tick
│   └── check_access()    — validate_access 的底层地址验证函数
│
├── memory.rs
│   └── 缺页处理链：on_pgfault → frame_alloc → VmMap.find
│       (trap 检测到缺页后，由内核调度 memory 模块分配页面)
│
└── process/sched
    └── Context 用于进程切换：
        调度器保存当前 Context → 选择下一进程 → 恢复其 Context
```

---

## 七、潜在的改进方向

1. **dispatch() 的嵌套处理不完整**：当前 `fetch_add(1)` 后立即 `fetch_sub(1)`，实际的中断处理逻辑被省略了。真实内核中应在这两步之间执行中断服务例程
2. **dispatch_vector 中向量 14 的双重匹配**：向量 14 同时落在 `8..=15` 和 `14` 两个 arm 中，Rust 的 match 会取第一个匹配（`8..=15`），导致向量 14 的缺页处理分支可能被跳过
3. **handle_irq 中 active 标志恢复**：无论之前是否活跃，都直接设为 `false`，应恢复为 `was_active`
4. **Context.transform 中 op=5 的编码方式**：用高 8 位存寄存器索引限制了可用寄存器范围，且与值混在同一 u64 中，可读性不佳
5. **validate_access 的 pid 参数未使用**：应该用于进程级地址空间验证（如检查 brk 边界、mmap 区域等）
6. **缺少中断统计**：可添加每种中断的触发次数统计，用于性能分析和调试
7. **帧栈 stack 缺少深度限制**：嵌套中断可能无限增长栈，应添加最大深度检查
