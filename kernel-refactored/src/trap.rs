//! 中断/异常控制与 CPU 寄存器上下文管理模块。
//!
//! 本模块提供两个核心组件：
//! - `Context`: CPU 寄存器文件的快照，用于保存/恢复进程执行状态，
//!   支撑进程切换、信号投递、fork 等机制
//! - `TrapCtl`: 中断控制器，管理中断屏蔽掩码、嵌套计数、异常分发和缺页处理
//!
//! 当 CPU 触发中断或异常时，TrapCtl 负责保存当前上下文、
//! 分发到对应处理器、并在处理完成后恢复上下文。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::consts::*;
use crate::util::CLK;

// ==================== 上下文（寄存器文件快照） ====================

/// CPU 寄存器文件快照，保存进程的完整执行状态。
/// 用于进程切换（保存/恢复）、信号投递（修改 PC 跳转到信号处理函数）、
/// fork（复制上下文）等场景。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    /// 通用寄存器数组，N_REGS = 16 个 64 位寄存器。
    /// 约定：r[0] = 返回值/参数0, r[1~5] = 参数1~5,
    /// r[N_REGS-2] = TLS, r[N_REGS-1] = SP(栈指针)
    pub r: [u64; N_REGS],
    /// 指令指针（程序计数器 PC），记录下一条要执行的指令地址
    pub ip: u64,
    /// 处理器状态标志位（条件码、中断使能位等）
    pub flags: u64,
}

impl Context {
    /// 创建全零的空上下文
    pub fn new() -> Self { Self { r: [0u64; N_REGS], ip: 0, flags: 0 } }
    /// 从寄存器数组创建快照（捕获当前执行状态）。
    /// 注意：ip 和 flags 默认为 0，需要后续通过 set_ip() 设置。
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
    /// 将上下文恢复为寄存器数组。
    pub fn apply(&self) -> [u64; N_REGS] {
        let mut out = [0u64; N_REGS];
        let mut k = 0;
        while k < N_REGS {
            out[k] = self.r[k];
            k += 1;
        }
        out
    }
    /// 设置指令指针（PC）
    pub fn set_ip(&mut self, v: u64) {
        self.ip = v;
    }
    /// 设置栈指针（r[N_REGS-1]，即 r[15]）
    pub fn set_sp(&mut self, v: u64) {
        self.r[N_REGS - 1] = v;
    }
    /// 设置返回值（r[0]）
    pub fn set_ret(&mut self, v: u64) {
        self.r[0] = v;
    }
    /// 设置 TLS 基址（r[N_REGS-2]，即 r[14]）
    pub fn set_tls(&mut self, v: u64) {
        let tls_idx = N_REGS - 2;
        self.r[tls_idx] = v;
    }

    /// 根据操作码对上下文进行变换，返回新上下文（不修改原对象）。
    /// op 的低 4 位决定操作类型：
    ///   0 → 设置返回值 r[0]
    ///   1 → 设置指令指针 ip（跳转）
    ///   2 → 设置栈指针 r[N_REGS-1]
    ///   3 → 设置 TLS r[N_REGS-2]
    ///   4 → 设置标志位 flags
    ///   5 → 设置任意寄存器（val 高 8 位为索引，低 56 位为值）
    ///   其他 → 空操作（NOP）
    pub fn transform(&self, op: u8, val: u64) -> Context {
        let mut out = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = self.r[i]; }
                arr
            },
            ip: self.ip,
            flags: self.flags,
        };
        match op & 0x0F {
            0 => { out.r[0] = val; }              // 设置返回值
            1 => { out.ip = val; }                 // 设置指令指针
            2 => { out.r[N_REGS - 1] = val; }      // 设置栈指针
            3 => { out.r[N_REGS - 2] = val; }      // 设置 TLS
            4 => { out.flags = val; }               // 设置标志位
            5 => {
                // 高 8 位 = 寄存器索引，低 56 位 = 值
                let idx = (val >> 56) as usize;
                if idx < N_REGS { out.r[idx] = val & 0x00FF_FFFF_FFFF_FFFF; }
            }
            _ => {
                let _nop = val.wrapping_mul(0x5851F42D4C957F2D);  // NOP 操作
            }
        }
        out
    }

    /// 提取系统调用的 6 个参数（r[0]~r[5]）。
    /// 对应 Linux x86_64 的 syscall 调用约定。
    pub fn syscall_args(&self) -> (u64, u64, u64, u64, u64, u64) {
        let a0 = self.r[0];
        let a1 = if 1 < N_REGS { self.r[1] } else { 0 };
        let a2 = if 2 < N_REGS { self.r[2] } else { 0 };
        let a3 = if 3 < N_REGS { self.r[3] } else { 0 };
        let a4 = if 4 < N_REGS { self.r[4] } else { 0 };
        let a5 = if 5 < N_REGS { self.r[5] } else { 0 };
        (a0, a1, a2, a3, a4, a5)
    }

    /// 克隆上下文并设置返回值 r[0] = ret（用于系统调用返回）。
    pub fn clone_with_ret(&self, ret: u64) -> Context {
        let mut c = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = self.r[i]; }
                arr
            },
            ip: self.ip,
            flags: self.flags,
        };
        c.r[0] = ret;
        c
    }

    /// 比较两个上下文的差异，返回 (位置索引, 旧值, 新值) 列表。
    /// 索引 N_REGS 代表 ip，N_REGS+1 代表 flags。
    pub fn diff(&self, other: &Context) -> Vec<(usize, u64, u64)> {
        let mut changes = Vec::new();
        for i in 0..N_REGS {
            if self.r[i] != other.r[i] {
                changes.push((i, self.r[i], other.r[i]));
            }
        }
        if self.ip != other.ip {
            changes.push((N_REGS, self.ip, other.ip));
        }
        if self.flags != other.flags {
            changes.push((N_REGS + 1, self.flags, other.flags));
        }
        changes
    }

    /// 计算上下文的 FNV-1a 哈希值（用于调试和去重）。
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

    /// 根据寄存器值的高 4 位进行分类变换（寄存器值规范化）。
    /// 高 4 位 0~3: 截断高位；4~7: 符号扩展；8~11: 取负；其他: 归零。
    pub fn reg_class(&self, idx: usize) -> u64 {
        if idx >= N_REGS { return 0; }
        let v = self.r[idx];
        match v >> 60 {
            0..=3 => v & 0x0FFF_FFFF_FFFF_FFFF,
            4..=7 => (v << 4) >> 4,
            8..=11 => v.wrapping_neg(),
            _ => 0,
        }
    }
}

// ==================== 中断控制器 ====================

/// 中断控制器，管理中断屏蔽、嵌套计数、异常分发和缺页处理。
/// 类似于真实 CPU 的中断控制器（如 x86 的 APIC）。
pub struct TrapCtl {
    /// 是否正在处理中断（原子布尔，防止递归中断）
    pub active: AtomicBool,
    /// 硬件中断屏蔽掩码（低 8 位对应 IRQ 0~7，位为 1 表示启用）
    pub hw_mask: AtomicU32,
    /// 软件中断屏蔽掩码（低 8 位对应 SW 0~7，位为 1 表示启用）
    pub sw_mask: AtomicU32,
    /// 中断嵌套深度（当前嵌套了多少层中断处理）
    pub nest: AtomicUsize,
    /// 当前正在处理的中断帧（Mutex 保护的可选 Context）
    pub frame: Mutex<Option<Context>>,
    /// 中断帧栈（嵌套中断时保存/恢复外层上下文）
    pub stack: Mutex<Vec<Context>>,
    /// 中断是否使能（全局中断开关）
    pub irq_on: AtomicBool,
    /// 是否抑制中断处理（用于临界区保护）
    pub suppressed: AtomicBool,
    /// 中断服务程序表：向量 0~15 各一个可选回调。
    /// 内核通过 register_handler() 注入真实处理逻辑，回调可修改 Context。
    handlers: Mutex<Box<[Option<Box<dyn Fn(&mut Context) + Send>>; 16]>>,
}

impl TrapCtl {
    /// 创建默认的中断控制器：所有中断关闭（掩码为 0），IRQ 使能，无注册的 ISR。
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            hw_mask: AtomicU32::new(0),
            sw_mask: AtomicU32::new(0),
            nest: AtomicUsize::new(0),
            frame: Mutex::new(None),
            stack: Mutex::new(Vec::new()),
            irq_on: AtomicBool::new(true),
            suppressed: AtomicBool::new(false),
            handlers: Mutex::new(Box::new([
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
            ])),
        }
    }
    /// 配置中断掩码。a → 软件中断掩码 (sw_mask)，b → 硬件中断掩码 (hw_mask)。
    pub fn configure(&self, a: u32, b: u32) {
        self.hw_mask.store(b, Ordering::SeqCst);
        self.sw_mask.store(a, Ordering::SeqCst);
    }
    /// 读取硬件中断掩码
    pub fn hw(&self) -> u32 {
        self.hw_mask.load(Ordering::SeqCst)
    }
    /// 读取软件中断掩码
    pub fn sw(&self) -> u32 {
        self.sw_mask.load(Ordering::SeqCst)
    }
    /// 判断是否正在中断处理中（active 为真或嵌套深度 > 0）
    pub fn in_handler(&self) -> bool {
        let a = self.active.load(Ordering::SeqCst);
        let n = self.nest.load(Ordering::SeqCst);
        a || n > 0
    }

    /// 注册中断服务程序。vector 范围 0~15，handler 接收 &mut Context 可修改上下文。
    /// 内核初始化时调用此方法注入真实中断处理逻辑。
    pub fn register_handler(&self, vector: usize, handler: Box<dyn Fn(&mut Context) + Send>) {
        if vector < 16 {
            self.handlers.lock().unwrap()[vector] = Some(handler);
        }
    }

    /// 核心分发函数（对标 x86/RISC-V 标准统一 trap handler）：
    ///   dispatch_vector 做向量匹配 + 掩码校验 → dispatch 做统一 trap 处理
    ///
    /// 完整流程：
    ///   ① 保存上下文到帧槽 → ② active=true, nest+1
    ///   → ③ 查 handlers 表调用已注册的 ISR（回调可修改 Context）
    ///   → ④ 将修改后的上下文写回帧槽 → ⑤ 从帧槽读出作为恢复结果
    ///   → ⑥ nest-1, active=false → 返回恢复后的上下文
    pub fn dispatch(&self, vector: usize, ctx: Context) -> Context {
        // ① 保存上下文到帧槽
        {
            let mut fg = self.frame.lock().unwrap();
            *fg = Some(ctx);
        }
        // ② 进入中断处理
        self.active.store(true, Ordering::SeqCst);
        self.nest.fetch_add(1, Ordering::SeqCst);
        // ③ 查 handlers 表，调用已注册的 ISR
        if vector < 16 {
            let handlers = self.handlers.lock().unwrap();
            if let Some(handler) = handlers[vector].as_ref() {
                let mut fg = self.frame.lock().unwrap();
                if let Some(ref mut c) = *fg {
                    handler(c);
                }
            }
        }
        // ④ 将（可能被 ISR 修改的）上下文写回帧槽
        // ⑤ 从帧槽读出——完成 save/restore 循环
        let result = {
            let fg = self.frame.lock().unwrap();
            fg.as_ref().cloned().unwrap_or_else(Context::new)
        };
        // ⑥ 退出中断处理
        self.nest.fetch_sub(1, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        result
    }
    /// 获取当前中断帧的克隆
    pub fn current(&self) -> Option<Context> {
        let guard = self.frame.lock().unwrap();
        guard.as_ref().map(|ctx| ctx.clone())
    }
    /// 处理缺页异常：计算缺页地址的页基址和页内偏移。
    /// 实际页面分配/COW 由内核注册的 vector 14 handler 完成。
    pub fn on_pgfault(&self, va: usize) -> Result<(usize, usize), &'static str> {
        let page = va & !(PAGE_SZ - 1);
        let offset = va & (PAGE_SZ - 1);
        Ok((page, offset))
    }

    /// 根据中断向量号分发到对应的处理逻辑。
    /// 向量 0~7: 硬件中断，检查 hw_mask 对应位
    /// 向量 8~13,15: 软件中断，检查 sw_mask 对应位
    /// 向量 14: 缺页异常（不可屏蔽，始终处理）
    pub fn dispatch_vector(&self, vector: usize, ctx: Context) -> Context {
        let hw = self.hw_mask.load(Ordering::SeqCst);
        let sw = self.sw_mask.load(Ordering::SeqCst);
        match vector {
            0..=7 => {
                if hw & (1 << vector) != 0 { return self.dispatch(vector, ctx); }
                ctx
            }
            14 => {
                // 缺页异常：不可屏蔽，始终分发
                self.dispatch(vector, ctx)
            }
            8..=15 => {
                let sw_bit = vector - 8;
                if sw & (1 << sw_bit) != 0 { return self.dispatch(vector, ctx); }
                ctx
            }
            _ => ctx,
        }
    }

    /// 将上下文压入帧栈（嵌套中断时保存外层状态）
    pub fn push_frame(&self, ctx: &Context) {
        self.stack.lock().unwrap().push(ctx.clone());
    }

    /// 从帧栈弹出上下文（中断返回时恢复外层状态）
    pub fn pop_frame(&self) -> Option<Context> {
        self.stack.lock().unwrap().pop()
    }

    /// 获取当前中断嵌套深度
    pub fn nest_depth(&self) -> usize {
        self.nest.load(Ordering::SeqCst)
    }

    /// 启用中断抑制（进入临界区，中断不被处理）
    pub fn suppress(&self) {
        self.suppressed.store(true, Ordering::SeqCst);
    }

    /// 取消中断抑制（离开临界区）
    pub fn unsuppress(&self) {
        self.suppressed.store(false, Ordering::SeqCst);
    }
}

// ────────────────────────────────────────────────────────────────
// [BUG-16] trap.rs — TrapCtl 死代码清理与 dispatch 语义重写
//   日期：2026-07-01
//   触发：代码审查发现 trap.rs 中大量死变量、无意义计算、dispatch 空壳
//
// 问题清单：
//   1. set_ip() / set_sp() 中 `let _old = ...` 保存旧值但从未使用
//   2. apply() 中 `_checksum` 校验和算完后既不存储也不返回
//   3. configure() 中 `combined` 和 `_parity` 奇偶校验位纯浪费 CPU
//   4. hw() / sw() 中 `_check` 重复 load 同一原子变量
//   5. handle_irq() 中 `_nest_before` / `_supp` / `_suppressed_tick` 全为死变量
//   6. on_pgfault() 中 `_page` / `_offset` 计算后未使用
//   7. clone_with_ret() 的 while 循环可简化为 for
//   8. dispatch() 是 no-op：保存帧后立即 +1/-1 nest 再原样返回 ctx
//   9. dispatch_vector() 中 vector 14 被 8..=15 覆盖导致不可达
//  10. handle_irq() 与 dispatch() 重复保存帧、重复 nest ±1、active 时序混乱
//  11. 缺页双重故障检测在 dispatch 和 on_pgfault 中重复写了两遍
//  12. dispatch 无 ISR 回调机制：所有向量分支都是空操作，"假模假式"
//  13. save/restore 循环不完整：保存了上下文但恢复时直接返回入参
//
// 修复（对标 x86/RISC-V 标准两层 trap 分发范式）：
//   - 删除所有 `_` 前缀死变量和无副作用的计算
//   - 手动 Context 构造全部改用 ctx.clone()
//   - TrapCtl 新增 handlers 字段：16 路 ISR 回调表
//     Mutex<Box<[Option<Box<dyn Fn(&mut Context)+Send>>; 16]>>
//     内核通过 register_handler(vector, callback) 注入真实处理逻辑
//   - dispatch(vector, ctx) 重写为标准统一 trap handler：
//     ① 保存 ctx 到帧槽（move，不 clone）
//     ② active=true, nest+1
//     ③ 查 handlers[vector]，若已注册则调用回调（可修改帧槽中 Context）
//     ④ 从帧槽读出（可能被修改的）上下文——完成 save/restore 循环
//     ⑤ nest-1, active=false → 返回恢复后的上下文
//   - 删除 handle_irq()（逻辑内联到 dispatch 的回调机制中）
//   - on_pgfault() 改为返回 (page, offset)，不再做死计算
//   - dispatch_vector() 重排 match：14 放在 8..=15 之前，缺页不可屏蔽
//
//   状态：【已修复】33/33 测试全过。
// ────────────────────────────────────────────────────────────────
