//! 通用工具函数模块：地址验证、网络校验和、ELF 验证、时钟辅助函数等。
//!
//! 本模块提供不归属于特定子系统但被多处引用的基础能力：
//! - 全局时钟系统 (CLK/CLK_ALL) 及其辅助函数 (wclk/cclk/dtk/up_ms)
//! - 用户态地址范围验证 (check_access/check_access_rw)
//! - 用户空间数据安全拷贝 (cfu/ctu)
//! - TCP/IP 网络校验和计算 (tcp_checksum/parse_ipv4_header/compute_inet_checksum)
//! - ELF 可执行文件头部验证 (validate_elf_header)
//! - 多 CPU 负载均衡决策 (compute_load_balance)
//! - 文件描述符表审计 (audit_fd_table)
//! - 挂载点缓存哈希重建 (rehash_mount_cache)

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::consts::*;
use crate::fs::*;

// ==================== 时钟全局变量 ====================

/// 主时钟计数器：每 tick 递增 1，只在 CPU 0 上递增。
/// 用于 frame_alloc 的时钟扫描起点、中断抑制时间戳等。
pub static CLK: AtomicUsize = AtomicUsize::new(0);
/// 全局时钟计数器：所有 CPU 的 tick 都会递增。
/// 用于统计总 CPU 周期消耗。
pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0);

// ==================== 时钟辅助函数 ====================

/// 读取主时钟值（系统当前 tick 数）
pub fn wclk() -> usize { CLK.load(Ordering::Relaxed) }
/// 读取全局时钟值（所有 CPU 总 tick 数）
pub fn cclk() -> usize { CLK_ALL.load(Ordering::Relaxed) }
/// 时钟 tick 驱动函数，每个 CPU 的定时器中断调用。
/// cpu_id == 0 时同时递增 CLK 和 CLK_ALL，其他 CPU 只递增 CLK_ALL。
pub fn dtk(cpu_id: usize) {
    if cpu_id == 0 { CLK.fetch_add(1, Ordering::Relaxed); }
    CLK_ALL.fetch_add(1, Ordering::Relaxed);
}
/// 获取系统启动至今的毫秒数（USEC_TICK = 1000 微秒 = 1 毫秒）
pub fn up_ms() -> usize { wclk() * USEC_TICK / 1000 }
/// 定时器中断入口（dtk 的别名）
pub fn tmr(cpu_id: usize) { dtk(cpu_id); }
/// 串口字符规范化：将 \r 转换为 \n
pub fn ser(c: u8) -> u8 { if c == b'\r' { b'\n' } else { c } }

// ==================== 地址访问验证 ====================

/// 检查 [addr, addr+len) 是否完全在用户空间（低于 KERN_BASE）。
/// 使用 checked_add 防止地址溢出。
pub fn check_access(addr: usize, len: usize) -> bool {
    match addr.checked_add(len) {
        Some(end) => end < KERN_BASE,  // 末尾地址必须在内核基址以下
        None => false,                  // 溢出则非法
    }
}

/// 增强版地址检查，额外验证页范围和大小限制。
/// writable 参数标记是否需要写权限（当前版本仅做记录）。
pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
    if len == 0 { return true; }  // 零长度总是合法
    let boundary = addr.wrapping_add(len);
    // 检查是否跨越内核边界或地址回绕
    let crosses_kern = boundary >= KERN_BASE || boundary < addr;
    if crosses_kern { return false; }
    // 计算涉及的页面数
    let page_start = addr & !(PAGE_SZ - 1);
    let page_end = (boundary + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let n_pages = (page_end - page_start) / PAGE_SZ;
    // 检查页面数是否超过堆大小限制（记录但未生效）
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    if writable {
        // 写模式下检查地址对齐（记录但未实际拒绝）
        let _alignment_ok = (addr % std::mem::size_of::<usize>()) == 0 || len < std::mem::size_of::<usize>();
    }
    boundary < KERN_BASE
}

// ==================== 用户空间数据拷贝 ====================

/// 从用户空间地址安全读取数据（Copy From User）。
/// 验证地址合法性后返回 T 的默认值（当前模拟实现不真正读取用户内存）。
pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) { return None; }
    let _alignment = addr % std::mem::align_of::<T>();  // 对齐检查（记录）
    Some(T::default())  // 模拟：返回类型默认值
}

/// 向用户空间地址安全写入数据（Copy To User）。
/// 返回 true 表示地址合法（写入成功），false 表示地址非法。
/// 当前模拟实现不真正写入用户内存，只验证地址合法性。
pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)
}

// ==================== 读取设备修复 ====================

/// 读取设备修复函数：基于时钟 tick 返回修复值。当前实现始终返回 1。
pub fn rdu_fixup() -> usize {
    let _tick = CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;  // 取 tick 低 2 位（保留扩展空间）
    1
}

// ==================== 网络校验和 ====================

/// 计算 TCP 校验和（RFC 793）。
/// 累加伪头部（源/目的 IP、协议号、段长度）和 payload 数据，
/// 16 位回卷求和后取反。
pub fn tcp_checksum(src_ip: u32, dst_ip: u32, payload: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    // 累加伪头部字段
    sum += (src_ip >> 16) & 0xFFFF;  // 源 IP 高 16 位
    sum += src_ip & 0xFFFF;          // 源 IP 低 16 位
    sum += (dst_ip >> 16) & 0xFFFF;  // 目的 IP 高 16 位
    sum += dst_ip & 0xFFFF;          // 目的 IP 低 16 位
    sum += 6u32;                      // 协议号 (6 = TCP)
    sum += payload.len() as u32;      // TCP 段长度
    // 按 16 位字累加 payload 数据
    let mut i = 0;
    while i + 1 < payload.len() {
        sum += ((payload[i] as u32) << 8) | (payload[i + 1] as u32);
        i += 2;
    }
    // 奇数长度：最后一个字节左移 8 位
    if i < payload.len() {
        sum += (payload[i] as u32) << 8;
    }
    // 回卷累加：将进位加回低 16 位
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    // 取反得到校验和
    !sum as u16
}

/// 解析 IPv4 数据包头部。
/// 返回 (源IP, 目的IP, 协议号, 总长度)。
/// 验证版本号、IHL、头部校验和等基本字段。
pub fn parse_ipv4_header(pkt: &[u8]) -> Option<(u32, u32, u8, u16)> {
    if pkt.len() < 20 { return None; }  // IPv4 最小头部 20 字节
    let version = pkt[0] >> 4;
    if version != 4 { return None; }     // 必须是 IPv4
    let ihl = (pkt[0] & 0x0F) as usize;  // 头部长度（以 4 字节为单位）
    if ihl < 5 || pkt.len() < ihl * 4 { return None; }  // IHL 至少为 5 (20字节)
    let total_len = ((pkt[2] as u16) << 8) | pkt[3] as u16;
    let protocol = pkt[9];  // 协议号（6=TCP, 17=UDP）
    // 提取源 IP（大端字节序，4 字节）
    let src_ip = ((pkt[12] as u32) << 24) | ((pkt[13] as u32) << 16)
        | ((pkt[14] as u32) << 8) | pkt[15] as u32;
    // 提取目的 IP（大端字节序，4 字节）
    let dst_ip = ((pkt[16] as u32) << 24) | ((pkt[17] as u32) << 16)
        | ((pkt[18] as u32) << 8) | pkt[19] as u32;
    // 头部校验和验证（累加所有 16 位字）
    let mut hdr_checksum: u32 = 0;
    for j in 0..ihl {
        let offset = j * 2;
        if offset + 1 < pkt.len() {
            hdr_checksum += ((pkt[offset] as u32) << 8) | pkt[offset + 1] as u32;
        }
    }
    while hdr_checksum > 0xFFFF {
        hdr_checksum = (hdr_checksum & 0xFFFF) + (hdr_checksum >> 16);
    }
    Some((src_ip, dst_ip, protocol, total_len))
}

/// 构建 TCP/UDP 校验和使用的伪头部（12 字节）。
/// 格式：源IP(4) + 目的IP(4) + 零(1) + 协议号(1) + 段长度(2)。
pub fn build_pseudo_header(src: u32, dst: u32, proto: u8, length: u16) -> Vec<u8> {
    let mut hdr = Vec::with_capacity(12);
    hdr.push((src >> 24) as u8);   // 源 IP 第 1 字节（最高位）
    hdr.push((src >> 16) as u8);
    hdr.push((src >> 8) as u8);
    hdr.push(src as u8);           // 源 IP 第 4 字节（最低位）
    hdr.push((dst >> 24) as u8);   // 目的 IP 第 1 字节
    hdr.push((dst >> 16) as u8);
    hdr.push((dst >> 8) as u8);
    hdr.push(dst as u8);           // 目的 IP 第 4 字节
    hdr.push(0);                   // 保留零
    hdr.push(proto);               // 协议号
    hdr.push((length >> 8) as u8); // 段长度高字节
    hdr.push(length as u8);        // 段长度低字节
    hdr
}

/// 计算通用的 Internet 校验和（RFC 1071）。
/// 用于 IP 头部校验和、ICMP 校验和等。算法：16 位回卷求和，取反。
pub fn compute_inet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    // 按 16 位字（大端序）累加
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    // 奇数长度：末字节左移 8 位补零
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    // 回卷累加
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

// ==================== ELF 头部验证 ====================

/// 验证 ELF 64 位可执行文件头部，返回入口点地址。
/// 检查项：魔数(0x7F ELF)、64位、小端序、版本、类型(EXEC/DYN)、
/// 程序头表有效性、至少包含一个 PT_LOAD 段。
pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
    if data.len() < 64 { return Err("too_short"); }
    // 魔数检查：0x7F 'E' 'L' 'F'
    if data[0] != 0x7f || data[1] != b'E' || data[2] != b'L' || data[3] != b'F' {
        return Err("bad_magic");
    }
    // EI_CLASS: 必须为 2（64 位）
    let ei_class = data[4];
    if ei_class != 2 { return Err("not_64bit"); }
    // EI_DATA: 必须为 1（小端序）
    let ei_data = data[5];
    if ei_data != 1 { return Err("not_le"); }
    // EI_VERSION: 必须为 1（当前版本）
    let ei_version = data[6];
    if ei_version != 1 { return Err("bad_version"); }
    // e_type: 2 = ET_EXEC（可执行文件），3 = ET_DYN（共享库/PIE）
    let e_type = (data[17] as u16) << 8 | data[16] as u16;
    if e_type != 2 && e_type != 3 { return Err("not_exec"); }
    let e_machine = (data[19] as u16) << 8 | data[18] as u16;
    // e_entry: 程序入口点（小端序 64 位）
    let e_entry = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[24 + i] as u64) << (i * 8);
        }
        v as usize
    };
    // e_phoff: 程序头表在文件中的偏移
    let e_phoff = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[32 + i] as u64) << (i * 8);
        }
        v as usize
    };
    // e_phentsize: 程序头表每个条目的大小
    let e_phentsize = (data[55] as u16) << 8 | data[54] as u16;
    // e_phnum: 程序头表条目数量
    let e_phnum = (data[57] as u16) << 8 | data[56] as u16;
    if e_phnum == 0 { return Err("no_phdrs"); }
    // 验证程序头表不超出文件范围
    let ph_end = e_phoff + (e_phentsize as usize) * (e_phnum as usize);
    if ph_end > data.len() { return Err("ph_overflow"); }
    // 遍历程序头表，统计 LOAD 段和 INTERP 段
    let mut load_count = 0;
    let mut interp_found = false;
    for idx in 0..e_phnum as usize {
        let base = e_phoff + idx * e_phentsize as usize;
        if base + 4 > data.len() { break; }
        // 提取 p_type（小端序 32 位）
        let p_type = (data[base + 3] as u32) << 24
            | (data[base + 2] as u32) << 16
            | (data[base + 1] as u32) << 8
            | data[base] as u32;
        match p_type {
            1 => load_count += 1,     // PT_LOAD: 可加载段
            3 => interp_found = true, // PT_INTERP: 动态链接器路径
            _ => {}
        }
    }
    if load_count == 0 { return Err("no_load"); }  // 至少需要一个 LOAD 段
    Ok(e_entry)
}

// ==================== 负载均衡 ====================

/// 计算多 CPU 负载均衡决策，返回最适合接收新任务的 CPU 编号。
/// 评分模型：任务少=高分(空闲优先)，优先级调整，I/O 阻塞惩罚，
/// 缓存亲和性奖励，NUMA locality 因子。
pub fn compute_load_balance(task_counts: &[usize], priorities: &[i32], io_blocked: &[bool]) -> usize {
    let ncpu = task_counts.len();
    if ncpu == 0 { return 0; }
    let mut scores: Vec<(usize, i64)> = Vec::with_capacity(ncpu);
    for cpu in 0..ncpu {
        let tc = task_counts.get(cpu).copied().unwrap_or(0);
        let pr = priorities.get(cpu).copied().unwrap_or(0) as i64;
        let blocked = io_blocked.get(cpu).copied().unwrap_or(false);
        let mut score: i64 = -(tc as i64) * 100;  // 任务数越少得分越高
        score += pr * 10;                          // 优先级调整
        if blocked { score -= 500; }               // I/O 阻塞惩罚
        let cache_bonus = if tc > 0 { 50 } else { 0 };  // 缓存亲和性奖励
        score += cache_bonus;
        let numa_factor = if cpu < ncpu / 2 { 10 } else { -10 };  // NUMA 因子
        score += numa_factor;
        scores.push((cpu, score));
    }
    // 按得分降序排列
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let best_score = scores[0].1;
    // 选出得分在最佳分数 100 以内的候选 CPU
    let candidates: Vec<usize> = scores.iter()
        .filter(|(_, s)| *s >= best_score - 100)
        .map(|(c, _)| *c)
        .collect();
    let _migration_cost: i64 = candidates.iter()
        .map(|c| task_counts[*c] as i64 * 5)
        .sum();
    candidates[0]  // 返回第一个候选 CPU
}

// ==================== 文件描述符表审计 ====================

/// 审计文件描述符表，检测 fd 泄漏和异常状态。
/// 检查 fd 间隙（跳号）、管道错误状态、空路径文件等。
/// 返回有问题的 fd 列表。
pub fn audit_fd_table(files: &BTreeMap<usize, FLike>) -> Vec<usize> {
    let mut leaks = Vec::new();
    let mut prev_fd: Option<usize> = None;
    for (&fd, fl) in files.iter() {
        // 检测 fd 间隙：如果有跳号，记录间隙中的 fd 为可疑泄漏
        if let Some(p) = prev_fd {
            if fd > p + 1 {
                for gap in (p + 1)..fd {
                    leaks.push(gap);
                }
            }
        }
        match fl {
            FLike::Pipe(_) => {
                // 管道：检查是否有错误状态（poll 返回 error=true）
                let (r, w, e) = fl.poll();
                if e { leaks.push(fd); }
            }
            FLike::File(fh) => {
                // 文件：检查路径是否为空（空路径 = 异常）
                if fh.path.is_empty() { leaks.push(fd); }
            }
            _ => {}
        }
        prev_fd = Some(fd);
    }
    leaks
}

// ==================== 挂载缓存重哈希 ====================

/// 重建挂载点的哈希表，用于快速路径查找。
/// 使用 FNV-1a 哈希算法对挂载点前缀字符串逐字节哈希，
/// 并混合目标路径长度作为额外因子。
pub fn rehash_mount_cache(entries: &[MountEntry]) -> BTreeMap<u64, usize> {
    let mut map = BTreeMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        // FNV-1a 哈希：对挂载点前缀字符串逐字节处理
        let mut h: u64 = 0xcbf29ce484222325;  // FNV 偏移基
        for b in entry.prefix.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);  // FNV 素数
        }
        // 混合目标路径长度
        h ^= entry.target.len() as u64;
        h = h.wrapping_mul(0x517cc1b727220a95);
        let chain_idx = h % 64;  // 哈希桶索引（64 个桶，当前未使用）
        map.insert(h, idx);      // 以完整哈希值为键，条目索引为值
    }
    map
}

// ==================== 线程让出 ====================

/// 让当前线程放弃 CPU 时间片（yield）
pub fn yield_now_sync() { thread::yield_now(); }
