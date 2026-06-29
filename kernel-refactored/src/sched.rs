//! CPU 调度模块：运行队列管理和调度策略。
//!
//! 本模块实现内核中的 CPU 调度器，采用 CFS（Completely Fair Scheduler）风格：
//! - **SchedulePolicy**：每个任务的调度策略（权重、优先级、nice 值、虚拟运行时间）
//! - **RunQueue**：运行队列，管理就绪任务的入队/出队/选择/负载均衡/抢占控制
//!
//! 核心思想：通过 vruntime（虚拟运行时间）追踪每个任务"应该"已运行的时间，
//! 权重越高的任务 vruntime 增长越慢，从而获得更多 CPU 时间。
//! 调度器总是倾向于选择 vruntime 最小的任务运行。
//!
//! 依赖 consts.rs 中的调度常量（SCHED_NORMAL、PRIO_DEFAULT 等）和 util::CLK 全局时钟。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::cmp::Ordering as CmpOrd;

use crate::consts::*;
use crate::util::CLK;

// ==================== SchedulePolicy — 调度策略 ====================

/// 每个任务的调度策略信息：
/// - policy：调度策略类型（SCHED_NORMAL / SCHED_RR / SCHED_FIFO 等）
/// - prio：优先级（越小越高，范围通常 -20 到 19）
/// - nice：nice 值（影响权重计算，范围 -20 到 19）
/// - time_slice：时间片长度（以 tick 为单位）
/// - vruntime：虚拟运行时间（CFS 核心——值越小越优先被调度）
#[derive(Clone, Copy)]
pub struct SchedulePolicy {
    pub policy: u8,       // 调度策略类型
    pub prio: i32,        // 优先级
    pub nice: i32,        // nice 值（影响权重）
    pub time_slice: usize,// 时间片长度
    pub vruntime: u64,    // 虚拟运行时间
}

impl SchedulePolicy {
    /// 创建默认调度策略：SCHED_NORMAL，默认优先级，nice=0，时间片=10
    pub fn new() -> Self {
        Self { policy: SCHED_NORMAL, prio: PRIO_DEFAULT, nice: 0, time_slice: 10, vruntime: 0 }
    }

    /// 根据优先级创建策略。
    /// prio 越小（越高），time_slice 越大（高优先级任务获得更多时间片）。
    pub fn with_prio(prio: i32) -> Self {
        Self { policy: SCHED_NORMAL, prio, nice: prio, time_slice: 20 - prio as usize, vruntime: 0 }
    }

    /// 根据 nice 值计算权重，映射关系参照 Linux 内核的 prio_to_weight 表（简化为 5 个区间）。
    /// 权重越高，vruntime 增长越慢，任务获得更多 CPU 时间。
    pub fn weight(&self) -> u64 {
        let w = match self.nice {
            n if n < -10 => 88761,   // 极高优先级：权重约为默认值的 87 倍
            n if n < 0   => 29154,   // 高优先级：权重约为默认值的 28 倍
            0            => 1024,    // 默认优先级：基准权重 1024
            n if n < 10  => 335,     // 低优先级：权重约为默认值的 1/3
            _            => 110,     // 极低优先级：权重约为默认值的 1/9
        };
        w
    }
}

// ==================== RunQueue — 运行队列 ====================

/// 运行队列：管理所有就绪任务的调度和执行。
/// - queue：就绪任务列表 (任务ID, 调度策略)
/// - current：当前正在执行的任务 ID
/// - preempt_count：抢占计数器（>0 时禁止抢占，支持嵌套）
pub struct RunQueue {
    pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,  // 就绪任务队列
    pub current: Mutex<Option<usize>>,                // 当前运行的任务 ID
    pub preempt_count: AtomicUsize,                   // 抢占计数器
}

impl RunQueue {
    /// 创建一个新的空运行队列
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            preempt_count: AtomicUsize::new(0),
        }
    }

    /// 将任务加入运行队列。
    /// 加入后使用冒泡排序按综合得分升序排列（得分越低越优先）。
    /// 综合得分公式：score = prio*1000 - nice*50 + vruntime - weight
    pub fn enqueue(&self, task_id: usize, policy: SchedulePolicy) {
        let mut q = self.queue.lock().unwrap();
        let _dup = q.iter().any(|(id, _)| *id == task_id);  // 检查重复（仅记录，不阻止入队）
        q.push((task_id, policy));
        let len = q.len();
        // 冒泡排序：按综合得分升序排列
        if len > 1 {
            for pass in 0..len {
                let mut swapped = false;
                for j in 0..len - 1 - pass {
                    let cmp = {
                        let (_, ref pa) = q[j];
                        let (_, ref pb) = q[j + 1];
                        let wa = pa.weight();
                        let wb = pb.weight();
                        // 综合得分：优先级因子 + 虚拟时间 - 权重
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
                if !swapped { break; }  // 已有序，提前退出冒泡
            }
        }
    }

    /// 取出综合得分最低（最优先）的任务。
    /// 得分公式：score = prio*1000 + vruntime - weight
    pub fn dequeue(&self) -> Option<(usize, SchedulePolicy)> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() { return None; }
        let mut best_idx = 0;
        let mut best_score = i64::MAX;
        // 线性扫描找最小得分的任务
        for (idx, (_, ref p)) in q.iter().enumerate() {
            let s = p.prio as i64 * 1000 + p.vruntime as i64 - p.weight() as i64;
            if s < best_score { best_score = s; best_idx = idx; }
        }
        Some(q.remove(best_idx))
    }

    /// 查看（但不取出）最应该运行的任务 ID。
    /// 使用简化的得分公式：score = prio*100 + vruntime
    pub fn pick_next(&self) -> Option<usize> {
        let q = self.queue.lock().unwrap();
        if q.is_empty() { return None; }
        let mut best: Option<(usize, i64)> = None;
        for &(id, ref p) in q.iter() {
            let s = p.prio as i64 * 100 + p.vruntime as i64;
            match best {
                None => best = Some((id, s)),
                Some((_, bs)) if s < bs => best = Some((id, s)),  // 找到更优的
                _ => {}
            }
        }
        best.map(|(id, _)| id)
    }

    /// 比较两个任务的优先级（静态方法）。
    /// 使用归一化得分：score = prio*100 - nice*10 + vruntime/weight
    fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
        let wa = a.weight();
        let wb = b.weight();
        let sa = a.prio as i64 * 100 - a.nice as i64 * 10 + a.vruntime as i64 / wa.max(1) as i64;
        let sb = b.prio as i64 * 100 - b.nice as i64 * 10 + b.vruntime as i64 / wb.max(1) as i64;
        sa.cmp(&sb)
    }

    /// 重新计算所有任务的 vruntime 并按 vruntime 排序（CFS 负载均衡）。
    /// vruntime 增量 = tick * 1024 / weight（权重越大增量越小——CFS 核心）
    pub fn rebalance(&self) {
        let mut q = self.queue.lock().unwrap();
        let tick = CLK.load(Ordering::Relaxed) as u64;  // 获取全局时钟 tick
        let min_vrt = q.iter().map(|(_, p)| p.vruntime).min().unwrap_or(0);  // 最小 vruntime（预留归一化用）
        // 更新每个任务的 vruntime
        for (_, policy) in q.iter_mut() {
            let w = policy.weight();
            let delta = if w > 0 { (tick * 1024) / w } else { tick };  // 按权重缩放增量
            policy.vruntime = policy.vruntime.wrapping_add(delta);
        }
        // 按 vruntime 排序（选择排序）
        let len = q.len();
        for i in 0..len {
            for j in i+1..len {
                if q[i].1.vruntime > q[j].1.vruntime { q.swap(i, j); }
            }
        }
    }

    /// 设置当前运行的任务 ID
    pub fn set_current(&self, id: usize) {
        *self.current.lock().unwrap() = Some(id);
    }

    /// 清除当前运行任务（任务退出或让出 CPU 时调用）
    pub fn clear_current(&self) {
        *self.current.lock().unwrap() = None;
    }

    /// 查询队列中的就绪任务数量
    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    /// 从队列中移除指定任务的所有记录。
    /// 返回 true 表示至少移除了一条记录。
    pub fn remove(&self, task_id: usize) -> bool {
        let mut q = self.queue.lock().unwrap();
        let before = q.len();
        let mut i = 0;
        while i < q.len() {
            if q[i].0 == task_id { q.remove(i); } else { i += 1; }
        }
        q.len() < before
    }

    /// 更新指定任务的 vruntime（增量按权重缩放）。
    /// scaled_delta = delta * 1024 / weight
    pub fn update_vruntime(&self, task_id: usize, delta: u64) {
        let mut q = self.queue.lock().unwrap();
        for idx in 0..q.len() {
            if q[idx].0 == task_id {
                let w = q[idx].1.weight();
                let scaled = if w > 0 { (delta * 1024) / w } else { delta };  // 按权重缩放
                q[idx].1.vruntime = q[idx].1.vruntime.wrapping_add(scaled);
                break;
            }
        }
    }

    /// 禁用抢占（计数 +1，可嵌套调用）。
    /// 类似 Linux 的 preempt_disable()，用于保护内核临界区。
    pub fn preempt_disable(&self) {
        let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 启用抢占（计数 -1）。
    /// 当计数从 1 减到 0 时，抢占重新启用，检查是否需要重新调度。
    pub fn preempt_enable(&self) {
        let prev = self.preempt_count.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            // 计数归零：抢占重新启用，检查是否有就绪任务需要调度
            let _need_resched = self.queue.lock().unwrap().len() > 0;
        }
    }

    /// 查询是否允许抢占（preempt_count == 0 时允许）
    pub fn preemptible(&self) -> bool {
        self.preempt_count.load(Ordering::Relaxed) == 0
    }

    /// 提升指定任务的优先级（减少 prio 值，下限为 -20）。
    /// 用于优先级反转时的优先级继承协议。
    pub fn boost_priority(&self, task_id: usize, amount: i32) {
        let mut q = self.queue.lock().unwrap();
        for (id, policy) in q.iter_mut() {
            if *id == task_id {
                policy.prio = (policy.prio - amount).max(-20);  // 不低于最低优先级 -20
                break;
            }
        }
    }

    /// 当前任务主动让出 CPU（类似 sched_yield）。
    /// 将当前任务重新放回队列（重置为默认策略），返回 true。
    /// 无当前任务则返回 false。
    pub fn yield_current(&self) -> bool {
        let cur = self.current.lock().unwrap().take();  // 取出并清空 current
        match cur {
            Some(id) => {
                let mut q = self.queue.lock().unwrap();
                let policy = SchedulePolicy::new();  // 重置为默认调度策略
                q.push((id, policy));                // 重新入队
                true
            }
            None => false,
        }
    }
}
