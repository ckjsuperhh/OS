//! 定时器系统模块：基于时间轮（Timer Wheel）的定时器调度和条目管理。
//!
//! 本模块实现了内核的定时器机制，包含两个核心结构体：
//! - `TimerEntry`：单个定时器条目，记录到期时间、间隔、回调 ID 和状态
//! - `TimerWheel`：时间轮调度器，使用环形槽数组高效管理大量定时器
//!
//! 时间轮算法原理：
//! 1. 使用 256 个槽（TIMER_WHEEL_SIZE）组成环形数组
//! 2. 定时器按 deadline % 256 放入对应槽
//! 3. 每次时钟中断（10ms），指针前进一格
//! 4. 检查当前槽中的定时器，到期的触发回调
//! 5. 周期定时器自动重新插入新槽
//!
//! 依赖：
//! - `consts::TIMER_WHEEL_SIZE`：时间轮槽数
//! - `util::CLK`：全局原子时钟计数器（每 tick +1）

use std::sync::atomic::Ordering;
use crate::consts::*;
use crate::util::CLK;

/// 内核定时器条目。
/// 表示一个待触发的定时任务，可以是一次性的或周期性的。
pub struct TimerEntry {
    /// 到期时间戳（以 tick 为单位的绝对时间）。
    /// 当全局时钟 CLK 超过此值时，定时器到期。
    pub deadline: usize,
    /// 重复间隔（tick 数）。0 表示一次性定时器，>0 表示周期性定时器。
    pub interval: usize,
    /// 回调函数 ID。内核维护一个回调函数表，通过此 ID 索引实际的回调函数。
    /// 使用 ID 而非函数指针避免了 Rust 中 fn() 的生命周期和 Send/Sync 约束问题。
    pub callback_id: usize,
    /// 定时器是否处于激活状态。false 表示已取消或一次性定时器已过期。
    pub active: bool,
    /// 是否循环触发。由 interval > 0 自动推导（空间换时间，避免重复判断）。
    pub repeat: bool,
}

impl TimerEntry {
    /// 创建一个新的定时器条目。
    /// - deadline: 到期的绝对时间戳（tick 数）
    /// - interval: 重复间隔（0 = 一次性，>0 = 周期性）
    /// - cb_id: 回调函数 ID
    pub fn new(deadline: usize, interval: usize, cb_id: usize) -> Self {
        Self {
            deadline,
            interval,
            callback_id: cb_id,
            active: true,              // 创建即激活
            repeat: interval > 0,      // 自动推导是否为周期定时器
        }
    }

    /// 检查定时器是否已到期。
    /// 通过原子加载全局时钟 CLK，与 deadline 比较。
    /// 使用 Relaxed 内存序：时钟单调递增，读到略过时的值只会导致延迟一 tick 检测。
    pub fn expired(&self) -> bool {
        CLK.load(Ordering::Relaxed) > self.deadline
    }

    /// 重置定时器。
    /// - 周期定时器：将 deadline 延长一个 interval（基于当前时间而非旧 deadline）
    /// - 一次性定时器：标记为非激活状态
    ///
    /// 注意：基于当前时间计算新 deadline，不会累积延迟误差，但可能跳过预期的触发。
    pub fn reset(&mut self) {
        if self.repeat {
            self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
        } else {
            self.active = false;
        }
    }

    /// 返回距离到期的剩余 tick 数。
    /// 如果已到期返回 0。
    pub fn remaining(&self) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        if now >= self.deadline { 0 } else { self.deadline - now }
    }

    /// 取消定时器（标记为非激活）。
    /// 不会立即从时间轮中移除，而是在 advance() 遍历时被跳过。
    pub fn cancel(&mut self) { self.active = false; }
}

/// 时间轮调度器。
/// 使用环形槽数组管理定时器，提供 O(1) 的插入操作。
/// 每次时钟中断调用 advance() 推进一格并返回到期的定时器。
pub struct TimerWheel {
    /// 时间轮槽数组。每个槽是一个 TimerEntry 向量，
    /// 包含 deadline 对 TIMER_WHEEL_SIZE 取模等于该槽索引的所有定时器。
    pub slots: Vec<Vec<TimerEntry>>,
    /// 当前指针位置。每次 advance() 前进一格（环形递增）。
    pub current_slot: usize,
}

impl TimerWheel {
    /// 创建一个空的时间轮。
    /// 分配 TIMER_WHEEL_SIZE (256) 个空槽，指针初始位于 0。
    pub fn new() -> Self {
        let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
        for _ in 0..TIMER_WHEEL_SIZE {
            slots.push(Vec::new());
        }
        Self { slots, current_slot: 0 }
    }

    /// 将定时器条目插入时间轮。
    /// 根据 deadline 取模确定目标槽位，复杂度 O(1)。
    /// 注意：不同周期（相差 256 的倍数）的定时器会落入同一槽，
    /// 由 advance() 中的 expired() 精确比较来区分。
    pub fn add_timer(&mut self, entry: TimerEntry) {
        let slot = entry.deadline % TIMER_WHEEL_SIZE;
        self.slots[slot].push(entry);
    }

    /// 推进时间轮一格，返回本轮到期的所有定时器。
    ///
    /// 处理流程：
    /// 1. 指针前移一格（环形）
    /// 2. 遍历当前槽中的所有条目：
    ///    - active 且 expired → 加入 fired 列表
    ///    - active 但未到期 → 保留在槽中
    ///    - 非 active → 丢弃
    /// 3. 对周期定时器：调用 reset() 更新 deadline，创建新条目插入新槽
    ///
    /// 返回的 fired 列表中的条目由调用者负责执行回调。
    pub fn advance(&mut self) -> Vec<TimerEntry> {
        // 指针前移一格
        self.current_slot = (self.current_slot + 1) % TIMER_WHEEL_SIZE;
        let mut fired = Vec::new();       // 到期的定时器集合
        let slot = &mut self.slots[self.current_slot];
        let mut remaining = Vec::new();   // 未到期但仍活跃的定时器

        // 遍历当前槽中的所有条目，分类处理
        for entry in slot.drain(..) {
            if entry.active && entry.expired() {
                fired.push(entry);          // 到期且活跃 → 触发
            } else if entry.active {
                remaining.push(entry);      // 活跃但未到期 → 保留
            }
            // !active 的条目直接丢弃（已取消或已过期的一次性定时器）
        }

        // 将未到期条目放回槽中
        *slot = remaining;

        // 处理周期定时器：重置 deadline 并重新插入对应槽
        for t in fired.iter_mut() {
            if t.repeat {
                t.reset();    // 更新 deadline = now + interval
                let new_slot = t.deadline % TIMER_WHEEL_SIZE;
                // 创建新条目插入（原条目需保留在 fired 中返回给调用者）
                let clone = TimerEntry::new(t.deadline, t.interval, t.callback_id);
                self.slots[new_slot].push(clone);
            }
        }

        fired
    }

    /// 按 callback_id 取消第一个匹配的活跃定时器。
    /// 使用"延迟删除"策略：不立即从槽中移除，只标记 active = false，
    /// 下次 advance() 遍历时自动跳过。
    ///
    /// 复杂度 O(N)，需遍历所有槽中的所有条目。
    /// 返回 true 表示找到并取消了，false 表示未找到匹配的活跃定时器。
    pub fn cancel(&mut self, cb_id: usize) -> bool {
        for slot in self.slots.iter_mut() {
            for entry in slot.iter_mut() {
                if entry.callback_id == cb_id && entry.active {
                    entry.active = false;
                    return true;
                }
            }
        }
        false
    }

    /// 统计所有槽中活跃的定时器总数。
    /// 遍历所有槽的所有条目，过滤出 active == true 的进行计数。
    pub fn active_count(&self) -> usize {
        self.slots.iter().flat_map(|s| s.iter()).filter(|e| e.active).count()
    }
}
