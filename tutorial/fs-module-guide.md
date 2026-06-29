# FS 模块阅读指南

> 文件路径: `kernel-refactored/src/fs.rs`
> 代码量: 1372 行 | 16 个核心结构体 | 依赖: `consts`, `sync`, `channel`, `util`

---

## 一、模块概述

`fs.rs` 是内核的 **文件系统子系统**，提供了从文件描述符到块设备 I/O 的完整文件系统栈。模块按功能可分为以下层次：

| 层次 | 结构体 | 用途 |
|---|---|---|
| 文件描述符 | `FdOpt`, `FdState`, `FHandle`, `FSeek` | 文件打开选项、状态、读写操作 |
| 管道 | `PipeDir`, `PipeBuf`, `PipeNode` | 基于事件总线的管道通信 |
| VFS 分发 | `FLike` | 统一 File/Pipe/Ep 三种文件类型的读写接口 |
| 伪文件系统 | `PseudoNode` | procfs 风格的只读伪文件 |
| Epoll | `EpData`, `EpEvent`, `EpCtlOp`, `EpInst` | 事件多路复用 |
| 终端 | `TrmIO`, `WinSz` | 终端属性与窗口大小 |
| 页缓存 | `PageCache`, `PageCacheEntry` | LRU 页面缓存管理 |
| 对象注册表 | `KObjRegistry`, `KObjEntry` | 内核对象追踪与 GC |
| 块缓存 | `CacheSlot`, `CacheChain`, `BlockCache` | 组相联块缓存 |
| 挂载表 | `MountTable`, `MountEntry` | 最长前缀匹配的挂载点解析 |
| I/O 队列 | `IoQueue`, `IoRequest` | SCAN 电梯调度器 |
| 磁盘 | `Disk` | 块设备（含故障注入和日志） |

**设计定位：** `fs.rs` 在内核中承担所有与"文件"和"存储"相关的职责——用户进程的文件读写、管道通信、epoll 事件通知、块缓存加速、挂载点路径解析，直到最终的磁盘 I/O 调度。它是 `kernel.rs` 中 `dispatch_syscall` 的主要后端。

---

## 二、FdOpt / FdState — 文件描述符选项与状态

### 2.1 FdOpt 结构体

```rust
#[derive(Clone, Copy)]
pub struct FdOpt {
    pub rd: bool,   // 可读标志
    pub wr: bool,   // 可写标志
    pub ap: bool,   // append 模式：每次写入自动追加到末尾
    pub nb: bool,   // non-blocking 模式：读写不阻塞
}
```

对应 Linux `open()` 的 flags：`O_RDONLY`/`O_WRONLY`/`O_RDWR`/`O_APPEND`/`O_NONBLOCK`。

### 2.2 FdState 结构体

```rust
struct FdState {
    off: u64,       // 当前文件偏移量（读写位置）
    opt: FdOpt,     // 文件打开选项
    flk: u8,        // 文件锁状态（保留字段）
}
```

`FdState` 被 `Arc<RwLock<>>` 包装，允许多个 `FHandle` 克隆共享同一个偏移量和选项——这就是 Linux 中 `dup()` 后两个 fd 共享偏移量的实现方式。

---

## 三、FHandle — 文件句柄

### 3.1 结构体定义

```rust
#[derive(Clone)]
pub struct FHandle {
    pub path: String,                     // 文件路径名
    pub data: Arc<Mutex<Vec<u8>>>,        // 文件数据（共享，clone 后指向同一份数据）
    desc: Arc<RwLock<FdState>>,           // 描述符状态（偏移量、选项，共享）
    pub pipe: bool,                       // 是否为管道文件
    pub cloexec: bool,                    // close-on-exec 标志
}
```

**核心设计：** `data` 和 `desc` 都用 `Arc` 包装，使得 `dup()` 产生的新 FHandle 与原句柄**共享数据和偏移量**，完全符合 POSIX 语义。

### 3.2 构造与复制

```rust
/// 创建空文件句柄
pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self

/// 用已有数据创建文件句柄（用于预加载文件内容）
pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self

/// 复制文件句柄（共享 data 和 desc，可指定新的 cloexec）
pub fn dup(&self, cloexec: bool) -> Self
```

### 3.3 读写方法

```rust
/// 从当前偏移量读取，自动推进偏移
pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str>

/// 从指定偏移量读取（不修改 desc 中的偏移）
pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str>

/// 写入（append 模式下自动追加到末尾）
pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str>

/// 从指定偏移量写入
pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str>
```

**读写流程：**
```
write() 调用
    │
    ├── append 模式？→ off = data.len()（追加到末尾）
    │                 → off = desc.off（当前位置）
    │
    ▼
write_at(off, buf)
    │
    ├── 检查 opt.wr 权限 → 无权限返回 "ebadf"
    ├── 如果 off + buf.len() > data.len() → resize 扩展
    └── copy_from_slice 写入数据
```

### 3.4 Seek 操作

```rust
pub enum FSeek {
    Start(u64),   // 从文件开头偏移 (SEEK_SET)
    End(i64),     // 从文件末尾偏移 (SEEK_END)
    Cur(i64),     // 从当前位置偏移 (SEEK_CUR)
}

pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str>
```

### 3.5 高级方法

```rust
/// 统一传输接口，支持读/写、带偏移/不带偏移
pub fn transfer(&self, dir: u8, offset: Option<usize>,
                buf_rd: Option<&mut [u8]>, buf_wr: Option<&[u8]>)
    -> Result<usize, &'static str>

/// splice 零拷贝传输：从 self 读取 count 字节写入 dst
pub fn splice_to(&self, dst: &FHandle, count: usize) -> Result<usize, &'static str>

/// fallocate 预分配空间
pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str>

/// 预读建议（当前为空实现）
pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str>
```

---

## 四、PipeNode — 管道

### 4.1 结构体定义

```rust
/// 管道方向枚举
pub enum PipeDir { Rd, Wr }

/// 管道内部缓冲区（共享于读写两端）
pub struct PipeBuf {
    pub buf: VecDeque<u8>,   // 双端队列存储管道数据
    pub bus: EvBus,          // 事件总线（通知可读/可写/关闭）
    pub ends: i32,           // 存活端数（初始为 2，每 drop 一个 PipeNode -1）
}

/// 管道节点（持有共享缓冲区的一端）
pub struct PipeNode {
    data: Arc<Mutex<PipeBuf>>,   // 共享缓冲区
    dir: PipeDir,                // 本端方向（读或写）
}
```

**事件总线机制：** `PipeBuf.bus` 使用 `EvBus`（来自 `sync.rs`），在以下时机触发事件：
- 写入数据时：设置 `EvFlag::READABLE`
- 读完数据时：清除 `EvFlag::READABLE`
- PipeNode drop 时：设置 `EvFlag::CLOSED`

### 4.2 管道对创建

```rust
/// 创建一对管道节点（读端 + 写端），共享同一个 PipeBuf
pub fn pair() -> (PipeNode, PipeNode) {
    let inner = PipeBuf { buf: VecDeque::new(), bus: EvBus::default(), ends: 2 };
    let d = Arc::new(Mutex::new(inner));
    (
        PipeNode { data: d.clone(), dir: PipeDir::Rd },
        PipeNode { data: d, dir: PipeDir::Wr },
    )
}
```

### 4.3 Drop 语义

```rust
impl Drop for PipeNode {
    fn drop(&mut self) {
        let mut d = self.data.lock().unwrap();
        d.ends -= 1;                    // 存活端数减一
        d.bus.set(EvFlag::CLOSED);      // 通知对端已关闭
    }
}
```

这确保了当写端被 drop 后，读端能检测到 `ends < 2` 并返回 EOF。

### 4.4 读写操作

```rust
/// 读端读取：空且两端都在则返回 "again"（EAGAIN），否则 pop_front
pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str>

/// 写端写入：push_back 并触发 READABLE 事件
pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str>
```

---

## 五、FLike — VFS 统一分发

### 5.1 枚举定义

```rust
pub enum FLike {
    File(FHandle),    // 普通文件
    Pipe(PipeNode),   // 管道
    Ep(EpInst),       // epoll 实例
}
```

**设计定位：** `FLike` 是内核中的"虚拟文件系统"层——每个文件描述符都存储为 `FLike`，读写时通过 `match` 分发到对应的底层实现。

### 5.2 统一接口

```rust
/// 复制文件描述符（共享底层数据）
pub fn dup(&self, cloexec: bool) -> FLike

/// 统一读取
pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str>

/// 统一写入
pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str>

/// ioctl 分发
pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str>

/// mmap 映射
pub fn mmap_fl(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str>

/// poll 状态查询：返回 (可读, 可写, 错误)
pub fn poll(&self) -> (bool, bool, bool)
```

### 5.3 poll 分发逻辑

| 类型 | 可读条件 | 可写条件 | 错误条件 |
|---|---|---|---|
| File | `opt.rd == true` | `opt.wr == true` | 路径和数据均空 |
| Pipe(Rd) | 有数据或写端已关 | - | - |
| Pipe(Wr) | - | 读端仍在 | 已关且有数据 |
| Ep | ready 队列非空 | - | - |

---

## 六、PseudoNode — 伪文件系统节点

```rust
pub struct PseudoNode {
    pub content: Vec<u8>,   // 文件内容（只读）
    pub ftype: u8,          // 文件类型标记
}
```

提供 `read_at()` 和 `metadata_sz()` 方法，用于实现 `/proc` 风格的伪文件（如 `/proc/self/status`）。`write_at()` 总是返回 `"nosup"` 错误。

---

## 七、Epoll 子系统

### 7.1 结构体定义

```rust
/// epoll 用户数据
pub struct EpData { pub ptr: u64 }

/// epoll 事件（events 位图 + 用户数据）
pub struct EpEvent {
    pub events: u32,    // 事件类型位掩码
    pub data: EpData,   // 用户自定义数据
}

/// epoll 控制操作常量
pub struct EpCtlOp;
impl EpCtlOp {
    pub const ADD: i32 = 1;   // EPOLL_CTL_ADD
    pub const DEL: i32 = 2;   // EPOLL_CTL_DEL
    pub const MOD: i32 = 3;   // EPOLL_CTL_MOD
}

/// epoll 实例
pub struct EpInst {
    pub events: BTreeMap<usize, EpEvent>,    // fd -> 注册的事件
    pub ready: Arc<Mutex<BTreeSet<usize>>>,  // 就绪的 fd 集合
    pub new_ctl: Arc<Mutex<BTreeSet<usize>>>,// 新注册的控制 fd
}
```

### 7.2 EpEvent 事件常量

```rust
impl EpEvent {
    pub const IN: u32 = 0x001;       // EPOLLIN - 可读
    pub const OUT: u32 = 0x004;      // EPOLLOUT - 可写
    pub const ERR: u32 = 0x008;      // EPOLLERR - 错误
    pub const HUP: u32 = 0x010;      // EPOLLHUP - 挂起
    pub const ET: u32 = 1 << 31;     // EPOLLET - 边缘触发
    pub const ONESHOT: u32 = 1 << 30;// EPOLLONESHOT - 一次性
    // ...
}
```

### 7.3 控制操作

```rust
pub fn control(&mut self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
    match op {
        1 => { /* ADD: 插入事件映射并记录到 new_ctl */ }
        3 => { /* MOD: 更新已有映射 */ }
        2 => { /* DEL: 移除事件映射 */ }
        _ => Err("eperm"),
    }
}
```

---

## 八、TrmIO / WinSz — 终端 I/O

```rust
/// termios 结构体（终端属性）
pub struct TrmIO {
    pub iflag: u32,    // 输入模式标志
    pub oflag: u32,    // 输出模式标志
    pub cflag: u32,    // 控制模式标志
    pub lflag: u32,    // 本地模式标志
    pub line: u8,      // 行规程
    pub cc: [u8; 32],  // 控制字符数组（INTR=3, EOF=4, ERASE=127 等）
    pub ispeed: u32,   // 输入波特率
    pub ospeed: u32,   // 输出波特率
}

/// 窗口大小
pub struct WinSz {
    pub row: u16,      // 行数
    pub col: u16,      // 列数
    pub xpx: u16,      // 水平像素
    pub ypx: u16,      // 垂直像素
}
```

这些结构体在 `SYS_IOCTL` 的 `TCGETS`/`TCSETS`/`TIOCGWINSZ` 等命令中使用。

---

## 九、PageCache — LRU 页缓存

### 9.1 结构体定义

```rust
/// 页缓存条目
pub struct PageCacheEntry {
    pub page_id: usize,       // 页标识
    pub data: Vec<u8>,        // 页数据
    pub dirty: bool,          // 脏页标志（需要写回磁盘）
    pub access_tick: usize,   // 最后访问时间戳
    pub pin_count: usize,     // 钉住计数（>0 时不可驱逐）
}

/// 页缓存管理器
pub struct PageCache {
    pub entries: HashMap<usize, PageCacheEntry>,  // 页表
    pub capacity: usize,                          // 最大页数
    pub hits: AtomicUsize,                        // 命中统计
    pub misses: AtomicUsize,                      // 未命中统计
    pub evictions: AtomicUsize,                   // 驱逐统计
    pub lru_order: VecDeque<usize>,               // LRU 顺序（队尾最新）
}
```

### 9.2 核心方法

```rust
/// 查找页：命中则移到 LRU 队尾，更新访问时间
pub fn lookup(&mut self, page_id: usize) -> Option<&[u8]>

/// 插入页：满时先驱逐 LRU 页
pub fn insert(&mut self, page_id: usize, data: Vec<u8>)

/// LRU 驱逐：从队头找第一个未钉住的页驱逐
pub fn evict_lru(&mut self) -> bool

/// 钉住/解除钉住
pub fn pin(&mut self, page_id: usize) -> bool
pub fn unpin(&mut self, page_id: usize) -> bool

/// 写回所有脏页
pub fn writeback_all(&mut self) -> usize

/// 批量刷写指定范围的脏页
pub fn flush_range(&mut self, start: usize, end: usize) -> usize
```

**LRU 驱逐流程：**
```
evict_lru()
    │
    ▼
从 lru_order 队头开始扫描
    │
    ├── 找到 pin_count == 0 的页 → 移除 → 返回 true
    ├── 跳过 pin_count > 0 的页（被钉住的不可驱逐）
    └── 所有页都被钉住 → 返回 false
```

---

## 十、KObjRegistry — 内核对象注册表

### 10.1 结构体定义

```rust
/// 内核对象条目
pub struct KObjEntry {
    pub obj_id: usize,          // 对象 ID（全局唯一递增）
    pub type_tag: u32,          // 类型标签（区分文件/管道/信号量等）
    pub owner_pid: usize,       // 所属进程 PID
    pub created_tick: usize,    // 创建时间戳
    pub ref_count: usize,       // 引用计数
    pub parent_id: Option<usize>, // 父对象 ID（用于构建对象树）
}

/// 对象注册表
pub struct KObjRegistry {
    pub objects: Mutex<BTreeMap<usize, KObjEntry>>,  // ID -> 条目
    pub seq: AtomicUsize,                            // ID 序列号
    pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>,// 类型 -> ID 列表
}
```

### 10.2 核心方法

```rust
/// 注册新对象，返回分配的 ID
pub fn register(&self, type_tag: u32, owner_pid: usize) -> usize

/// 注册子对象（带 parent_id）
pub fn register_child(&self, type_tag: u32, owner_pid: usize, parent: usize) -> usize

/// 注销对象
pub fn unregister(&self, id: usize) -> bool

/// 按类型查找所有对象 ID
pub fn find_by_type(&self, tag: u32) -> Vec<usize>

/// 导出对象关系图（parent, child）边列表
pub fn dump_graph(&self) -> Vec<(usize, usize)>

/// GC 回收引用计数为 0 的对象
pub fn gc_sweep(&self) -> usize

/// 引用计数增减
pub fn ref_up(&self, id: usize) -> bool
pub fn ref_down(&self, id: usize) -> bool
```

---

## 十一、BlockCache — 组相联块缓存

### 11.1 结构体定义

```rust
/// 缓存槽（单个数据块）
pub struct CacheSlot {
    pub id: usize,           // 块 ID
    pub payload: Vec<u8>,    // 块数据（512 字节）
    pub modified: bool,      // 脏块标志
}

/// 缓存链（一组槽位，用自旋锁保护）
pub struct CacheChain {
    pub lk: Spin,                // 自旋锁
    pub items: Mutex<Vec<CacheSlot>>, // 槽位列表
}

/// 块缓存（多个链组成的组相联结构）
pub struct BlockCache {
    pub chains: Vec<CacheChain>,  // 缓存链数组
    pub width: usize,             // 链数量（组数）
    pub ops: AtomicUsize,         // 操作计数
}
```

### 11.2 缓存查找流程

```rust
pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>>
```

```
fetch(k) 调用
    │
    ▼
计算链索引 ci = mix(k) % width
    │
    ▼
获取 chain[ci] 的自旋锁
    │
    ├── 在 items 中找到 id == k 的槽 → 克隆数据 → 释放锁 → 返回
    │
    └── 未命中
         │
         ▼
    模拟磁盘延迟 thread::sleep(lat)
         │
         ▼
    生成 512 字节块数据
    插入新 CacheSlot
    释放锁 → 返回数据
```

### 11.3 其他方法

```rust
/// 同步所有脏块（需要获取全局内核锁 GKL）
pub fn sync_all(&self, id: usize)

/// 使指定块失效
pub fn invalidate(&self, k: usize)

/// 统计总条目数
pub fn total_entries(&self) -> usize

/// 统计脏块数
pub fn dirty_count(&self) -> usize

/// 驱逐冷数据（超过 max_age 的脏块）
pub fn evict_cold(&self, max_age: usize) -> usize
```

---

## 十二、MountTable — 挂载表

### 12.1 结构体定义

```rust
/// 挂载条目
pub struct MountEntry {
    pub prefix: String,   // 挂载点前缀（如 "/dev"）
    pub target: String,   // 目标设备（如 "sda1"）
}

/// 挂载表
pub struct MountTable {
    pub entries: RwLock<Vec<MountEntry>>,  // 按前缀长度降序排列
}
```

### 12.2 核心方法

```rust
/// 绑定挂载点（自动按前缀长度排序，最长的在前）
pub fn bind(&self, pfx: &str, tgt: &str)

/// 路径解析（最长前缀匹配 + 递归解析）
pub fn resolve(&self, path: &str) -> Result<String, &'static str>

/// 卸载指定前缀的所有挂载点
pub fn unmount(&self, pfx: &str) -> bool

/// 查找匹配路径的最佳挂载点
pub fn find_mount(&self, path: &str) -> Option<MountEntry>
```

**resolve 最长前缀匹配算法：**
```
resolve("/dev/sda1/data")
    │
    ▼
扫描所有挂载条目，找最长匹配前缀
    假设 "/dev" -> "sda1" 匹配
    │
    ▼
rest = "/sda1/data"
递归 resolve(rest)
    │
    ▼
拼接结果: "sda1:" + sub_result
```

---

## 十三、IoQueue — SCAN 电梯调度器

### 13.1 结构体定义

```rust
/// I/O 请求
pub struct IoRequest {
    pub block: usize,           // 目标块号
    pub write: bool,            // 是否为写操作
    pub priority: u8,           // 优先级
    pub submitted_tick: usize,  // 提交时间戳
}

/// I/O 调度队列
pub struct IoQueue {
    pub pending: Mutex<VecDeque<IoRequest>>,  // 待处理请求队列
    pub head_pos: AtomicUsize,                // 磁头当前位置
    pub direction_up: AtomicBool,             // 扫描方向（true=向高块号）
    pub dispatched: AtomicUsize,              // 已调度计数
    pub merged: AtomicUsize,                  // 已合并计数
}
```

### 13.2 SCAN 调度算法

```rust
pub fn dispatch(&self) -> Option<(usize, bool)>
```

SCAN（电梯）算法的核心思想：磁头沿一个方向移动，依次服务沿途的请求，到达端点后反转方向。

```
磁头位置 head=50, 方向向上
请求队列: [30, 55, 70, 20, 80]
    │
    ▼
选择距离 head 最近且方向一致的请求
    → 55（距离 5）
    │
    ▼
移动磁头到 55，检查是否需要反转方向
    → 如果所有剩余请求都在反方向 → 反转
```

### 13.3 请求合并

```rust
/// 合并相邻块的请求（block+1 且同方向）
pub fn merge_adjacent(&self) -> usize
```

当队列深度超过 `IOQUEUE_DEPTH` 时自动触发合并。

---

## 十四、Disk — 块设备

### 14.1 结构体定义

```rust
pub struct Disk {
    pub errs: AtomicUsize,                    // 剩余错误次数（0=正常, MAX=永久故障）
    pub ops: AtomicUsize,                     // 操作计数
    pub label: String,                        // 设备标签
    pub journal: Option<Arc<Disk>>,           // 日志设备（可选）
}
```

### 14.2 故障注入模型

```rust
/// 正常磁盘
pub fn new(s: &str) -> Self           // errs = 0

/// 带故障注入的磁盘
pub fn failing(s: &str, n: usize) -> Self  // errs = n
```

- `errs == 0`：正常操作
- `errs == usize::MAX`：永久故障
- `errs > 0 且 < MAX`：前 n 次操作失败，之后恢复正常

### 14.3 读写方法

```rust
/// 读块（循环重试直到成功或达到限制）
pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str>

/// 带重试次数限制的读块
pub fn read_block_n(&self, blk: usize, out: &mut [u8], lim: usize) -> Result<usize, &'static str>

/// 写块
pub fn write_block(&self, blk: usize, data: &[u8]) -> Result<(), &'static str>

/// 刷新（同步日志设备）
pub fn flush(&self) -> Result<(), &'static str>
```

**日志设备协作：**
```
read_block() 失败时
    │
    ├── 有 journal → 在日志设备上读 5 字节（尝试恢复）
    └── 无 journal → 退避重试
```

---

## 十五、使用场景

### 15.1 文件读写

```rust
// 创建文件并写入
let fh = FHandle::new("/test.txt", FdOpt { rd: true, wr: true, .. }, false, false);
fh.write(b"hello world");
// seek 到开头并读取
fh.seek(FSeek::Start(0));
let mut buf = [0u8; 11];
fh.read(&mut buf);
```

### 15.2 管道通信

```rust
let (rd, wr) = PipeNode::pair();
wr.write_at(b"data");
let mut buf = [0u8; 4];
rd.read_at(&mut buf);  // 读到 "data"
drop(wr);              // 关闭写端
rd.read_at(&mut buf);  // 检测到 ends < 2，返回 EOF
```

### 15.3 块缓存加速

```rust
let cache = BlockCache::new(16);
// 首次访问：缓存未命中，模拟磁盘延迟
let data = cache.fetch(42, Duration::from_millis(1));
// 再次访问：缓存命中，直接返回
let data2 = cache.fetch(42, Duration::from_millis(1));
```

### 15.4 测试引用

- `group_03` - 文件读写、seek、append 模式
- `group_04` - 管道读写、关闭检测、poll
- `group_05` - 块缓存命中/未命中
- `group_06` - 挂载表解析
- `group_07` - epoll 事件注册
- `group_08` - I/O 队列调度
- `group_09` - 磁盘故障注入
- `group_10` - 页缓存 LRU

---

## 十六、跨模块连接

```
fs.rs
├── channel.rs: CircBuf（管道底层缓冲，实际 PipeNode 用 VecDeque）
├── sync.rs: EvBus/EvFlag（管道事件通知）、Spin（块缓存锁）、GKL（全局锁）
├── process.rs: Task 的 files 字段存储 FLike
├── kernel.rs: dispatch_syscall 中调用 FHandle/PipeNode/EpInst 的方法
├── ipc.rs: 信号量/共享内存与文件系统的协调
└── consts.rs: PAGE_SZ, O_NONBLOCK, N_CHAINS 等常量
```

---

## 十七、潜在的改进方向

1. **PipeNode 使用 VecDeque 而非 CircBuf**：尽管导入了 `CircBuf`，`PipeBuf` 实际使用 `VecDeque`，失去了固定容量环形缓冲的优势
2. **BlockCache 的哈希冲突处理**：当前使用简单取模，相同哈希的块会在同一链中线性扫描，链过长时性能退化
3. **PageCache 的 LRU 实现**：使用 `VecDeque::retain()` 移动到队尾是 O(n) 操作，大规模缓存下应考虑使用双向链表
4. **MountTable 的 resolve 递归**：极端情况下可能导致栈溢出，可改为迭代实现
5. **Disk.read_block 的无限循环**：如果没有 journal 且 errs 不为 0 也不为 MAX，`read_block` 可能死循环
6. **FLike 的 read/write 与 FHandle 的 read/write 代码重复**：FLike 内联了完整的读写逻辑，没有复用 FHandle 的方法
