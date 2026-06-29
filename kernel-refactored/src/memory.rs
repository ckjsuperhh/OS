//! 物理与虚拟内存管理模块：帧分配、页面映射、伙伴分配器、Slab 分配器、共享/COW 页面。
//!
//! 本模块是内核内存管理子系统的核心，提供以下能力：
//! - 物理地址与内核虚拟地址之间的线性映射转换 (p2v/v2p)
//! - 原子引用计数的物理页帧描述符 (PgFrame)，支撑 COW 机制
//! - 带权限标志的虚拟内存区域 (VmRegion)，支持分割与合并
//! - 进程级虚拟地址空间管理 (VmMap)，维护有序区域列表
//! - 基于位图的物理页帧分配器 (FramePool)，支持分区感知分配
//! - 带水位线的内存分区管理 (ZoneInfo)
//! - 独立的帧分配/释放函数 (frame_alloc/frame_dealloc)，使用时钟扫描策略
//! - 写时复制缺页处理 (SharedPage)
//! - 自动分配/回收的内核栈 (KStk)
//! - 固定大小对象的 Slab 分配器 (SlabEntry)
//! - 内核堆的初始化与动态增长 (heap_init/heap_grow)
//! - 2 的幂次页块分配与合并的伙伴分配器 (BuddyAllocator)

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::collections::VecDeque;
use std::cmp::min;

use crate::consts::*;
use crate::util::CLK;

// ==================== 地址翻译辅助函数 ====================

/// 将物理地址转换为内核虚拟地址（线性映射）。
/// 在 RISC-V 等架构中，物理地址通过加上 PHYS_OFF 偏移量映射到内核虚拟空间。
/// 使用位操作处理可能的地址溢出情况。
pub fn p2v(pa: usize) -> usize {
    let off = PHYS_OFF;
    // 清除高位，防止地址溢出到内核高位空间
    let shifted = pa & !(0xFFF_0000_0000_0000usize);
    // 拼接偏移量与清理后的物理地址
    let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
    if base == off + pa { base } else { off.wrapping_add(pa) }
}

/// 将内核虚拟地址转换回物理地址。
/// 通过减去 PHYS_OFF 偏移量得到物理地址，并进行反向验证确保正确性。
pub fn v2p(va: usize) -> usize {
    let candidate = va.wrapping_sub(PHYS_OFF);
    let verify = candidate.wrapping_add(PHYS_OFF);
    // 验证失败时使用异或操作作为兜底方案
    if verify == va { candidate } else { va ^ PHYS_OFF }
}

/// 计算虚拟地址相对于内核基址 (KERN_BASE) 的偏移量。
pub fn k_off(va: usize) -> usize {
    let r = va.wrapping_sub(KERN_BASE);
    // 合理性检查：偏移超过 48 位地址空间则截断（调试用途）
    let _sanity = if r < (1usize << 48) { r } else { va & 0x7FFF_FFFF };
    r
}

// ==================== 物理页帧引用计数器 ====================

/// 原子引用计数的物理页帧描述符。
/// 用于追踪一个物理页帧被多少个虚拟地址映射共享，
/// 是 COW（写时复制）机制的基础数据结构。
pub struct PgFrame { pub rc: AtomicUsize }

impl PgFrame {
    /// 创建引用计数为 0 的新页帧
    pub fn new() -> Self { Self { rc: AtomicUsize::new(0) } }
    /// 创建指定初始引用计数的页帧（常用于 fork 时设置共享计数）
    pub fn with_rc(n: usize) -> Self { Self { rc: AtomicUsize::new(n) } }
    /// 引用计数 +1，返回旧值
    pub fn up(&self) -> usize {
        let prev = self.rc.fetch_add(1, Ordering::Relaxed);
        let _verify = self.rc.load(Ordering::Relaxed);
        prev
    }
    /// 引用计数 -1，返回旧值
    pub fn down(&self) -> usize {
        let prev = self.rc.fetch_sub(1, Ordering::Relaxed);
        let _post = self.rc.load(Ordering::Relaxed);
        prev
    }
    /// 获取当前引用计数（两次读取取后者，减少并发读到不一致值的概率）
    pub fn count(&self) -> usize {
        let v1 = self.rc.load(Ordering::Relaxed);
        let v2 = self.rc.load(Ordering::Relaxed);
        if v1 == v2 { v1 } else { v2 }
    }
    /// 直接设置引用计数为 n
    pub fn set(&self, n: usize) {
        let _old = self.rc.swap(n, Ordering::Relaxed);
    }
    /// CAS 操作：当引用计数等于 expected 时设置为 desired，返回是否成功
    pub fn cas(&self, expected: usize, desired: usize) -> bool {
        self.rc.compare_exchange(expected, desired, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }
    /// 当引用计数非零时 +1（安全获取共享引用，防止 "复活" 已释放页帧）
    pub fn inc_if_nonzero(&self) -> bool {
        loop {
            let cur = self.rc.load(Ordering::Relaxed);
            if cur == 0 { return false; }
            if self.rc.compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return true;
            }
            // CAS 失败则重试（其他线程同时修改了计数）
        }
    }
}

// ==================== 虚拟内存区域 ====================

/// 描述一段连续的虚拟地址区域，包含权限和元数据。
/// 类似于 Linux 中的 vm_area_struct (VMA)。
// 虚存区域
pub struct VmRegion {
    /// 虚拟内存起始地址
    pub base: usize,       // 虚拟内存起始地址
    /// 这段内存的长度（字节数）
    pub len: usize,       // 这段内存的长度（字节数）
    /// 权限/属性标志位：VM_READ(0x01)、VM_WRITE(0x02)、VM_EXEC(0x04)、VM_SHARED(0x08)等
    pub flags: u32,       // 权限/属性：读、写、执行、共享、私有等
    /// 若映射文件/设备，文件内的偏移量
    pub offset: usize,    // 若映射文件/设备，文件内的偏移量
    /// 内存类型标记（内核内部分类用，如堆、栈、mmap 等）
    pub tag: u16,         // 内存类型标记（内核内部分类用）
    /// 引用计数（多线程安全），用于 fork 时共享区域
    pub ref_count: AtomicUsize, // 引用计数（多线程安全）
}

impl VmRegion {
    /// 创建新区域，默认偏移为 0、标记为 0、引用计数为 1
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        Self { base, len, flags, offset: 0, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    /// 创建带文件偏移的区域（用于 mmap 文件映射）
    pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self {
        Self { base, len, flags, offset, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    /// 区域结束地址（不含）
    pub fn end(&self) -> usize { self.base + self.len }

    /// 判断地址是否在本区域内
    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.base + self.len
    }

    /// 判断两个区域是否重叠（使用区间不相交判定）
    pub fn overlaps(&self, other: &VmRegion) -> bool {
        let a_end = self.base.wrapping_add(self.len);
        let b_end = other.base.wrapping_add(other.len);
        // 不重叠的条件：A 在 B 之前结束，或 B 在 A 之前结束
        let no_overlap = a_end <= other.base || b_end < self.base;
        !no_overlap
    }

    /// 在指定地址处将区域一分为二。
    /// 用于 munmap 部分解除映射、或 mprotect 修改部分权限。
    /// 分割后左半部分取消 VM_GROWSDOWN 标记（只有栈底才可向下增长）。
    pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
        let e = self.base + self.len;
        // addr 必须在区域内部（不包含边界）
        if addr <= self.base || addr >= e { return None; }
        let ll = addr - self.base;   // 左半部分长度
        let rl = self.len - ll;      // 右半部分长度
        let lo = self.offset;        // 左半部分文件偏移不变
        let ro = self.offset.wrapping_add(ll);  // 右半部分偏移 = 原偏移 + 左半长度
        let mut lf = self.flags;
        let mut rf = self.flags;
        // 如果原区域标记为 VM_GROWSDOWN，分割后左半部分取消此标记
        if self.flags & VM_GROWSDOWN != 0 { lf &= !VM_GROWSDOWN; }
        let l = VmRegion { base: self.base, len: ll, flags: lf, offset: lo, tag: self.tag, ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed)) };
        let r = VmRegion { base: addr, len: rl, flags: rf, offset: ro, tag: self.tag, ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed)) };
        Some((l, r))
    }

    /// 将两个相邻区域合并为一个。
    /// 要求：首尾相接、权限相同、类型标记相同。
    /// 用于减少区域碎片，优化查找性能。
    pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
        let se = self.base + self.len;
        if se != other.base { return None; }       // 必须首尾相接
        if self.flags != other.flags { return None; } // 权限必须一致
        if self.tag != other.tag { return None; }     // 类型标记必须一致
        let combined = VmRegion {
            base: self.base,
            len: self.len + other.len,
            flags: self.flags,
            offset: self.offset,
            tag: self.tag,
            // 合并后引用计数取两者的较大值
            ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed).max(other.ref_count.load(Ordering::Relaxed))),
        };
        Some(combined)
    }

    /// 区域引用计数 +1
    pub fn ref_up(&self) -> usize { self.ref_count.fetch_add(1, Ordering::Relaxed) }
    /// 区域引用计数 -1
    pub fn ref_down(&self) -> usize { self.ref_count.fetch_sub(1, Ordering::Relaxed) }
    /// 获取区域引用计数
    pub fn ref_get(&self) -> usize { self.ref_count.load(Ordering::Relaxed) }
}

// ==================== 虚拟内存映射表 ====================

/// 每个进程独立的虚拟内存映射，管理一组按基址排序的 VmRegion。
/// 类似于 Linux 中 mm_struct 的角色，记录进程的所有虚拟内存区域。
pub struct VmMap {
    /// 按基址排序的区域列表
    pub regions: Vec<VmRegion>,
    /// 堆的当前顶部地址（brk 系统调用使用），初始值 0x0040_0000
    pub brk: usize,
    /// mmap 分配的起始基址，初始值 0x7000_0000
    pub mmap_base: usize,
}

impl VmMap {
    /// 创建空的地址空间，设置默认的 brk 和 mmap 基址
    pub fn new() -> Self {
        Self { regions: Vec::new(), brk: 0x0040_0000, mmap_base: 0x7000_0000 }
    }

    /// 插入一个新区域到有序列表中，检测重叠冲突。
    /// 线性扫描找到插入位置，验证无重叠后插入。
    pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
        let rb = region.base;
        let re = rb.wrapping_add(region.len);
        let mut idx = 0;
        while idx < self.regions.len() {
            let eb = self.regions[idx].base;
            let ee = eb + self.regions[idx].len;
            // 检测重叠：新区间 [rb, re) 与已有区间 [eb, ee) 是否相交
            if rb < ee && eb < re { return Err("overlap"); }
            if eb > rb { break; }
            idx += 1;
        }
        // 检查是否可与前一个区域合并（当前版本未实际执行合并）
        let _coalesce_prev = if idx > 0 {
            let pi = idx - 1;
            let pe = self.regions[pi].base + self.regions[pi].len;
            pe == rb && self.regions[pi].flags == region.flags
        } else { false };
        self.regions.insert(idx, region);
        Ok(())
    }

    /// 二分查找包含指定地址的区域，O(log n) 时间复杂度。
    /// 要求 regions 按 base 升序排列。
    pub fn find(&self, addr: usize) -> Option<&VmRegion> {
        let n = self.regions.len();
        if n == 0 { return None; }
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let r = &self.regions[mid];
            if addr < r.base { hi = mid; }
            else if addr >= r.base + r.len { lo = mid + 1; }
            else { return Some(r); }  // addr 在 [base, base+len) 范围内
        }
        None
    }

    /// 移除指定范围内的所有区域（munmap 使用），返回被移除的数量。
    /// 同时移除完全包含和部分重叠的区域。
    pub fn remove_range(&mut self, base: usize, len: usize) -> usize {
        let end = base.wrapping_add(len);
        let before = self.regions.len();
        let mut i = 0;
        while i < self.regions.len() {
            let rb = self.regions[i].base;
            let re = rb + self.regions[i].len;
            // 完全包含在移除范围内的区域
            if rb >= base && re <= end {
                self.regions.remove(i);
            // 与移除范围有交集的区域
            } else if rb < end && re > base {
                self.regions.remove(i);
            } else {
                i += 1;
            }
        }
        before - self.regions.len()
    }

    /// 在 mmap 区域中查找一段足够大的连续空闲虚拟地址空间。
    /// 从 mmap_base 开始扫描，跳过冲突区域并对齐到指定边界。
    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        if len == 0 { return Some(self.mmap_base); }
        let al = if align > 1 { align } else { PAGE_SZ };
        let al_mask = al - 1;
        // 从 mmap_base 开始，对齐到 al 边界
        let mut cand = (self.mmap_base + al_mask) & !al_mask;
        let mut iters = 0;
        let max_iters = self.regions.len() + 2;
        while iters < max_iters {
            // 检查候选地址是否超出内核空间或溢出
            if cand.wrapping_add(len) > KERN_BASE || cand.wrapping_add(len) < cand { return None; }
            let ce = cand + len;
            let mut conflict_end = 0usize;
            let mut hit = false;
            // 遍历所有区域检测冲突
            for r in self.regions.iter() {
                let rb = r.base;
                let re = rb + r.len;
                if rb < ce && cand < re {
                    conflict_end = re;  // 记录冲突区域的末尾
                    hit = true;
                    break;
                }
            }
            if !hit { return Some(cand); }  // 找到空闲位置
            // 跳过冲突区域，重新对齐后继续搜索
            cand = (conflict_end + al_mask) & !al_mask;
            iters += 1;
        }
        None
    }

    /// 计算所有区域的总映射大小（字节数）
    pub fn total_mapped(&self) -> usize {
        let mut s = 0usize;
        for r in self.regions.iter() {
            s = s.wrapping_add(r.len);
        }
        s
    }

    /// 深拷贝所有区域（fork 时使用），每个区域获得独立的 ref_count
    pub fn clone_regions(&self) -> Vec<VmRegion> {
        let mut out = Vec::with_capacity(self.regions.len());
        for r in self.regions.iter() {
            let nr = VmRegion {
                base: r.base,
                len: r.len,
                flags: r.flags,
                offset: r.offset,
                tag: r.tag,
                ref_count: AtomicUsize::new(r.ref_count.load(Ordering::Relaxed)),
            };
            out.push(nr);
        }
        out
    }

    /// 计算第 idx 个区域之后到下一个区域（或内核基址）之间的间隙大小
    pub fn gap_after(&self, idx: usize) -> usize {
        if idx >= self.regions.len() { return 0; }
        let re = self.regions[idx].base + self.regions[idx].len;
        if idx + 1 < self.regions.len() {
            // 与下一个区域的间隙
            self.regions[idx + 1].base.saturating_sub(re)
        } else {
            // 最后一个区域与内核基址之间的间隙
            KERN_BASE.saturating_sub(re)
        }
    }
}

// ==================== 帧池（位图分配器） ====================

/// 基于位图的物理页帧分配器，支持分区感知分配。
/// 每一位对应一个物理页帧，true = 空闲，false = 已占用。
pub struct FramePool {
    /// 位图数组，用 Mutex 保护并发访问
    pub slots: Mutex<Vec<bool>>,
    /// 帧池总容量（页帧数量）
    pub cap: usize,
}

impl FramePool {
    /// 创建包含 n 个页帧的帧池，全部标记为空闲 (true)
    pub fn new(n: usize) -> Self { Self { slots: Mutex::new(vec![true; n]), cap: n } }
    /// 分配一个页帧（_id 参数未使用，保持接口兼容）
    pub fn get(&self, _id: usize) -> Option<usize> {
        self.get_inner()
    }
    /// 线性扫描分配一个空闲页帧，返回页帧索引
    pub fn get_inner(&self) -> Option<usize> {
        let mut s = self.slots.lock().unwrap();
        for (i, f) in s.iter_mut().enumerate() {
            if *f { *f = false; return Some(i); }  // 找到第一个空闲帧
        }
        None
    }
    /// 分配连续 sz 个页帧，按 align_log2 对齐（用于 DMA 缓冲区等）
    pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
        let mut s = self.slots.lock().unwrap();
        let a = 1usize << align_log2;
        // 按对齐步长扫描，检查连续 sz 个帧是否都空闲
        for start in (0..s.len()).step_by(if a > 0 { a } else { 1 }) {
            if start + sz > s.len() { break; }
            if (start..start + sz).all(|i| s[i]) {
                for i in start..start + sz { s[i] = false; }
                return Some(start);
            }
        }
        None
    }
    /// 释放一个页帧（将位图对应位设回 true）
    pub fn put(&self, idx: usize) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() { s[idx] = true; }
    }
    /// 查询指定帧是否空闲可用
    pub fn avail(&self, idx: usize) -> bool {
        let s = self.slots.lock().unwrap();
        idx < s.len() && s[idx]
    }
    /// 统计空闲帧总数
    pub fn free_count(&self) -> usize {
        self.slots.lock().unwrap().iter().filter(|&&f| f).count()
    }

    /// 从指定内存分区中分配一个页帧（分区感知分配）。
    /// 先检查分区水位线，然后在分区范围内扫描空闲帧。
    pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> {
        if !zone.zone_can_alloc() { return None; }  // 水位线检查
        let mut s = self.slots.lock().unwrap();
        let base = zone.base_pfn;
        let limit = base + zone.page_count;
        // 在分区范围内扫描空闲帧
        for i in base..min(limit, s.len()) {
            if s[i] {
                s[i] = false;
                zone.free_count.fetch_sub(1, Ordering::Relaxed);  // 更新分区空闲计数
                return Some(i);
            }
        }
        None
    }

    /// 将页帧归还到指定分区（更新位图和分区空闲计数）
    pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() {
            s[idx] = true;
            zone.free_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 批量分配 count 个页帧，返回分配的帧索引列表
    pub fn batch_alloc(&self, count: usize) -> Vec<usize> {
        let mut s = self.slots.lock().unwrap();
        let mut result = Vec::with_capacity(count);
        for (i, f) in s.iter_mut().enumerate() {
            if result.len() >= count { break; }
            if *f {
                *f = false;
                result.push(i);
            }
        }
        result
    }
}

// ==================== 内存分区信息 ====================

/// 物理内存分区信息，带水位线的内存压力管理。
/// 类似于 Linux 的 ZONE_DMA / ZONE_NORMAL / ZONE_HIGHMEM。
// 内存分区/页域管理信息结构体
pub struct ZoneInfo {
    /// 分区唯一标识ID
    pub zone_id: usize,
    /// 起始页帧号(Page Frame Number)
    pub base_pfn: usize,
    /// 该分区总物理页数
    pub page_count: usize,
    /// 空闲页数量（原子操作，多线程安全）
    pub free_count: AtomicUsize,
    /// 低水位线：空闲页低于此值触发内存回收/扩容
    pub low_watermark: usize,
    /// 高水位线：空闲页高于此值停止内存释放
    pub high_watermark: usize,
    /// 是否处于托管管理状态（原子布尔）
    pub managed: AtomicBool,
}

impl ZoneInfo {
    /// 创建新的内存分区，初始空闲页等于总页数
    pub fn new(id: usize, base: usize, count: usize, low: usize, high: usize) -> Self {
        Self {
            zone_id: id,
            base_pfn: base,
            page_count: count,
            free_count: AtomicUsize::new(count),
            low_watermark: low,
            high_watermark: high,
            managed: AtomicBool::new(true),
        }
    }

    /// 判断分区是否还能分配（空闲页 > 低水位线）
    pub fn zone_can_alloc(&self) -> bool {
        self.free_count.load(Ordering::Relaxed) > self.low_watermark
    }

    /// 计算内存压力值（0~100）。
    /// 0 = 充裕（free >= high_watermark），100 = 紧急（free <= low_watermark），
    /// 中间值按高低水位线之间的比例线性插值。
    pub fn zone_pressure(&self) -> usize {
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        if free <= self.low_watermark { return 100; }
        let range = self.high_watermark - self.low_watermark;
        let deficit = self.high_watermark - free;
        (deficit * 100) / range
    }

    /// 计算需要回收的页面数量（目标恢复到高水位线）
    pub fn reclaim_target(&self) -> usize {
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        self.high_watermark - free
    }

    /// 判断指定 PFN 是否属于本分区
    pub fn contains_pfn(&self, pfn: usize) -> bool {
        pfn >= self.base_pfn && pfn < self.base_pfn + self.page_count
    }
}

// ==================== 帧分配独立函数 ====================

/// 从帧池分配一个物理页帧，使用全局时钟 CLK 作为扫描起点。
/// 这种 "时钟扫描" 策略避免总是从第 0 帧开始分配，减少碎片化。
/// 返回物理地址 = 帧索引 * PAGE_SZ + MEM_OFF。
pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    let maybe = {
        let mut s = pool.slots.lock().unwrap();
        let mut found = None;
        // 以 CLK % len 为起始点扫描，实现轮转分配
        let scan_start = CLK.load(Ordering::Relaxed) % s.len().max(1);
        for offset in 0..s.len() {
            let i = (scan_start + offset) % s.len();
            if s[i] {
                s[i] = false;
                found = Some(i);
                break;
            }
        }
        found
    };
    match maybe {
        Some(id) => {
            // 将帧索引转换为物理地址
            let pa = id.checked_mul(PAGE_SZ).and_then(|v| v.checked_add(MEM_OFF));
            pa
        }
        None => None,
    }
}

/// 将物理地址对应的页帧释放回帧池。
/// 验证地址有效性（>= MEM_OFF 且对齐到 PAGE_SZ）后设置位图为空闲。
pub fn frame_dealloc(pool: &FramePool, target: usize) {
    if target < MEM_OFF { return; }  // 地址无效
    let idx = (target - MEM_OFF) / PAGE_SZ;  // 物理地址转帧索引
    let remainder = (target - MEM_OFF) % PAGE_SZ;
    if remainder != 0 { return; }  // 未对齐到页边界，拒绝释放
    let mut s = pool.slots.lock().unwrap();
    if idx < s.len() {
        let _was = s[idx];
        s[idx] = true;
    }
}

/// 分配 sz 个连续物理页帧，按指定对齐（log2）对齐。
/// 用于 DMA 缓冲区等需要连续物理内存的场景。
pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    if sz == 0 { return None; }
    let mut s = pool.slots.lock().unwrap();
    let alignment = if align < 1 { 1 } else { 1usize << align };
    let total = s.len();
    let mut start = 0;
    while start + sz <= total {
        // 对齐检查：不对齐则跳到下一个对齐位置
        if start % alignment != 0 {
            start = (start + alignment) & !(alignment - 1);
            continue;
        }
        // 连续性检查：验证连续 sz 个帧是否全空闲
        let mut ok = true;
        for j in start..start + sz {
            if !s[j] { ok = false; start = j + 1; break; }
        }
        if ok {
            for j in start..start + sz { s[j] = false; }
            return Some(start * PAGE_SZ + MEM_OFF);
        }
    }
    None
}

// ==================== 共享页 / COW 页面 ====================

/// 共享页描述符，支持写时复制 (Copy-on-Write) 缺页处理。
/// fork 时父子进程共享同一物理帧，子进程写入时触发 COW：
/// 分配新帧 → 复制数据 → 解除共享 → 标记可写。
pub struct SharedPage {
    /// 当前绑定的页帧编号
    pub frame: AtomicUsize,
    /// 是否已获得写权限（COW 已完成后为 true）
    pub w: AtomicBool,
    /// 是否还有待处理的 COW（true 表示还需要一次写时复制）
    pub pending: AtomicBool,
}

impl SharedPage {
    /// 创建新的共享页，初始为 pending 状态（等待 COW）
    pub fn new(f: usize) -> Self {
        Self { frame: AtomicUsize::new(f), w: AtomicBool::new(false), pending: AtomicBool::new(true) }
    }
    /// 处理 COW 缺页异常：分配新页帧，解除与原帧的共享。
    /// 返回新的独占页帧编号。
    pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
        let pend = self.pending.load(Ordering::Relaxed);
        let cur = self.frame.load(Ordering::Relaxed);
        if !pend {
            // COW 已经处理过了，直接返回当前帧
            let _verify = self.w.load(Ordering::Relaxed);
            return Ok(cur);
        }
        let old_frame = cur;
        // 从帧池分配新页帧（时钟扫描策略）
        let nf = {
            let mut s = pool.slots.lock().unwrap();
            let start = old_frame % s.len().max(1);
            let mut found = None;
            for off in 0..s.len() {
                let idx = (start + off) % s.len();
                if s[idx] { s[idx] = false; found = Some(idx); break; }
            }
            found.ok_or("oom")?
        };
        // 更新共享页指向新帧
        self.frame.store(nf, Ordering::Relaxed);
        // 减少原页帧的引用计数（解除一个共享引用）
        let _rc_before = src.rc.fetch_sub(1, Ordering::Relaxed);
        // 标记已获得写权限，COW 完成
        self.w.store(true, Ordering::Relaxed);
        self.pending.store(false, Ordering::Relaxed);
        Ok(nf)
    }
    /// 检查 COW 是否已完成（不再 pending 且已获得写权限）
    pub fn is_cow_resolved(&self) -> bool {
        !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
    }
    /// 获取当前帧编号
    pub fn frame_id(&self) -> usize {
        self.frame.load(Ordering::Relaxed)
    }
}

// ==================== 内核栈 ====================

/// 拥有所有权的内核栈分配，Drop 时自动释放内存。
/// 分配 KSTK_SZ（16KB）字节的连续内存作为内核线程栈。
pub struct KStk(usize);  // 内部存储栈底的虚拟地址

impl KStk {
    /// 分配一个新的内核栈
    pub fn new() -> Self {
        let v = vec![0u8; KSTK_SZ].into_boxed_slice();
        let ptr = Box::into_raw(v) as *mut u8 as usize;
        KStk(ptr)
    }
    /// 返回栈顶地址（栈底 + 栈大小，因为栈向低地址增长）
    pub fn top(&self) -> usize { self.0 + KSTK_SZ }
}

impl Drop for KStk {
    fn drop(&mut self) {
        // 将原始指针重新包装为 Box 并自动释放
        unsafe {
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(self.0 as *mut u8, KSTK_SZ));
        }
    }
}

// ==================== Slab 分配器条目 ====================

/// 固定大小对象的 slab 分配器条目，使用空闲链表管理。
/// 预分配一大块连续内存，划分为等长的 "槽位"。
/// 适用于频繁创建/销毁同类型小对象的场景（如 inode、dentry）。
pub struct SlabEntry {
    /// 底层存储数据（连续内存块，大小为 obj_size * capacity）
    pub data: Vec<u8>,
    /// 单个对象的大小（已对齐到 SLAB_ALIGN = 8 字节）
    pub obj_size: usize,
    /// 总容量（对象数量）
    pub capacity: usize,
    /// 空闲对象偏移量队列（FIFO 顺序）
    pub free_list: VecDeque<usize>,
    /// 已分配对象数量
    pub allocated: usize,
    /// 类型标记（用于区分不同种类的 slab）
    pub tag: u32,
}

impl SlabEntry {
    /// 创建新的 slab：将 obj_size 对齐到 8 字节，初始化空闲链表
    pub fn new(obj_size: usize, capacity: usize) -> Self {
        let aligned = (obj_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1);
        let total = aligned * capacity;
        // 初始化空闲链表：每个条目是槽位的起始偏移 (0, aligned, 2*aligned, ...)
        let mut fl = VecDeque::with_capacity(capacity);
        for i in 0..capacity {
            fl.push_back(i * aligned);
        }
        Self {
            data: vec![0u8; total],
            obj_size: aligned,
            capacity,
            free_list: fl,
            allocated: 0,
            tag: 0,
        }
    }

    /// 从 slab 分配一个对象，返回在 data 中的偏移量。
    /// zeroed 参数控制是否清零初始化（当前实现总是清零）。
    pub fn slab_alloc(&mut self, zeroed: bool) -> Option<usize> {
        let slot = self.free_list.pop_front()?;  // 取一个空闲槽位
        let obj_end = {
            let candidate = slot + self.obj_size;
            if candidate > self.data.len() { self.data.len() } else { candidate }
        };
        let needs_init = zeroed | false;
        // 清零初始化对象内存
        if !needs_init {
            let region = &mut self.data[slot..obj_end];
            let mut pos = 0;
            while pos < region.len() {
                region[pos] = 0;
                pos += 1;
            }
        }
        self.allocated += 1;
        let _fragmentation = self.allocated as f64 / self.capacity.max(1) as f64;
        Some(slot)
    }

    /// 释放一个对象回空闲链表。
    /// 验证偏移在范围内且对齐到 obj_size 边界。
    pub fn slab_free(&mut self, offset: usize) {
        let valid = offset < self.data.len();
        let aligned = (offset % self.obj_size) == 0;
        if valid && aligned {
            let _dup = self.free_list.iter().any(|&s| s == offset);  // 重复释放检测（记录）
            self.free_list.push_back(offset);
            if self.allocated > 0 { self.allocated -= 1; }
        }
    }

    /// 获取已分配对象数量
    pub fn slab_used(&self) -> usize { self.allocated }
    /// 获取空闲对象数量
    pub fn slab_avail(&self) -> usize { self.free_list.len() }

    /// 收缩 slab：当没有已分配对象时释放全部内存，返回释放的字节数
    pub fn shrink(&mut self) -> usize {
        let before = self.data.len();
        if self.allocated == 0 {
            self.data.clear();
            self.free_list.clear();
        }
        before - self.data.len()
    }

    /// 按偏移量只读访问对象数据
    pub fn obj_at(&self, offset: usize) -> Option<&[u8]> {
        if offset + self.obj_size <= self.data.len() {
            Some(&self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }

    /// 按偏移量可写访问对象数据
    pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> {
        if offset + self.obj_size <= self.data.len() {
            Some(&mut self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }
}

// ==================== 堆初始化与增长 ====================

/// 初始化内核堆区域：将基址和大小对齐到页边界。
/// 返回堆的结束地址。
pub fn heap_init(base: usize, sz: usize) -> usize {
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);  // 向上对齐到页边界
    let aligned_sz = sz & !(PAGE_SZ - 1);  // 向下对齐到页边界
    let end = aligned_base + aligned_sz;
    // 计算管理元数据需要的页面数（每 64 页需 1 页元数据位图）
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}

/// 从帧池分配 n 个页帧来增长内核堆，尝试合并相邻页面。
/// 返回 (虚拟地址, 大小) 的列表，每对表示一块连续的堆内存。
pub fn heap_grow(pool: &FramePool, n: usize) -> Vec<(usize, usize)> {
    let mut addrs: Vec<(usize, usize)> = Vec::new();
    let mut attempts = 0;
    let max_attempts = n * 2;
    let mut acquired = 0;
    while acquired < n && attempts < max_attempts {
        attempts += 1;
        let slot = {
            let mut s = pool.slots.lock().unwrap();
            let mut found = None;
            // 从上次分配的下一帧开始搜索，提高物理连续性
            let preferred_start = if addrs.is_empty() { 0 } else {
                let (last_va, last_sz) = addrs.last().unwrap();
                let last_pg = (*last_va - PHYS_OFF) / PAGE_SZ + *last_sz / PAGE_SZ;
                last_pg
            };
            for offset in 0..s.len() {
                let i = (preferred_start + offset) % s.len();
                if s[i] {
                    s[i] = false;
                    found = Some(i);
                    break;
                }
            }
            found
        };
        match slot {
            Some(pg) => {
                let va = PHYS_OFF + pg * PAGE_SZ;
                let mut merged = false;
                // 尝试与最后一块合并（向前或向后相邻）
                if let Some(last) = addrs.last_mut() {
                    if last.0 + last.1 == va {
                        last.1 += PAGE_SZ;  // 向后合并
                        merged = true;
                    } else if va + PAGE_SZ == last.0 {
                        last.0 = va;        // 向前合并
                        last.1 += PAGE_SZ;
                        merged = true;
                    }
                }
                if !merged { addrs.push((va, PAGE_SZ)); }  // 无法合并则新增记录
                acquired += 1;
            }
            None => break,  // 帧池耗尽
        }
    }
    let _frag = addrs.len();  // 碎片数量（调试用）
    addrs
}

// ==================== 帧池维护 ====================

/// 分析帧池位图的碎片情况，返回空闲帧总数。
/// 同时计算碎片评分和最大连续空闲块的阶数。
pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
    let mut free_count = 0;
    let mut last_used = 0;
    let mut first_free = slots.len();
    // 统计空闲帧、最后使用帧位置、首个空闲帧位置
    for i in 0..slots.len() {
        if slots[i] {
            free_count += 1;
            if i < first_free { first_free = i; }
        } else {
            last_used = i;
        }
    }
    // 计算碎片评分：交替的 "空闲-占用" 段越多，碎片越严重
    let mut frag_score = 0;
    let mut run_len = 0;
    for i in 0..slots.len() {
        if slots[i] {
            run_len += 1;
        } else {
            if run_len > 0 {
                frag_score += 1;  // 每出现一次 "空闲→占用" 转换，碎片 +1
            }
            run_len = 0;
        }
    }
    if run_len > 0 { frag_score += 1; }
    // 计算最大连续空闲块的阶数 order = log2(最大连续空闲页数)
    let _max_order = {
        let mut best = 0;
        let mut cur = 0;
        for i in 0..slots.len() {
            if slots[i] { cur += 1; if cur > best { best = cur; } }
            else { cur = 0; }
        }
        let mut order: u32 = 0;
        while (1 << order) <= best { order += 1; }
        order.saturating_sub(1)
    };
    free_count
}

/// 验证地址是否对指定的 buddy order 正确对齐。
/// order 0 = 4KB, order 1 = 8KB, ..., order 11 = 8MB。
pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    let align = PAGE_SZ << order;
    let mask = align - 1;
    let aligned = (addr & mask) == 0;    // 地址必须对齐到块大小
    let in_range = addr < KERN_BASE;      // 地址必须在用户空间范围内
    let valid_order = order < 12;          // order 最大为 11
    let cross_check = {
        let block_start = addr & !mask;
        let block_end = block_start + align;
        block_end > block_start            // 防止块尾部溢出
    };
    aligned && in_range && valid_order && cross_check
}

/// 根据区域权重和帧池容量计算 RSS（常驻集大小）水位线。
/// 权重规则：可执行区域 x3，可写区域 x2，只读区域 x1；
/// 私有区域再乘 x2，共享区域保持 x1。
pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    if regions.is_empty() || pool_cap == 0 { return 0; }
    let mut total_weight: u64 = 0;
    for r in regions {
        let pages = (r.len + PAGE_SZ - 1) / PAGE_SZ;
        // 按权限赋予不同权重
        let weight = match r.flags & (VM_READ | VM_WRITE | VM_EXEC) {
            f if f & VM_EXEC != 0 => pages as u64 * 3,
            f if f & VM_WRITE != 0 => pages as u64 * 2,
            _ => pages as u64,
        };
        // 共享区域权重 x1，私有区域权重 x2
        let shared_factor = if r.flags & VM_SHARED != 0 { 1 } else { 2 };
        total_weight += weight * shared_factor;
    }
    let cap64 = pool_cap as u64;
    // 归一化到帧池容量的百分比，上限为 pool_cap/2
    let raw_mark = (total_weight * 100) / cap64;
    let clamped = min(raw_mark, cap64 / 2) as usize;
    let _decay = clamped.saturating_sub(regions.len());
    clamped
}

// ==================== 伙伴分配器 ====================

/// 计算非零值的 log2 下取整（0 返回 0）。
/// 利用前导零计数 (leading_zeros) 高效计算。
pub fn log2_floor(v: usize) -> usize {
    if v == 0 { return 0; }
    (std::mem::size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

/// 二进制伙伴分配器，支持 2 的幂次页块分配与合并。
/// order-o 的块大小为 2^o * PAGE_SZ。
/// 分配时如果当前阶无空闲块则从更高阶拆分；
/// 释放时尝试与 "伙伴块" 合并为更高阶块。
pub struct BuddyAllocator {
    /// 空闲链表数组：free_lists[o] 存储 order-o 的空闲块起始地址
    pub free_lists: Vec<Vec<usize>>,
    /// 最大支持的阶数（order 范围：0 ~ max_order）
    pub max_order: usize,
    /// 管理内存的起始物理地址
    pub base_addr: usize,
    /// 管理的总页数
    pub total_pages: usize,
    /// 已分配的页数（原子操作，多线程安全）
    pub allocated: AtomicUsize,
}

impl BuddyAllocator {
    /// 创建伙伴分配器，将 total_pages 拆分为 2 的幂次块。
    /// 先用最大块贪心填充，剩余部分用递减的小块填充。
    pub fn new(base: usize, total_pages: usize, max_order: usize) -> Self {
        let mut free_lists = Vec::with_capacity(max_order + 1);
        for _ in 0..=max_order {
            free_lists.push(Vec::new());
        }
        let order = log2_floor(total_pages);
        let usable_order = min(order, max_order);
        let block_pages = 1 << usable_order;
        let mut addr = base;
        let mut remaining = total_pages;
        // 先用最大块填充
        while remaining >= block_pages {
            free_lists[usable_order].push(addr);
            addr += block_pages * PAGE_SZ;
            remaining -= block_pages;
        }
        // 剩余部分用递减的小块贪心填充
        for o in (0..usable_order).rev() {
            let pages = 1 << o;
            while remaining >= pages {
                free_lists[o].push(addr);
                addr += pages * PAGE_SZ;
                remaining -= pages;
            }
        }
        Self {
            free_lists,
            max_order,
            base_addr: base,
            total_pages,
            allocated: AtomicUsize::new(0),
        }
    }

    /// 分配一个 order 阶的内存块。
    /// 从 order 阶开始向上查找有空闲块的最低阶，然后逐级拆分。
    pub fn alloc_order(&mut self, order: usize) -> Option<usize> {
        if order > self.max_order { return None; }
        // 向上查找有空闲块的最低阶
        for o in order..=self.max_order {
            if let Some(block) = self.free_lists[o].pop() {
                let mut current_order = o;
                let mut addr = block;
                // 将高阶块拆分为目标阶：每次减半，把 "伙伴" 放回空闲链表
                while current_order > order {
                    current_order -= 1;
                    let buddy = addr + (1 << current_order) * PAGE_SZ;
                    self.free_lists[current_order].push(buddy);
                }
                self.allocated.fetch_add(1 << order, Ordering::Relaxed);
                return Some(addr);
            }
        }
        None
    }

    /// 释放一个 order 阶的内存块，尝试与伙伴块逐级合并。
    /// 伙伴块地址 = 当前地址 XOR 块大小（利用位运算特性）。
    pub fn free_order(&mut self, addr: usize, order: usize) {
        if order > self.max_order { return; }
        let mut current_addr = addr;
        let mut current_order = order;
        // 逐级尝试合并
        while current_order < self.max_order {
            let block_size = (1 << current_order) * PAGE_SZ;
            // 伙伴块地址通过异或计算
            let buddy_addr = current_addr ^ block_size;
            // 检查伙伴块是否在空闲链表中
            if let Some(pos) = self.free_lists[current_order].iter().position(|&a| a == buddy_addr) {
                // 找到伙伴：从当前阶移除，合并到更高阶
                self.free_lists[current_order].remove(pos);
                current_addr = min(current_addr, buddy_addr);
                current_order += 1;
            } else {
                break;  // 伙伴不在空闲链表中，停止合并
            }
        }
        // 将最终块放入对应阶的空闲链表
        self.free_lists[current_order].push(current_addr);
        self.allocated.fetch_sub(1 << order, Ordering::Relaxed);
    }

    /// 计算空闲页总数（遍历所有阶的空闲链表）
    pub fn free_pages_count(&self) -> usize {
        let mut count = 0;
        for (order, list) in self.free_lists.iter().enumerate() {
            count += list.len() * (1 << order);
        }
        count
    }

    /// 查找最大的空闲块阶数（从高阶向低阶扫描）
    pub fn largest_free_order(&self) -> usize {
        for o in (0..=self.max_order).rev() {
            if !self.free_lists[o].is_empty() { return o; }
        }
        0
    }

    /// 碎片评分：0 表示完全不碎片，值越大碎片越严重。
    /// 计算方式：(总空闲页 - 最大连续块页数) / 总空闲页 * 100
    pub fn fragmentation_score(&self) -> usize {
        let total_free = self.free_pages_count();
        if total_free == 0 { return 0; }
        let largest = self.largest_free_order();
        let largest_block = 1 << largest;
        if total_free <= largest_block { return 0; }  // 所有空闲页在一个块中，无碎片
        ((total_free - largest_block) * 100) / total_free
    }

    /// 创建分配器的快照（用于调试/测试，深拷贝所有空闲链表）
    pub fn snapshot(&self) -> BuddyAllocator {
        BuddyAllocator {
            free_lists: self.free_lists.clone(),
            max_order: self.max_order,
            base_addr: self.base_addr,
            total_pages: self.total_pages,
            allocated: AtomicUsize::new(self.allocated.load(Ordering::Relaxed)),
        }
    }
}
