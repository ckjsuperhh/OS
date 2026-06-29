# Util 模块阅读指南

> 文件路径: `kernel-refactored/src/util.rs`
> 代码量: 288 行 | 全局变量 + 多组工具函数 | 依赖: `consts`, `fs`

---

## 一、模块概述

`util.rs` 是内核的 **通用工具函数** 集合，提供一系列不归属于特定子系统但被多处引用的基础能力：

| 功能组 | 关键函数/变量 | 用途 |
|---|---|---|
| 时钟系统 | `CLK`, `CLK_ALL`, `wclk`, `cclk`, `dtk`, `up_ms` | 内核全局时钟计数与时间查询 |
| 地址验证 | `check_access`, `check_access_rw` | 用户态地址范围的合法性检查 |
| 用户空间拷贝 | `cfu`, `ctu` | 从/向用户空间安全拷贝数据 |
| 网络校验和 | `tcp_checksum`, `parse_ipv4_header`, `compute_inet_checksum`, `build_pseudo_header` | TCP/IP 协议栈的校验和计算 |
| ELF 验证 | `validate_elf_header` | ELF 可执行文件格式验证 |
| 负载均衡 | `compute_load_balance` | 多 CPU 任务分配决策 |
| 文件描述符审计 | `audit_fd_table` | 检测 fd 泄漏和异常 |
| 挂载缓存 | `rehash_mount_cache` | 挂载点哈希表重建 |

**设计定位：** 类似于 Linux 内核中 `lib/` 目录下的通用工具代码。这些函数没有自己的状态管理（除了全局时钟），是纯粹的 "工具库"，被 `trap.rs`、`memory.rs`、`fs.rs` 等多个模块依赖。

---

## 二、时钟系统

### 2.1 全局变量

```rust
/// 主时钟计数器：每 tick 递增 1，只在 CPU 0 上递增
/// 用于 frame_alloc 的时钟扫描起点、中断抑制时间戳等
pub static CLK: AtomicUsize = AtomicUsize::new(0);

/// 全局时钟计数器：所有 CPU 的 tick 都会递增
/// 用于统计总 CPU 周期消耗
pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0);
```

**两个时钟的区别：** `CLK` 只在 CPU 0 上递增，代表 "系统时间"；`CLK_ALL` 所有 CPU 都递增，代表 "总 CPU tick 数"。

### 2.2 时钟操作函数

```rust
/// 读取主时钟值（系统当前 tick）
pub fn wclk() -> usize { CLK.load(Ordering::Relaxed) }

/// 读取全局时钟值（所有 CPU 总 tick）
pub fn cclk() -> usize { CLK_ALL.load(Ordering::Relaxed) }

/// 时钟 tick 驱动函数，每个 CPU 的定时器中断调用
/// cpu_id == 0 时同时递增 CLK 和 CLK_ALL
/// 其他 CPU 只递增 CLK_ALL
pub fn dtk(cpu_id: usize) {
    if cpu_id == 0 { CLK.fetch_add(1, Ordering::Relaxed); }
    CLK_ALL.fetch_add(1, Ordering::Relaxed);
}

/// 获取系统启动至今的毫秒数
/// USEC_TICK = 1000（1 tick = 1000 微秒 = 1 毫秒）
/// 因此 up_ms = wclk * 1000 / 1000 = wclk
pub fn up_ms() -> usize { wclk() * USEC_TICK / 1000 }

/// 定时器中断入口（dtk 的别名）
pub fn tmr(cpu_id: usize) { dtk(cpu_id); }

/// 串口字符规范化：将 \r 转换为 \n
pub fn ser(c: u8) -> u8 { if c == b'\r' { b'\n' } else { c } }
```

**时钟关系图：**

```
CPU 0 定时器中断 → dtk(0) → CLK += 1, CLK_ALL += 1
CPU 1 定时器中断 → dtk(1) → CLK_ALL += 1
CPU 2 定时器中断 → dtk(2) → CLK_ALL += 1

系统运行时间 = wclk() * USEC_TICK 微秒
             = up_ms() 毫秒
```

### 2.3 rdu_fixup

```rust
/// 读取设备修复函数：基于时钟 tick 返回修复值
/// 当前实现始终返回 1
pub fn rdu_fixup() -> usize {
    let _tick = CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;  // 取 tick 的低 2 位（保留扩展空间）
    1
}
```

---

## 三、地址访问验证

### 3.1 check_access — 基础地址范围检查

```rust
/// 检查 [addr, addr+len) 是否完全在用户空间（低于 KERN_BASE）
/// 使用 checked_add 防止溢出
pub fn check_access(addr: usize, len: usize) -> bool {
    match addr.checked_add(len) {
        Some(end) => end < KERN_BASE,  // 末尾地址必须在内核基址以下
        None => false,                  // 溢出则非法
    }
}
```

**安全检查逻辑：**
```
addr = 0x1000, len = 0x2000
  → end = 0x3000 < KERN_BASE → true (合法)

addr = KERN_BASE, len = 1
  → end = KERN_BASE + 1 ≥ KERN_BASE → false (非法)

addr = KERN_BASE - 1, len = usize::MAX
  → checked_add 溢出 → None → false (非法)
```

### 3.2 check_access_rw — 读写地址验证（增强版）

```rust
/// 增强版地址检查，额外验证页范围和大小限制
/// writable: 是否需要写权限（当前版本仅做记录，未实际检查写保护）
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
    // 检查页面数是否超过堆大小限制
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    // 写模式下检查对齐
    if writable {
        let _alignment_ok = (addr % std::mem::size_of::<usize>()) == 0
            || len < std::mem::size_of::<usize>();
    }
    boundary < KERN_BASE
}
```

---

## 四、用户空间数据拷贝

### 4.1 cfu — Copy From User

```rust
/// 从用户空间地址安全读取数据
/// 如果地址不合法返回 None，合法则返回 T 的默认值
/// 注意：当前实现不真正读取用户内存（模拟内核），只验证地址合法性
pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) { return None; }
    let _alignment = addr % std::mem::align_of::<T>();  // 对齐检查（记录）
    Some(T::default())  // 模拟：返回类型默认值
}
```

### 4.2 ctu — Copy To User

```rust
/// 向用户空间地址安全写入数据
/// 返回 true 表示地址合法（写入成功），false 表示地址非法
/// 注意：当前实现不真正写入用户内存（模拟内核），只验证地址合法性
pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)
}
```

**cfu/ctu 在真实内核中的作用：**
```
系统调用 read(fd, buf, count):
  1. cfu(buf, count) → 验证用户传入的 buf 地址合法
  2. 从内核缓冲区读取数据
  3. ctu(buf, count, &data) → 将数据安全写入用户空间

当前模拟实现省略了实际的数据拷贝步骤。
```

---

## 五、网络校验和

### 5.1 tcp_checksum — TCP 校验和

```rust
/// 计算 TCP 校验和（RFC 793）
/// src_ip, dst_ip: 源/目的 IP 地址（32 位整数格式）
/// payload: TCP 报文段数据（含 TCP 头部）
pub fn tcp_checksum(src_ip: u32, dst_ip: u32, payload: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    // 累加伪头部字段
    sum += (src_ip >> 16) & 0xFFFF;  // 源 IP 高 16 位
    sum += src_ip & 0xFFFF;          // 源 IP 低 16 位
    sum += (dst_ip >> 16) & 0xFFFF;  // 目的 IP 高 16 位
    sum += dst_ip & 0xFFFF;          // 目的 IP 低 16 位
    sum += 6u32;                      // 协议号 (6 = TCP)
    sum += payload.len() as u32;      // TCP 段长度

    // 累加 payload 数据（按 16 位字累加）
    let mut i = 0;
    while i + 1 < payload.len() {
        sum += ((payload[i] as u32) << 8) | (payload[i + 1] as u32);
        i += 2;
    }
    // 奇数长度：最后一个字节左移 8 位
    if i < payload.len() { sum += (payload[i] as u32) << 8; }

    // 回卷累加：将进位加回低 16 位
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    // 取反得到校验和
    !sum as u16
}
```

**TCP 校验和计算过程：**

```
伪头部：
  ┌─────────────┬─────────────┐
  │  Source IP  │  Dest IP    │
  ├─────────────┼─────┬───────┤
  │   Zero      │Proto│ Length│
  └─────────────┴─────┴───────┘

TCP 段：
  ┌─────────────────────────────┐
  │  TCP Header (20+ bytes)     │
  ├─────────────────────────────┤
  │  TCP Data                   │
  └─────────────────────────────┘

校验和 = ~(伪头部 + TCP段) 的 16 位回卷求和
```

### 5.2 parse_ipv4_header — IPv4 头部解析

```rust
/// 解析 IPv4 数据包头部
/// 返回 (源IP, 目的IP, 协议号, 总长度)
pub fn parse_ipv4_header(pkt: &[u8]) -> Option<(u32, u32, u8, u16)> {
    if pkt.len() < 20 { return None; }  // IPv4 最小头部 20 字节
    let version = pkt[0] >> 4;
    if version != 4 { return None; }     // 必须是 IPv4
    let ihl = (pkt[0] & 0x0F) as usize;  // 头部长度（以 4 字节为单位）
    if ihl < 5 || pkt.len() < ihl * 4 { return None; }  // IHL 至少为 5
    let total_len = ((pkt[2] as u16) << 8) | pkt[3] as u16;
    let protocol = pkt[9];  // 协议号（6=TCP, 17=UDP）
    // 提取源/目的 IP（大端字节序）
    let src_ip = ((pkt[12] as u32) << 24) | ((pkt[13] as u32) << 16)
        | ((pkt[14] as u32) << 8) | pkt[15] as u32;
    let dst_ip = ((pkt[16] as u32) << 24) | ((pkt[17] as u32) << 16)
        | ((pkt[18] as u32) << 8) | pkt[19] as u32;
    // 头部校验和验证（累加所有 16 位字，结果应为 0xFFFF）
    let mut hdr_checksum: u32 = 0;
    for j in 0..ihl { ... }
    Some((src_ip, dst_ip, protocol, total_len))
}
```

**IPv4 头部布局：**

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|Version|  IHL  |Type of Service|          Total Length         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|         Identification        |Flags|      Fragment Offset    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  Time to Live |    Protocol   |         Header Checksum       |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       Source Address                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                    Destination Address                        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

### 5.3 build_pseudo_header — 构建伪头部

```rust
/// 构建 TCP/UDP 校验和使用的伪头部（12 字节）
/// 格式：源IP(4) + 目的IP(4) + 零(1) + 协议号(1) + 段长度(2)
pub fn build_pseudo_header(src: u32, dst: u32, proto: u8, length: u16) -> Vec<u8> {
    let mut hdr = Vec::with_capacity(12);
    hdr.push((src >> 24) as u8);  // 源 IP 第 1 字节
    hdr.push((src >> 16) as u8);  // 源 IP 第 2 字节
    hdr.push((src >> 8) as u8);   // 源 IP 第 3 字节
    hdr.push(src as u8);          // 源 IP 第 4 字节
    hdr.push((dst >> 24) as u8);  // 目的 IP ...
    hdr.push((dst >> 16) as u8);
    hdr.push((dst >> 8) as u8);
    hdr.push(dst as u8);
    hdr.push(0);                  // 保留零
    hdr.push(proto);              // 协议号
    hdr.push((length >> 8) as u8);// 段长度高字节
    hdr.push(length as u8);       // 段长度低字节
    hdr
}
```

### 5.4 compute_inet_checksum — 通用 Internet 校验和

```rust
/// 计算通用的 Internet 校验和（RFC 1071）
/// 用于 IP 头部校验和、ICMP 校验和等
/// 算法：16 位回卷求和，取反
pub fn compute_inet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;  // 奇数长度，末字节左移
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);  // 回卷
    }
    !sum as u16
}
```

---

## 六、ELF 头部验证

```rust
/// 验证 ELF 可执行文件头部，返回入口点地址
/// 检查项：魔数、64位、小端序、版本、类型、机器架构、程序头表
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
    // EI_VERSION: 必须为 1
    let ei_version = data[6];
    if ei_version != 1 { return Err("bad_version"); }
    // e_type: 2 = ET_EXEC（可执行文件），3 = ET_DYN（共享库/PIE）
    let e_type = (data[17] as u16) << 8 | data[16] as u16;
    if e_type != 2 && e_type != 3 { return Err("not_exec"); }
    // e_entry: 程序入口点（小端序 64 位）
    let e_entry = { ... };
    // e_phoff: 程序头表偏移
    let e_phoff = { ... };
    // e_phentsize, e_phnum: 程序头表条目大小和数量
    let e_phentsize = ...;
    let e_phnum = ...;
    if e_phnum == 0 { return Err("no_phdrs"); }
    // 验证程序头表不超出文件范围
    let ph_end = e_phoff + (e_phentsize as usize) * (e_phnum as usize);
    if ph_end > data.len() { return Err("ph_overflow"); }
    // 遍历程序头表，统计 LOAD 段和 INTERP 段
    let mut load_count = 0;
    let mut interp_found = false;
    for idx in 0..e_phnum as usize {
        let p_type = ...;
        match p_type {
            1 => load_count += 1,   // PT_LOAD: 可加载段
            3 => interp_found = true, // PT_INTERP: 动态链接器路径
            _ => {}
        }
    }
    if load_count == 0 { return Err("no_load"); }
    Ok(e_entry)  // 返回入口点地址
}
```

**ELF 64 头部结构：**

```
偏移  大小  字段          检查内容
─────────────────────────────────
 0    4    e_ident[0..3]  魔数 0x7F ELF
 4    1    EI_CLASS       2 = 64 位
 5    1    EI_DATA        1 = 小端序
 6    1    EI_VERSION     1 = 当前版本
16    2    e_type         2=EXEC, 3=DYN
18    2    e_machine      (未检查)
24    8    e_entry        入口点地址
32    8    e_phoff        程序头表偏移
54    2    e_phentsize    程序头表条目大小
56    2    e_phnum        程序头表条目数量
```

---

## 七、负载均衡

```rust
/// 计算多 CPU 负载均衡决策，返回最适合接收新任务的 CPU 编号
/// task_counts: 每个 CPU 上的任务数
/// priorities: 每个 CPU 的优先级调整值
/// io_blocked: 每个 CPU 是否有 I/O 阻塞
pub fn compute_load_balance(
    task_counts: &[usize],
    priorities: &[i32],
    io_blocked: &[bool]
) -> usize {
    let ncpu = task_counts.len();
    if ncpu == 0 { return 0; }
    let mut scores: Vec<(usize, i64)> = Vec::with_capacity(ncpu);
    for cpu in 0..ncpu {
        let tc = task_counts.get(cpu).copied().unwrap_or(0);
        let pr = priorities.get(cpu).copied().unwrap_or(0) as i64;
        let blocked = io_blocked.get(cpu).copied().unwrap_or(false);
        let mut score: i64 = 0;
        // 任务数越少得分越高（任务少 = 空闲 = 适合接收新任务）
        score += -(tc as i64) * 100;
        // 优先级调整：高优先级 CPU 得分更高
        score += pr * 10;
        // I/O 阻塞惩罚
        if blocked { score -= 500; }
        // 缓存亲和性奖励：已有任务的 CPU 获得小幅加分
        let cache_bonus = if tc > 0 { 50 } else { 0 };
        score += cache_bonus;
        // NUMA 因子：前半部分 CPU 加分，后半部分减分
        let numa_factor = if cpu < ncpu / 2 { 10 } else { -10 };
        score += numa_factor;
        scores.push((cpu, score));
    }
    // 按得分降序排列
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    // 选出得分最高的 CPU（得分差距在 100 以内的视为等价候选）
    let best_score = scores[0].1;
    let candidates: Vec<usize> = scores.iter()
        .filter(|(_, s)| *s >= best_score - 100)
        .map(|(c, _)| *c)
        .collect();
    candidates[0]  // 返回第一个候选
}
```

**评分模型：**

```
score = -tasks * 100     (空闲优先)
      + priority * 10    (优先级调整)
      - 500 if io_blocked (I/O 阻塞惩罚)
      + 50 if tasks > 0   (缓存亲和)
      + 10/-10            (NUMA locality)

示例：4 CPU，任务数 [3, 1, 0, 2]
CPU 0: -300 + 50 = -250
CPU 1: -100 + 50 = -50
CPU 2:    0 + 0  =   0  ← 最佳（无任务但无缓存奖励）
CPU 3: -200 + 50 = -150
```

---

## 八、文件描述符审计

```rust
/// 审计文件描述符表，检测泄漏和异常的 fd
/// 返回有问题的 fd 列表
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
                // 管道：检查是否有错误状态
                let (r, w, e) = fl.poll();
                if e { leaks.push(fd); }  // 有错误的管道 = 泄漏
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
```

**审计逻辑：**
```
fd 表：{0: stdin, 1: stdout, 3: pipe, 5: file}
  → 间隙检测：fd=2 缺失（泄漏），fd=4 缺失（泄漏）
  → 管道检查：fd=3 的 pipe 是否有错误
  → 文件检查：fd=5 的 file 路径是否为空
  → 返回 [2, 4, ...]
```

---

## 九、挂载缓存重哈希

```rust
/// 重建挂载点的哈希表，用于快速路径查找
/// 使用 FNV-1a 哈希算法
pub fn rehash_mount_cache(entries: &[MountEntry]) -> BTreeMap<u64, usize> {
    let mut map = BTreeMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        // FNV-1a 哈希：对挂载点前缀字符串逐字节哈希
        let mut h: u64 = 0xcbf29ce484222325;  // FNV 偏移基
        for b in entry.prefix.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);  // FNV 素数
        }
        // 混合目标路径长度作为额外因子
        h ^= entry.target.len() as u64;
        h = h.wrapping_mul(0x517cc1b727220a95);
        let chain_idx = h % 64;  // 哈希桶索引（64 个桶）
        map.insert(h, idx);      // 以完整哈希值为键，索引为值
    }
    map
}
```

**用途：** 当挂载表发生变化（新增/删除挂载点）时，重新计算所有挂载点的哈希值，构建快速查找表。路径查找时通过哈希直接定位挂载点，避免遍历整个挂载表。

---

## 十、其他工具函数

```rust
/// 线程让出：让当前线程放弃 CPU 时间片
pub fn yield_now_sync() { thread::yield_now(); }
```

---

## 十一、使用场景

### 11.1 地址验证

测试 `group_10::basic_access_ok_valid_range` 和 `group_10::basic_access_ok_overflow`：

```rust
// 合法地址范围
assert!(check_access(0x1000, 0x100));
// 内核空间地址不合法
assert!(!check_access(KERN_BASE, 1));
// 溢出检测
let result = check_access(KERN_BASE - 1, usize::MAX);
assert!(!result);  // addr + len 溢出 → 不合法
```

### 11.2 mmap 场景中的地址验证

测试 `group_11::basic_mmap_file_io_workload` 中结合了帧分配和地址验证：

```rust
let pool = FramePool::new(32);
assert!(check_access(0x1000, 0x2000));  // 用户空间地址合法
// COW 操作
let f = pool.get_inner().unwrap();
let sp = SharedPage::new(f);
sp.fault(&pool, &src);
// 溢出检查
assert!(!check_access(0x1000, usize::MAX));  // 溢出 → 不合法
```

### 11.3 时钟驱动

```rust
// 系统启动后，定时器中断周期性调用 dtk()
dtk(0);  // CPU 0: CLK=1, CLK_ALL=1
dtk(1);  // CPU 1: CLK=1, CLK_ALL=2
dtk(0);  // CPU 0: CLK=2, CLK_ALL=3

assert_eq!(wclk(), 2);    // 系统 tick = 2
assert_eq!(cclk(), 3);    // 总 CPU tick = 3
assert_eq!(up_ms(), 2);   // 运行时间 = 2ms
```

---

## 十二、跨模块连接

```
util.rs
├── consts.rs
│   ├── KERN_BASE    — 地址验证的上界
│   ├── PAGE_SZ      — 页面大小，地址对齐用
│   ├── KHEAP_SZ     — 堆大小限制
│   └── USEC_TICK    — tick 到微秒的转换系数
│
├── fs.rs
│   ├── FLike        — audit_fd_table 审计的文件描述符类型
│   └── MountEntry   — rehash_mount_cache 的挂载点条目
│
├── memory.rs
│   └── CLK          — frame_alloc 的时钟扫描起点
│       (memory.rs 通过 use crate::util::CLK 访问)
│
└── trap.rs
    ├── CLK          — handle_irq 中记录中断抑制时间
    └── check_access — validate_access 的底层验证函数
```

**依赖方向图：**

```
         consts.rs
            │
            ▼
         util.rs ◄──── fs.rs (FLike, MountEntry)
         │    │
         │    └──► trap.rs (CLK, check_access)
         │
         └──► memory.rs (CLK)
```

---

## 十三、潜在的改进方向

1. **cfu/ctu 不真正拷贝数据**：当前实现只验证地址合法性，不实际执行数据拷贝。真实内核需要使用 `copy_from_user` / `copy_to_user` 安全地跨地址空间传输数据
2. **CLK 使用 Relaxed 内存序**：在多核环境下可能导致时钟值不一致。建议至少使用 `Acquire/Release` 语义
3. **check_access_rw 的 _span_check 未生效**：计算了页面数限制但未实际返回错误
4. **parse_ipv4_header 不验证校验和**：计算了头部校验和但没有验证结果是否为 0xFFFF，即没有检查数据包完整性
5. **compute_load_balance 不考虑迁移成本**：虽然计算了 `_migration_cost` 但没有在决策中使用它
6. **rehash_mount_cache 使用 BTreeMap 而非 HashMap**：BTreeMap 是 O(log n) 查找，不如 HashMap 的 O(1)
7. **缺少单元测试**：网络校验和、ELF 验证、负载均衡等函数缺少独立的单元测试覆盖
8. **dtk() 的非原子性问题**：CLK 和 CLK_ALL 的递增不是原子操作，可能导致中间状态被其他 CPU 观察到
