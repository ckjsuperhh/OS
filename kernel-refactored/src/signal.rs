//! 信号处理模块：信号动作定义、待处理/屏蔽信号掩码管理、信号投递与查询。
//!
//! 本模块实现了 POSIX 风格的信号机制，包含两个核心结构体：
//! - `SigAction`：描述单个信号的处理方式（处理函数地址、标志、临时屏蔽掩码）
//! - `SigSet`：管理进程的信号集合状态，包括 pending（待处理）、blocked（屏蔽）
//!   以及每个信号的处理动作
//!
//! 信号的生命周期：
//! 1. 事件发生 → `sig_raise()` 将信号加入 pending 集合
//! 2. 返回用户态前 → `deliverable()` 检查 pending & ~blocked 找到可投递信号
//! 3. 获取处理动作 → `get_action()` 查询该信号的 SigAction
//! 4. 执行处理 → 默认动作 / 用户自定义函数 / 忽略

use crate::consts::*;

/// 信号处理动作描述符。
/// 对应 POSIX `struct sigaction`，定义了当某个信号到达时内核应如何处理。
pub struct SigAction {
    /// 信号处理函数的用户态地址。
    /// 特殊值：SIG_DFL (0) = 使用内核默认动作，SIG_IGN (1) = 忽略信号。
    /// 其他值为自定义处理函数的虚拟地址。
    pub handler: usize,
    /// 处理行为标志位（如 SA_RESTART 重启被中断的系统调用）。
    /// 当前实现中预留了接口，具体标志位尚未解析。
    pub flags: u32,
    /// 处理该信号期间临时屏蔽的其他信号位掩码。
    /// 防止信号处理函数执行时被其他信号打断（嵌套中断）。
    pub mask: u64,
}

/// 信号集合状态管理器。
/// 每个进程持有一个 SigSet 实例，用于追踪该进程的信号状态。
/// 使用 u64 位掩码表示信号集，支持最多 64 个信号（信号编号 1~63）。
pub struct SigSet {
    /// 待处理信号位掩码：每一位代表一个信号，1 表示该信号已投递但尚未处理。
    /// 例如 bit 9 = 1 表示 SIGKILL 正在等待处理。
    pub pending: u64,
    /// 屏蔽信号位掩码：每一位代表一个信号，1 表示该信号被暂时屏蔽不投递。
    /// 注意：SIGKILL 和 SIGSTOP 永远不能被屏蔽（POSIX 强制规定）。
    pub blocked: u64,
    /// 每个信号对应的处理动作数组。
    /// 索引 0 不使用，索引 1~NSIG 对应信号 1~64。
    pub actions: Vec<SigAction>,
}

impl SigSet {
    /// 创建一个新的空信号集合。
    /// 所有信号初始为默认处理（SIG_DFL），无待处理信号，无屏蔽信号。
    pub fn new() -> Self {
        let mut actions = Vec::with_capacity(NSIG as usize + 1);
        // 为信号 0~NSIG 分配默认动作
        for _ in 0..=NSIG {
            actions.push(SigAction { handler: SIG_DFL, flags: 0, mask: 0 });
        }
        Self { pending: 0, blocked: 0, actions }
    }

    /// 检查指定信号是否在 pending（待处理）集合中。
    /// 通过位与操作检测第 signo 位是否为 1。
    pub fn sig_pending(&self, signo: u32) -> bool {
        (self.pending & (1u64 << signo)) != 0
    }

    /// 向进程投递一个信号（将其加入 pending 集合）。
    /// 信号不会立即被处理，而是等到进程返回用户态时由 deliverable() 检查。
    /// 这是 POSIX 信号的标准行为：异步投递、同步处理。
    pub fn sig_raise(&mut self, signo: u32) {
        if signo < NSIG {
            self.pending |= 1u64 << signo;  // 将第 signo 位置 1
        }
    }

    /// 计算所有可投递的待处理信号（pending 中未被 blocked 屏蔽的）。
    /// 返回一个位掩码，每一位代表一个可投递的信号。
    /// 排除信号 0（不使用）。
    pub fn coalesce_pending(&mut self) -> u64 {
        self.pending & !self.blocked & !1
    }

    /// 从 pending 集合中清除指定信号（标记为已处理完毕）。
    pub fn sig_clear(&mut self, signo: u32) {
        if signo < NSIG {
            self.pending &= !(1u64 << signo);  // 将第 signo 位清 0
        }
    }

    /// 添加屏蔽信号（将 mask 中的位合并到 blocked）。
    /// SIGKILL 和 SIGSTOP 强制不可屏蔽——这是 POSIX 标准的硬性规定，
    /// 确保系统管理员始终能终止或暂停任何进程。
    pub fn sig_block(&mut self, mask: u64) {
        self.blocked |= mask;
        // 强制清除 SIGKILL (9) 和 SIGSTOP (19) 的屏蔽位
        self.blocked &= !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    }

    /// 解除屏蔽信号（将 mask 中的位从 blocked 中移除）。
    pub fn sig_unblock(&mut self, mask: u64) {
        self.blocked &= !mask;
    }

    /// 直接设置屏蔽掩码（替换整个 blocked 值）。
    /// 同样强制排除 SIGKILL 和 SIGSTOP。
    pub fn sig_setmask(&mut self, mask: u64) {
        self.blocked = mask & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    }

    /// 返回下一个可投递的信号编号。
    /// 可投递 = 在 pending 中且未被 blocked 屏蔽。
    /// 使用 trailing_zeros() 直接定位最低位，O(1) 复杂度。
    pub fn deliverable(&self) -> Option<u32> {
        let actionable = self.pending & !self.blocked & !1;
        if actionable == 0 { return None; }
        Some(actionable.trailing_zeros())
    }

    /// 设置指定信号的处理动作。
    /// SIGKILL 和 SIGSTOP 的处理动作不可修改（POSIX 规定）。
    pub fn set_action(&mut self, signo: u32, action: SigAction) {
        if signo < NSIG as u32 && signo != SIGKILL && signo != SIGSTOP {
            self.actions[signo as usize] = action;
        }
    }

    /// 获取指定信号的处理动作。
    /// 超出范围时返回 actions[0]（安全的默认值，handler = SIG_DFL）。
    pub fn get_action(&self, signo: u32) -> &SigAction {
        if signo < NSIG {
            &self.actions[signo as usize]
        } else {
            &self.actions[0]
        }
    }

    /// 检查指定信号是否被设置为忽略（handler == SIG_IGN）。
    pub fn is_ignored(&self, signo: u32) -> bool {
        if signo < NSIG {
            self.actions[signo as usize].handler == SIG_IGN
        } else {
            false
        }
    }

    /// 将所有自定义处理函数重置为默认动作（SIG_DFL）。
    /// 在 exec() 系统调用后调用——新程序不继承旧程序的信号处理函数，
    /// 因为旧函数指针在新地址空间中已无意义。
    /// SIG_IGN（忽略）状态在 exec 后保留——这是 POSIX 标准的规定。
    pub fn clear_non_caught(&mut self) {
        for i in 1..NSIG as usize {
            // 只重置自定义处理函数（既非 SIG_DFL 也非 SIG_IGN 的）
            if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
                self.actions[i].handler = SIG_DFL;
            }
        }
    }
}
// ── Signal Debug Notes ───────────────────────────────────────────
// [BUG-08] coalesce_pending() 用循环逐位遍历构建结果掩码，完全冗余。
//   `pending & !blocked` 已经是正确的可投递集合，循环只是原样复制每一位。
//   修复：直接返回 `self.pending & !self.blocked & !1`（& !1 排除信号 0）。
//
// [BUG-09] deliverable() 用 for 循环从 1 到 NSIG 逐位查找第一个可投递信号。
//   Rust 提供 u64::trailing_zeros()，可在 O(1) 内定位最低置位。
//   修复：`actionable.trailing_zeros()` 替代循环，同样用 `& !1` 排除信号 0。
//
// [BUG-10] get_action() / is_ignored() / clear_non_caught() 用 self.actions.len()
//   做边界检查，而其他方法（sig_raise、sig_clear、sig_block 等）统一用 NSIG 常量。
//   风格不一致，且 actions.len() == NSIG + 1，语义不直接。
//   修复：统一改用 `signo < NSIG` 或 `1..NSIG as usize`。
// ─────────────────────────────────────────────────────────────────
