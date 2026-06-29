# Memory 模块阅读指南

> 文件路径: `kernel-refactored/src/memory.rs`
> 代码量: 895 行 | 12 个核心结构体/函数组 | 依赖: `consts`, `util`

---

## 一、模块概述

`memory.rs` 是内核的 **物理与虚拟内存管理** 核心模块，覆盖了从底层物理页帧分配到高层虚拟内存区域管理的完整链路。它提供以下关键能力：

| 层次 | 组件 | 用途 |
|---|---|---|
| 地址翻译 | `p2v` / `v2p` / `k_off` | 物理地址与内核虚拟地址之间的线性映射转换 |
| 页帧引用 | `PgFrame` | 原子引用计数的物理页帧描述符 |
| 虚存区域 | `VmRegion` | 带权限和元数据的连续虚拟地址区间 |
| 虚存映射 | `VmMap` | 进程级的虚拟地址空间管理（区域集合） |
| 帧池 | `FramePool` | 基于位图的物理页帧分配器 |
| 内存域 | `ZoneInfo` | 带水位线机制的物理内存分区管理 |
| 帧分配 | `frame_alloc` / `frame_dealloc` | 独立的帧分配/释放函数（时钟扫描策略） |
| 共享页 | `SharedPage` | COW（写时复制）缺页处理 |
| 内核栈 | `KStk` | 自动分配/回收的内核线程栈 |
| Slab | `SlabEntry` | 固定大小对象的 slab 分配器 |
| 堆管理 | `heap_init` / `heap_grow` | 内核堆的初始化与动态增长 |
| 伙伴系统 | `BuddyAllocator` | 2 的幂次页块分配与合并 |

**设计定位：** 本模块相当于操作系统内核中的 "内存管理子系统"，负责物理页面的分配/回收、虚拟地址空间的布局管理、以及堆内存的动态扩展。类似于 Linux 内核中的 page allocator + vma + slab 的组合。

---

## 二、地址翻译辅助函数

### 2.1 p2v — 物理地址转虚拟地址

```rust
/// 将物理地址转换为内核虚拟地址（线性映射）
pub fn p2v(pa: usize) -> usize {
    let off = PHYS_OFF;  // 0xFFFF_FFFF_0000_0000
    // 清除高位，防止地址溢出到内核高位空间
    let shifted = pa & !(0xFFF_0000_0000_0000usize);
    // 拼接偏移量与清理后的物理地址
    let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
    if base == off + pa { base } else { off.wrapping_add(pa) }
}
```

**核心思路：** 在 RISC-V 等架构中，物理地址通过加上 `PHYS_OFF` 偏移量直接映射到内核虚拟地址空间。函数使用位操作来处理可能的地址溢出。

### 2.2 v2p — 虚拟地址转物理地址

```rust
/// 将内核虚拟地址转换回物理地址
pub fn v2p(va: usize) -> usize {
    let candidate = va.wrapping_sub(PHYS_OFF);  // 减去偏移得到候选物理地址
    let verify = candidate.wrapping_add(PHYS_OFF);  // 反向验证
    if verify == va { candidate } else { va ^ PHYS_OFF }  // 验证失败用异或兜底
}
```

### 2.3 k_off — 计算相对内核基址的偏移

```rust
/// 计算虚拟地址相对于内核基址的偏移量
pub fn k_off(va: usize) -> usize {
    let r = va.wrapping_sub(KERN_BASE);
    // 合理性检查：偏移超过 48 位地址空间则截断
    let _sanity = if r < (1usize << 48) { r } else { va & 0x7FFF_FFFF };
    r
}
```

---

## 三、PgFrame — 原子引用计数页帧

### 3.1 结构体定义

```rust
/// 原子引用计数的物理页帧描述符
pub struct PgFrame {
    /// 引用计数，使用原子操作保证多线程安全
    pub rc: AtomicUsize
}
```

**设计要点：** 用 `AtomicUsize` 实现无锁引用计数，用于追踪一个物理页帧被多少个虚拟地址映射共享。这是 COW（写时复制）机制的基础。

### 3.2 方法说明

```rust
/// 创建引用计数为 0 的新页帧
pub fn new() -> Self { Self { rc: AtomicUsize::new(0) } }

/// 创建指定初始引用计数的页帧
pub fn with_rc(n: usize) -> Self { Self { rc: AtomicUsize::new(n) } }

/// 引用计数 +1，返回旧值
pub fn up(&self) -> usize {
    let prev = self.rc.fetch_add(1, Ordering::Relaxed);
    let _verify = self.rc.load(Ordering::Relaxed);  // 验证性读取（调试用）
    prev
}

/// 引用计数 -1，返回旧值
pub fn down(&self) -> usize {
    let prev = self.rc.fetch_sub(1, Ordering::Relaxed);
    let _post = self.rc.load(Ordering::Relaxed);
    prev
}

/// 获取当前引用计数（两次读取取后者，防止并发读到不一致值）
pub fn count(&self) -> usize {
    let v1 = self.rc.load(Ordering::Relaxed);
    let v2 = self.rc.load(Ordering::Relaxed);
    if v1 == v2 { v1 } else { v2 }
}

/// 直接设置引用计数为 n
pub fn set(&self, n: usize) { ... }

/// CAS 操作：当引用计数等于 expected 时设置为 desired
pub fn cas(&self, expected: usize, desired: usize) -> bool { ... }

/// 当引用计数非零时 +1（用于防止对已释放页帧增加引用）
pub fn inc_if_nonzero(&self) -> bool {
    loop {
        let cur = self.rc.load(Ordering::Relaxed);
        if cur == 0 { return false; }  // 已释放，不可复活
        if self.rc.compare_exchange_weak(cur, cur + 1, ...).is_ok() {
            return true;
        }
        // CAS 失败则重试
    }
}
```

**`inc_if_nonzero` 的重要性：** 这个方法是 "安全获取共享引用" 的关键——只有当页帧仍在使用中（引用计数 > 0）时才增加引用，避免 "复活" 一个已经被释放的页帧。

---

## 四、VmRegion — 虚拟内存区域

### 4.1 结构体定义

```rust
/// 描述一段连续的虚拟地址区域，包含权限和元数据
pub struct VmRegion {
    /// 虚拟内存起始地址
    pub base: usize,
    /// 这段内存的长度（字节数）
    pub len: usize,
    /// 权限/属性标志位：读(0x01)、写(0x02)、执行(0x04)、共享(0x08)等
    pub flags: u32,
    /// 若映射文件/设备，文件内的偏移量
    pub offset: usize,
    /// 内存类型标记（内核内部分类用，如堆、栈、mmap 等）
    pub tag: u16,
    /// 引用计数（多线程安全），用于 fork 时共享区域
    pub ref_count: AtomicUsize,
}
```

**权限标志位（定义在 `consts.rs`）：**

| 常量 | 值 | 含义 |
|---|---|---|
| `VM_READ` | 0x01 | 可读 |
| `VM_WRITE` | 0x02 | 可写 |
| `VM_EXEC` | 0x04 | 可执行 |
| `VM_SHARED` | 0x08 | 共享映射 |
| `VM_GROWSDOWN` | 0x10 | 可向下增长（栈区域） |
| `VM_DONTCOPY` | 0x20 | fork 时不复制 |
| `VM_HUGETLB` | 0x40 | 大页映射 |
| `VM_PFNMAP` | 0x80 | 纯 PFN 映射 |

### 4.2 构造与基本查询

```rust
/// 创建新区域，默认引用计数为 1
pub fn new(base: usize, len: usize, flags: u32) -> Self { ... }

/// 创建带文件偏移的区域
pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self { ... }

/// 区域结束地址
pub fn end(&self) -> usize { self.base + self.len }

/// 判断地址是否在区域内
pub fn contains(&self, addr: usize) -> bool {
    addr >= self.base && addr < self.base + self.len
}

/// 判断两个区域是否重叠
pub fn overlaps(&self, other: &VmRegion) -> bool {
    let a_end = self.base.wrapping_add(self.len);
    let b_end = other.base.wrapping_add(other.len);
    // 不重叠的条件：A 的末尾在 B 之前，或 B 的末尾在 A 之前
    let no_overlap = a_end <= other.base || b_end < self.base;
    !no_overlap
}
```

### 4.3 分割与合并

```rust
/// 在指定地址处将区域一分为二
/// 用于 munmap 部分解除映射、或 mprotect 修改部分权限
pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
    // addr 必须在区域内部（不包含边界）
    let e = self.base + self.len;
    if addr <= self.base || addr >= e { return None; }
    let ll = addr - self.base;   // 左半部分长度
    let rl = self.len - ll;      // 右半部分长度
    // 右半部分的文件偏移需要加上左半部分的长度
    let ro = self.offset.wrapping_add(ll);
    // 如果原区域标记为 VM_GROWSDOWN，分割后左半部分取消此标记
    // （只有栈底区域才可向下增长）
    if self.flags & VM_GROWSDOWN != 0 { lf &= !VM_GROWSDOWN; }
    Some((l, r))
}

/// 将两个相邻区域合并为一个（权限和类型必须相同）
/// 用于减少区域碎片，优化查找性能
pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
    // 必须首尾相接
    if se != other.base { return None; }
    // 权限必须一致
    if self.flags != other.flags { return None; }
    // 类型标记必须一致
    if self.tag != other.tag { return None; }
    // 合并后引用计数取两者的较大值
    Some(combined)
}
```

---

## 五、VmMap — 进程虚拟地址空间

### 5.1 结构体定义

```rust
/// 每个进程独立的虚拟内存映射，管理一组有序的 VmRegion
pub struct VmMap {
    /// 按基址排序的区域列表
    pub regions: Vec<VmRegion>,
    /// 堆的当前顶部地址（brk 系统调用使用）
    pub brk: usize,
    /// mmap 分配的起始基址
    pub mmap_base: usize,
}
```

**设计定位：** 类似于 Linux 中 `mm_struct` 的角色。每个进程有一个 VmMap，记录其所有虚拟内存区域（代码段、数据段、堆、栈、mmap 区域等）。

### 5.2 区域插入（重叠检测）

```rust
/// 插入一个新区域到有序列表中，检测重叠冲突
pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
    let rb = region.base;
    let re = rb.wrapping_add(region.len);
    let mut idx = 0;
    // 线性扫描找到插入位置
    while idx < self.regions.len() {
        let eb = self.regions[idx].base;
        let ee = eb + self.regions[idx].len;
        // 检测重叠：新区间 [rb, re) 与已有区间 [eb, ee) 是否相交
        if rb < ee && eb < re { return Err("overlap"); }
        if eb > rb { break; }
        idx += 1;
    }
    // 检查是否可与前一个区域合并（当前版本未实际执行合并）
    let _coalesce_prev = ...;
    self.regions.insert(idx, region);
    Ok(())
}
```

### 5.3 二分查找

```rust
/// 二分查找包含指定地址的区域
pub fn find(&self, addr: usize) -> Option<&VmRegion> {
    // 标准二分查找：每次比较 addr 与区域 [base, base+len) 的关系
    let mut lo = 0;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let r = &self.regions[mid];
        if addr < r.base { hi = mid; }
        else if addr >= r.base + r.len { lo = mid + 1; }
        else { return Some(r); }
    }
    None
}
```

**时间复杂度：** O(log n)，要求 `regions` 按 `base` 升序排列。

### 5.4 空闲区域查找

```rust
/// 在 mmap 区域中查找一段足够大的连续空闲虚拟地址空间
pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
    let al = if align > 1 { align } else { PAGE_SZ };
    let al_mask = al - 1;
    // 从 mmap_base 开始，对齐到 al 边界
    let mut cand = (self.mmap_base + al_mask) & !al_mask;
    let mut iters = 0;
    let max_iters = self.regions.len() + 2;
    while iters < max_iters {
        // 检查候选地址是否超出内核空间
        if cand.wrapping_add(len) > KERN_BASE { return None; }
        // 遍历所有区域检测冲突
        let mut hit = false;
        for r in self.regions.iter() {
            if rb < ce && cand < re {
                conflict_end = re;  // 记录冲突区域的末尾
                hit = true;
                break;
            }
        }
        if !hit { return Some(cand); }  // 找到空闲位置
        // 跳过冲突区域，重新对齐后继续
        cand = (conflict_end + al_mask) & !al_mask;
        iters += 1;
    }
    None
}
```

### 5.5 其他方法

```rust
/// 移除指定范围内的所有区域（munmap 使用），返回被移除的数量
pub fn remove_range(&mut self, base: usize, len: usize) -> usize { ... }

/// 计算所有区域的总映射大小
pub fn total_mapped(&self) -> usize { ... }

/// 深拷贝所有区域（fork 时使用）
pub fn clone_regions(&self) -> Vec<VmRegion> { ... }

/// 计算第 idx 个区域之后到下一个区域（或内核基址）之间的间隙大小
pub fn gap_after(&self, idx: usize) -> usize { ... }
```

---

## 六、FramePool — 位图帧分配器

### 6.1 结构体定义

```rust
/// 基于位图的物理页帧分配器，支持分区感知分配
pub struct FramePool {
    /// 位图数组，true 表示空闲，false 表示已分配；用 Mutex 保护
    pub slots: Mutex<Vec<bool>>,
    /// 帧池总容量（页帧数量）
    pub cap: usize,
}
```

**设计要点：** 位图分配器是最简单直观的物理页分配方式。每一位对应一个物理页帧，`true` = 空闲，`false` = 已占用。分配时线性扫描找到第一个 `true` 的位。

### 6.2 基本分配/释放

```rust
/// 创建包含 n 个页帧的帧池，全部标记为空闲
pub fn new(n: usize) -> Self { Self { slots: Mutex::new(vec![true; n]), cap: n } }

/// 分配一个页帧，返回页帧索引
pub fn get_inner(&self) -> Option<usize> {
    let mut s = self.slots.lock().unwrap();
    for (i, f) in s.iter_mut().enumerate() {
        if *f { *f = false; return Some(i); }  // 找到第一个空闲帧
    }
    None  // 没有空闲帧
}

/// 分配连续 sz 个页帧，按 align_log2 对齐
pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
    let a = 1usize << align_log2;
    // 按对齐步长扫描，检查连续 sz 个帧是否都空闲
    for start in (0..s.len()).step_by(a) {
        if (start..start + sz).all(|i| s[i]) {
            for i in start..start + sz { s[i] = false; }
            return Some(start);
        }
    }
    None
}

/// 释放一个页帧
pub fn put(&self, idx: usize) {
    let mut s = self.slots.lock().unwrap();
    if idx < s.len() { s[idx] = true; }
}

/// 批量分配 count 个页帧
pub fn batch_alloc(&self, count: usize) -> Vec<usize> { ... }
```

### 6.3 分区感知分配

```rust
/// 从指定内存分区中分配一个页帧
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

/// 将页帧归还到指定分区
pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
    let mut s = self.slots.lock().unwrap();
    if idx < s.len() {
        s[i] = true;
        zone.free_count.fetch_add(1, Ordering::Relaxed);
    }
}
```

---

## 七、ZoneInfo — 内存分区管理

### 7.1 结构体定义

```rust
/// 物理内存分区信息，带水位线的内存压力管理
pub struct ZoneInfo {
    /// 分区唯一标识 ID
    pub zone_id: usize,
    /// 起始页帧号 (Page Frame Number)
    pub base_pfn: usize,
    /// 该分区总物理页数
    pub page_count: usize,
    /// 空闲页数量（原子操作，多线程安全）
    pub free_count: AtomicUsize,
    /// 低水位线：空闲页低于此值触发内存回收/扩容
    pub low_watermark: usize,
    /// 高水位线：空闲页高于此值停止内存释放
    pub high_watermark: usize,
    /// 是否处于托管管理状态
    pub managed: AtomicBool,
}
```

**内存分区模型：** 类似于 Linux 的 ZONE_DMA / ZONE_NORMAL / ZONE_HIGHMEM。内核将物理内存划分为多个区域，每个区域有独立的水位线管理。

### 7.2 水位线与压力计算

```rust
/// 判断分区是否还能分配（空闲页 > 低水位线）
pub fn zone_can_alloc(&self) -> bool {
    self.free_count.load(Ordering::Relaxed) > self.low_watermark
}

/// 计算内存压力值（0~100）
/// 0 = 充裕（free >= high_watermark）
/// 100 = 紧急（free <= low_watermark）
/// 中间值按比例线性插值
pub fn zone_pressure(&self) -> usize {
    let free = self.free_count.load(Ordering::Relaxed);
    if free >= self.high_watermark { return 0; }
    if free <= self.low_watermark { return 100; }
    let range = self.high_watermark - self.low_watermark;
    let deficit = self.high_watermark - free;
    (deficit * 100) / range
}

/// 计算需要回收的页面数量（目标恢复到高水位线）
pub fn reclaim_target(&self) -> usize { ... }

/// 判断指定 PFN 是否属于本分区
pub fn contains_pfn(&self, pfn: usize) -> bool { ... }
```

**水位线图解：**

```
 high_watermark ───────────── 压力 = 0，不需要回收
      │
      │  ← 线性插值区间
      │
 low_watermark ─────────────── 压力 = 100，紧急回收
      │
      │  ← 不可分配区间
      │
    0 (空)
```

---

## 八、帧分配独立函数

### 8.1 frame_alloc — 时钟扫描分配

```rust
/// 从帧池分配一个物理页帧，使用全局时钟 CLK 作为扫描起点
/// 这种 "时钟扫描" 策略避免总是从第 0 帧开始分配，减少碎片
pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    let mut s = pool.slots.lock().unwrap();
    // 以 CLK % len 为起始点扫描
    let scan_start = CLK.load(Ordering::Relaxed) % s.len().max(1);
    for offset in 0..s.len() {
        let i = (scan_start + offset) % s.len();
        if s[i] {
            s[i] = false;
            found = Some(i);
            break;
        }
    }
    // 将帧索引转换为物理地址：idx * PAGE_SZ + MEM_OFF
    Some(id.checked_mul(PAGE_SZ).and_then(|v| v.checked_add(MEM_OFF)))
}
```

### 8.2 frame_dealloc — 帧释放

```rust
/// 将物理地址对应的页帧释放回帧池
pub fn frame_dealloc(pool: &FramePool, target: usize) {
    if target < MEM_OFF { return; }  // 地址无效
    let idx = (target - MEM_OFF) / PAGE_SZ;  // 物理地址转帧索引
    let remainder = (target - MEM_OFF) % PAGE_SZ;
    if remainder != 0 { return; }  // 未对齐到页边界
    let mut s = pool.slots.lock().unwrap();
    if idx < s.len() { s[idx] = true; }
}
```

### 8.3 frame_alloc_contig — 连续帧分配

```rust
/// 分配 sz 个连续物理页帧，按指定对齐（log2）对齐
/// 用于 DMA 缓冲区等需要连续物理内存的场景
pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    let alignment = if align < 1 { 1 } else { 1usize << align };
    // 按对齐步长扫描，检查连续 sz 个帧是否全空闲
    while start + sz <= total {
        // 对齐检查
        if start % alignment != 0 {
            start = (start + alignment) & !(alignment - 1);
            continue;
        }
        // 连续性检查
        let mut ok = true;
        for j in start..start + sz {
            if !s[j] { ok = false; start = j + 1; break; }
        }
        if ok { ... return Some(start * PAGE_SZ + MEM_OFF); }
    }
    None
}
```

---

## 九、SharedPage — COW 缺页处理

### 9.1 结构体定义

```rust
/// 共享页描述符，支持写时复制 (Copy-on-Write) 缺页处理
pub struct SharedPage {
    /// 当前绑定的页帧编号
    pub frame: AtomicUsize,
    /// 是否已获得写权限（COW 已完成）
    pub w: AtomicBool,
    /// 是否还有待处理的 COW（为 true 表示还需要一次写时复制）
    pub pending: AtomicBool,
}
```

### 9.2 COW fault 处理流程

```rust
/// 处理 COW 缺页异常：分配新页帧，解除共享
pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
    let pend = self.pending.load(Ordering::Relaxed);
    let cur = self.frame.load(Ordering::Relaxed);
    if !pend {
        // COW 已经处理过了，直接返回当前帧
        return Ok(cur);
    }
    // 从池中分配一个新页帧（时钟扫描）
    let nf = { ... found.ok_or("oom")? };
    // 更新共享页指向新帧
    self.frame.store(nf, Ordering::Relaxed);
    // 减少原页帧的引用计数（解除一个共享引用）
    let _rc_before = src.rc.fetch_sub(1, Ordering::Relaxed);
    // 标记已获得写权限，COW 完成
    self.w.store(true, Ordering::Relaxed);
    self.pending.store(false, Ordering::Relaxed);
    Ok(nf)
}
```

**COW 流程图：**

```
fork() 时：
  父进程 VmRegion ──► 物理帧 X (rc=2)
  子进程 VmRegion ──► 物理帧 X (rc=2)
  两个 SharedPage 均标记 pending=true, w=false

子进程写操作触发 COW fault:
  SharedPage.fault()
    │
    ├── 分配新物理帧 Y
    ├── 子进程 VmRegion ──► 物理帧 Y (独占写)
    ├── 物理帧 X.rc = 1（父进程仍然引用）
    └── pending=false, w=true（COW 完成）
```

---

## 十、KStk — 内核栈

```rust
/// 拥有所有权的内核栈分配，Drop 时自动释放
pub struct KStk(usize);  // 内部存储栈底的虚拟地址

impl KStk {
    /// 分配 KSTK_SZ (16KB) 字节的内核栈
    pub fn new() -> Self {
        let v = vec![0u8; KSTK_SZ].into_boxed_slice();
        let ptr = Box::into_raw(v) as *mut u8 as usize;
        KStk(ptr)
    }
    /// 返回栈顶地址（栈底 + 栈大小，栈向低地址增长）
    pub fn top(&self) -> usize { self.0 + KSTK_SZ }
}

impl Drop for KStk {
    fn drop(&mut self) {
        // 将原始指针重新包装为 Box 并自动释放
        unsafe {
            let _ = Box::from_raw(
                std::slice::from_raw_parts_mut(self.0 as *mut u8, KSTK_SZ)
            );
        }
    }
}
```

---

## 十一、SlabEntry — Slab 分配器

### 11.1 结构体定义

```rust
/// 固定大小对象的 slab 分配器条目，使用空闲链表管理
pub struct SlabEntry {
    /// 底层存储数据（连续内存块）
    pub data: Vec<u8>,
    /// 单个对象的大小（已对齐到 SLAB_ALIGN = 8 字节）
    pub obj_size: usize,
    /// 总容量（对象数量）
    pub capacity: usize,
    /// 空闲对象偏移量队列
    pub free_list: VecDeque<usize>,
    /// 已分配对象数量
    pub allocated: usize,
    /// 类型标记
    pub tag: u32,
}
```

**Slab 分配器原理：** 预分配一大块连续内存，划分为等长的 "槽位"。分配时从空闲链表取一个槽位，释放时归还。适用于频繁创建/销毁同类型小对象的场景（如 inode、dentry 等内核数据结构）。

### 11.2 核心操作

```rust
/// 创建一个新的 slab：将 obj_size 对齐到 8 字节，初始化空闲链表
pub fn new(obj_size: usize, capacity: usize) -> Self {
    let aligned = (obj_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1);
    let total = aligned * capacity;
    // 初始化空闲链表：每个条目是槽位的起始偏移
    let mut fl = VecDeque::with_capacity(capacity);
    for i in 0..capacity {
        fl.push_back(i * aligned);  // 0, 8, 16, 24, ...
    }
    Self { data: vec![0u8; total], obj_size: aligned, capacity, free_list: fl, allocated: 0, tag: 0 }
}

/// 从 slab 分配一个对象，返回在 data 中的偏移量
pub fn slab_alloc(&mut self, zeroed: bool) -> Option<usize> {
    let slot = self.free_list.pop_front()?;  // 取一个空闲槽位
    // 清零初始化
    if !needs_init {
        let region = &mut self.data[slot..obj_end];
        for pos in 0..region.len() { region[pos] = 0; }
    }
    self.allocated += 1;
    Some(slot)
}

/// 释放一个对象回空闲链表
pub fn slab_free(&mut self, offset: usize) {
    // 校验：偏移在范围内，且对齐到 obj_size 边界
    let valid = offset < self.data.len();
    let aligned = (offset % self.obj_size) == 0;
    if valid && aligned {
        self.free_list.push_back(offset);
        if self.allocated > 0 { self.allocated -= 1; }
    }
}

/// 收缩 slab：当没有已分配对象时释放全部内存
pub fn shrink(&mut self) -> usize { ... }

/// 按偏移量只读/可写地访问对象数据
pub fn obj_at(&self, offset: usize) -> Option<&[u8]> { ... }
pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> { ... }
```

---

## 十二、堆管理函数

### 12.1 heap_init — 堆初始化

```rust
/// 初始化内核堆区域：将基址和大小对齐到页边界
pub fn heap_init(base: usize, sz: usize) -> usize {
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);  // 向上对齐
    let aligned_sz = sz & !(PAGE_SZ - 1);  // 向下对齐
    let end = aligned_base + aligned_sz;
    // 计算管理元数据需要的页面数（每 64 页需 1 页元数据）
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end  // 返回堆的结束地址
}
```

### 12.2 heap_grow — 堆动态增长

```rust
/// 从帧池分配 n 个页帧来增长内核堆，尝试合并相邻页面
pub fn heap_grow(pool: &FramePool, n: usize) -> Vec<(usize, usize)> {
    let mut addrs: Vec<(usize, usize)> = Vec::new();  // (虚拟地址, 大小) 列表
    while acquired < n && attempts < max_attempts {
        // 从上次分配的下一帧开始搜索，提高连续性
        let preferred_start = if addrs.is_empty() { 0 } else {
            let (last_va, last_sz) = addrs.last().unwrap();
            let last_pg = (*last_va - PHYS_OFF) / PAGE_SZ + *last_sz / PAGE_SZ;
            last_pg
        };
        // 分配成功后尝试与上一块合并
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
    }
    addrs
}
```

---

## 十三、帧池维护

### 13.1 defragment_frame_pool — 碎片分析

```rust
/// 分析帧池位图的碎片情况，返回空闲帧总数
pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
    // 统计空闲帧、最后使用帧、首个空闲帧
    for i in 0..slots.len() { ... }
    // 计算碎片评分：交替的 "空闲-占用" 段越多，碎片越严重
    let mut frag_score = 0;
    for i in 0..slots.len() {
        if slots[i] { run_len += 1; }
        else { if run_len > 0 { frag_score += 1; } run_len = 0; }
    }
    // 计算最大连续空闲块的阶数（order = log2(最大连续空闲页数)）
    let _max_order = { ... };
    free_count
}
```

### 13.2 verify_page_alignment — 页对齐验证

```rust
/// 验证地址是否对指定的 buddy order 正确对齐
pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    let align = PAGE_SZ << order;  // order 0 = 4K, order 1 = 8K, ...
    let mask = align - 1;
    let aligned = (addr & mask) == 0;  // 地址必须对齐到块大小
    let in_range = addr < KERN_BASE;   // 地址必须在用户空间
    let valid_order = order < 12;       // order 最大为 11 (8MB 块)
    aligned && in_range && valid_order && cross_check
}
```

### 13.3 compute_rss_watermark — RSS 水位线计算

```rust
/// 根据区域权重和帧池容量计算 RSS（常驻集大小）水位线
pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    let mut total_weight: u64 = 0;
    for r in regions {
        let pages = (r.len + PAGE_SZ - 1) / PAGE_SZ;
        // 按权限赋予不同权重：
        // 可执行区域 x3（代码段最重要）
        // 可写区域 x2（数据段次之）
        // 只读区域 x1
        let weight = match r.flags & (VM_READ | VM_WRITE | VM_EXEC) {
            f if f & VM_EXEC != 0 => pages as u64 * 3,
            f if f & VM_WRITE != 0 => pages as u64 * 2,
            _ => pages as u64,
        };
        // 共享区域权重 x1，私有区域权重 x2
        let shared_factor = if r.flags & VM_SHARED != 0 { 1 } else { 2 };
        total_weight += weight * shared_factor;
    }
    // 归一化到帧池容量的百分比，上限为 pool_cap/2
    let raw_mark = (total_weight * 100) / cap64;
    let clamped = min(raw_mark, cap64 / 2) as usize;
    clamped
}
```

---

## 十四、BuddyAllocator — 伙伴分配器

### 14.1 结构体定义

```rust
/// 二进制伙伴分配器，支持 2 的幂次页块分配与合并
pub struct BuddyAllocator {
    /// 空闲链表数组：free_lists[o] 存储 order-o 的空闲块起始地址
    pub free_lists: Vec<Vec<usize>>,
    /// 最大支持的阶数（order 范围：0 ~ max_order）
    pub max_order: usize,
    /// 管理内存的起始物理地址
    pub base_addr: usize,
    /// 管理的总页数
    pub total_pages: usize,
    /// 已分配的页数（原子操作）
    pub allocated: AtomicUsize,
}
```

**伙伴系统原理：** 将内存按 2 的幂次划分为块。每个 order-o 的块大小为 `2^o * PAGE_SZ`。分配时如果当前阶没有空闲块，就从更高阶拆分；释放时尝试与 "伙伴块" 合并为更高阶块。

### 14.2 初始化

```rust
/// 创建伙伴分配器，将 total_pages 拆分为 2 的幂次块
pub fn new(base: usize, total_pages: usize, max_order: usize) -> Self {
    let order = log2_floor(total_pages);
    let usable_order = min(order, max_order);
    let block_pages = 1 << usable_order;
    // 先用最大块填充
    let mut addr = base;
    let mut remaining = total_pages;
    while remaining >= block_pages {
        free_lists[usable_order].push(addr);
        addr += block_pages * PAGE_SZ;
        remaining -= block_pages;
    }
    // 剩余部分用递减的小块填充（贪心分解）
    for o in (0..usable_order).rev() {
        let pages = 1 << o;
        while remaining >= pages {
            free_lists[o].push(addr);
            addr += pages * PAGE_SZ;
            remaining -= pages;
        }
    }
    Self { ... }
}
```

**初始化示例：** `total_pages = 13, max_order = 3`
```
13 = 8 + 4 + 1
free_lists[3] = [base + 0]       (1 个 8 页块)
free_lists[2] = [base + 8*4096]  (1 个 4 页块)
free_lists[0] = [base + 12*4096] (1 个 1 页块)
```

### 14.3 分配 — alloc_order

```rust
/// 分配一个 order 阶的内存块
pub fn alloc_order(&mut self, order: usize) -> Option<usize> {
    if order > self.max_order { return None; }
    // 从 order 阶开始向上查找有空闲块的最低阶
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
```

### 14.4 释放 — free_order（含伙伴合并）

```rust
/// 释放一个 order 阶的内存块，尝试与伙伴块合并
pub fn free_order(&mut self, addr: usize, order: usize) {
    let mut current_addr = addr;
    let mut current_order = order;
    // 逐级尝试合并
    while current_order < self.max_order {
        let block_size = (1 << current_order) * PAGE_SZ;
        // 伙伴块地址 = 当前地址 XOR 块大小（位运算特性）
        let buddy_addr = current_addr ^ block_size;
        // 检查伙伴块是否在空闲链表中
        if let Some(pos) = self.free_lists[current_order]
            .iter().position(|&a| a == buddy_addr) {
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
```

**合并过程图解：**

```
释放 order-0 块 A (addr=0x0000):
  order 0: A 的伙伴是 B(0x1000)
  ├── B 在空闲链表中 → 合并为 order-1 块 (0x0000, 大小 8K)
  │   order 1: (0x0000) 的伙伴是 (0x2000)
  │   ├── (0x2000) 不在空闲链表中 → 停止
  │   └── 将 (0x0000) 放入 free_lists[1]
  └── B 不在空闲链表中 → 直接将 A 放入 free_lists[0]
```

### 14.5 统计方法

```rust
/// 计算空闲页总数
pub fn free_pages_count(&self) -> usize { ... }

/// 查找最大的空闲块阶数
pub fn largest_free_order(&self) -> usize { ... }

/// 碎片评分：0 表示完全不碎片，100 表示极度碎片
/// 计算方式：(总空闲页 - 最大连续块页数) / 总空闲页 * 100
pub fn fragmentation_score(&self) -> usize { ... }

/// 创建分配器的快照（用于调试/测试）
pub fn snapshot(&self) -> BuddyAllocator { ... }
```

---

## 十五、使用场景

### 15.1 物理页帧分配（FramePool）

测试 `group_01::basic_cross_module_lock_order` 展示了 FramePool 与内核全局锁的协作：

```rust
let pool = Arc::new(FramePool::new(16));  // 16 个页帧的帧池
GKL.enter(1003);
p.get(1004);  // 持有全局锁时分配页帧
GKL.leave();
```

### 15.2 引用计数与 COW

测试 `group_04::basic_cow_single_thread` 验证了 COW 的单线程行为：

```rust
let pool = FramePool::new(16);
let src = PgFrame::with_rc(2);     // 原页帧引用计数 = 2（父子进程共享）
let sp = SharedPage::new(0);
let initial_free = pool.free_count();
let result = sp.fault(&pool, &src);  // 触发 COW
assert!(result.is_ok());
assert_eq!(pool.free_count(), initial_free - 1);  // 消耗了一个新帧
assert_eq!(src.count(), 1);  // 原帧引用计数减 1
```

### 15.3 多进程集成场景

测试 `group_11::basic_fork_exec_workload` 展示了完整的 fork + COW + 帧管理流程：

```rust
let kern = Kernel::new(64);
kern.proc_init();
// 分配 4 个帧
for _ in 0..4 { frames.push(kern.pool.get_inner().unwrap()); }
// COW 缺页处理
let sp = SharedPage::new(frames[0]);
sp.fault(&kern.pool, &src);
// 4 次直接分配 + 1 次 COW = 消耗 5 帧
assert_eq!(kern.pool.free_count(), 59);  // 64 - 5 = 59
```

### 15.4 并发引用计数

测试 `group_04::basic_refcount_concurrent_increment` 验证了 64 个线程并发增加引用计数的正确性：

```rust
let f = Arc::new(PgFrame::with_rc(0));
let handles: Vec<_> = (0..64)
    .map(|_| { let f = f.clone(); thread::spawn(move || { f.up(); }) })
    .collect();
for h in handles { h.join().unwrap(); }
assert_eq!(f.count(), 64);
```

---

## 十六、跨模块连接

```
memory.rs
├── consts.rs
│   ├── PAGE_SZ, N_FRAMES, MEM_OFF, PHYS_OFF, KERN_BASE  — 地址空间参数
│   ├── VM_READ/WRITE/EXEC/SHARED/GROWSDOWN  — 区域权限标志
│   ├── KSTK_SZ  — 内核栈大小
│   ├── SLAB_ALIGN  — slab 对齐要求
│   └── KHEAP_SZ  — 堆总大小
│
├── util.rs
│   └── CLK  — frame_alloc 的时钟扫描起点
│
├── trap.rs
│   └── on_pgfault  — 缺页异常处理入口（调用链的起点）
│
└── fs.rs
    └── mmap 文件映射  — VmRegion 的 offset 字段用于文件映射
```

---

## 十七、潜在的改进方向

1. **FramePool 的线性扫描性能**：当前 `get_inner()` 使用 O(n) 线性扫描，帧池较大时效率低。可改为空闲链表或分层位图
2. **VmMap.insert() 缺少合并逻辑**：虽然检测了 `_coalesce_prev`，但实际未执行合并，可能导致区域碎片积累
3. **BuddyAllocator.free_order() 的伙伴查找**：当前使用 `iter().position()` 线性查找伙伴块 O(n)，可改用 HashSet 实现 O(1) 查找
4. **SlabEntry 不支持动态扩容**：容量固定，满了只能返回 None。可添加 slab 链表实现多 slab 扩展
5. **SharedPage.fault() 缺少数据复制**：COW 时分配了新帧但未从旧帧复制数据，实际使用时需要 memcpy
6. **heap_grow 合并策略有限**：只与最后一块尝试合并，可改进为对所有已获取块做全局合并
7. **缺少 NUMA 感知**：ZoneInfo 虽然分区了物理内存，但未考虑 NUMA 节点的 locality
8. **Ordering::Relaxed 的安全性**：大量使用 Relaxed 内存序，在弱一致性架构（如 RISC-V）上可能需要加强
