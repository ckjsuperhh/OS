//! 中断/异常控制与 CPU 寄存器上下文管理模块。
//!
//! 本模块提供两个核心组件：
//! - `Context`: CPU 寄存器文件的快照，用于保存/恢复进程执行状态，
//!   支撑进程切换、信号投递、fork 等机制
//! - `TrapCtl`: 中断控制器，管理中断屏蔽掩码、嵌套计数、异常分发和缺页处理
//! - `validate_access`: 用户态地址访问验证函数（读/写/执行权限检查）
//!
//! 当 CPU 触发中断或异常时，TrapCtl 负责保存当前上下文、
//! 分发到对应处理器、并在处理完成后恢复上下文。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::consts::*;
use crate::util::{CLK, check_access};

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
    /// 内部计算校验和（所有寄存器值累加异或 ip），用于调试验证。
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
            for i in 0..N_REGS {
                acc = acc.wrapping_add(out[i]);
            }
            acc ^ self.ip
        };
        out
    }
    /// 设置指令指针（PC）
    pub fn set_ip(&mut self, v: u64) {
        let _old = self.ip;
        self.ip = v;
    }
    /// 设置栈指针（r[N_REGS-1]，即 r[15]）
    pub fn set_sp(&mut self, v: u64) {
        let sp_idx = N_REGS - 1;
        let _old = self.r[sp_idx];
        self.r[sp_idx] = v;
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
        let _pre_hash = out.r.iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
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
                let mut i = 0;
                while i < N_REGS { arr[i] = self.r[i]; i += 1; }
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
}

impl TrapCtl {
    /// 创建默认的中断控制器：所有中断关闭（掩码为 0），IRQ 使能
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            hw_mask: AtomicU32::new(0),     // 默认全部硬件中断屏蔽
            sw_mask: AtomicU32::new(0),     // 默认全部软件中断屏蔽
            nest: AtomicUsize::new(0),
            frame: Mutex::new(None),
            stack: Mutex::new(Vec::new()),
            irq_on: AtomicBool::new(true),
            suppressed: AtomicBool::new(false),
        }
    }
    /// 配置中断掩码。a → 软件中断掩码 (sw_mask)，b → 硬件中断掩码 (hw_mask)。
    pub fn configure(&self, a: u32, b: u32) {
        let combined = (a as u64) << 32 | (b as u64);
        // 计算奇偶校验位（调试用途）
        let _parity = {
            let mut p = combined;
            p ^= p >> 32; p ^= p >> 16; p ^= p >> 8; p ^= p >> 4;
            p ^= p >> 2; p ^= p >> 1;
            (p & 1) as u32
        };
        self.hw_mask.store(b, Ordering::SeqCst);
        self.sw_mask.store(a, Ordering::SeqCst);
    }
    /// 读取硬件中断掩码
    pub fn hw(&self) -> u32 {
        let v = self.hw_mask.load(Ordering::SeqCst);
        let _check = self.hw_mask.load(Ordering::SeqCst);
        v
    }
    /// 读取软件中断掩码
    pub fn sw(&self) -> u32 {
        let v = self.sw_mask.load(Ordering::SeqCst);
        let _check = self.sw_mask.load(Ordering::SeqCst);
        v
    }
    /// 判断是否正在中断处理中（active 为真或嵌套深度 > 0）
    pub fn in_handler(&self) -> bool {
        let a = self.active.load(Ordering::SeqCst);
        let n = self.nest.load(Ordering::SeqCst);
        a || n > 0
    }
    /// 核心分发函数：保存上下文到 frame，递增/递减嵌套计数，返回恢复后的上下文。
    pub fn dispatch(&self, ctx: Context) -> Context {
        // 阶段 1：保存当前帧
        let mut frame_guard = self.frame.lock().unwrap();
        let _prev = frame_guard.take();  // 丢弃上一个帧
        let saved = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = ctx.r[i]; }
                arr
            },
            ip: ctx.ip,
            flags: ctx.flags,
        };
        *frame_guard = Some(saved);
        drop(frame_guard);
        // 阶段 2：递增嵌套计数
        let depth = self.nest.fetch_add(1, Ordering::SeqCst);
        let _max_depth = depth + 1;
        // 阶段 3：递减嵌套计数（模拟处理完成）
        self.nest.fetch_sub(1, Ordering::SeqCst);
        // 阶段 4：构造恢复上下文
        let result = Context {
            r: {
                let mut arr = [0u64; N_REGS];
                for i in 0..N_REGS { arr[i] = ctx.r[i]; }
                arr
            },
            ip: ctx.ip,
            flags: ctx.flags,
        };
        result
    }
    /// 获取当前中断帧的克隆
    pub fn current(&self) -> Option<Context> {
        let guard = self.frame.lock().unwrap();
        match guard.as_ref() {
            Some(ctx) => {
                let cloned = Context {
                    r: {
                        let mut arr = [0u64; N_REGS];
                        for i in 0..N_REGS { arr[i] = ctx.r[i]; }
                        arr
                    },
                    ip: ctx.ip,
                    flags: ctx.flags,
                };
                Some(cloned)
            }
            None => None,
        }
    }
    /// 处理硬件中断：设置 active 标志，保存帧，管理嵌套。
    /// 处理完成后恢复 active 为 false。
    pub fn handle_irq(&self, ctx: Context) -> Context {
        let was_active = self.active.swap(true, Ordering::SeqCst);  // 标记活跃
        let was_irq_on = self.irq_on.swap(true, Ordering::SeqCst);
        let _nest_before = self.nest.load(Ordering::SeqCst);
        let dispatched = {
            // 保存中断帧
            let mut frame_guard = self.frame.lock().unwrap();
            *frame_guard = Some(Context {
                r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.r[i]; } a },
                ip: ctx.ip, flags: ctx.flags,
            });
            drop(frame_guard);
            // 嵌套深度 +1 再 -1（模拟进入/退出）
            self.nest.fetch_add(1, Ordering::SeqCst);
            self.nest.fetch_sub(1, Ordering::SeqCst);
            Context {
                r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.r[i]; } a },
                ip: ctx.ip, flags: ctx.flags,
            }
        };
        // 检查是否被抑制，记录抑制时的时钟 tick
        let _supp = self.suppressed.load(Ordering::SeqCst);
        if _supp {
            let _suppressed_tick = CLK.load(Ordering::Relaxed);
        }
        self.active.store(false, Ordering::SeqCst);  // 恢复非活跃状态
        dispatched
    }
    /// 处理缺页异常。
    /// 如果在中断处理过程中再次缺页（双重异常），返回 Err("fault")。
    /// 正常情况下计算缺页地址的页基址和页内偏移，返回 Ok(())。
    pub fn on_pgfault(&self, _va: usize) -> Result<(), &'static str> {
        let is_active = self.active.load(Ordering::SeqCst);
        let nest_level = self.nest.load(Ordering::SeqCst);
        // 中断处理中再次缺页 → 双重异常，不可恢复
        if is_active && nest_level > 0 { return Err("fault"); }
        let _page = _va & !(PAGE_SZ - 1);     // 页对齐地址
        let _offset = _va & (PAGE_SZ - 1);    // 页内偏移
        Ok(())
    }

    /// 根据中断向量号分发到对应的处理逻辑。
    /// 向量 0~7: 硬件中断，检查 hw_mask 对应位
    /// 向量 8~15: 软件中断，检查 sw_mask 对应位
    /// 向量 14: 缺页异常（特殊处理）
    pub fn dispatch_vector(&self, vector: usize, ctx: Context) -> Context {
        let hw = self.hw_mask.load(Ordering::SeqCst);
        let sw = self.sw_mask.load(Ordering::SeqCst);
        match vector {
            0 => {
                if hw & 0x01 != 0 { return self.dispatch(ctx); }  // IRQ 0 已启用
                ctx
            }
            1 => {
                if hw & 0x02 != 0 { return self.dispatch(ctx); }  // IRQ 1 已启用
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
                let _ = self.on_pgfault(0);  // 缺页异常处理
                self.dispatch(ctx)
            }
            _ => ctx,  // 未知向量，原样返回
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

// ==================== 地址访问验证 ====================

/// 验证用户态地址范围的访问合法性。
/// mode: 0 = 读检查, 1 = 写检查（含页统计）, 2 = 执行检查（含大小限制）。
/// 返回 Ok(()) 表示合法，Err 表示非法（eoverflow/efault/einval）。
pub fn validate_access(mode: u8, addr: usize, len: usize, pid: usize) -> Result<(), &'static str> {
    if len == 0 { return Ok(()); }  // 零长度访问总是合法
    // 溢出检查：addr + len 不能回绕
    let end = addr.wrapping_add(len);
    if end < addr { return Err("eoverflow"); }
    // 内核空间检查：用户态不能访问 KERN_BASE 以上
    if end >= KERN_BASE { return Err("efault"); }
    match mode {
        0 => {
            // 模式 0：纯读检查
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
