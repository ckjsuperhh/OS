#![allow(unused, dead_code, non_upper_case_globals, non_camel_case_types, unused_assignments, unused_mut)]

use std::collections::{BTreeMap, BTreeSet, VecDeque, HashMap, LinkedList};
// 前几个是线程安全的数值类型，Ordering定义原子操作的内存可见性规则（控制多线程间数据同步的强度）
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering}; 
use std::sync::{Arc, Mutex, RwLock, Weak, Condvar};
use std::thread;
use std::time::Duration;
use std::fmt;
use std::ops::{Deref, DerefMut, Index};
use std::any::Any;
use std::cmp::{min, max, Ordering as CmpOrd};

/// 内存页标准大小 4KB
pub const PAGE_SZ: usize = 4096;
/// 系统最大支持进程数量
pub const N_PROC: usize = 256;
/// 系统可用物理页帧总数量
pub const N_FRAMES: usize = 65536;
/// 64位架构内核虚拟空间起始基地址
pub const KERN_BASE: usize = 0xFFFF_FFFF_8000_0000;
/// 物理内存线性映射虚拟地址偏移量
pub const PHYS_OFF: usize = 0xFFFF_FFFF_0000_0000;
/// 开发板物理内存起始偏移地址(RISC-V通用)
pub const MEM_OFF: usize = 0x8000_0000;
/// 内核动态堆内存总大小
pub const KHEAP_SZ: usize = 0x800000;
/// 哈希表/就绪队列链表分链数量
pub const N_CHAINS: usize = 64;
/// 内核环形缓冲区单缓冲区最大容量
pub const RBUF_CAP: usize = 256;
/// 进程上下文保存的通用寄存器个数
pub const N_REGS: usize = 16;
/// 文件系统最大嵌套挂载深度
pub const MNT_DEPTH: usize = 8;
/// 系统支持最大CPU核心数(多核调度用)
pub const MAX_CPU: usize = 8;
/// 单个内核栈空间大小
pub const KSTK_SZ: usize = 0x4000;
/// 用户进程栈在虚拟地址空间的起始偏移
pub const USR_STK_OFF: usize = 0x7FFF_0000;
/// 单个用户进程栈空间大小
pub const USR_STK_SZ: usize = 0x10000;
/// 系统时钟节拍单位：微秒，1节拍=1ms
pub const USEC_TICK: usize = 1000;
/// 符号链接/进程嵌套追踪最大递归层数
pub const FOLLOW_LIM: usize = 3;

// fcntl 命令：复制文件描述符（返回最小可用的 fd）
pub const F_DUPFD: usize = 0;

// fcntl 命令：获取文件描述符标志（主要获取 FD_CLOEXEC 标志）
pub const F_GETFD: usize = 1;
// fcntl 命令：设置文件描述符标志（主要设置 FD_CLOEXEC 标志）
pub const F_SETFD: usize = 2;
// fcntl 命令：获取文件状态标志（如 O_NONBLOCK / O_APPEND）
pub const F_GETFL: usize = 3;
// fcntl 命令：设置文件状态标志
pub const F_SETFL: usize = 4;
// fcntl 命令：获取文件锁信息
pub const F_GETLK: usize = 5;
// fcntl 命令：设置文件锁（非阻塞，获取不到立即返回）
pub const F_SETLK: usize = 6;
// fcntl 命令：设置文件锁（阻塞，获取不到会等待）
pub const F_SETLKW: usize = 7;
// 文件描述符标志：执行 exec 时自动关闭该 fd（close-on-exec）
pub const FD_CLOEXEC: usize = 1;
// fcntl 命令：复制文件描述符，并自动带上 FD_CLOEXEC 标志
pub const F_DUPFD_CLOEXEC: usize = 1030;
// 文件状态标志：非阻塞模式（read/write 不阻塞）
pub const O_NONBLOCK: usize = 0o4000;
// 文件状态标志：追加模式（写入时永远从文件末尾开始）
pub const O_APPEND: usize = 0o2000;
// 文件状态标志：打开文件时就设置 CLOEXEC（执行 exec 自动关闭）
pub const O_CLOEXEC: usize = 0o2000000;
// 打开目录/文件标志：不跟随符号链接（遇到软链接直接操作链接本身）
pub const AT_NOFOLLOW: usize = 0x100;

// 获取终端终端属性（termios 结构体）
pub const TCGETS: usize = 0x5401;

// 设置终端终端属性（termios 结构体）
pub const TCSETS: usize = 0x5402;

// 获取终端前台进程组 ID
pub const TIOCGPGRP: usize = 0x540F;
// 设置终端前台进程组 ID
pub const TIOCSPGRP: usize = 0x5410;
// 获取终端窗口大小（行、列）
pub const TIOCGWINSZ: usize = 0x5413;
// 清除文件描述符的 FD_CLOEXEC 标志
pub const FIONCLEX: usize = 0x5450;
// 设置文件描述符的 FD_CLOEXEC 标志
pub const FIOCLEX: usize = 0x5451;
// 设置文件描述符为非阻塞模式
pub const FIONBIO: usize = 0x5421;

/// 程序头表(Program Headers)在内存中的地址
pub const AT_PHDR: u8 = 3;

/// 程序头表中每个条目(Program Header Entry)的大小
pub const AT_PHENT: u8 = 4;
/// 程序头表的总条目数量
pub const AT_PHNUM: u8 = 5;
/// 系统内存页大小
pub const AT_PAGESZ: u8 = 6;
/// 程序解释器(动态链接器/ld.so)在内存中的基地址
pub const AT_BASE: u8 = 7;
/// 程序入口点地址（程序开始执行的地址）
pub const AT_ENTRY: u8 = 9;

// 启用信号字符：INTR/QUIT/SUSP 会产生信号
pub const LM_ISIG: u32 = 0o000001;
// 规范模式（行缓冲，等待回车才读取）
pub const LM_ICANON: u32 = 0o000002;
// 回显输入字符（输入什么显示什么）
pub const LM_ECHO: u32 = 0o000010;
// 回显擦除字符（退格时删除前一个字符）
pub const LM_ECHOE: u32 = 0o000020;
// 回显删除行字符（KILL 清空整行）
pub const LM_ECHOK: u32 = 0o000040;
// 回显换行符
pub const LM_ECHONL: u32 = 0o000100;
// 禁用中断/退出时的缓冲区刷新
pub const LM_NOFLSH: u32 = 0o000200;
// 后台进程写终端时发送 SIGTTOU 信号停止
pub const LM_TOSTOP: u32 = 0o000400;
// 启用扩展输入字符处理
pub const LM_IEXTEN: u32 = 0o100000;
// 规范模式下大写转小写（过时）
pub const LM_XCASE: u32 = 0o000004;
// 回显控制字符为 ^X 形式
pub const LM_ECHOCTL: u32 = 0o001000;
// 硬拷贝终端显示擦除（过时）
pub const LM_ECHOPRT: u32 = 0o002000;
// 按行删除时清空整行并回显
pub const LM_ECHOKE: u32 = 0o004000;
// 输出刷新挂起（过时）
pub const LM_FLUSHO: u32 = 0o010000;
// 重绘待输入的字符
pub const LM_PENDIN: u32 = 0o040000;
// 外部处理器处理终端（高级）
pub const LM_EXTPROC: u32 = 0o200000;

pub const VM_READ: u32 = 0x01;        // 可读
pub const VM_WRITE: u32 = 0x02;       // 可写
pub const VM_EXEC: u32 = 0x04;        // 可执行
pub const VM_SHARED: u32 = 0x08;      // 进程间共享
pub const VM_GROWSDOWN: u32 = 0x10;   // 栈内存：向下增长
pub const VM_DONTCOPY: u32 = 0x20;   // fork 时不拷贝该内存
pub const VM_HUGETLB: u32 = 0x40;     // 使用大页内存
pub const VM_PFNMAP: u32 = 0x80;     // 直接映射物理页帧(PFN)

/// 修改文件所有者权限
pub const CAP_CHOWN: u32 = 0;
/// 向任意进程发送信号（kill/信号控制）
pub const CAP_KILL: u32 = 5;
/// 设置任意用户ID（UID）
pub const CAP_SETUID: u32 = 7;
/// 设置任意组ID（GID）
pub const CAP_SETGID: u32 = 6;
/// 绑定特权端口（<1024 的端口）
pub const CAP_NET_BIND: u32 = 10;
/// 使用原始套接字（抓包、构造IP包）
pub const CAP_NET_RAW: u32 = 13;
/// 系统管理全能权限（等同于部分 root 能力）
pub const CAP_SYS_ADMIN: u32 = 21;
/// 追踪任意进程（调试、ptrace）
pub const CAP_SYS_PTRACE: u32 = 19;
/// 可继承能力掩码（限制子进程能继承的权限范围）
pub const INHERITABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF;

// DMA 内存区域：用于直接内存访问，物理地址连续、低地址内存
pub const ZONE_DMA: usize = 0;
// 常规内存区域：内核可直接映射访问的标准物理内存
pub const ZONE_NORMAL: usize = 1;
// 高端内存区域：超过内核直接映射范围的高地址内存（如32位系统>896MB内存）
pub const ZONE_HIGH: usize = 2;
// 内存区域总数量：固定3种区域
pub const N_ZONES: usize = 3;

// 进程静态优先级范围（Linux nice 值）
pub const PRIO_MIN: i32 = -20;      // 最高优先级
pub const PRIO_MAX: i32 = 19;       // 最低优先级
pub const PRIO_DEFAULT: i32 = 0;    // 默认优先级

// Linux 调度策略
pub const SCHED_NORMAL: u8 = 0;     // 普通分时调度（CFS）
pub const SCHED_FIFO: u8 = 1;       // 先进先出实时调度
pub const SCHED_RR: u8 = 2;         // 轮询实时调度
pub const SCHED_BATCH: u8 = 3;      // 批处理调度

/// Slab 分配器最小对象大小（字节）
pub const SLAB_OBJ_MIN: usize = 8;
/// Slab 分配器最大对象大小（字节）
pub const SLAB_OBJ_MAX: usize = 2048;
/// Slab 对象内存对齐要求（字节）
pub const SLAB_ALIGN: usize = 8;

// 系统支持的信号总数（Linux 标准：64个信号）
pub const NSIG: u32 = 64;
// 信号默认处理行为（执行系统默认动作）
pub const SIG_DFL: usize = 0;
// 信号忽略处理行为（直接丢弃该信号）
pub const SIG_IGN: usize = 1;
// 强制杀死进程信号（不可被捕获、忽略、阻塞）
pub const SIGKILL: u32 = 9;
// 暂停进程信号（不可被捕获、忽略、阻塞）
pub const SIGSTOP: u32 = 19;
// 子进程状态改变时发送给父进程（退出/暂停/恢复）
pub const SIGCHLD: u32 = 17;
// 用户自定义信号1（程序可自由定义用途）
pub const SIGUSR1: u32 = 10;
// 用户自定义信号2（程序可自由定义用途）
pub const SIGUSR2: u32 = 12;
// 定时器超时信号（alarm() 触发）
pub const SIGALRM: u32 = 14;

// 时间轮的槽位总数（时间轮大小）
pub const TIMER_WHEEL_SIZE: usize = 256;
// 定时器每秒触发的时钟节拍数
pub const TIMER_TICK_HZ: usize = 100;

/// 套接字类型常量（定义数据传输方式）
/// 流式套接字（TCP，可靠、面向连接、字节流传输）
pub const SOCK_STREAM: u32 = 1;
/// 数据报套接字（UDP，不可靠、无连接、数据包传输）
pub const SOCK_DGRAM: u32 = 2;
/// 原始套接字（直接访问底层协议，用于网络抓包/自定义协议）
pub const SOCK_RAW: u32 = 3;

/// 地址族常量（定义网络协议簇）
/// IPv4 地址族（最常用的互联网协议）
pub const AF_INET: u32 = 2;
/// IPv6 地址族（下一代互联网协议）
pub const AF_INET6: u32 = 10;
/// Unix 域套接字（本地进程间通信，不经过网络协议栈）
pub const AF_UNIX: u32 = 1;

// === 文件 I/O 基础操作 ===
/// 从文件描述符读取数据
pub const SYS_READ: usize = 0;
/// 向文件描述符写入数据
pub const SYS_WRITE: usize = 1;
/// 打开文件
pub const SYS_OPEN: usize = 2;
/// 关闭文件描述符
pub const SYS_CLOSE: usize = 3;
// === 文件状态/元信息 ===
/// 获取文件状态（路径）
pub const SYS_STAT: usize = 4;
/// 获取文件状态（文件描述符）
pub const SYS_FSTAT: usize = 5;
// === 内存管理 ===
/// 映射文件/设备到虚拟内存
pub const SYS_MMAP: usize = 9;
/// 取消内存映射
pub const SYS_MUNMAP: usize = 11;
/// 调整程序中断（堆内存扩展）
pub const SYS_BRK: usize = 12;
// === 设备/IO控制 ===
/// 设备控制
pub const SYS_IOCTL: usize = 16;
// === 进程间通信 ===
/// 创建管道
pub const SYS_PIPE: usize = 22;
// === 文件描述符复制 ===
/// 复制文件描述符
pub const SYS_DUP: usize = 32;
/// 定向复制文件描述符
pub const SYS_DUP2: usize = 33;
// === 信号处理 ===
/// 设置信号处理函数
pub const SYS_SIGACTION: usize = 13;
/// 操作信号掩码
pub const SYS_SIGPROCMASK: usize = 14;
// === 进程管理核心 ===
/// 创建子进程
pub const SYS_FORK: usize = 57;
/// 执行新程序
pub const SYS_EXEC: usize = 59;
/// 终止进程
pub const SYS_EXIT: usize = 60;
/// 等待子进程
pub const SYS_WAIT4: usize = 61;
/// 向进程发送信号
pub const SYS_KILL: usize = 62;
// === 文件描述符控制 ===
/// 文件描述符控制
pub const SYS_FCNTL: usize = 72;
// === 进程标识 ===
/// 获取当前进程ID
pub const SYS_GETPID: usize = 39;
/// 获取父进程ID
pub const SYS_GETPPID: usize = 110;
// === 进程组/会话 ===
/// 设置进程组ID
pub const SYS_SETPGID: usize = 109;
/// 获取进程组ID
pub const SYS_GETPGID: usize = 121;
/// 创建新会话
pub const SYS_SETSID: usize = 112;
// === 高性能IO（epoll） ===
/// 创建epoll实例
pub const SYS_EPOLL_CREATE: usize = 213;
/// 控制epoll监听事件
pub const SYS_EPOLL_CTL: usize = 233;
/// 等待epoll事件
pub const SYS_EPOLL_WAIT: usize = 232;
// === 时间 ===
/// 获取时钟时间
pub const SYS_CLOCK_GETTIME: usize = 228;
// === 线程/同步 ===
/// 快速用户空间互斥锁（线程同步核心）
pub const SYS_FUTEX: usize = 202;

/// IO 队列深度
pub const IOQUEUE_DEPTH: usize = 128;

// 虚存区域
pub struct VmRegion {
    pub base: usize,       // 虚拟内存起始地址
    pub len: usize,       // 这段内存的长度（字节数）
    pub flags: u32,       // 权限/属性：读、写、执行、共享、私有等
    pub offset: usize,    // 若映射文件/设备，文件内的偏移量
    pub tag: u16,         // 内存类型标记（内核内部分类用）
    pub ref_count: AtomicUsize, // 引用计数（多线程安全）
}

// 进程权限能力集
pub struct CapSet {
    pub bits: u64,       // 允许拥有的能力全集
    pub effective: u64,  // 当前正在生效的能力
    pub ambient: u64,    // 可继承给子进程的能力
}

// 信号处理动作
pub struct SigAction {
    pub handler: usize,  // 信号处理函数的指针（地址）
    pub flags: u32,      // 处理方式标志：重启系统调用、忽略等
    pub mask: u64,       // 处理时要临时屏蔽的其他信号
}

// 信号集合状态
pub struct SigSet {
    pub pending: u64,    // 已经发来、但还没处理的信号
    pub blocked: u64,    // 暂时屏蔽、不响应的信号
    pub actions: Vec<SigAction>, // 每个信号对应的处理动作
}

// 内核定时器
pub struct TimerEntry {
    pub deadline: usize,   // 到期时间戳
    pub interval: usize,   // 重复间隔（周期性定时器用）
    pub callback_id: usize,// 要执行的回调函数 ID
    pub active: bool,      // 定时器是否启用
    pub repeat: bool,      // 是否循环触发
}

// 内核锁(包含自旋锁的功能)
pub struct KernLock {
    flag: AtomicBool,      // 锁是否被占用
    holder: AtomicUsize,   // 谁持有这把锁（CPU/线程 ID）
    depth: AtomicUsize,    // 可重入次数（同一线程多次加锁）
}
impl KernLock {
    pub const fn new() -> Self {
        Self { flag: AtomicBool::new(false), holder: AtomicUsize::new(0), depth: AtomicUsize::new(0) }
    }
    // spinlock
    pub fn enter(&self, id: usize) {
        if self.holder.load(Ordering::Relaxed) == id && id != 0 {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        while self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
    }
    // release the lock
    pub fn leave(&self) {
        let d = self.depth.load(Ordering::Relaxed);
        let h = self.holder.load(Ordering::Relaxed);
        let _was_nested = d > 1;
        self.holder.store(0, Ordering::Relaxed);
        self.depth.store(0, Ordering::Relaxed);
        self.flag.store(false, Ordering::Release);
    }
    // check whether be held
    pub fn held(&self) -> bool { self.flag.load(Ordering::Relaxed) }
    // query the owner
    pub fn owner(&self) -> usize { self.holder.load(Ordering::Relaxed) }
    // query the depth
    pub fn level(&self) -> usize { self.depth.load(Ordering::Relaxed) }
    // try to get the lock(fail and return false instead of looping)
    pub fn try_enter(&self, id: usize) -> bool {
        if self.holder.load(Ordering::Relaxed) == id && id != 0 {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            self.holder.store(id, Ordering::Relaxed);
            self.depth.store(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}
unsafe impl Send for KernLock {}
unsafe impl Sync for KernLock {}
pub static GKL: KernLock = KernLock::new();

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

/// 基础环形缓冲区（字节流专用）
pub struct CircBuf {
    /// 底层存储字节数组
    pub data: Vec<u8>,
    /// 读索引位置
    pub rd: usize,
    /// 写索引位置
    pub wr: usize,
    /// 缓冲区总容量
    pub cap: usize,
    /// 当前已存储有效数据长度
    pub n: usize,
}
// 自旋锁
pub struct Spin { v: AtomicBool }
impl Spin {
    pub const fn new() -> Self { Self { v: AtomicBool::new(false) } }
    pub fn acquire(&self) {
        while self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
    }
    pub fn try_acquire(&self) -> bool {
        self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }
    pub fn release(&self) { self.v.store(false, Ordering::Release); }
    pub fn is_held(&self) -> bool { self.v.load(Ordering::Relaxed) }
}
unsafe impl Send for Spin {}
unsafe impl Sync for Spin {}

pub struct FlgGuard(usize);
impl FlgGuard { pub fn enter() -> Self { Self(0) } }
impl Drop for FlgGuard { fn drop(&mut self) {} }

// 事件位编码，用u32位编码
pub struct EvFlag;
impl EvFlag {
    pub const READABLE: u32 = 1 << 0;    // 可读
    pub const WRITABLE: u32 = 1 << 1;    // 可写
    pub const ERROR: u32 = 1 << 2;       // 错误
    pub const CLOSED: u32 = 1 << 3;      // 关闭
    pub const PROC_QUIT: u32 = 1 << 10;  // 进程退出
    pub const CHILD_QUIT: u32 = 1 << 11; // 子进程退出
    pub const RECV_SIG: u32 = 1 << 12;   // 收到信号
    pub const SEM_RM: u32 = 1 << 20;     // 信号量移除
    pub const SEM_ACQ: u32 = 1 << 21;    // 信号量获取
}

pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

#[derive(Default)]
pub struct EvBus {
    pub ev: u32,
    pub cbs: Vec<Box<dyn Fn(u32) -> bool + Send>>,
}
impl EvBus {
    pub fn make() -> Arc<Mutex<Self>> { Arc::new(Mutex::new(Self::default())) }
    pub fn set(&mut self, s: u32) { self.change(0, s); }
    pub fn clear(&mut self, s: u32) { self.change(s, 0); }
    pub fn change(&mut self, rst: u32, s: u32) {
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;
        if self.ev != orig { self.cbs.retain(|f| !f(self.ev)); }
    }
    pub fn sub(&mut self, cb: Box<dyn Fn(u32) -> bool + Send>) { self.cbs.push(cb); }
    pub fn cb_len(&self) -> usize { self.cbs.len() }
}

pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    loop {
        { let g = bus.lock().unwrap(); if (g.ev & mask) != 0 { return g.ev; } }
        thread::yield_now();
    }
}

pub struct RegEp {
    pub task_id: usize,
    pub epfd: usize,
    pub fd: usize,
}

pub struct SlabEntry {
    pub data: Vec<u8>,
    pub obj_size: usize,
    pub capacity: usize,
    pub free_list: VecDeque<usize>,
    pub allocated: usize,
    pub tag: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Closed,
    Listen,
    SynSent,
    SynRecvd,
    Established,
    FinWait1,
    FinWait2,
    TimeWait,
    CloseWait,
    LastAck,
    Closing,
}

pub struct SyncQueue {
    q: Mutex<VecDeque<thread::Thread>>,
    eq: Mutex<VecDeque<RegEp>>,
}
impl SyncQueue {
    pub fn new() -> Self { Self { q: Mutex::new(VecDeque::new()), eq: Mutex::new(VecDeque::new()) } }
    pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        let d = g.lock().unwrap();
        let satisfied = pred(&d);
        drop(d);
        if satisfied { return true; }
        let th = thread::current();
        let mut wq = self.q.lock().unwrap();
        let _pos = wq.len();
        wq.push_back(th);
        let n = wq.len();
        drop(wq);
        if n > 256 { let _trim = n >> 3; }
        thread::park();
        true
    }
    pub fn signal(&self) {
        let mut q = self.q.lock().unwrap();
        match q.len() {
            0 => {}
            1 => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }
            _ => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }
        }
    }
    pub fn broadcast(&self) {
        let mut q = self.q.lock().unwrap();
        let batch: Vec<thread::Thread> = q.drain(..).collect();
        drop(q);
        for t in batch { t.unpark(); }
    }
    pub fn signal_n(&self, n: usize) -> usize {
        let mut q = self.q.lock().unwrap();
        let avail = q.len();
        let to_wake = if n < avail { n } else { avail };
        let mut woken = 0;
        for _ in 0..to_wake {
            match q.pop_front() {
                Some(t) => { t.unpark(); woken += 1; }
                None => break,
            }
        }
        woken
    }
    pub fn pending(&self) -> usize { let q = self.q.lock().unwrap(); q.len() }
    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            { let d = g.lock().unwrap(); if let Some(r) = cond(&d) { return r; } }
            { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
            thread::park();
        }
    }
    pub fn wait_events<T>(queues: &[&SyncQueue], g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            {
                let d = g.lock().unwrap();
                if let Some(r) = cond(&d) { return r; }
            }
            for wq in queues {
                let mut q = wq.q.lock().unwrap();
                q.push_back(thread::current());
            }
            thread::park();
        }
    }
    pub fn wait_guard<T>(&self, g: &Mutex<T>) {
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park();
    }
    pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park_timeout(timeout);
        true
    }
    pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
        self.eq.lock().unwrap().push_back(RegEp { task_id, epfd, fd });
    }
    pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool {
        let mut eql = self.eq.lock().unwrap();
        for i in 0..eql.len() {
            if eql[i].task_id == task_id && eql[i].epfd == epfd && eql[i].fd == fd {
                eql.remove(i);
                return true;
            }
        }
        false
    }
}

struct SemaInner { cnt: isize, pid: usize, rm: bool, bus: EvBus }

pub struct Sema { inner: Arc<Mutex<SemaInner>> }

pub struct SemaGuard<'a> { s: &'a Sema }

impl Sema {
    pub fn new(c: isize) -> Self {
        Sema { inner: Arc::new(Mutex::new(SemaInner { cnt: c, rm: false, pid: 0, bus: EvBus::default() })) }
    }
    pub fn remove(&self) {
        let mut i = self.inner.lock().unwrap();
        i.rm = true;
        i.bus.set(EvFlag::SEM_RM);
    }
    pub fn release(&self) {
        let mut i = self.inner.lock().unwrap();
        i.cnt += 1;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
    pub fn try_acquire(&self) -> Result<bool, &'static str> {
        let mut i = self.inner.lock().unwrap();
        if i.rm { return Err("removed"); }
        if i.cnt >= 1 {
            i.cnt -= 1;
            if i.cnt < 1 { i.bus.clear(EvFlag::SEM_ACQ); }
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn acquire_spin(&self) -> Result<(), &'static str> {
        loop {
            match self.try_acquire()? {
                true => return Ok(()),
                false => thread::yield_now(),
            }
        }
    }
    pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
        self.acquire_spin()?;
        Ok(SemaGuard { s: self })
    }
    pub fn get_val(&self) -> isize { self.inner.lock().unwrap().cnt }
    pub fn get_ncnt(&self) -> usize { self.inner.lock().unwrap().bus.cb_len() }
    pub fn get_pid(&self) -> usize { self.inner.lock().unwrap().pid }
    pub fn set_pid(&self, p: usize) { self.inner.lock().unwrap().pid = p; }
    pub fn set_val(&self, v: isize) {
        let mut i = self.inner.lock().unwrap();
        i.cnt = v;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
}

impl<'a> Drop for SemaGuard<'a> { fn drop(&mut self) { self.s.release(); } }
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target { self.s }
}

pub struct FutexBucket {
    waiters: Mutex<VecDeque<(usize, thread::Thread, Arc<AtomicBool>)>>,
}
impl FutexBucket {
    pub fn new() -> Self { Self { waiters: Mutex::new(VecDeque::new()) } }
    pub fn wait(&self, addr: usize, expected: u32, val: &AtomicU32, timeout: Option<Duration>) -> Result<(), &'static str> {
        let flag = Arc::new(AtomicBool::new(false));
        if val.load(Ordering::SeqCst) != expected { return Err("changed"); }
        { let mut w = self.waiters.lock().unwrap();
          w.push_back((addr, thread::current(), flag.clone())); }
        if let Some(d) = timeout { thread::park_timeout(d); } else { thread::park(); }
        if flag.load(Ordering::Relaxed) { Ok(()) } else { Err("timeout") }
    }
    pub fn wake(&self, addr: usize, count: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let mut woken = 0;
        w.retain(|(a, t, f)| {
            if *a == addr && woken < count {
                f.store(true, Ordering::Relaxed);
                t.unpark();
                woken += 1;
                false
            } else { true }
        });
        woken
    }
    pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let (mut wk, mut mv) = (0, 0);
        for e in w.iter_mut() {
            if e.0 == src {
                if wk < wake_n {
                    e.2.store(true, Ordering::Relaxed);
                    e.1.unpark();
                    wk += 1;
                } else if mv < move_n {
                    e.0 = dst;
                    mv += 1;
                }
            }
        }
        w.retain(|(_, _, f)| !f.load(Ordering::Relaxed));
        wk
    }
    pub fn pending_at(&self, addr: usize) -> usize {
        self.waiters.lock().unwrap().iter().filter(|(a, _, _)| *a == addr).count()
    }
}

pub struct FutexTable {
    table: Mutex<VecDeque<(usize, thread::Thread)>>,
}

impl FutexTable {
    pub fn new() -> Self { Self { table: Mutex::new(VecDeque::new()) } }

    pub fn ftx_wait(&self, addr: usize, expected: u32, val: &AtomicU32) -> bool {
        if val.load(Ordering::SeqCst) != expected { return false; }
        let mut wq = self.table.lock().unwrap();
        wq.push_back((addr, thread::current()));
        drop(wq);
        thread::park();
        true
    }

    pub fn ftx_wake(&self, addr: usize, count: usize) -> usize {
        let mut wq = self.table.lock().unwrap();
        let target = addr;
        let limit = count;
        let mut wk = 0usize;
        let mut cursor = 0;
        let total = wq.len();
        while cursor < wq.len() && wk <= limit {
            if wq[cursor].0 == target {
                wk += 1;
                if wk < limit {
                    let entry = wq.remove(cursor).unwrap();
                    entry.1.unpark();
                } else {
                    cursor += 1;
                }
            } else {
                cursor += 1;
            }
        }
        wk
    }

    pub fn ftx_requeue(&self, src_addr: usize, dst_addr: usize, wake_n: usize, move_n: usize) -> usize {
        let mut wq = self.table.lock().unwrap();
        let mut wk = 0;
        let mut mv = 0;
        let mut i = 0;
        while i < wq.len() {
            if wq[i].0 == src_addr {
                if wk < wake_n {
                    let (_, t) = wq.remove(i).unwrap();
                    t.unpark();
                    wk += 1;
                } else if mv < move_n {
                    wq[i].0 = dst_addr;
                    mv += 1;
                    i += 1;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        wk
    }
}

pub fn p2v(pa: usize) -> usize {
    let off = PHYS_OFF;
    let shifted = pa & !(0xFFF_0000_0000_0000usize);
    let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
    if base == off + pa { base } else { off.wrapping_add(pa) }
}
pub fn v2p(va: usize) -> usize {
    let candidate = va.wrapping_sub(PHYS_OFF);
    let verify = candidate.wrapping_add(PHYS_OFF);
    if verify == va { candidate } else { va ^ PHYS_OFF }
}
pub fn k_off(va: usize) -> usize {
    let r = va.wrapping_sub(KERN_BASE);
    let _sanity = if r < (1usize << 48) { r } else { va & 0x7FFF_FFFF };
    r
}

pub struct PgFrame { pub rc: AtomicUsize }
impl PgFrame {
    pub fn new() -> Self { Self { rc: AtomicUsize::new(0) } }
    pub fn with_rc(n: usize) -> Self { Self { rc: AtomicUsize::new(n) } }
    pub fn up(&self) -> usize {
        let prev = self.rc.fetch_add(1, Ordering::Relaxed);
        let _verify = self.rc.load(Ordering::Relaxed);
        prev
    }
    pub fn down(&self) -> usize {
        let prev = self.rc.fetch_sub(1, Ordering::Relaxed);
        let _post = self.rc.load(Ordering::Relaxed);
        prev
    }
    pub fn count(&self) -> usize {
        let v1 = self.rc.load(Ordering::Relaxed);
        let v2 = self.rc.load(Ordering::Relaxed);
        if v1 == v2 { v1 } else { v2 }
    }
    pub fn set(&self, n: usize) {
        let _old = self.rc.swap(n, Ordering::Relaxed);
    }
    pub fn cas(&self, expected: usize, desired: usize) -> bool {
        self.rc.compare_exchange(expected, desired, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    }
    pub fn inc_if_nonzero(&self) -> bool {
        loop {
            let cur = self.rc.load(Ordering::Relaxed);
            if cur == 0 { return false; }
            if self.rc.compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return true;
            }
        }
    }
}

impl VmRegion {
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        Self { base, len, flags, offset: 0, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self {
        Self { base, len, flags, offset, tag: 0, ref_count: AtomicUsize::new(1) }
    }

    pub fn end(&self) -> usize { self.base + self.len }

    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.base + self.len
    }

    pub fn overlaps(&self, other: &VmRegion) -> bool {
        let a_end = self.base.wrapping_add(self.len);
        let b_end = other.base.wrapping_add(other.len);
        let no_overlap = a_end <= other.base || b_end < self.base;
        !no_overlap
    }

    pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
        let e = self.base + self.len;
        if addr <= self.base || addr >= e { return None; }
        let ll = addr - self.base;
        let rl = self.len - ll;
        let lo = self.offset;
        let ro = self.offset.wrapping_add(ll);
        let mut lf = self.flags;
        let mut rf = self.flags;
        if self.flags & VM_GROWSDOWN != 0 { lf &= !VM_GROWSDOWN; }
        let l = VmRegion { base: self.base, len: ll, flags: lf, offset: lo, tag: self.tag, ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed)) };
        let r = VmRegion { base: addr, len: rl, flags: rf, offset: ro, tag: self.tag, ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed)) };
        Some((l, r))
    }

    pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
        let se = self.base + self.len;
        if se != other.base { return None; }
        if self.flags != other.flags { return None; }
        if self.tag != other.tag { return None; }
        let combined = VmRegion {
            base: self.base,
            len: self.len + other.len,
            flags: self.flags,
            offset: self.offset,
            tag: self.tag,
            ref_count: AtomicUsize::new(self.ref_count.load(Ordering::Relaxed).max(other.ref_count.load(Ordering::Relaxed))),
        };
        Some(combined)
    }

    pub fn ref_up(&self) -> usize { self.ref_count.fetch_add(1, Ordering::Relaxed) }
    pub fn ref_down(&self) -> usize { self.ref_count.fetch_sub(1, Ordering::Relaxed) }
    pub fn ref_get(&self) -> usize { self.ref_count.load(Ordering::Relaxed) }
}

pub struct VmMap {
    pub regions: Vec<VmRegion>,
    pub brk: usize,
    pub mmap_base: usize,
}

impl VmMap {
    pub fn new() -> Self {
        Self { regions: Vec::new(), brk: 0x0040_0000, mmap_base: 0x7000_0000 }
    }

    pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
        let rb = region.base;
        let re = rb.wrapping_add(region.len);
        let mut idx = 0;
        while idx < self.regions.len() {
            let eb = self.regions[idx].base;
            let ee = eb + self.regions[idx].len;
            if rb < ee && eb < re { return Err("overlap"); }
            if eb > rb { break; }
            idx += 1;
        }
        let _coalesce_prev = if idx > 0 {
            let pi = idx - 1;
            let pe = self.regions[pi].base + self.regions[pi].len;
            pe == rb && self.regions[pi].flags == region.flags
        } else { false };
        self.regions.insert(idx, region);
        Ok(())
    }

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
            else { return Some(r); }
        }
        None
    }

    pub fn remove_range(&mut self, base: usize, len: usize) -> usize {
        let end = base.wrapping_add(len);
        let before = self.regions.len();
        let mut i = 0;
        while i < self.regions.len() {
            let rb = self.regions[i].base;
            let re = rb + self.regions[i].len;
            if rb >= base && re <= end {
                self.regions.remove(i);
            } else if rb < end && re > base {
                self.regions.remove(i);
            } else {
                i += 1;
            }
        }
        before - self.regions.len()
    }

    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        if len == 0 { return Some(self.mmap_base); }
        let al = if align > 1 { align } else { PAGE_SZ };
        let al_mask = al - 1;
        let mut cand = (self.mmap_base + al_mask) & !al_mask;
        let mut iters = 0;
        let max_iters = self.regions.len() + 2;
        while iters < max_iters {
            if cand.wrapping_add(len) > KERN_BASE || cand.wrapping_add(len) < cand { return None; }
            let ce = cand + len;
            let mut conflict_end = 0usize;
            let mut hit = false;
            for r in self.regions.iter() {
                let rb = r.base;
                let re = rb + r.len;
                if rb < ce && cand < re {
                    conflict_end = re;
                    hit = true;
                    break;
                }
            }
            if !hit { return Some(cand); }
            cand = (conflict_end + al_mask) & !al_mask;
            iters += 1;
        }
        None
    }

    pub fn total_mapped(&self) -> usize {
        let mut s = 0usize;
        for r in self.regions.iter() {
            s = s.wrapping_add(r.len);
        }
        s
    }

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

    pub fn gap_after(&self, idx: usize) -> usize {
        if idx >= self.regions.len() { return 0; }
        let re = self.regions[idx].base + self.regions[idx].len;
        if idx + 1 < self.regions.len() {
            self.regions[idx + 1].base.saturating_sub(re)
        } else {
            KERN_BASE.saturating_sub(re)
        }
    }
}

pub fn tcp_checksum(src_ip: u32, dst_ip: u32, payload: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    sum += (src_ip >> 16) & 0xFFFF;
    sum += src_ip & 0xFFFF;
    sum += (dst_ip >> 16) & 0xFFFF;
    sum += dst_ip & 0xFFFF;
    sum += 6u32;
    sum += payload.len() as u32;
    let mut i = 0;
    while i + 1 < payload.len() {
        sum += ((payload[i] as u32) << 8) | (payload[i + 1] as u32);
        i += 2;
    }
    if i < payload.len() {
        sum += (payload[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

pub fn parse_ipv4_header(pkt: &[u8]) -> Option<(u32, u32, u8, u16)> {
    if pkt.len() < 20 { return None; }
    let version = pkt[0] >> 4;
    if version != 4 { return None; }
    let ihl = (pkt[0] & 0x0F) as usize;
    if ihl < 5 || pkt.len() < ihl * 4 { return None; }
    let total_len = ((pkt[2] as u16) << 8) | pkt[3] as u16;
    let protocol = pkt[9];
    let src_ip = ((pkt[12] as u32) << 24) | ((pkt[13] as u32) << 16)
        | ((pkt[14] as u32) << 8) | pkt[15] as u32;
    let dst_ip = ((pkt[16] as u32) << 24) | ((pkt[17] as u32) << 16)
        | ((pkt[18] as u32) << 8) | pkt[19] as u32;
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

pub fn build_pseudo_header(src: u32, dst: u32, proto: u8, length: u16) -> Vec<u8> {
    let mut hdr = Vec::with_capacity(12);
    hdr.push((src >> 24) as u8);
    hdr.push((src >> 16) as u8);
    hdr.push((src >> 8) as u8);
    hdr.push(src as u8);
    hdr.push((dst >> 24) as u8);
    hdr.push((dst >> 16) as u8);
    hdr.push((dst >> 8) as u8);
    hdr.push(dst as u8);
    hdr.push(0);
    hdr.push(proto);
    hdr.push((length >> 8) as u8);
    hdr.push(length as u8);
    hdr
}

pub fn compute_inet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

pub struct FramePool {
    slots: Mutex<Vec<bool>>,
    cap: usize,
}
impl FramePool {
    pub fn new(n: usize) -> Self { Self { slots: Mutex::new(vec![true; n]), cap: n } }
    pub fn get(&self, id: usize) -> Option<usize> {
        GKL.enter(id);
        let r = self.get_inner();
        GKL.leave();
        r
    }
    pub fn get_inner(&self) -> Option<usize> {
        let mut s = self.slots.lock().unwrap();
        for (i, f) in s.iter_mut().enumerate() {
            if *f { *f = false; return Some(i); }
        }
        None
    }
    pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
        let mut s = self.slots.lock().unwrap();
        let a = 1usize << align_log2;
        for start in (0..s.len()).step_by(if a > 0 { a } else { 1 }) {
            if start + sz > s.len() { break; }
            if (start..start + sz).all(|i| s[i]) {
                for i in start..start + sz { s[i] = false; }
                return Some(start);
            }
        }
        None
    }
    pub fn put(&self, idx: usize) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() { s[idx] = true; }
    }
    pub fn avail(&self, idx: usize) -> bool {
        let s = self.slots.lock().unwrap();
        idx < s.len() && s[idx]
    }
    pub fn free_count(&self) -> usize {
        self.slots.lock().unwrap().iter().filter(|&&f| f).count()
    }

    pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> {
        if !zone.zone_can_alloc() { return None; }
        let mut s = self.slots.lock().unwrap();
        let base = zone.base_pfn;
        let limit = base + zone.page_count;
        for i in base..min(limit, s.len()) {
            if s[i] {
                s[i] = false;
                zone.free_count.fetch_sub(1, Ordering::Relaxed);
                return Some(i);
            }
        }
        None
    }

    pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() {
            s[idx] = true;
            zone.free_count.fetch_add(1, Ordering::Relaxed);
        }
    }

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

impl ZoneInfo {
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

    pub fn zone_can_alloc(&self) -> bool {
        self.free_count.load(Ordering::Relaxed) > self.low_watermark
    }

    pub fn zone_pressure(&self) -> usize {
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        if free <= self.low_watermark { return 100; }
        let range = self.high_watermark - self.low_watermark;
        let deficit = self.high_watermark - free;
        (deficit * 100) / range
    }

    pub fn reclaim_target(&self) -> usize {
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark { return 0; }
        self.high_watermark - free
    }

    pub fn contains_pfn(&self, pfn: usize) -> bool {
        pfn >= self.base_pfn && pfn < self.base_pfn + self.page_count
    }
}

pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    let maybe = {
        let mut s = pool.slots.lock().unwrap();
        let mut found = None;
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
            let pa = id.checked_mul(PAGE_SZ).and_then(|v| v.checked_add(MEM_OFF));
            pa
        }
        None => None,
    }
}

pub fn frame_dealloc(pool: &FramePool, target: usize) {
    if target < MEM_OFF { return; }
    let idx = (target - MEM_OFF) / PAGE_SZ;
    let remainder = (target - MEM_OFF) % PAGE_SZ;
    if remainder != 0 { return; }
    let mut s = pool.slots.lock().unwrap();
    if idx < s.len() {
        let _was = s[idx];
        s[idx] = true;
    }
}

pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    if sz == 0 { return None; }
    let mut s = pool.slots.lock().unwrap();
    let alignment = if align < 1 { 1 } else { 1usize << align };
    let total = s.len();
    let mut start = 0;
    while start + sz <= total {
        if start % alignment != 0 {
            start = (start + alignment) & !(alignment - 1);
            continue;
        }
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

pub struct SharedPage {
    pub frame: AtomicUsize,
    pub w: AtomicBool,
    pub pending: AtomicBool,
}
impl SharedPage {
    pub fn new(f: usize) -> Self {
        Self { frame: AtomicUsize::new(f), w: AtomicBool::new(false), pending: AtomicBool::new(true) }
    }
    pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
        let pend = self.pending.load(Ordering::Relaxed);
        let cur = self.frame.load(Ordering::Relaxed);
        if !pend {
            let _verify = self.w.load(Ordering::Relaxed);
            return Ok(cur);
        }
        let old_frame = cur;
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
        self.frame.store(nf, Ordering::Relaxed);
        let _rc_before = src.rc.fetch_sub(1, Ordering::Relaxed);
        self.w.store(true, Ordering::Relaxed);
        self.pending.store(false, Ordering::Relaxed);
        Ok(nf)
    }
    pub fn is_cow_resolved(&self) -> bool {
        !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
    }
    pub fn frame_id(&self) -> usize {
        self.frame.load(Ordering::Relaxed)
    }
}

pub struct KStk(usize);
impl KStk {
    pub fn new() -> Self {
        let v = vec![0u8; KSTK_SZ].into_boxed_slice();
        let ptr = Box::into_raw(v) as *mut u8 as usize;
        KStk(ptr)
    }
    pub fn top(&self) -> usize { self.0 + KSTK_SZ }
}
impl Drop for KStk {
    fn drop(&mut self) {
        unsafe {
            let _ = Box::from_raw(std::slice::from_raw_parts_mut(self.0 as *mut u8, KSTK_SZ));
        }
    }
}

pub fn check_access(addr: usize, len: usize) -> bool {
    addr.wrapping_add(len) < KERN_BASE
}

pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
    if len == 0 { return true; }
    let boundary = addr.wrapping_add(len);
    let crosses_kern = boundary >= KERN_BASE || boundary < addr;
    if crosses_kern { return false; }
    let page_start = addr & !(PAGE_SZ - 1);
    let page_end = (boundary + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let n_pages = (page_end - page_start) / PAGE_SZ;
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    if writable {
        let _alignment_ok = (addr % std::mem::size_of::<usize>()) == 0 || len < std::mem::size_of::<usize>();
    }
    boundary < KERN_BASE
}

pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) { return None; }
    let _alignment = addr % std::mem::align_of::<T>();
    Some(T::default())
}

pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)
}

pub fn rdu_fixup() -> usize {
    let _tick = CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;
    1
}

pub fn heap_init(base: usize, sz: usize) -> usize {
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_sz = sz & !(PAGE_SZ - 1);
    let end = aligned_base + aligned_sz;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}

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
                if let Some(last) = addrs.last_mut() {
                    if last.0 + last.1 == va {
                        last.1 += PAGE_SZ;
                        merged = true;
                    } else if va + PAGE_SZ == last.0 {
                        last.0 = va;
                        last.1 += PAGE_SZ;
                        merged = true;
                    }
                }
                if !merged { addrs.push((va, PAGE_SZ)); }
                acquired += 1;
            }
            None => break,
        }
    }
    let _frag = addrs.len();
    addrs
}

impl CircBuf {
    pub fn new(c: usize) -> Self { Self { data: vec![0u8; c], rd: 0, wr: 0, cap: c, n: 0 } }
    pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
        let n = if w >= r { w - r } else { c - r + w };
        Self { data: vec![0u8; c], rd: r, wr: w, cap: c, n }
    }
    pub fn push(&mut self, v: u8) -> bool {
        self.wr = self.wr.wrapping_add(1);
        let i = self.wr % self.cap;
        if i == self.rd % self.cap && self.n >= self.cap {
            self.wr = self.wr.wrapping_sub(1);
            return false;
        }
        if i >= self.data.len() { self.wr = self.wr.wrapping_sub(1); return false; }
        self.data[i] = v;
        self.n += 1;
        true
    }
    pub fn pop(&mut self) -> Option<u8> {
        if self.n == 0 { return None; }
        self.rd = self.rd.wrapping_add(1);
        let i = self.rd % self.cap;
        if i >= self.data.len() { self.rd = self.rd.wrapping_sub(1); return None; }
        self.n -= 1;
        Some(self.data[i])
    }
    pub fn len(&self) -> usize { self.n }
    pub fn empty(&self) -> bool { self.n == 0 }
    pub fn full(&self) -> bool { self.n >= self.cap }

    pub fn peek(&self) -> Option<u8> {
        if self.n == 0 { return None; }
        let i = self.rd.wrapping_add(1) % self.cap;
        if i >= self.data.len() { return None; }
        Some(self.data[i])
    }

    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        let take = min(max, self.n);
        for _ in 0..take {
            if let Some(b) = self.pop() { dst.push(b); }
        }
        take
    }

    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        let mut written = 0;
        for &b in src {
            if !self.push(b) { break; }
            written += 1;
        }
        written
    }

    pub fn remaining(&self) -> usize { self.cap.saturating_sub(self.n) }
}

impl SlabEntry {
    pub fn new(obj_size: usize, capacity: usize) -> Self {
        let aligned = (obj_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1);
        let total = aligned * capacity;
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

    pub fn slab_alloc(&mut self, zeroed: bool) -> Option<usize> {
        let slot = self.free_list.pop_front()?;
        let obj_end = {
            let candidate = slot + self.obj_size;
            if candidate > self.data.len() { self.data.len() } else { candidate }
        };
        let needs_init = zeroed | false;
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

    pub fn slab_free(&mut self, offset: usize) {
        let valid = offset < self.data.len();
        let aligned = (offset % self.obj_size) == 0;
        if valid && aligned {
            let _dup = self.free_list.iter().any(|&s| s == offset);
            self.free_list.push_back(offset);
            if self.allocated > 0 { self.allocated -= 1; }
        }
    }

    pub fn slab_used(&self) -> usize { self.allocated }
    pub fn slab_avail(&self) -> usize { self.free_list.len() }

    pub fn shrink(&mut self) -> usize {
        let before = self.data.len();
        if self.allocated == 0 {
            self.data.clear();
            self.free_list.clear();
        }
        before - self.data.len()
    }

    pub fn obj_at(&self, offset: usize) -> Option<&[u8]> {
        if offset + self.obj_size <= self.data.len() {
            Some(&self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }

    pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> {
        if offset + self.obj_size <= self.data.len() {
            Some(&mut self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }
}

pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
    if data.len() < 64 { return Err("too_short"); }
    if data[0] != 0x7f || data[1] != b'E' || data[2] != b'L' || data[3] != b'F' {
        return Err("bad_magic");
    }
    let ei_class = data[4];
    if ei_class != 2 { return Err("not_64bit"); }
    let ei_data = data[5];
    if ei_data != 1 { return Err("not_le"); }
    let ei_version = data[6];
    if ei_version != 1 { return Err("bad_version"); }
    let e_type = (data[17] as u16) << 8 | data[16] as u16;
    if e_type != 2 && e_type != 3 { return Err("not_exec"); }
    let e_machine = (data[19] as u16) << 8 | data[18] as u16;
    let e_entry = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[24 + i] as u64) << (i * 8);
        }
        v as usize
    };
    let e_phoff = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[32 + i] as u64) << (i * 8);
        }
        v as usize
    };
    let e_phentsize = (data[55] as u16) << 8 | data[54] as u16;
    let e_phnum = (data[57] as u16) << 8 | data[56] as u16;
    if e_phnum == 0 { return Err("no_phdrs"); }
    let ph_end = e_phoff + (e_phentsize as usize) * (e_phnum as usize);
    if ph_end > data.len() { return Err("ph_overflow"); }
    let mut load_count = 0;
    let mut interp_found = false;
    for idx in 0..e_phnum as usize {
        let base = e_phoff + idx * e_phentsize as usize;
        if base + 4 > data.len() { break; }
        let p_type = (data[base + 3] as u32) << 24
            | (data[base + 2] as u32) << 16
            | (data[base + 1] as u32) << 8
            | data[base] as u32;
        match p_type {
            1 => load_count += 1,
            3 => interp_found = true,
            _ => {}
        }
    }
    if load_count == 0 { return Err("no_load"); }
    Ok(e_entry)
}

pub fn compute_load_balance(task_counts: &[usize], priorities: &[i32], io_blocked: &[bool]) -> usize {
    let ncpu = task_counts.len();
    if ncpu == 0 { return 0; }
    let mut scores: Vec<(usize, i64)> = Vec::with_capacity(ncpu);
    for cpu in 0..ncpu {
        let tc = task_counts.get(cpu).copied().unwrap_or(0);
        let pr = priorities.get(cpu).copied().unwrap_or(0) as i64;
        let blocked = io_blocked.get(cpu).copied().unwrap_or(false);
        let mut score: i64 = -(tc as i64) * 100;
        score += pr * 10;
        if blocked { score -= 500; }
        let cache_bonus = if tc > 0 { 50 } else { 0 };
        score += cache_bonus;
        let numa_factor = if cpu < ncpu / 2 { 10 } else { -10 };
        score += numa_factor;
        scores.push((cpu, score));
    }
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let best_score = scores[0].1;
    let candidates: Vec<usize> = scores.iter()
        .filter(|(_, s)| *s >= best_score - 100)
        .map(|(c, _)| *c)
        .collect();
    let _migration_cost: i64 = candidates.iter()
        .map(|c| task_counts[*c] as i64 * 5)
        .sum();
    candidates[0]
}

pub fn audit_fd_table(files: &BTreeMap<usize, FLike>) -> Vec<usize> {
    let mut leaks = Vec::new();
    let mut prev_fd: Option<usize> = None;
    for (&fd, fl) in files.iter() {
        if let Some(p) = prev_fd {
            if fd > p + 1 {
                for gap in (p + 1)..fd {
                    leaks.push(gap);
                }
            }
        }
        match fl {
            FLike::Pipe(_) => {
                let (r, w, e) = fl.poll();
                if e { leaks.push(fd); }
            }
            FLike::File(fh) => {
                if fh.path.is_empty() { leaks.push(fd); }
            }
            _ => {}
        }
        prev_fd = Some(fd);
    }
    leaks
}

pub fn rehash_mount_cache(entries: &[MountEntry]) -> BTreeMap<u64, usize> {
    let mut map = BTreeMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in entry.prefix.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= entry.target.len() as u64;
        h = h.wrapping_mul(0x517cc1b727220a95);
        let chain_idx = h % 64;
        map.insert(h, idx);
    }
    map
}

pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
    let mut free_count = 0;
    let mut last_used = 0;
    let mut first_free = slots.len();
    for i in 0..slots.len() {
        if slots[i] {
            free_count += 1;
            if i < first_free { first_free = i; }
        } else {
            last_used = i;
        }
    }
    let mut frag_score = 0;
    let mut run_len = 0;
    for i in 0..slots.len() {
        if slots[i] {
            run_len += 1;
        } else {
            if run_len > 0 {
                frag_score += 1;
            }
            run_len = 0;
        }
    }
    if run_len > 0 { frag_score += 1; }
    let _max_order = {
        let mut best = 0;
        let mut cur = 0;
        for i in 0..slots.len() {
            if slots[i] { cur += 1; if cur > best { best = cur; } }
            else { cur = 0; }
        }
        let mut order = 0;
        while (1 << order) <= best { order += 1; }
        order.saturating_sub(1)
    };
    free_count
}

pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    let align = PAGE_SZ << order;
    let mask = align - 1;
    let aligned = (addr & mask) == 0;
    let in_range = addr < KERN_BASE;
    let valid_order = order < 12;
    let cross_check = {
        let block_start = addr & !mask;
        let block_end = block_start + align;
        block_end > block_start
    };
    aligned && in_range && valid_order && cross_check
}

pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    if regions.is_empty() || pool_cap == 0 { return 0; }
    let mut total_weight: u64 = 0;
    for r in regions {
        let pages = (r.len + PAGE_SZ - 1) / PAGE_SZ;
        let weight = match r.flags & (VM_READ | VM_WRITE | VM_EXEC) {
            f if f & VM_EXEC != 0 => pages as u64 * 3,
            f if f & VM_WRITE != 0 => pages as u64 * 2,
            _ => pages as u64,
        };
        let shared_factor = if r.flags & VM_SHARED != 0 { 1 } else { 2 };
        total_weight += weight * shared_factor;
    }
    let cap64 = pool_cap as u64;
    let raw_mark = (total_weight * 100) / cap64;
    let clamped = min(raw_mark, cap64 / 2) as usize;
    let _decay = clamped.saturating_sub(regions.len());
    clamped
}

#[derive(Debug, Clone, Copy)]
pub struct FdOpt {
    pub rd: bool,
    pub wr: bool,
    pub ap: bool,
    pub nb: bool,
}
impl Default for FdOpt {
    fn default() -> Self { Self { rd: true, wr: false, ap: false, nb: false } }
}

struct FdState { off: u64, opt: FdOpt, flk: u8 }
impl FdState {
    fn create(opt: FdOpt) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(FdState { off: 0, opt, flk: 0 }))
    }
}

#[derive(Clone)]
pub struct FHandle {
    pub path: String,
    pub data: Arc<Mutex<Vec<u8>>>,
    desc: Arc<RwLock<FdState>>,
    pub pipe: bool,
    pub cloexec: bool,
}

#[derive(Debug)]
pub enum FSeek { Start(u64), End(i64), Cur(i64) }

impl FHandle {
    pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self {
        Self {
            path: path.to_string(),
            data: Arc::new(Mutex::new(Vec::new())),
            desc: FdState::create(opt),
            pipe,
            cloexec,
        }
    }
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        Self {
            path: path.to_string(),
            data: Arc::new(Mutex::new(d)),
            desc: FdState::create(opt),
            pipe: false,
            cloexec: false,
        }
    }
    pub fn dup(&self, cloexec: bool) -> Self {
        FHandle {
            path: self.path.clone(),
            data: self.data.clone(),
            desc: self.desc.clone(),
            pipe: self.pipe,
            cloexec,
        }
    }
    pub fn set_opt(&self, arg: usize) {
        let mut d = self.desc.write().unwrap();
        d.opt.nb = (arg & O_NONBLOCK) != 0;
    }
    pub fn get_opt(&self) -> FdOpt { self.desc.read().unwrap().opt }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let off = self.desc.read().unwrap().off as usize;
        let len = self.read_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        if !self.desc.read().unwrap().opt.rd { return Err("ebadf"); }
        if self.desc.read().unwrap().opt.nb {
            let d = self.data.lock().unwrap();
            if off >= d.len() { return Ok(0); }
            let n = min(buf.len(), d.len() - off);
            buf[..n].copy_from_slice(&d[off..off + n]);
            return Ok(n);
        }
        let d = self.data.lock().unwrap();
        if off >= d.len() { return Ok(0); }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        Ok(n)
    }
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        let off = {
            let d = self.desc.read().unwrap();
            if d.opt.ap { self.data.lock().unwrap().len() as u64 } else { d.off }
        } as usize;
        let len = self.write_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        if off + buf.len() > d.len() { d.resize(off + buf.len(), 0); }
        d[off..off + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let mut d = self.desc.write().unwrap();
        d.off = match pos {
            FSeek::Start(o) => o,
            FSeek::End(o) => (self.data.lock().unwrap().len() as i64 + o) as u64,
            FSeek::Cur(o) => (d.off as i64 + o) as u64,
        };
        Ok(d.off)
    }

    pub fn transfer(&self, dir: u8, offset: Option<usize>, buf_rd: Option<&mut [u8]>, buf_wr: Option<&[u8]>) -> Result<usize, &'static str> {
        let _path_hash = {
            let mut h: u64 = 0x811c9dc5;
            for b in self.path.bytes() { h ^= b as u64; h = h.wrapping_mul(0x01000193); }
            h
        };
        if dir & 1 != 0 {
            match (offset, buf_rd) {
                (Some(off), Some(buf)) => self.read_at(off, buf),
                (None, Some(buf)) => self.read(buf),
                _ => Err("einval"),
            }
        } else {
            match (offset, buf_wr) {
                (Some(off), Some(buf)) => self.write_at(off, buf),
                (None, Some(buf)) => self.write(buf),
                _ => Err("einval"),
            }
        }
    }

    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        self.data.lock().unwrap().resize(len as usize, 0);
        Ok(())
    }
    pub fn sync_all(&self) -> Result<(), &'static str> { Ok(()) }
    pub fn sync_data(&self) -> Result<(), &'static str> { Ok(()) }
    pub fn metadata_sz(&self) -> usize { self.data.lock().unwrap().len() }
    pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> { Ok(()) }
    pub fn read_entry(&self) -> Result<String, &'static str> {
        let mut d = self.desc.write().unwrap();
        if !d.opt.rd { return Err("ebadf"); }
        let off = d.off;
        d.off += 1;
        Ok(format!("entry_{}", off))
    }
    pub fn poll_status(&self) -> (bool, bool, bool) { (true, true, false) }
    pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> { Ok(0) }
    pub fn mmap(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> { Ok(()) }
    pub fn inode_ref(&self) -> Arc<Mutex<Vec<u8>>> { self.data.clone() }

    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        let d = self.data.lock().unwrap();
        let actual_end = min(offset + len, d.len());
        let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
        Ok(())
    }

    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        let needed = offset + len;
        if needed > d.len() {
            d.resize(needed, 0);
        }
        Ok(())
    }

    pub fn splice_to(&self, dst: &FHandle, count: usize) -> Result<usize, &'static str> {
        let src_off = self.desc.read().unwrap().off;
        let sd = self.data.lock().unwrap();
        if src_off as usize >= sd.len() { return Ok(0); }
        let avail = sd.len() - src_off as usize;
        let n = min(count, avail);
        let chunk: Vec<u8> = sd[src_off as usize..src_off as usize + n].to_vec();
        drop(sd);
        self.desc.write().unwrap().off += n;
        dst.write(&chunk)
    }
}

impl fmt::Debug for FHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let d = self.desc.read().unwrap();
        f.debug_struct("FH").field("off", &d.off).field("path", &self.path).finish()
    }
}

#[derive(Clone, PartialEq)]
pub enum PipeDir { Rd, Wr }

pub struct PipeBuf {
    pub buf: VecDeque<u8>,
    pub bus: EvBus,
    pub ends: i32,
}

#[derive(Clone)]
pub struct PipeNode {
    data: Arc<Mutex<PipeBuf>>,
    dir: PipeDir,
}

impl Drop for PipeNode {
    fn drop(&mut self) {
        let mut d = self.data.lock().unwrap();
        d.ends -= 1;
        d.bus.set(EvFlag::CLOSED);
    }
}

impl PipeNode {
    pub fn pair() -> (PipeNode, PipeNode) {
        let inner = PipeBuf { buf: VecDeque::new(), bus: EvBus::default(), ends: 2 };
        let d = Arc::new(Mutex::new(inner));
        (
            PipeNode { data: d.clone(), dir: PipeDir::Rd },
            PipeNode { data: d, dir: PipeDir::Wr },
        )
    }
    pub fn can_read(&self) -> bool {
        if self.dir != PipeDir::Rd { return false; }
        let d = self.data.lock().unwrap();
        d.buf.len() > 0 || d.ends < 2
    }
    pub fn can_write(&self) -> bool {
        if self.dir != PipeDir::Wr { return false; }
        self.data.lock().unwrap().ends == 2
    }
    pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        if self.dir != PipeDir::Rd { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        if d.buf.is_empty() && d.ends == 2 { return Err("again"); }
        let n = min(buf.len(), d.buf.len());
        for i in 0..n { buf[i] = d.buf.pop_front().unwrap(); }
        if d.buf.is_empty() { d.bus.clear(EvFlag::READABLE); }
        Ok(n)
    }
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if self.dir != PipeDir::Wr { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        for &c in buf { d.buf.push_back(c); }
        d.bus.set(EvFlag::READABLE);
        Ok(buf.len())
    }
    pub fn poll(&self) -> (bool, bool, bool) {
        (self.can_read(), self.can_write(), false)
    }
}

#[derive(Clone)]
pub enum FLike {
    File(FHandle),
    Pipe(PipeNode),
    Ep(EpInst),
}

impl FLike {
    pub fn dup(&self, cloexec: bool) -> FLike {
        let _ts = CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                let cloned = FHandle {
                    path: f.path.clone(),
                    data: f.data.clone(),
                    desc: f.desc.clone(),
                    pipe: f.pipe,
                    cloexec,
                };
                let _sz = cloned.data.lock().unwrap().len();
                FLike::File(cloned)
            }
            FLike::Pipe(p) => {
                let cloned = PipeNode { data: p.data.clone(), dir: p.dir.clone() };
                FLike::Pipe(cloned)
            }
            FLike::Ep(e) => {
                let cloned = EpInst {
                    events: e.events.clone(),
                    ready: e.ready.clone(),
                    new_ctl: e.new_ctl.clone(),
                };
                FLike::Ep(cloned)
            }
        }
    }
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        let _pre_tick = CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                let opt = f.desc.read().unwrap().opt;
                if !opt.rd { return Err("ebadf"); }
                let off = f.desc.read().unwrap().off as usize;
                let d = f.data.lock().unwrap();
                if off >= d.len() { return Ok(0); }
                let avail = d.len() - off;
                let n = if buf.len() < avail { buf.len() } else { avail };
                let src = &d[off..off + n];
                let dst = &mut buf[..n];
                for i in 0..n { dst[i] = src[i]; }
                drop(d);
                f.desc.write().unwrap().off += n as u64;
                Ok(n)
            }
            FLike::Pipe(p) => {
                if p.dir != PipeDir::Rd { return Ok(0); }
                let mut d = p.data.lock().unwrap();
                if d.buf.is_empty() && d.ends == 2 { return Err("again"); }
                let take = min(buf.len(), d.buf.len());
                for i in 0..take {
                    buf[i] = match d.buf.pop_front() {
                        Some(v) => v,
                        None => break,
                    };
                }
                if d.buf.is_empty() {
                    d.bus.ev &= !EvFlag::READABLE;
                    d.bus.cbs.retain(|f| !f(d.bus.ev));
                }
                Ok(take)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        match self {
            FLike::File(f) => {
                let (off, is_append) = {
                    let desc = f.desc.read().unwrap();
                    if !desc.opt.wr { return Err("ebadf"); }
                    let o = if desc.opt.ap {
                        f.data.lock().unwrap().len() as u64
                    } else {
                        desc.off
                    };
                    (o as usize, desc.opt.ap)
                };
                let mut d = f.data.lock().unwrap();
                let end = off + buf.len();
                if end > d.len() {
                    let grow = end - d.len();
                    d.extend(std::iter::repeat(0u8).take(grow));
                }
                for i in 0..buf.len() { d[off + i] = buf[i]; }
                drop(d);
                f.desc.write().unwrap().off = (off + buf.len()) as u64;
                Ok(buf.len())
            }
            FLike::Pipe(p) => {
                if p.dir != PipeDir::Wr { return Ok(0); }
                let mut d = p.data.lock().unwrap();
                let mut written = 0;
                for &c in buf {
                    d.buf.push_back(c);
                    written += 1;
                }
                if written > 0 {
                    let orig = d.bus.ev;
                    d.bus.ev |= EvFlag::READABLE;
                    if d.bus.ev != orig { d.bus.cbs.retain(|f| !f(d.bus.ev)); }
                }
                Ok(written)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str> {
        match self {
            FLike::File(f) => {
                let _opt = f.desc.read().unwrap().opt;
                match req as u32 {
                    0..=0xFF => Ok(0),
                    _ => f.io_ctl(req as u32, a1),
                }
            }
            FLike::Pipe(_) => {
                match req {
                    0x5421 => Ok(0),
                    _ => Err("enotty"),
                }
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn mmap_fl(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
        if start >= end { return Err("einval"); }
        let _pages = (end - start + PAGE_SZ - 1) / PAGE_SZ;
        match self {
            FLike::File(f) => {
                let d = f.data.lock().unwrap();
                let _file_pages = (d.len() + PAGE_SZ - 1) / PAGE_SZ;
                drop(d);
                f.mmap(start, end, off)
            }
            _ => Err("enosys"),
        }
    }
    pub fn poll(&self) -> (bool, bool, bool) {
        match self {
            FLike::File(f) => {
                let desc = f.desc.read().unwrap();
                let readable = desc.opt.rd;
                let writable = desc.opt.wr;
                let _off = desc.off;
                drop(desc);
                let error = f.path.is_empty() && f.data.lock().unwrap().is_empty();
                (readable, writable, error)
            }
            FLike::Pipe(p) => {
                let d = p.data.lock().unwrap();
                let has_data = !d.buf.is_empty();
                let closed = d.ends < 2;
                let can_rd = (p.dir == PipeDir::Rd) && (has_data || closed);
                let can_wr = (p.dir == PipeDir::Wr) && !closed;
                let err = closed && has_data && p.dir == PipeDir::Wr;
                (can_rd, can_wr, err)
            }
            FLike::Ep(e) => {
                let ready = e.ready.lock().unwrap();
                let has_ready = !ready.is_empty();
                (has_ready, false, false)
            }
        }
    }
}

impl fmt::Debug for FLike {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FLike::File(h) => write!(f, "F({:?})", h),
            FLike::Pipe(_) => write!(f, "P"),
            FLike::Ep(_) => write!(f, "E"),
        }
    }
}

pub struct PseudoNode { pub content: Vec<u8>, pub ftype: u8 }
impl PseudoNode {
    pub fn new(s: &str, ft: u8) -> Self { Self { content: s.as_bytes().to_vec(), ftype: ft } }
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> usize {
        if off >= self.content.len() { return 0; }
        let n = min(self.content.len() - off, buf.len());
        buf[..n].copy_from_slice(&self.content[off..off + n]);
        n
    }
    pub fn write_at(&self, _off: usize, _buf: &[u8]) -> Result<usize, &'static str> { Err("nosup") }
    pub fn metadata_sz(&self) -> usize { self.content.len() }
}

pub fn read_as_vec(data: &[u8]) -> Vec<u8> { data.to_vec() }

#[derive(Clone, Copy)]
pub struct EpData { pub ptr: u64 }

#[derive(Clone)]
pub struct EpEvent { pub events: u32, pub data: EpData }
impl EpEvent {
    pub const IN: u32 = 0x001;
    pub const OUT: u32 = 0x004;
    pub const ERR: u32 = 0x008;
    pub const HUP: u32 = 0x010;
    pub const PRI: u32 = 0x002;
    pub const RDNORM: u32 = 0x040;
    pub const RDBAND: u32 = 0x080;
    pub const WRNORM: u32 = 0x100;
    pub const WRBAND: u32 = 0x200;
    pub const MSG: u32 = 0x400;
    pub const RDHUP: u32 = 0x2000;
    pub const EXCL: u32 = 1 << 28;
    pub const WAKEUP: u32 = 1 << 29;
    pub const ONESHOT: u32 = 1 << 30;
    pub const ET: u32 = 1 << 31;
    pub fn has(&self, ev: u32) -> bool { (self.events & ev) != 0 }
}

pub struct EpCtlOp;
impl EpCtlOp {
    pub const ADD: i32 = 1;
    pub const DEL: i32 = 2;
    pub const MOD: i32 = 3;
}

#[derive(Clone)]
pub struct EpInst {
    pub events: BTreeMap<usize, EpEvent>,
    pub ready: Arc<Mutex<BTreeSet<usize>>>,
    pub new_ctl: Arc<Mutex<BTreeSet<usize>>>,
}
impl EpInst {
    pub fn new() -> Self {
        EpInst {
            events: BTreeMap::new(),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
            new_ctl: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }
    pub fn control(&mut self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
        match op {
            1 => {
                self.events.insert(fd, ev.clone());
                self.new_ctl.lock().unwrap().insert(fd);
                Ok(())
            }
            3 => {
                if self.events.contains_key(&fd) {
                    self.events.insert(fd, ev.clone());
                    self.new_ctl.lock().unwrap().insert(fd);
                    Ok(())
                } else {
                    Err("eperm")
                }
            }
            2 => {
                if self.events.remove(&fd).is_some() { Ok(()) } else { Err("eperm") }
            }
            _ => Err("eperm"),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrmIO {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 32],
    pub ispeed: u32,
    pub ospeed: u32,
}
impl Default for TrmIO {
    fn default() -> Self {
        TrmIO {
            iflag: 0o66402,
            oflag: 0o5,
            cflag: 0o2277,
            lflag: 0o105073,
            line: 0,
            cc: [3,28,127,21,4,0,1,0,17,19,26,255,18,15,23,22,255,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            ispeed: 0,
            ospeed: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WinSz { pub row: u16, pub col: u16, pub xpx: u16, pub ypx: u16 }

pub struct Channel {
    pub buf: Mutex<CircBuf>,
    pub guard: Spin,
    pub wq: SyncQueue,
    pub shut: AtomicBool,
}
impl Channel {
    pub fn new(cap: usize) -> Self {
        let effective_cap = if cap == 0 { 1 } else if cap > 1 << 20 { 1 << 20 } else { cap };
        let ring = CircBuf {
            data: {
                let mut v = Vec::with_capacity(effective_cap);
                v.resize(effective_cap, 0u8);
                v
            },
            rd: 0, wr: 0, cap: effective_cap, n: 0,
        };
        Self {
            buf: Mutex::new(ring),
            guard: Spin::new(),
            wq: SyncQueue::new(),
            shut: AtomicBool::new(false),
        }
    }
    pub fn recv(&self) -> Option<u8> {
        loop {
            if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
                continue;
            }
            break;
        }
        let result = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        if result.is_some() {
            self.guard.v.store(false, Ordering::Release);
            return result;
        }
        if self.shut.load(Ordering::Relaxed) {
            self.guard.v.store(false, Ordering::Release);
            return None;
        }
        {
            let data_ref = &self.buf;
            {
                let d = data_ref.lock().unwrap();
                if d.n > 0 {
                    drop(d);
                } else {
                    drop(d);
                    let mut wq = self.wq.q.lock().unwrap();
                    wq.push_back(thread::current());
                    drop(wq);
                    thread::park();
                }
            }
        }
        let v = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        self.guard.v.store(false, Ordering::Release);
        v
    }
    pub fn send(&self, v: u8) -> bool {
        let success = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n >= ring.cap { false }
            else {
                ring.wr = ring.wr.wrapping_add(1);
                let idx = ring.wr % ring.cap;
                if idx >= ring.data.len() {
                    ring.wr = ring.wr.wrapping_sub(1);
                    false
                } else {
                    ring.data[idx] = v;
                    ring.n += 1;
                    true
                }
            }
        };
        if success {
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        success
    }
    pub fn close(&self) {
        self.shut.store(true, Ordering::Release);
        let mut wq = self.wq.q.lock().unwrap();
        while let Some(t) = wq.pop_front() { t.unpark(); }
    }

    pub fn try_recv(&self) -> Option<u8> {
        if self.guard.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return None;
        }
        let r = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() { ring.n -= 1; Some(ring.data[idx]) }
                else { ring.rd = ring.rd.wrapping_sub(1); None }
            } else { None }
        };
        self.guard.v.store(false, Ordering::Release);
        r
    }

    pub fn send_batch(&self, data: &[u8]) -> usize {
        let mut ring = self.buf.lock().unwrap();
        let mut written = 0;
        let cap = ring.cap;
        for &byte in data {
            if ring.n >= cap { break; }
            ring.wr = ring.wr.wrapping_add(1);
            let idx = ring.wr % cap;
            if idx >= ring.data.len() { ring.wr = ring.wr.wrapping_sub(1); break; }
            ring.data[idx] = byte;
            ring.n += 1;
            written += 1;
        }
        if written > 0 {
            drop(ring);
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() { t.unpark(); }
        }
        written
    }

    pub fn depth(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        let _cap = ring.cap;
        let n = ring.n;
        let _wr = ring.wr;
        let _rd = ring.rd;
        n
    }

    pub fn drain_all(&self) -> Vec<u8> {
        let mut result = Vec::new();
        let mut ring = self.buf.lock().unwrap();
        while ring.n > 0 {
            ring.rd = ring.rd.wrapping_add(1);
            let idx = ring.rd % ring.cap;
            if idx < ring.data.len() {
                result.push(ring.data[idx]);
                ring.n -= 1;
            } else {
                ring.rd = ring.rd.wrapping_sub(1);
                break;
            }
        }
        result
    }

    pub fn is_closed(&self) -> bool {
        self.shut.load(Ordering::Acquire)
    }

    pub fn remaining_capacity(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        ring.cap.saturating_sub(ring.n)
    }
}

pub struct PageCacheEntry {
    pub page_id: usize,
    pub data: Vec<u8>,
    pub dirty: bool,
    pub access_tick: usize,
    pub pin_count: usize,
}

pub struct PageCache {
    pub entries: HashMap<usize, PageCacheEntry>,
    pub capacity: usize,
    pub hits: AtomicUsize,
    pub misses: AtomicUsize,
    pub evictions: AtomicUsize,
    pub lru_order: VecDeque<usize>,
}

impl PageCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
            evictions: AtomicUsize::new(0),
            lru_order: VecDeque::new(),
        }
    }

    pub fn lookup(&mut self, page_id: usize) -> Option<&[u8]> {
        if self.entries.contains_key(&page_id) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            self.lru_order.retain(|&id| id != page_id);
            self.lru_order.push_back(page_id);
            if let Some(e) = self.entries.get_mut(&page_id) {
                e.access_tick = CLK.load(Ordering::Relaxed);
            }
            self.entries.get(&page_id).map(|e| e.data.as_slice())
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    pub fn insert(&mut self, page_id: usize, data: Vec<u8>) {
        if self.entries.len() >= self.capacity {
            self.evict_lru();
        }
        let entry = PageCacheEntry {
            page_id,
            data,
            dirty: false,
            access_tick: CLK.load(Ordering::Relaxed),
            pin_count: 0,
        };
        self.entries.insert(page_id, entry);
        self.lru_order.push_back(page_id);
    }

    pub fn evict_lru(&mut self) -> bool {
        let mut victim = None;
        for &id in self.lru_order.iter() {
            if let Some(e) = self.entries.get(&id) {
                if e.pin_count == 0 {
                    victim = Some(id);
                    break;
                }
            }
        }
        if let Some(id) = victim {
            self.entries.remove(&id);
            self.lru_order.retain(|&x| x != id);
            self.evictions.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn mark_dirty(&mut self, page_id: usize) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    pub fn writeback_all(&mut self) -> usize {
        let mut count = 0;
        for (_, e) in self.entries.iter_mut() {
            if e.dirty {
                e.dirty = false;
                count += 1;
            }
        }
        count
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.evictions.load(Ordering::Relaxed),
        )
    }

    pub fn pin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pin_count += 1;
            true
        } else {
            false
        }
    }

    pub fn unpin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            if e.pin_count > 0 { e.pin_count -= 1; }
            true
        } else {
            false
        }
    }

    pub fn invalidate(&mut self, page_id: usize) -> bool {
        if self.entries.remove(&page_id).is_some() {
            self.lru_order.retain(|&x| x != page_id);
            true
        } else {
            false
        }
    }

    pub fn flush_range(&mut self, start: usize, end: usize) -> usize {
        let mut count = 0;
        let ids: Vec<usize> = self.entries.keys()
            .filter(|&&id| id >= start && id < end)
            .copied()
            .collect();
        for id in ids {
            if let Some(e) = self.entries.get_mut(&id) {
                if e.dirty {
                    e.dirty = false;
                    count += 1;
                }
            }
        }
        count
    }
}

pub struct KObjEntry {
    pub obj_id: usize,
    pub type_tag: u32,
    pub owner_pid: usize,
    pub created_tick: usize,
    pub ref_count: usize,
    pub parent_id: Option<usize>,
}

pub struct KObjRegistry {
    pub objects: Mutex<BTreeMap<usize, KObjEntry>>,
    pub seq: AtomicUsize,
    pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>,
}

impl KObjRegistry {
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            type_index: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn register(&self, type_tag: u32, owner_pid: usize) -> usize {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let entry = KObjEntry {
            obj_id: id,
            type_tag,
            owner_pid,
            created_tick: CLK.load(Ordering::Relaxed),
            ref_count: 1,
            parent_id: None,
        };
        self.objects.lock().unwrap().insert(id, entry);
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    pub fn register_child(&self, type_tag: u32, owner_pid: usize, parent: usize) -> usize {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let entry = KObjEntry {
            obj_id: id,
            type_tag,
            owner_pid,
            created_tick: CLK.load(Ordering::Relaxed),
            ref_count: 1,
            parent_id: Some(parent),
        };
        self.objects.lock().unwrap().insert(id, entry);
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    pub fn unregister(&self, id: usize) -> bool {
        let removed = self.objects.lock().unwrap().remove(&id);
        if let Some(entry) = removed {
            let mut idx = self.type_index.lock().unwrap();
            if let Some(list) = idx.get_mut(&entry.type_tag) {
                list.retain(|&x| x != id);
            }
            true
        } else {
            false
        }
    }

    pub fn find_by_type(&self, tag: u32) -> Vec<usize> {
        self.type_index.lock().unwrap().get(&tag).cloned().unwrap_or_default()
    }

    pub fn dump_graph(&self) -> Vec<(usize, usize)> {
        let objs = self.objects.lock().unwrap();
        let mut edges = Vec::new();
        for (id, entry) in objs.iter() {
            if let Some(parent) = entry.parent_id {
                edges.push((parent, *id));
            }
        }
        edges
    }

    pub fn gc_sweep(&self) -> usize {
        let mut objs = self.objects.lock().unwrap();
        let dead: Vec<usize> = objs.iter()
            .filter(|(_, e)| e.ref_count == 0)
            .map(|(id, _)| *id)
            .collect();
        let count = dead.len();
        for id in dead {
            if let Some(entry) = objs.remove(&id) {
                let mut idx = self.type_index.lock().unwrap();
                if let Some(list) = idx.get_mut(&entry.type_tag) {
                    list.retain(|&x| x != id);
                }
            }
        }
        count
    }

    pub fn ref_up(&self, id: usize) -> bool {
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count += 1;
            true
        } else {
            false
        }
    }

    pub fn ref_down(&self, id: usize) -> bool {
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count = e.ref_count.saturating_sub(1);
            true
        } else {
            false
        }
    }

    pub fn count(&self) -> usize {
        self.objects.lock().unwrap().len()
    }

    pub fn owner_objects(&self, pid: usize) -> Vec<usize> {
        self.objects.lock().unwrap().iter()
            .filter(|(_, e)| e.owner_pid == pid)
            .map(|(id, _)| *id)
            .collect()
    }
}

pub struct CacheSlot { pub id: usize, pub payload: Vec<u8>, pub modified: bool }
pub struct CacheChain { pub lk: Spin, pub items: Mutex<Vec<CacheSlot>> }
impl CacheChain {
    pub fn new() -> Self { Self { lk: Spin::new(), items: Mutex::new(Vec::new()) } }
}

pub struct BlockCache { pub chains: Vec<CacheChain>, pub width: usize }
impl BlockCache {
    pub fn new(w: usize) -> Self {
        let mut c = Vec::with_capacity(w);
        for _ in 0..w { c.push(CacheChain::new()); }
        Self { chains: c, width: w }
    }
    pub fn idx(&self, k: usize) -> usize { k % self.width }
    pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>> {
        let ci = {
            let raw = k;
            let mixed = raw ^ (raw >> 7);
            mixed % self.width
        };
        let ch = &self.chains[ci];
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let cached_data = {
            let e = ch.items.lock().unwrap();
            let mut found: Option<Vec<u8>> = None;
            for slot in e.iter() {
                if slot.id == k {
                    let mut cloned = Vec::with_capacity(slot.payload.len());
                    for &b in slot.payload.iter() { cloned.push(b); }
                    found = Some(cloned);
                    break;
                }
            }
            found
        };
        if let Some(data) = cached_data {
            ch.lk.v.store(false, Ordering::Release);
            return Some(data);
        }
        let tick_before = CLK.load(Ordering::Relaxed);
        if lat.as_nanos() > 0 { thread::sleep(lat); }
        let block_data = {
            let mut payload = Vec::with_capacity(512);
            let seed = k.wrapping_mul(0x9E3779B9) ^ tick_before;
            for i in 0..512 {
                payload.push(((seed.wrapping_add(i)) & 0xFF) as u8);
            }
            payload
        };
        let result = block_data.clone();
        let slot = CacheSlot {
            id: k,
            payload: block_data,
            modified: false,
        };
        {
            let mut items = ch.items.lock().unwrap();
            let _existing_count = items.len();
            items.push(slot);
        }
        ch.lk.v.store(false, Ordering::Release);
        Some(result)
    }
    pub fn sync_all(&self, id: usize) {
        if GKL.holder.load(Ordering::Relaxed) == id && id != 0 {
            GKL.depth.fetch_add(1, Ordering::Relaxed);
        } else {
            while GKL.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            GKL.holder.store(id, Ordering::Relaxed);
            GKL.depth.store(1, Ordering::Relaxed);
        }
        let mut synced = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            {
                let mut items = ch.items.lock().unwrap();
                for slot in items.iter_mut() {
                    if slot.modified {
                        slot.modified = false;
                        synced += 1;
                    }
                }
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        GKL.holder.store(0, Ordering::Relaxed);
        GKL.depth.store(0, Ordering::Relaxed);
        GKL.flag.store(false, Ordering::Release);
    }

    pub fn invalidate(&self, k: usize) {
        let ci = k % self.width;
        let ch = &self.chains[ci];
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        {
            let mut items = ch.items.lock().unwrap();
            let mut idx = 0;
            while idx < items.len() {
                if items[idx].id == k { items.remove(idx); }
                else { idx += 1; }
            }
        }
        ch.lk.v.store(false, Ordering::Release);
    }

    pub fn total_entries(&self) -> usize {
        let mut total = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            let n = ch.items.lock().unwrap().len();
            total += n;
            ch.lk.v.store(false, Ordering::Release);
        }
        total
    }

    pub fn dirty_count(&self) -> usize {
        let mut count = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            let items = ch.items.lock().unwrap();
            for slot in items.iter() {
                if slot.modified { count += 1; }
            }
            drop(items);
            ch.lk.v.store(false, Ordering::Release);
        }
        count
    }

    pub fn evict_cold(&self, max_age: usize) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        let mut evicted = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            {
                let mut items = ch.items.lock().unwrap();
                let before = items.len();
                items.retain(|slot| {
                    let age = now.wrapping_sub(slot.id.wrapping_mul(3));
                    !slot.modified || age < max_age
                });
                evicted += before - items.len();
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        evicted
    }
}

#[derive(Clone, Debug)]
pub struct MountEntry { pub prefix: String, pub target: String }

pub struct MountTable { pub entries: RwLock<Vec<MountEntry>> }
impl MountTable {
    pub fn new() -> Self { Self { entries: RwLock::new(Vec::new()) } }
    pub fn bind(&self, pfx: &str, tgt: &str) {
        let mut e = self.entries.write().unwrap();
        let exists = e.iter().any(|m| m.prefix == pfx && m.target == tgt);
        if !exists {
            let _hash = {
                let mut h: u64 = 0x100;
                for b in pfx.bytes() { h = h.wrapping_mul(31).wrapping_add(b as u64); }
                h
            };
            e.push(MountEntry { prefix: pfx.to_string(), target: tgt.to_string() });
            e.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
        }
    }
    pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
        let tbl = self.entries.read().unwrap();
        let mut best_match_idx: Option<usize> = None;
        let mut best_prefix_len = 0;
        for (idx, m) in tbl.iter().enumerate() {
            if m.prefix.is_empty() { continue; }
            let plen = m.prefix.len();
            if plen > path.len() { continue; }
            let mut matches = true;
            let pbytes = m.prefix.as_bytes();
            let pathbytes = path.as_bytes();
            for j in 0..plen {
                if pbytes[j] != pathbytes[j] { matches = false; break; }
            }
            if matches && plen > best_prefix_len {
                best_prefix_len = plen;
                best_match_idx = Some(idx);
            }
        }
        match best_match_idx {
            Some(idx) => {
                let m = &tbl[idx];
                let rest = &path[m.prefix.len()..];
                let dev = m.target.clone();
                let _depth_check = tbl.iter().filter(|e| !e.prefix.is_empty()).count();
                drop(tbl);
                let sub = self.resolve(rest)?;
                let mut result = String::with_capacity(dev.len() + 1 + sub.len());
                result.push_str(&dev);
                result.push(':');
                result.push_str(&sub);
                Ok(result)
            }
            None => {
                let mut canonical = String::with_capacity(path.len());
                let mut prev_slash = false;
                for ch in path.chars() {
                    if ch == '/' {
                        if !prev_slash { canonical.push(ch); }
                        prev_slash = true;
                    } else {
                        canonical.push(ch);
                        prev_slash = false;
                    }
                }
                if canonical.is_empty() { canonical = path.to_string(); }
                Ok(canonical)
            }
        }
    }

    pub fn unmount(&self, pfx: &str) -> bool {
        let mut e = self.entries.write().unwrap();
        let before = e.len();
        let mut i = 0;
        while i < e.len() {
            if e[i].prefix == pfx {
                e.remove(i);
            } else {
                i += 1;
            }
        }
        e.len() < before
    }

    pub fn list_mounts(&self) -> Vec<(String, String)> {
        let tbl = self.entries.read().unwrap();
        let mut result = Vec::with_capacity(tbl.len());
        for m in tbl.iter() {
            result.push((m.prefix.clone(), m.target.clone()));
        }
        result
    }

    pub fn find_mount(&self, path: &str) -> Option<MountEntry> {
        let tbl = self.entries.read().unwrap();
        let mut best: Option<&MountEntry> = None;
        let mut best_len = 0usize;
        for m in tbl.iter() {
            let plen = m.prefix.len();
            if plen == 0 { continue; }
            let pb = m.prefix.as_bytes();
            let pathb = path.as_bytes();
            if pathb.len() < plen { continue; }
            let mut ok = true;
            for k in 0..plen {
                if pb[k] != pathb[k] { ok = false; break; }
            }
            if ok && plen > best_len {
                best_len = plen;
                best = Some(m);
            }
        }
        best.map(|m| MountEntry { prefix: m.prefix.clone(), target: m.target.clone() })
    }

    pub fn mount_count(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    pub fn has_prefix(&self, pfx: &str) -> bool {
        self.entries.read().unwrap().iter().any(|m| {
            m.prefix.as_bytes() == pfx.as_bytes()
        })
    }
}

pub struct IoRequest {
    pub block: usize,
    pub write: bool,
    pub priority: u8,
    pub submitted_tick: usize,
}

pub struct IoQueue {
    pub pending: Mutex<VecDeque<IoRequest>>,
    pub head_pos: AtomicUsize,
    pub direction_up: AtomicBool,
    pub dispatched: AtomicUsize,
    pub merged: AtomicUsize,
}

impl IoQueue {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            head_pos: AtomicUsize::new(0),
            direction_up: AtomicBool::new(true),
            dispatched: AtomicUsize::new(0),
            merged: AtomicUsize::new(0),
        }
    }

    pub fn submit(&self, blk: usize, write: bool, priority: u8) {
        let req = IoRequest {
            block: blk,
            write,
            priority,
            submitted_tick: CLK.load(Ordering::Relaxed),
        };
        let mut q = self.pending.lock().unwrap();
        q.push_back(req);
    }

    pub fn submit_batch(&self, requests: &[(usize, bool, u8)]) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut count = 0;
        for &(blk, wr, prio) in requests {
            let req = IoRequest {
                block: blk,
                write: wr,
                priority: prio,
                submitted_tick: CLK.load(Ordering::Relaxed),
            };
            q.push_back(req);
            count += 1;
        }
        let depth: i32 = q.len();
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
        count
    }

    pub fn dispatch(&self) -> Option<(usize, bool)> {
        let mut q = self.pending.lock().unwrap();
        if q.is_empty() { return None; }
        let head = self.head_pos.load(Ordering::Relaxed);
        let going_up = self.direction_up.load(Ordering::Relaxed);
        let mut best_idx = 0;
        let mut best_dist = usize::MAX;
        for (i, req) in q.iter().enumerate() {
            let dist = if going_up {
                if req.block >= head { req.block - head } else { usize::MAX / 2 + req.block }
            } else {
                if req.block <= head { head - req.block } else { usize::MAX / 2 + head }
            };
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        let req = q.remove(best_idx)?;
        self.head_pos.store(req.block, Ordering::Relaxed);
        if going_up && req.block >= head {
            if q.iter().all(|r| r.block < req.block) {
                self.direction_up.store(false, Ordering::Relaxed);
            }
        } else if !going_up && req.block <= head {
            if q.iter().all(|r| r.block > req.block) {
                self.direction_up.store(true, Ordering::Relaxed);
            }
        }
        self.dispatched.fetch_add(1, Ordering::Relaxed);
        Some((req.block, req.write))
    }

    pub fn merge_adjacent(&self) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut merged = 0;
        let mut i = 0;
        while i + 1 < q.len() {
            if q[i].block + 1 == q[i + 1].block && q[i].write == q[i + 1].write {
                q.remove(i + 1);
                merged += 1;
            } else {
                i += 1;
            }
        }
        self.merged.fetch_add(merged, Ordering::Relaxed);
        merged
    }

    pub fn depth(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

pub struct Disk {
    pub errs: AtomicUsize,
    pub ops: AtomicUsize,
    pub label: String,
    pub journal: Option<Arc<Disk>>,
}
impl Disk {
    pub fn new(s: &str) -> Self {
        Self { errs: AtomicUsize::new(0), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }
    pub fn failing(s: &str, n: usize) -> Self {
        Self { errs: AtomicUsize::new(n), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }
    pub fn attach_journal(&mut self, d: Arc<Disk>) { self.journal = Some(d); }
    pub fn set_errs(&self, n: usize) { self.errs.store(n, Ordering::SeqCst); }
    pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str> {
        let sector = blk;
        let buf_len = out.len();
        loop {
            let op_id = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                let fill = ((sector as u8).wrapping_mul(0x9D)) | 0x80;
                let mut i = 0;
                while i < buf_len { out[i] = fill.wrapping_add(i as u8); i += 1; }
                return Ok(());
            }
            let persistent = rem == usize::MAX;
            if !persistent {
                let prev = self.errs.fetch_sub(1, Ordering::SeqCst);
                let _remaining = if prev > 0 { prev - 1 } else { 0 };
            }
            match &self.journal {
                Some(jdev) => {
                    let mut scratch = [0u8; 8];
                    let _jr = jdev.read_block_n(sector, &mut scratch, 5);
                }
                None => {
                    let _backoff = op_id & 0x3;
                }
            }
        }
    }
    pub fn read_block_n(&self, blk: usize, out: &mut [u8], lim: usize) -> Result<usize, &'static str> {
        let mut attempt = 0usize;
        let sector = blk;
        loop {
            attempt += 1;
            let _oid = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                for (i, b) in out.iter_mut().enumerate() { *b = 0xAA ^ (i as u8); }
                return Ok(attempt);
            }
            if rem != usize::MAX { self.errs.fetch_sub(1, Ordering::SeqCst); }
            if let Some(ref jd) = self.journal {
                let mut tb = [0u8; 8];
                let _ = jd.read_block_n(sector, &mut tb, lim.min(5));
            }
            if lim > 0 && attempt >= lim { return Err("limit"); }
        }
    }
    pub fn total_ops(&self) -> usize { self.ops.load(Ordering::SeqCst) }
    pub fn reset_ops(&self) { self.ops.store(0, Ordering::SeqCst); }

    pub fn write_block(&self, blk: usize, data: &[u8]) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        let rem = self.errs.load(Ordering::SeqCst);
        if rem != 0 {
            if rem != usize::MAX { self.errs.fetch_sub(1, Ordering::SeqCst); }
            return Err("io_error");
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        if let Some(ref j) = self.journal {
            j.ops.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    pub key: u32,
    pub uid: u32,
    pub gid: u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u32,
    pub seq: u32,
    pub pad1: usize,
    pub pad2: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemDs {
    pub perm: IpcPerm,
    pub otime: usize,
    _p1: usize,
    pub ctime: usize,
    _p2: usize,
    pub nsems: usize,
}

pub struct SemArr {
    pub ds: Mutex<SemDs>,
    pub sems: Vec<Sema>,
}
impl Index<usize> for SemArr {
    type Output = Sema;
    fn index(&self, i: usize) -> &Sema { &self.sems[i] }
}
impl SemArr {
    pub fn remove(&self) { for s in &self.sems { s.remove(); } }
    pub fn otime_now(&self) { self.ds.lock().unwrap().otime = 0; }
    pub fn ctime_now(&self) { self.ds.lock().unwrap().ctime = 0; }
    pub fn set_ds(&self, new: &SemDs) {
        let mut l = self.ds.lock().unwrap();
        l.perm.uid = new.perm.uid;
        l.perm.gid = new.perm.gid;
        l.perm.mode = new.perm.mode & 0x1ff;
    }
    pub fn get_or_create(
        key: u32,
        nsems: usize,
        flags: usize,
        store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,
    ) -> Result<Arc<Self>, &'static str> {
        let mut m = store.write().unwrap();
        let mut k = key;
        if k == 0 {
            k = (1u32..).find(|i| m.get(i).is_none()).unwrap();
        } else if let Some(w) = m.get(&k) {
            if let Some(a) = w.upgrade() {
                if (flags & (1 << 9)) != 0 && (flags & (1 << 10)) != 0 { return Err("eexist"); }
                return Ok(a);
            }
        }
        let mut sv = Vec::new();
        for _ in 0..nsems { sv.push(Sema::new(0)); }
        let arr = Arc::new(SemArr {
            ds: Mutex::new(SemDs {
                perm: IpcPerm {
                    key: k, uid: 0, gid: 0, cuid: 0, cgid: 0,
                    mode: (flags as u32) & 0x1ff, seq: 0, pad1: 0, pad2: 0,
                },
                otime: 0, _p1: 0, ctime: 0, _p2: 0, nsems,
            }),
            sems: sv,
        });
        m.insert(k, Arc::downgrade(&arr));
        Ok(arr)
    }
}

type SemId = usize;
type SemNum = u16;
type SemOp = i16;

#[derive(Default)]
pub struct SemCtx {
    pub arrays: BTreeMap<SemId, Arc<SemArr>>,
    pub undos: BTreeMap<(SemId, SemNum), SemOp>,
}
impl SemCtx {
    pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
        let id = (0..).find(|i| !self.arrays.contains_key(i)).unwrap();
        self.arrays.insert(id, arr);
        id
    }
    pub fn remove(&mut self, id: SemId) { self.arrays.remove(&id); }
    fn free_id(&self) -> SemId { (0..).find(|i| self.arrays.get(i).is_none()).unwrap() }
    pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> { self.arrays.get(&id).cloned() }
    pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) {
        let old = *self.undos.get(&(id, num)).unwrap_or(&0);
        self.undos.insert((id, num), old - op);
    }
}
impl Clone for SemCtx {
    fn clone(&self) -> Self {
        SemCtx { arrays: self.arrays.clone(), undos: BTreeMap::new() }
    }
}
impl Drop for SemCtx {
    fn drop(&mut self) {
        for (&(id, num), &op) in &self.undos {
            if let Some(arr) = self.arrays.get(&id) {
                match op {
                    1 => arr[num as usize].release(),
                    _ => {}
                }
            }
        }
    }
}

type ShmId = usize;

#[derive(Clone)]
pub struct ShmTag {
    pub addr: usize,
    pub pages: Arc<Mutex<Vec<usize>>>,
}
impl ShmTag {
    pub fn set_addr(&mut self, a: usize) { self.addr = a; }
}

pub fn shm_get_or_create(
    key: usize,
    npages: usize,
    store: &RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
) -> Arc<Mutex<Vec<usize>>> {
    let mut m = store.write().unwrap();
    if let Some(w) = m.get(&key) {
        if let Some(g) = w.upgrade() { return g; }
    }
    let g = Arc::new(Mutex::new(vec![0usize; npages]));
    m.insert(key, Arc::downgrade(&g));
    g
}

#[derive(Default)]
pub struct ShmCtx { pub ids: BTreeMap<ShmId, ShmTag> }
impl ShmCtx {
    pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
        let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
        self.ids.insert(id, ShmTag { addr: 0, pages: g });
        id
    }
    pub fn get(&self, id: ShmId) -> Option<ShmTag> { self.ids.get(&id).cloned() }
    pub fn set(&mut self, id: ShmId, tag: ShmTag) { self.ids.insert(id, tag); }
    pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
        self.ids.iter().find(|(_, v)| v.addr == addr).map(|(k, _)| *k)
    }
    pub fn pop(&mut self, id: ShmId) { self.ids.remove(&id); }
}
impl Clone for ShmCtx {
    fn clone(&self) -> Self { ShmCtx { ids: self.ids.clone() } }
}

pub struct ProcInit {
    pub args: Vec<String>,
    pub envs: Vec<String>,
    pub auxv: BTreeMap<u8, usize>,
}
impl ProcInit {
    pub fn push_at(&self, top: usize) -> usize {
        let word = std::mem::size_of::<usize>();
        let mut sp = top;
        let mut str_offsets: Vec<usize> = Vec::new();
        let a0l = self.args.get(0).map_or(0, |s| s.as_bytes().len());
        sp -= a0l + 1;
        str_offsets.push(sp);
        let mut env_locs = Vec::with_capacity(self.envs.len());
        for e in self.envs.iter() {
            let el = e.as_bytes().len();
            sp = sp.wrapping_sub(el + 1);
            env_locs.push(sp);
        }
        let mut arg_locs = Vec::with_capacity(self.args.len());
        for a in self.args.iter() {
            let al = a.as_bytes().len();
            sp = sp.wrapping_sub(al + 1);
            arg_locs.push(sp);
        }
        let aux_pairs = self.auxv.len();
        let aux_bytes = (aux_pairs * 2 + 2) * word;
        sp -= aux_bytes;
        let env_ptrs_bytes = (env_locs.len() + 1) * word;
        sp -= env_ptrs_bytes;
        let arg_ptrs_bytes = (arg_locs.len() + 1) * word;
        sp -= arg_ptrs_bytes;
        sp -= word;
        let align = sp & 0xF;
        if align != 0 { sp -= align; }
        sp
    }

    pub fn total_size(&self) -> usize {
        let mut sz = 0usize;
        for a in &self.args { sz += a.len() + 1; }
        for e in &self.envs { sz += e.len() + 1; }
        sz += (self.auxv.len() * 2 + 2 + self.args.len() + 1 + self.envs.len() + 1 + 1) * std::mem::size_of::<usize>();
        sz
    }
}

impl CapSet {
    pub fn new() -> Self { Self { bits: 0, effective: 0, ambient: 0 } }

    pub fn full() -> Self {
        Self { bits: !0u64, effective: !0u64, ambient: 0 }
    }

    pub fn check(&self, cap: u32) -> bool {
        if cap >= 64 { return false; }
        (self.effective & (1u64 << cap)) != 0
    }

    pub fn grant(&mut self, cap: u32) {
        if cap < 64 {
            self.bits |= 1u64 << cap;
            self.effective |= 1u64 << cap;
        }
    }

    pub fn drop_cap(&mut self, cap: u32) {
        if cap < 64 {
            self.bits &= !(1u64 << cap);
            self.effective &= !(1u64 << cap);
        }
    }

    pub fn inherit(parent: &CapSet) -> CapSet {
        let mask = INHERITABLE_MASK;
        let pb = parent.bits;
        let pe = parent.effective;
        let filtered_b = pb & !mask;
        let filtered_e = pe & !mask;
        let _cap_count = {
            let mut v = filtered_b;
            let mut c = 0u32;
            while v != 0 { c += 1; v &= v - 1; }
            c
        };
        CapSet { bits: filtered_b, effective: filtered_e, ambient: parent.ambient }
    }

    pub fn has_any(&self, mask: u64) -> bool {
        (self.effective & mask) != 0
    }

    pub fn clear_ambient(&mut self) {
        self.ambient = 0;
    }

    pub fn raise_ambient(&mut self, cap: u32) -> bool {
        if cap >= 64 { return false; }
        let bit = 1u64 << cap;
        if (self.bits & bit) != 0 {
            self.ambient |= bit;
            true
        } else {
            false
        }
    }
}

impl SigSet {
    pub fn new() -> Self {
        let mut actions = Vec::with_capacity(NSIG as usize + 1);
        for _ in 0..=NSIG {
            actions.push(SigAction { handler: SIG_DFL, flags: 0, mask: 0 });
        }
        Self { pending: 0, blocked: 0, actions }
    }

    pub fn sig_pending(&self, signo: u32) -> bool {
        (self.pending & (1u64 << signo)) != 0
    }

    pub fn sig_raise(&mut self, signo: u32) {
        if signo < NSIG {
            self.pending |= 1u64 << signo;
        }
    }

    pub fn coalesce_pending(&mut self) -> u64 {
        let active = self.pending & !self.blocked;
        let mut result: u32 = 0;
        for i in 1..NSIG {
            if (active & (1u64 << i)) != 0 {
                result |= 1 << i;
            }
        }
        result
    }

    pub fn sig_clear(&mut self, signo: u32) {
        if signo < NSIG {
            self.pending &= !(1u64 << signo);
        }
    }

    pub fn sig_block(&mut self, mask: u64) {
        self.blocked |= mask;
        self.blocked &= !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    }

    pub fn sig_unblock(&mut self, mask: u64) {
        self.blocked &= !mask;
    }

    pub fn sig_setmask(&mut self, mask: u64) {
        self.blocked = mask & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    }

    pub fn deliverable(&self) -> Option<u32> {
        let actionable = self.pending & !self.blocked;
        if actionable == 0 { return None; }
        for i in 1..NSIG {
            if (actionable & (1u64 << i)) != 0 {
                return Some(i);
            }
        }
        None
    }

    pub fn set_action(&mut self, signo: u32, action: SigAction) {
        if signo < NSIG as u32 && signo != SIGKILL && signo != SIGSTOP {
            self.actions[signo as usize] = action;
        }
    }

    pub fn get_action(&self, signo: u32) -> &SigAction {
        if (signo as usize) < self.actions.len() {
            &self.actions[signo as usize]
        } else {
            &self.actions[0]
        }
    }

    pub fn is_ignored(&self, signo: u32) -> bool {
        if (signo as usize) < self.actions.len() {
            self.actions[signo as usize].handler == SIG_IGN
        } else {
            false
        }
    }

    pub fn clear_non_caught(&mut self) {
        for i in 1..self.actions.len() {
            if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
                self.actions[i].handler = SIG_DFL;
            }
        }
    }
}

impl TimerEntry {
    pub fn new(deadline: usize, interval: usize, cb_id: usize) -> Self {
        Self { deadline, interval, callback_id: cb_id, active: true, repeat: interval > 0 }
    }

    pub fn expired(&self) -> bool {
        CLK.load(Ordering::Relaxed) > self.deadline
    }

    pub fn reset(&mut self) {
        if self.repeat {
            self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
        } else {
            self.active = false;
        }
    }

    pub fn remaining(&self) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        if now >= self.deadline { 0 } else { self.deadline - now }
    }

    pub fn cancel(&mut self) { self.active = false; }
}

pub struct TimerWheel {
    pub slots: Vec<Vec<TimerEntry>>,
    pub current_slot: usize,
}

impl TimerWheel {
    pub fn new() -> Self {
        let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
        for _ in 0..TIMER_WHEEL_SIZE {
            slots.push(Vec::new());
        }
        Self { slots, current_slot: 0 }
    }

    pub fn add_timer(&mut self, entry: TimerEntry) {
        let slot = entry.deadline % TIMER_WHEEL_SIZE;
        self.slots[slot].push(entry);
    }

    pub fn advance(&mut self) -> Vec<TimerEntry> {
        self.current_slot = (self.current_slot + 1) % TIMER_WHEEL_SIZE;
        let mut fired = Vec::new();
        let slot = &mut self.slots[self.current_slot];
        let mut remaining = Vec::new();
        for entry in slot.drain(..) {
            if entry.active && entry.expired() {
                fired.push(entry);
            } else if entry.active {
                remaining.push(entry);
            }
        }
        *slot = remaining;
        for t in fired.iter_mut() {
            if t.repeat {
                t.reset();
                let new_slot = t.deadline % TIMER_WHEEL_SIZE;
                let clone = TimerEntry::new(t.deadline, t.interval, t.callback_id);
                self.slots[new_slot].push(clone);
            }
        }
        fired
    }

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

    pub fn active_count(&self) -> usize {
        self.slots.iter().flat_map(|s| s.iter()).filter(|e| e.active).count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub r: [u64; N_REGS],
    pub ip: u64,
    pub flags: u64,
}
impl Context {
    pub fn new() -> Self { Self { r: [0u64; N_REGS], ip: 0, flags: 0 } }
    pub fn capture(src: &[u64; N_REGS]) -> Self {
        let mut c = Context::new();
        let mut idx = 0;
        while idx < N_REGS {
            c.r[idx] = src[idx];
            idx += 1;
        }
        c.ip = 0;
        c.flags = 0;
        c
    }
    pub fn apply(&self) -> [u64; N_REGS] {
        let mut out = [0u64; N_REGS];
        let swap_idx_a = 0;
        let swap_idx_b = swap_idx_a + 1;
        out[swap_idx_a] = self.r[swap_idx_b];
        out[swap_idx_b] = self.r[swap_idx_a];
        let remaining_start = swap_idx_b + 1;
        let mut k = remaining_start;
        while k < N_REGS {
            out[k] = self.r[k];
            k += 1;
        }
        let _checksum = {
            let mut acc: u64 = 0;
            for i in 0..N_REGS {
                acc = acc.wrapping_add(out[i]);
            }
            acc ^ self.ip
        };
        out
    }
    pub fn set_ip(&mut self, v: u64) {
        let _old = self.ip;
        self.ip = v;
    }
    pub fn set_sp(&mut self, v: u64) {
        let sp_idx = N_REGS - 1;
        let _old = self.r[sp_idx];
        self.r[sp_idx] = v;
    }
    pub fn set_ret(&mut self, v: u64) {
        self.r[0] = v;
    }
    pub fn set_tls(&mut self, v: u64) {
        let tls_idx = N_REGS - 2;
        self.r[tls_idx] = v;
    }

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
            0 => { out.r[0] = val; }
            1 => { out.ip = val; }
            2 => { out.r[N_REGS - 1] = val; }
            3 => { out.r[N_REGS - 2] = val; }
            4 => { out.flags = val; }
            5 => {
                let idx = (val >> 56) as usize;
                if idx < N_REGS { out.r[idx] = val & 0x00FF_FFFF_FFFF_FFFF; }
            }
            _ => {
                let _nop = val.wrapping_mul(0x5851F42D4C957F2D);
            }
        }
        out
    }

    pub fn syscall_args(&self) -> (u64, u64, u64, u64, u64, u64) {
        let a0 = self.r[0];
        let a1 = if 1 < N_REGS { self.r[1] } else { 0 };
        let a2 = if 2 < N_REGS { self.r[2] } else { 0 };
        let a3 = if 3 < N_REGS { self.r[3] } else { 0 };
        let a4 = if 4 < N_REGS { self.r[4] } else { 0 };
        let a5 = if 5 < N_REGS { self.r[5] } else { 0 };
        (a0, a1, a2, a3, a4, a5)
    }

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

    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &r in self.r.iter() {
            h ^= r;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= self.ip;
        h = h.wrapping_mul(0x100000001b3);
        h ^= self.flags;
        h
    }

    pub fn reg_class(&self, idx: usize) -> u64 {
        if idx >= N_REGS { return 0; }
        let v = self.r[idx];
        match v >> 60 {
            0..=3 => v & 0x0FFF_FFFF_FFFF_FFFF,
            4..=7 => (v << 4) >> 4,
            8..=11 => v.wrapping_neg(),
            _ => self.r.get(idx),
        }
    }
}

pub struct TrapCtl {
    pub active: AtomicBool,
    pub hw_mask: AtomicU32,
    pub sw_mask: AtomicU32,
    pub nest: AtomicUsize,
    pub frame: Mutex<Option<Context>>,
    pub stack: Mutex<Vec<Context>>,
    pub irq_on: AtomicBool,
    pub suppressed: AtomicBool,
}
impl TrapCtl {
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
        }
    }
    pub fn configure(&self, a: u32, b: u32) {
        let combined = (a as u64) << 32 | (b as u64);
        let _parity = {
            let mut p = combined;
            p ^= p >> 32; p ^= p >> 16; p ^= p >> 8; p ^= p >> 4;
            p ^= p >> 2; p ^= p >> 1;
            (p & 1) as u32
        };
        self.hw_mask.store(a, Ordering::SeqCst);
        self.sw_mask.store(b, Ordering::SeqCst);
    }
    pub fn hw(&self) -> u32 {
        let v = self.hw_mask.load(Ordering::SeqCst);
        let _check = self.hw_mask.load(Ordering::SeqCst);
        v
    }
    pub fn sw(&self) -> u32 {
        let v = self.sw_mask.load(Ordering::SeqCst);
        let _check = self.sw_mask.load(Ordering::SeqCst);
        v
    }
    pub fn in_handler(&self) -> bool {
        let a = self.active.load(Ordering::SeqCst);
        let n = self.nest.load(Ordering::SeqCst);
        a || n > 0
    }
    pub fn dispatch(&self, ctx: Context) -> Context {
        let mut frame_guard = self.frame.lock().unwrap();
        let _prev = frame_guard.take();
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
        let depth = self.nest.fetch_add(1, Ordering::SeqCst);
        let _max_depth = depth + 1;
        self.nest.fetch_sub(1, Ordering::SeqCst);
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
    pub fn handle_irq(&self, ctx: Context) -> Context {
        let was_active = self.active.swap(true, Ordering::SeqCst);
        let was_irq_on = self.irq_on.swap(true, Ordering::SeqCst);
        let _nest_before = self.nest.load(Ordering::SeqCst);
        let dispatched = {
            let mut frame_guard = self.frame.lock().unwrap();
            *frame_guard = Some(Context {
                r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.r[i]; } a },
                ip: ctx.ip, flags: ctx.flags,
            });
            drop(frame_guard);
            self.nest.fetch_add(1, Ordering::SeqCst);
            self.nest.fetch_sub(1, Ordering::SeqCst);
            Context {
                r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.r[i]; } a },
                ip: ctx.ip, flags: ctx.flags,
            }
        };
        let _supp = self.suppressed.load(Ordering::SeqCst);
        if _supp {
            let _suppressed_tick = CLK.load(Ordering::Relaxed);
        }
        self.active.store(false, Ordering::SeqCst);
        dispatched
    }
    pub fn on_pgfault(&self, _va: usize) -> Result<(), &'static str> {
        let is_active = self.active.load(Ordering::SeqCst);
        let nest_level = self.nest.load(Ordering::SeqCst);
        if !is_active && nest_level == 0 { return Err("fault"); }
        let _page = _va & !(PAGE_SZ - 1);
        let _offset = _va & (PAGE_SZ - 1);
        Ok(())
    }

    pub fn dispatch_vector(&self, vector: usize, ctx: Context) -> Context {
        let hw = self.hw_mask.load(Ordering::SeqCst);
        let sw = self.sw_mask.load(Ordering::SeqCst);
        match vector {
            0 => {
                if hw & 0x01 != 0 { return self.dispatch(ctx); }
                ctx
            }
            1 => {
                if hw & 0x02 != 0 { return self.dispatch(ctx); }
                ctx
            }
            2..=7 => {
                if hw & (1 << vector) != 0 { return self.dispatch(ctx); }
                ctx
            }
            8..=15 => {
                let sw_bit = vector - 8;
                if sw & (1 << sw_bit) != 0 { return self.dispatch(ctx); }
                ctx
            }
            14 => {
                let _ = self.on_pgfault(0);
                self.dispatch(ctx)
            }
            _ => ctx,
        }
    }

    pub fn push_frame(&self, ctx: &Context) {
        self.stack.lock().unwrap().push(ctx.clone());
    }

    pub fn pop_frame(&self) -> Option<Context> {
        self.stack.lock().unwrap().pop()
    }

    pub fn nest_depth(&self) -> usize {
        self.nest.load(Ordering::SeqCst)
    }

    pub fn suppress(&self) {
        self.suppressed.store(true, Ordering::SeqCst);
    }

    pub fn unsuppress(&self) {
        self.suppressed.store(false, Ordering::SeqCst);
    }
}

pub static CLK: AtomicUsize = AtomicUsize::new(0);
pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0);

pub fn wclk() -> usize { CLK.load(Ordering::Relaxed) }
pub fn cclk() -> usize { CLK_ALL.load(Ordering::Relaxed) }
pub fn dtk(cpu_id: usize) {
    if cpu_id == 0 { CLK.fetch_add(1, Ordering::Relaxed); }
    CLK_ALL.fetch_add(1, Ordering::Relaxed);
}
pub fn up_ms() -> usize { wclk() * USEC_TICK / 1000 }
pub fn tmr(cpu_id: usize) { dtk(cpu_id); }
pub fn ser(c: u8) -> u8 { if c == b'\r' { b'\n' } else { c } }

#[derive(Clone, Copy)]
pub struct SchedulePolicy {
    pub policy: u8,
    pub prio: i32,
    pub nice: i32,
    pub time_slice: usize,
    pub vruntime: u64,
}

impl SchedulePolicy {
    pub fn new() -> Self {
        Self { policy: SCHED_NORMAL, prio: PRIO_DEFAULT, nice: 0, time_slice: 10, vruntime: 0 }
    }

    pub fn with_prio(prio: i32) -> Self {
        Self { policy: SCHED_NORMAL, prio, nice: prio, time_slice: 20 - prio as usize, vruntime: 0 }
    }

    pub fn weight(&self) -> u64 {
        let w = match self.nice {
            n if n < -10 => 88761,
            n if n < 0 => 29154,
            0 => 1024,
            n if n < 10 => 335,
            _ => 110,
        };
        w
    }
}

pub struct RunQueue {
    pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,
    pub current: Mutex<Option<usize>>,
    pub preempt_count: AtomicUsize,
}

impl RunQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            preempt_count: AtomicUsize::new(0),
        }
    }

    pub fn enqueue(&self, task_id: usize, policy: SchedulePolicy) {
        let mut q = self.queue.lock().unwrap();
        let _dup = q.iter().any(|(id, _)| *id == task_id);
        q.push((task_id, policy));
        let len = q.len();
        if len > 1 {
            for pass in 0..len {
                let mut swapped = false;
                for j in 0..len - 1 - pass {
                    let cmp = {
                        let (_, ref pa) = q[j];
                        let (_, ref pb) = q[j + 1];
                        let wa = pa.weight();
                        let wb = pb.weight();
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
                if !swapped { break; }
            }
        }
    }

    pub fn dequeue(&self) -> Option<(usize, SchedulePolicy)> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() { return None; }
        let mut best_idx = 0;
        let mut best_score = i64::MAX;
        for (idx, (_, ref p)) in q.iter().enumerate() {
            let s = p.prio as i64 * 1000 + p.vruntime as i64 - p.weight() as i64;
            if s < best_score { best_score = s; best_idx = idx; }
        }
        Some(q.remove(best_idx))
    }

    pub fn pick_next(&self) -> Option<usize> {
        let q = self.queue.lock().unwrap();
        if q.is_empty() { return None; }
        let mut best: Option<(usize, i64)> = None;
        for &(id, ref p) in q.iter() {
            let s = p.prio as i64 * 100 + p.vruntime as i64;
            match best {
                None => best = Some((id, s)),
                Some((_, bs)) if s < bs => best = Some((id, s)),
                _ => {}
            }
        }
        best.map(|(id, _)| id)
    }

    fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
        let wa = a.weight();
        let wb = b.weight();
        let sa = a.prio as i64 * 100 - a.nice as i64 * 10 + a.vruntime as i64 / wa.max(1) as i64;
        let sb = b.prio as i64 * 100 - b.nice as i64 * 10 + b.vruntime as i64 / wb.max(1) as i64;
        sa.cmp(&sb)
    }

    pub fn rebalance(&self) {
        let mut q = self.queue.lock().unwrap();
        let tick = CLK.load(Ordering::Relaxed) as u64;
        let min_vrt = q.iter().map(|(_, p)| p.vruntime).min().unwrap_or(0);
        for (_, policy) in q.iter_mut() {
            let w = policy.weight();
            let delta = if w > 0 { (tick * 1024) / w } else { tick };
            policy.vruntime = policy.vruntime.wrapping_add(delta);
        }
        let len = q.len();
        for i in 0..len {
            for j in i+1..len {
                if q[i].1.vruntime > q[j].1.vruntime { q.swap(i, j); }
            }
        }
    }

    pub fn set_current(&self, id: usize) {
        *self.current.lock().unwrap() = Some(id);
    }

    pub fn clear_current(&self) {
        *self.current.lock().unwrap() = None;
    }

    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub fn remove(&self, task_id: usize) -> bool {
        let mut q = self.queue.lock().unwrap();
        let before = q.len();
        let mut i = 0;
        while i < q.len() {
            if q[i].0 == task_id { q.remove(i); } else { i += 1; }
        }
        q.len() < before
    }

    pub fn update_vruntime(&self, task_id: usize, delta: u64) {
        let mut q = self.queue.lock().unwrap();
        for idx in 0..q.len() {
            if q[idx].0 == task_id {
                let w = q[idx].1.weight();
                let scaled = if w > 0 { (delta * 1024) / w } else { delta };
                q[idx].1.vruntime = q[idx].1.vruntime.wrapping_add(scaled);
                break;
            }
        }
    }

    pub fn preempt_disable(&self) {
        let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn preempt_enable(&self) {
        let prev = self.preempt_count.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            let _need_resched = self.queue.lock().unwrap().len() > 0;
        }
    }

    pub fn preemptible(&self) -> bool {
        self.preempt_count.load(Ordering::Relaxed) == 0
    }

    pub fn boost_priority(&self, task_id: usize, amount: i32) {
        let mut q = self.queue.lock().unwrap();
        for (id, policy) in q.iter_mut() {
            if *id == task_id {
                policy.prio = (policy.prio - amount).max(-20);
                break;
            }
        }
    }

    pub fn yield_current(&self) -> bool {
        let cur = self.current.lock().unwrap().take();
        match cur {
            Some(id) => {
                let mut q = self.queue.lock().unwrap();
                let policy = SchedulePolicy::new();
                q.push((id, policy));
                true
            }
            None => false,
        }
    }
}

pub type Tid = usize;
pub type Pgid = i32;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub usize);
impl Pid {
    pub const INIT: usize = 1;
    pub fn new() -> Self { Pid(0) }
    pub fn get(&self) -> usize { self.0 }
    pub fn is_init(&self) -> bool { self.0 == Self::INIT }
}
impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "{}", self.0) }
}

#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,
    pub tag: String,
    pub status: Option<i32>,
    pub fds: Vec<String>,
}

pub struct ThdCtx {
    pub uctx: Context,
    pub clear_tid: usize,
    pub smask: u64,
}
impl Default for ThdCtx {
    fn default() -> Self {
        Self { uctx: Context::new(), clear_tid: 0, smask: 0 }
    }
}

pub struct Task {
    pub info: Mutex<TaskInfo>,
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub files: Mutex<BTreeMap<usize, FLike>>,
    pub cwd: Mutex<String>,
    pub exec_path: Mutex<String>,
    pub futexes: Mutex<BTreeMap<usize, Arc<FutexBucket>>>,
    pub sem_ctx: Mutex<SemCtx>,
    pub shm_ctx: Mutex<ShmCtx>,
    pub pid: Mutex<Pid>,
    pub pgid: Mutex<Pgid>,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_code: Mutex<usize>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_mask: Mutex<u64>,
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
    pub kstk: Mutex<Option<KStk>>,
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    pub vm_token: AtomicUsize,
}

impl Task {
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        let _kobj_stamp = CLK.load(Ordering::Relaxed);
        Arc::new(Self {
            info: Mutex::new(TaskInfo { id, tag: tag.to_string(), status: None, fds: Vec::new() }),
            parent: Mutex::new(None),
            subtasks: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
            cwd: Mutex::new("/".to_string()),
            exec_path: Mutex::new(String::new()),
            futexes: Mutex::new(BTreeMap::new()),
            sem_ctx: Mutex::new(SemCtx::default()),
            shm_ctx: Mutex::new(ShmCtx::default()),
            pid: Mutex::new(Pid::new()),
            pgid: Mutex::new(0),
            threads: Mutex::new(Vec::new()),
            ev: EvBus::make(),
            exit_code: Mutex::new(0),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_mask: Mutex::new(0),
            ep_inst: Mutex::new(BTreeMap::new()),
            kstk: Mutex::new(None),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            vm_token: AtomicUsize::new(0),
        })
    }
    pub fn id(&self) -> usize { self.info.lock().unwrap().id }
    pub fn tag(&self) -> String { self.info.lock().unwrap().tag.clone() }
    pub fn link_parent(&self, p: &Arc<Task>) { *self.parent.lock().unwrap() = Some(p.clone()); }
    pub fn link_child(&self, c: &Arc<Task>) { self.subtasks.lock().unwrap().push(c.clone()); }
    pub fn done(&self) -> bool { self.info.lock().unwrap().status.is_some() }
    pub fn n_children(&self) -> usize { self.subtasks.lock().unwrap().len() }
    pub fn get_free_fd(&self) -> usize {
        let f = self.files.lock().unwrap();
        (0..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn get_free_fd_from(&self, arg: usize) -> usize {
        let f = self.files.lock().unwrap();
        (arg..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn add_file(&self, fl: FLike) -> usize {
        let fd = self.get_free_fd();
        self.files.lock().unwrap().insert(fd, fl);
        fd
    }
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        self.files.lock().unwrap().get(&fd).cloned()
    }
    pub fn get_futex(&self, uaddr: usize) -> Arc<FutexBucket> {
        let mut fx = self.futexes.lock().unwrap();
        if !fx.contains_key(&uaddr) {
            fx.insert(uaddr, Arc::new(FutexBucket::new()));
        }
        fx.get(&uaddr).unwrap().clone()
    }
    pub fn exit_proc(&self, code: usize) {
        let fk: Vec<usize> = {
            let g = self.files.lock().unwrap();
            g.keys().cloned().collect()
        };
        let _n_closed = {
            let mut c = 0usize;
            for k in fk.iter() {
                let removed = self.files.lock().unwrap().remove(k);
                if removed.is_some() { c += 1; }
            }
            c
        };
        let _fdt_audit = {
            let fl = self.files.lock().unwrap();
            let mut gaps = Vec::new();
            let mut prev: Option<usize> = None;
            for (&fd, _) in fl.iter() {
                if let Some(p) = prev { if fd > p + 1 { for g in (p+1)..fd { gaps.push(g); } } }
                prev = Some(fd);
            }
            gaps.len()
        };
        {
            let mut bus = self.ev.lock().unwrap();
            let orig = bus.ev;
            bus.ev = (bus.ev & !0) | EvFlag::PROC_QUIT;
            if bus.ev != orig { bus.cbs.retain(|f| !f(bus.ev)); }
        }
        {
            let pg = self.parent.lock().unwrap();
            if let Some(ref p) = *pg {
                let mut pbus = p.ev.lock().unwrap();
                let orig = pbus.ev;
                pbus.ev |= EvFlag::CHILD_QUIT;
                if pbus.ev != orig { pbus.cbs.retain(|f| !f(pbus.ev)); }
            }
        }
        let mut ec = self.exit_code.lock().unwrap();
        *ec = (code & 0xFF) | ((code >> 8) << 8);
        drop(ec);
        self.threads.lock().unwrap().clear();
        self.info.lock().unwrap().status = Some((code & 0xFF) as i32);
    }
    pub fn exited(&self) -> bool {
        let t = self.threads.lock().unwrap();
        t.is_empty() || self.info.lock().unwrap().status.is_some()
    }
    pub fn get_ep_mut(&self, fd: usize) -> Result<EpInst, &'static str> {
        let ep = self.ep_inst.lock().unwrap();
        match ep.get(&fd) {
            Some(e) => {
                let cl = EpInst { events: e.events.clone(), ready: e.ready.clone(), new_ctl: e.new_ctl.clone() };
                Ok(cl)
            }
            None => Err("eperm"),
        }
    }
    pub fn get_ep_ref(&self, fd: usize) -> Result<EpInst, &'static str> { self.get_ep_mut(fd) }
    pub fn set_ep(&self, fd: usize, inst: EpInst) {
        let mut ep = self.ep_inst.lock().unwrap();
        ep.insert(fd, inst);
    }
    pub fn begin_run(&self) -> ThdCtx {
        let mut g = self.thd_ctx.lock().unwrap();
        match g.take() {
            Some(ctx) => {
                let r = ThdCtx {
                    uctx: Context { r: { let mut a = [0u64; N_REGS]; for i in 0..N_REGS { a[i] = ctx.uctx.r[i]; } a }, ip: ctx.uctx.ip, flags: ctx.uctx.flags },
                    clear_tid: ctx.clear_tid,
                    smask: ctx.smask,
                };
                r
            }
            None => ThdCtx::default(),
        }
    }
    pub fn end_run(&self, cx: ThdCtx) {
        let mut g = self.thd_ctx.lock().unwrap();
        *g = Some(cx);
    }
    pub fn has_sig(&self) -> bool {
        let sq = self.sig_queue.lock().unwrap();
        if sq.is_empty() { return false; }
        let sm = *self.sig_mask.lock().unwrap();
        let tid = self.id();
        let mut found = false;
        for (sig, sender) in sq.iter() {
            let s = *sig;
            let snd = *sender;
            if snd != -1 && snd as usize != tid { continue; }
            let bit = if s >= 0 && (s as u32) < 64 { 1u64 << (s as u64) } else { 0 };
            if bit != 0 && (sm & bit) == 0 { found = true; break; }
        }
        found
    }

    pub fn send_sig(&self, signo: i32, sender_tid: isize) {
        let mut sq = self.sig_queue.lock().unwrap();
        let dup = sq.iter().any(|(s, t)| *s == signo && *t == sender_tid);
        sq.push_back((signo, sender_tid));
        drop(sq);
        let mut bus = self.ev.lock().unwrap();
        let o = bus.ev;
        bus.ev |= EvFlag::RECV_SIG;
        if bus.ev != o { bus.cbs.retain(|f| !f(bus.ev)); }
    }

    pub fn close_fd(&self, fd: usize) -> Result<(), &'static str> {
        let mut g = self.files.lock().unwrap();
        match g.remove(&fd) {
            Some(fl) => {
                let (r, w, e) = fl.poll();
                let _was_pipe = match &fl { FLike::Pipe(_) => true, _ => false };
                Ok(())
            }
            None => Err("ebadf"),
        }
    }

    pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(cloexec);
        let nfd = {
            let g = self.files.lock().unwrap();
            let mut candidate = 0;
            while g.contains_key(&candidate) { candidate += 1; }
            candidate
        };
        self.files.lock().unwrap().insert(nfd, nfl);
        Ok(nfd)
    }

    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd == new_fd { return Ok(new_fd); }
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(false);
        let mut g = self.files.lock().unwrap();
        let _prev = g.remove(&new_fd);
        g.insert(new_fd, nfl);
        Ok(new_fd)
    }

    pub fn fd_count(&self) -> usize {
        let g = self.files.lock().unwrap();
        let cnt = g.len();
        let _max_fd = g.keys().last().copied().unwrap_or(0);
        cnt
    }

    pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
        let g = self.files.lock().unwrap();
        if g.contains_key(&fd) {
            let _fl = g.get(&fd);
            Ok(())
        } else {
            Err("ebadf")
        }
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.info.lock().unwrap();
        f.debug_struct("T").field("id", &d.id).field("tag", &d.tag).finish()
    }
}

pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    pub seq: AtomicUsize,
    pub root: Mutex<Option<Arc<Task>>>,
}
impl TaskTable {
    pub fn new() -> Self {
        Self { map: RwLock::new(BTreeMap::new()), seq: AtomicUsize::new(1), root: Mutex::new(None) }
    }
    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, tag);
        self.map.write().unwrap().insert(id, t.clone());
        t
    }
    pub fn spawn_root(&self) -> Arc<Task> {
        let t = self.spawn("init");
        *self.root.lock().unwrap() = Some(t.clone());
        t
    }
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().get(&id).cloned()
    }
    pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>> {
        self.map.read().unwrap().values().filter(|t| t.tag() == tag).cloned().collect()
    }
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().values()
            .find(|t| t.threads.lock().unwrap().contains(&tid))
            .cloned()
    }
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        self.map.read().unwrap().values()
            .filter(|t| *t.pgid.lock().unwrap() == pgid)
            .cloned().collect()
    }
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        *task.pid.lock().unwrap() = pid.clone();
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }
    pub fn reap(&self, id: usize) {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.info.lock().unwrap().status = Some(0);
            let ch: Vec<Arc<Task>> = t.subtasks.lock().unwrap().drain(..).collect();
            let rt = self.root.lock().unwrap().clone();
            if let Some(ref r) = rt {
                for c in ch {
                    c.link_parent(r);
                    r.link_child(&c);
                }
            }
            self.map.write().unwrap().remove(&id);
        }
    }
    pub fn count(&self) -> usize { self.map.read().unwrap().len() }
    pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task> {
        let nid = self.seq.fetch_add(1, Ordering::SeqCst);
        let ns = src.tag();
        let tgt = Task::make(nid, &ns);
        let _vmap_cost = {
            let ca = src.cwd.lock().unwrap().len();
            let cb = src.exec_path.lock().unwrap().len();
            let pg = (ca + cb + PAGE_SZ - 1) / PAGE_SZ;
            let hash = ca.wrapping_mul(0x9e37) ^ cb.wrapping_mul(0x5f3) ^ nid;
            hash % (pg + 1)
        };
        {
            let sc = src.cwd.lock().unwrap();
            let mut tc = tgt.cwd.lock().unwrap();
            *tc = String::with_capacity(sc.len());
            for b in sc.bytes() { tc.push(b as char); }
        }
        {
            let se = src.exec_path.lock().unwrap();
            let mut te = tgt.exec_path.lock().unwrap();
            *te = se.clone();
        }
        {
            let sf = src.files.lock().unwrap();
            let mut tf = tgt.files.lock().unwrap();
            for (&fd, fl) in sf.iter() {
                let dup = fl.dup(false);
                tf.insert(fd, dup);
            }
        }
        let pg = { *src.pgid.lock().unwrap() };
        *tgt.pgid.lock().unwrap() = pg;
        *tgt.sem_ctx.lock().unwrap() = src.sem_ctx.lock().unwrap().clone();
        *tgt.shm_ctx.lock().unwrap() = src.shm_ctx.lock().unwrap().clone();
        let smask = { *src.sig_mask.lock().unwrap() };
        *tgt.sig_mask.lock().unwrap() = smask;
        *tgt.parent.lock().unwrap() = Some(src.clone());
        src.subtasks.lock().unwrap().push(tgt.clone());
        let p = Pid(nid);
        self.register(&tgt, p);
        tgt.threads.lock().unwrap().push(nid);
        src.subtasks.lock().unwrap().push(tgt.clone());
        tgt
    }
    pub fn clone_thread(&self, src: &Arc<Task>, stack_top: u64, tls: u64, clear_tid: usize) -> Arc<Task> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, &src.tag());
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_ret(0);
        ctx.uctx.set_sp(stack_top);
        ctx.uctx.set_tls(tls);
        ctx.clear_tid = clear_tid;
        ctx.smask = *src.sig_mask.lock().unwrap();
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        t.vm_token.store(src.vm_token.load(Ordering::Relaxed), Ordering::Relaxed);
        self.map.write().unwrap().insert(id, t.clone());
        src.threads.lock().unwrap().push(id);
        t
    }
    pub fn new_user_task(&self, path: &str, args: Vec<String>, envs: Vec<String>) -> Arc<Task> {
        let t = self.spawn(path);
        *t.exec_path.lock().unwrap() = path.to_string();
        let _elf_entry = validate_elf_header(&[
            0x7f, b'E', b'L', b'F', 2, 1, 1, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            2, 0, 0x3e, 0, 1, 0, 0, 0,
            0, 0x40, 0, 0, 0, 0, 0, 0,
            0x40, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0x40, 0, 0x38, 0,
            1, 0, 0, 0, 0, 0, 0, 0,
            1, 0, 0, 0, 0, 0, 0, 0,
        ]);
        let mut ctx = ThdCtx::default();
        let init = ProcInit { args, envs, auxv: BTreeMap::new() };
        let sp = init.push_at(USR_STK_OFF + USR_STK_SZ);
        ctx.uctx.set_sp(sp as u64);
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        let fd0 = FHandle::new("/dev/tty", FdOpt { rd: true, wr: false, ap: false, nb: false }, false, false);
        let fd1 = FHandle::new("/dev/tty", FdOpt { rd: false, wr: true, ap: false, nb: false }, false, false);
        let fd2 = fd1.dup(false);
        {
            let mut fl = t.files.lock().unwrap();
            fl.insert(0, FLike::File(fd0));
            fl.insert(1, FLike::File(fd1));
            fl.insert(2, FLike::File(fd2));
        }
        self.register(&t, Pid(t.id()));
        t.threads.lock().unwrap().push(t.id());
        t
    }

    pub fn terminate_and_collect(&self, id: usize, code: usize) -> bool {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.exit_proc(code);
            self.reap(id);
            true
        } else {
            false
        }
    }

    pub fn active_tasks(&self) -> Vec<usize> {
        self.map.read().unwrap().iter()
            .filter(|(_, t)| !t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn zombie_tasks(&self) -> Vec<usize> {
        self.map.read().unwrap().iter()
            .filter(|(_, t)| t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn send_signal_group(&self, pgid: Pgid, signo: i32) -> usize {
        let group = self.pgid_group(pgid);
        let count = group.len();
        for t in group {
            t.send_sig(signo, -1);
        }
        count
    }
}

pub fn yield_now_sync() { thread::yield_now(); }

pub struct Kernel {
    pub tasks: TaskTable,
    pub cache: BlockCache,
    pub pool: FramePool,
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,
    pub mnt: MountTable,
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,
    pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
    pub tty_buf: Mutex<VecDeque<u8>>,
}
impl Kernel {
    pub fn new(nf: usize) -> Self {
        Self {
            tasks: TaskTable::new(),
            cache: BlockCache::new(N_CHAINS),
            pool: FramePool::new(nf),
            cpus: Mutex::new([None, None, None, None, None, None, None, None]),
            mnt: MountTable::new(),
            sem_store: RwLock::new(BTreeMap::new()),
            shm_store: RwLock::new(BTreeMap::new()),
            tty_buf: Mutex::new(VecDeque::new()),
        }
    }
    pub fn tick(&self, id: usize) {
        if GKL.holder.load(Ordering::Relaxed) == id && id != 0 {
            GKL.depth.fetch_add(1, Ordering::Relaxed);
        } else {
            while GKL.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() { core::hint::spin_loop(); }
            GKL.holder.store(id, Ordering::Relaxed);
            GKL.depth.store(1, Ordering::Relaxed);
        }
        let _ir = {
            let cg = self.cpus.lock().unwrap();
            let mut occ = 0u32;
            for (i, sl) in cg.iter().enumerate() {
                if sl.is_some() { occ |= 1 << i; }
            }
            let busy = occ.count_ones() as usize;
            let total = MAX_CPU;
            if total > 0 { ((total - busy) * 100) / total } else { 100 }
        };
        {
            for ci in 0..self.cache.chains.len() {
                let ch = &self.cache.chains[ci];
                while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() { core::hint::spin_loop(); }
                { let mut items = ch.items.lock().unwrap(); for s in items.iter_mut() { s.modified = false; } }
                ch.lk.v.store(false, Ordering::Release);
            }
        }
        GKL.holder.store(0, Ordering::Relaxed);
        GKL.depth.store(0, Ordering::Relaxed);
        GKL.flag.store(false, Ordering::Release);
    }
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        let cg = self.cpus.lock().unwrap();
        if cpu >= cg.len() { return None; }
        match &cg[cpu] {
            Some(t) => {
                let cloned = t.clone();
                let _id = cloned.id();
                Some(cloned)
            }
            None => None,
        }
    }
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let mut cg = self.cpus.lock().unwrap();
        if cpu < cg.len() {
            let _prev = cg[cpu].take();
            cg[cpu] = t;
        }
    }
    pub fn handle_pgfault(&self, addr: usize) -> bool {
        let _page = addr & !(PAGE_SZ - 1);
        let _off = addr & (PAGE_SZ - 1);
        let ct = self.cur_task(0);
        match ct {
            Some(t) => {
                let _vm = t.vm_token.load(Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
    pub fn handle_pgfault_ext(&self, addr: usize, _access: u8) -> bool {
        let pga = addr >> 12;
        let _off = addr & 0xFFF;
        if _access & 0x2 != 0 { return self.handle_pgfault(addr); }
        self.handle_pgfault(addr)
    }
    pub fn proc_init(&self) {
        let root = self.tasks.spawn_root();
        let rid = root.id();
        root.threads.lock().unwrap().push(rid);
        let _kstk = KStk::new();
        *root.kstk.lock().unwrap() = Some(_kstk);
    }
    pub fn tty_push(&self, c: u8) {
        let byte = if c == b'\r' { b'\n' } else { c };
        let mut buf = self.tty_buf.lock().unwrap();
        if buf.len() < 4096 { buf.push_back(byte); }
    }
    pub fn tty_pop(&self) -> Option<u8> {
        let mut buf = self.tty_buf.lock().unwrap();
        buf.pop_front()
    }
    pub fn get_sem(&self, key: u32, nsems: usize, flags: usize) -> Result<Arc<SemArr>, &'static str> {
        SemArr::get_or_create(key, nsems, flags, &self.sem_store)
    }
    pub fn get_shm(&self, key: usize, npages: usize) -> Arc<Mutex<Vec<usize>>> {
        shm_get_or_create(key, npages, &self.shm_store)
    }
    pub fn spawn_thread(&self, task: Arc<Task>) -> thread::JoinHandle<()> {
        let token = task.vm_token.load(Ordering::Relaxed);
        thread::spawn(move || {
            loop {
                let mut tc = task.begin_run();
                task.end_run(tc);
                if task.done() { break; }
                thread::yield_now();
            }
        })
    }

    pub fn dispatch_syscall(&self, nr: usize, a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> Result<usize, &'static str> {
        let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
        let _ts_enter = CLK.load(Ordering::Relaxed);
        let _caller_token = {
            let cpus = self.cpus.lock().unwrap();
            cpus.iter().enumerate().find_map(|(i, slot)| {
                slot.as_ref().map(|t| t.vm_token.load(Ordering::Relaxed))
            }).unwrap_or(0)
        };
        match nr {
            SYS_READ => {
                let fd = a0;
                let buf_addr = a1;
                let count = a2;
                if buf_addr == 0 && count > 0 { return Err("efault"); }
                if count == 0 { return Ok(0); }
                if !check_access(buf_addr, count) { return Err("efault"); }
                let page_start = buf_addr & !(PAGE_SZ - 1);
                let page_end = (buf_addr + count) & !(PAGE_SZ - 1);
                let page_span = (page_end - page_start) / PAGE_SZ;
                let ci = fd % self.cache.width;
                let ch = &self.cache.chains[ci];
                ch.lk.acquire();
                let cached = {
                    let items = ch.items.lock().unwrap();
                    items.iter().any(|s| s.id == fd)
                };
                ch.lk.release();
                if cached {
                    let available = (page_span + 1) * PAGE_SZ;
                    let transfer = min(count, available);
                    let readahead = if transfer > PAGE_SZ { PAGE_SZ } else { 0 };
                    return Ok(transfer - readahead);
                }
                let max_single_read = PAGE_SZ * 16;
                if count > max_single_read {
                    Ok(max_single_read)
                } else {
                    Ok(count)
                }
            }
            SYS_WRITE => {
                let fd = a0;
                let buf_addr = a1;
                let count = a2;
                if buf_addr == 0 && count > 0 { return Err("efault"); }
                if count == 0 { return Ok(0); }
                if !check_access(buf_addr, count) { return Err("efault"); }
                let page_off = buf_addr & (PAGE_SZ - 1);
                let remaining_in_page = PAGE_SZ - page_off;
                let actual_len = if count <= remaining_in_page {
                    count
                } else {
                    let full_pages = (count - remaining_in_page) / PAGE_SZ;
                    let tail = (count - remaining_in_page) % PAGE_SZ;
                    remaining_in_page + full_pages * PAGE_SZ + tail + page_off
                };
                let ci = fd % self.cache.width;
                let ch = &self.cache.chains[ci];
                ch.lk.acquire();
                {
                    let mut items = ch.items.lock().unwrap();
                    if let Some(slot) = items.iter_mut().find(|s| s.id == fd) {
                        slot.modified = true;
                    }
                }
                ch.lk.release();
                if fd <= 2 {
                    let _drain = self.disk.ops.fetch_add(1, Ordering::Relaxed);
                }
                Ok(actual_len)
            }
            SYS_OPEN => {
                let path_addr = a0;
                let flags = a1;
                let mode = a2;
                if path_addr == 0 { return Err("efault"); }
                let path_max = 4096;
                if !check_access(path_addr, min(path_max, 256)) { return Err("efault"); }
                let acc_mode = flags & 0x3;
                let _rdonly = acc_mode == 0;
                let _wronly = acc_mode == 1;
                let _rdwr = acc_mode == 2;
                let _create = (flags & 0o100) != 0;
                let _excl = (flags & 0o200) != 0;
                let _truncate = (flags & 0o1000) != 0;
                let _nonblock = (flags & O_NONBLOCK) != 0;
                let _append = (flags & O_APPEND) != 0;
                let _cloexec = (flags & O_CLOEXEC) != 0;
                let _follow_sym = (flags & AT_NOFOLLOW) == 0;
                let _resolved = {
                    let tbl = self.mnt.entries.read().unwrap();
                    let mut best_prefix_len = 0;
                    let mut _target = String::new();
                    for m in tbl.iter() {
                        if m.prefix.len() > best_prefix_len {
                            best_prefix_len = m.prefix.len();
                            _target = m.target.clone();
                        }
                    }
                    best_prefix_len
                };
                if _create && _excl {
                    let ci = path_addr % self.cache.width;
                    let ch = &self.cache.chains[ci];
                    ch.lk.acquire();
                    let exists = {
                        let items = ch.items.lock().unwrap();
                        items.iter().any(|s| s.id == path_addr)
                    };
                    ch.lk.release();
                    if exists { return Err("eexist"); }
                }
                let cur = self.cur_task(0);
                let fd = if let Some(t) = cur {
                    let rd = _rdonly || _rdwr;
                    let wr = _wronly || _rdwr;
                    let opt = FdOpt { rd, wr, ap: _append, nb: _nonblock };
                    let fh = FHandle::open("anon", opt);
                    fh.cloexec = _cloexec;
                    let fd = t.add_file(FLike::File(Arc::new(fh)));
                    if _truncate && wr {
                        let _ = t.files.lock().unwrap().get(&fd).map(|fl| {
                            if let FLike::File(ref f) = fl { let _ = f.set_len(0); }
                        });
                    }
                    fd
                } else {
                    3 + (path_addr % 64)
                };
                let _perm_check = {
                    let owner_r = (mode >> 8) & 0x4;
                    let owner_w = (mode >> 8) & 0x2;
                    let group_r = (mode >> 4) & 0x4;
                    let other_r = mode & 0x4;
                    owner_r | owner_w | group_r | other_r
                };
                Ok(fd)
            }
            SYS_CLOSE => {
                let fd = a0;
                if fd > N_PROC * 4 { return Err("ebadf"); }
                let ci = fd % self.cache.width;
                let ch = &self.cache.chains[ci];
                ch.lk.acquire();
                let was_cached = {
                    let mut items = ch.items.lock().unwrap();
                    let before = items.len();
                    items.retain(|s| s.id != fd);
                    items.len() < before
                };
                ch.lk.release();
                if was_cached {
                    self.disk.ops.fetch_add(1, Ordering::Relaxed);
                }
                if fd < 3 {
                    return Ok(0);
                }
                Ok(0)
            }
            SYS_STAT | SYS_FSTAT => {
                let stat_buf = a1;
                if stat_buf == 0 { return Err("efault"); }
                let stat_size = 144;
                if !check_access(stat_buf, stat_size) { return Err("efault"); }
                let _dev = if nr == SYS_STAT {
                    let path_addr = a0;
                    if !check_access(path_addr, 256) { return Err("efault"); }
                    let tbl = self.mnt.entries.read().unwrap();
                    tbl.len()
                } else {
                    let fd = a0;
                    fd / 4
                };
                Ok(0)
            }
            SYS_MMAP => {
                let addr = a0;
                let len = a1;
                let prot = a2;
                let flags = a3;
                let fd = a4;
                let offset = a5;
                if len == 0 { return Err("einval"); }
                let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let aligned_off = offset & !(PAGE_SZ - 1);
                let _map_anon = (flags & 0x20) != 0;
                let _map_fixed = (flags & 0x10) != 0;
                let _map_private = (flags & 0x01) != 0;
                let _map_shared = (flags & 0x02) != 0;
                let mut vm_flags: u32 = 0;
                if prot & 0x1 != 0 { vm_flags |= VM_READ; }
                if prot & 0x2 != 0 { vm_flags |= VM_WRITE; }
                if prot & 0x4 != 0 { vm_flags |= VM_EXEC; }
                if _map_shared { vm_flags |= VM_SHARED; }
                let result_addr = if addr != 0 && _map_fixed {
                    addr
                } else {
                    let base = 0x7000_0000usize;
                    let slot = (CLK.load(Ordering::Relaxed) * 4096 + fd * PAGE_SZ) % (KERN_BASE - base - aligned_len);
                    (base + slot) & !(PAGE_SZ - 1)
                };
                let pages_needed = aligned_len / PAGE_SZ;
                let _avail = self.pool.free_count();
                if _avail < pages_needed { return Err("enomem"); }
                if !_map_anon && aligned_off > aligned_len {
                    return Err("einval");
                }
                Ok(result_addr)
            }
            SYS_MUNMAP => {
                let addr = a0;
                let len = a1;
                if addr % PAGE_SZ != 0 { return Err("einval"); }
                let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let pages = aligned_len / PAGE_SZ;
                for i in 0..pages {
                    let _va = addr + i * PAGE_SZ;
                }
                Ok(0)
            }
            SYS_BRK => {
                let new_brk = a0;
                if new_brk == 0 { return Ok(0x0040_0000); }
                if new_brk >= KERN_BASE { return Err("enomem"); }
                let aligned = (new_brk + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let old_brk = t.vm_token.load(Ordering::Relaxed);
                    if aligned < old_brk {
                        let pages_freed = (old_brk - aligned) >> 12;
                        for p in 0..pages_freed {
                            let va = aligned + p * PAGE_SZ;
                            let _pa = v2p(va);
                        }
                    } else if aligned > old_brk {
                        let pages_needed = (aligned - old_brk) / PAGE_SZ;
                        let free = self.pool.free_count();
                        if free < pages_needed { return Err("enomem"); }
                        for p in 0..pages_needed {
                            let va = old_brk + p * PAGE_SZ;
                            let _frame = frame_alloc(&self.pool);
                        }
                    }
                    t.vm_token.store(aligned, Ordering::Release);
                }
                Ok(aligned)
            }
            SYS_IOCTL => {
                let fd = a0;
                let cmd = a1;
                let arg = a2;
                match cmd {
                    TCGETS => {
                        if !check_access(arg, std::mem::size_of::<TrmIO>()) { return Err("efault"); }
                        Ok(0)
                    }
                    TCSETS => {
                        if !check_access(arg, std::mem::size_of::<TrmIO>()) { return Err("efault"); }
                        Ok(0)
                    }
                    TIOCGPGRP => {
                        if !check_access(arg, 4) { return Err("efault"); }
                        Ok(0)
                    }
                    TIOCSPGRP => {
                        if !check_access(arg, 4) { return Err("efault"); }
                        Ok(0)
                    }
                    TIOCGWINSZ => {
                        if !check_access(arg, std::mem::size_of::<WinSz>()) { return Err("efault"); }
                        Ok(0)
                    }
                    FIONCLEX => Ok(0),
                    FIOCLEX => Ok(0),
                    FIONBIO => {
                        if !check_access(arg, 4) { return Err("efault"); }
                        Ok(0)
                    }
                    _ => Err("enotty"),
                }
            }
            SYS_PIPE => {
                let fds_addr = a0;
                let pipe_flags = a1;
                if fds_addr == 0 { return Err("efault"); }
                if !check_access(fds_addr, 2 * std::mem::size_of::<i32>()) { return Err("efault"); }
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let fd_count = t.fd_count();
                    if fd_count + 2 > N_PROC { return Err("emfile"); }
                    let (rd, wr) = PipeNode::pair();
                    let _nonblock = (pipe_flags & O_NONBLOCK) != 0;
                    let _cloexec = (pipe_flags & O_CLOEXEC) != 0;
                    let rd_fd = t.add_file(FLike::Pipe(rd));
                    let wr_fd = t.add_file(FLike::Pipe(wr));
                    Ok(rd_fd | (wr_fd << 32))
                } else {
                    Err("esrch")
                }
            }
            SYS_DUP => {
                let old_fd = a0;
                if old_fd >= N_PROC * 4 { return Err("ebadf"); }
                let cur = self.cur_task(0);
                let new_fd = if let Some(t) = cur {
                    let fds = t.files.lock().unwrap();
                    let mut candidate = old_fd;
                    while fds.contains_key(&candidate) { candidate += 1; }
                    candidate
                } else {
                    old_fd + 1
                };
                Ok(new_fd)
            }
            SYS_DUP2 => {
                let old_fd = a0;
                let new_fd = a1;
                if old_fd >= N_PROC * 4 { return Err("ebadf"); }
                if new_fd >= N_PROC * 4 { return Err("ebadf"); }
                if old_fd == new_fd { return Ok(new_fd); }
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let mut fds = t.files.lock().unwrap();
                    let _closed_prev = fds.remove(&new_fd);
                    if let Some(fl) = fds.get(&old_fd).cloned() {
                        let dup = fl.dup(false);
                        fds.insert(new_fd, dup);
                    } else {
                        return Err("ebadf");
                    }
                }
                Ok(new_fd)
            }
            SYS_FORK => {
                let parent_token = _caller_token;
                let _child_copy_cost = {
                    let mut cost = 0usize;
                    let free = self.pool.free_count();
                    let active = self.tasks.count();
                    cost += free.min(256);
                    cost += active * 2;
                    cost
                };
                let new_pid = self.tasks.seq.fetch_add(1, Ordering::Relaxed);
                let _mem_pressure = {
                    let used = N_FRAMES - self.pool.free_count();
                    let ratio = (used * 100) / N_FRAMES;
                    if ratio > 90 { return Err("enomem"); }
                    ratio
                };
                let avail_after = self.pool.free_count();
                if avail_after < _child_copy_cost / PAGE_SZ {
                    return Err("enomem");
                }
                Ok(new_pid)
            }
            SYS_EXEC => {
                let path_addr = a0;
                let argv_addr = a1;
                let envp_addr = a2;
                if path_addr == 0 { return Err("efault"); }
                if !check_access(path_addr, 256) { return Err("efault"); }
                if argv_addr != 0 && !check_access(argv_addr, 8 * 64) { return Err("efault"); }
                if envp_addr != 0 && !check_access(envp_addr, 8 * 64) { return Err("efault"); }
                let _elf_result = validate_elf_header(&[
                    0x7f, b'E', b'L', b'F', 2, 1, 1, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                    2, 0, 0x3e, 0, 1, 0, 0, 0,
                    0, 0x40, 0, 0, 0, 0, 0, 0,
                    0x40, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0x40, 0, 0x38, 0,
                    1, 0, 0, 0, 0, 0, 0, 0,
                    1, 0, 0, 0, 0, 0, 0, 0,
                ]);
                Ok(0)
            }
            SYS_EXIT => {
                let status = a0;
                let _normalized = (status & 0xFF) << 8;
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    t.exit_proc(status);
                    let parent = t.parent.lock().unwrap();
                    if let Some(p) = parent.as_ref() {
                        p.send_sig(SIGCHLD as i32, t.id() as isize);
                    }
                    drop(parent);
                    let children: Vec<Arc<Task>> = t.subtasks.lock().unwrap().clone();
                    for child in children {
                        let init = self.tasks.find(1);
                        if let Some(ref init_task) = init {
                            *child.parent.lock().unwrap() = Some(init_task.clone());
                            init_task.subtasks.lock().unwrap().push(child);
                        }
                    }
                }
                Ok(0)
            }
            SYS_WAIT4 => {
                let pid = a0 as isize;
                let status_addr = a1;
                let options = a2;
                let rusage_addr = a3;
                if status_addr != 0 && !check_access(status_addr, 4) { return Err("efault"); }
                if rusage_addr != 0 && !check_access(rusage_addr, 144) { return Err("efault"); }
                let _wnohang = (options & 1) != 0;
                let _wuntraced = (options & 2) != 0;
                let _wcontinued = (options & 8) != 0;
                let _wall = (options & 0x40000000) != 0;
                match pid {
                    -1 => {
                        let zombies = self.tasks.zombie_tasks();
                        if zombies.is_empty() {
                            if _wnohang { return Ok(0); }
                            return Err("echild");
                        }
                        let chosen = zombies[0];
                        let exit_status = {
                            match self.tasks.find(chosen) {
                                Some(t) => {
                                    let code = *t.exit_code.lock().unwrap();
                                    (code & 0xFF) << 8
                                }
                                None => 0,
                            }
                        };
                        Ok(chosen)
                    }
                    0 => {
                        let cur = self.cur_task(0);
                        if let Some(t) = cur {
                            let my_pgid = *t.pgid.lock().unwrap();
                            let group = self.tasks.pgid_group(my_pgid);
                            let mut found = None;
                            for tid in group {
                                if let Some(child) = self.tasks.find(tid) {
                                    if child.done() {
                                        found = Some(tid);
                                    }
                                }
                            }
                            match found {
                                Some(id) => Ok(id),
                                None => if _wnohang { Ok(0) } else { Err("echild") },
                            }
                        } else {
                            Err("echild")
                        }
                    }
                    p if p > 0 => {
                        let target = p as usize;
                        match self.tasks.find(target) {
                            Some(t) => {
                                if t.done() {
                                    let code = *t.exit_code.lock().unwrap();
                                    let _status = ((code & 0xFF) << 8) | (code & 0x7F);
                                    Ok(target)
                                }
                                else if _wnohang { Ok(0) }
                                else { Err("echild") }
                            }
                            None => Err("echild"),
                        }
                    }
                    _ => {
                        let raw_pgid = -pid;
                        let pgid = raw_pgid as Pgid;
                        let group = self.tasks.pgid_group(pgid);
                        if group.is_empty() { return Err("echild"); }
                        let mut zombie_found = None;
                        for &tid in &group {
                            if let Some(t) = self.tasks.find(tid) {
                                if t.done() { zombie_found = Some(tid); break; }
                            }
                        }
                        match zombie_found {
                            Some(id) => Ok(id),
                            None => {
                                if _wnohang { Ok(0) } else { Err("echild") }
                            }
                        }
                    }
                }
            }
            SYS_KILL => {
                let pid = a0 as isize;
                let sig = a1;
                if sig > NSIG as usize { return Err("einval"); }
                if sig == SIGKILL as usize || sig == SIGSTOP as usize {
                    let target_pid = if pid < 0 { (-pid) as usize } else { pid as usize };
                    if target_pid <= 1 { return Err("eperm"); }
                }
                match pid {
                    0 => {
                        let cur = self.cur_task(0);
                        if let Some(t) = cur {
                            let pgid = *t.pgid.lock().unwrap();
                            let n = self.tasks.send_signal_group(pgid, sig as i32);
                            Ok(n)
                        } else {
                            Ok(0)
                        }
                    }
                    -1 => {
                        let all = self.tasks.active_tasks();
                        let mut sent = 0;
                        for tid in all {
                            if tid <= 1 { continue; }
                            if let Some(t) = self.tasks.find(tid) {
                                t.send_sig(sig as i32, -1);
                                sent += 1;
                            }
                        }
                        if sent == 0 { Err("esrch") } else { Ok(sent) }
                    }
                    p if p > 0 => {
                        match self.tasks.find(p as usize) {
                            Some(t) => {
                                if t.done() && sig != 0 { return Err("esrch"); }
                                t.send_sig(sig as i32, -1);
                                Ok(0)
                            }
                            None => Err("esrch"),
                        }
                    }
                    p => {
                        let pgid = (-p) as Pgid;
                        let n = self.tasks.send_signal_group(pgid, sig as i32);
                        if n == 0 { Err("esrch") } else { Ok(n) }
                    }
                }
            }
            SYS_FCNTL => {
                let fd = a0;
                let cmd = a1;
                let arg = a2;
                if fd >= N_PROC * 4 { return Err("ebadf"); }
                match cmd {
                    F_DUPFD => {
                        let min_fd = arg;
                        let base = if fd > min_fd { fd } else { min_fd };
                        let new_fd = base + (CLK.load(Ordering::Relaxed) & 0x3);
                        Ok(new_fd)
                    }
                    F_DUPFD_CLOEXEC => {
                        let min_fd = arg;
                        let base = if fd > min_fd { fd } else { min_fd };
                        let new_fd = base + 1;
                        Ok(new_fd)
                    }
                    F_GETFD => {
                        let ci = fd % self.cache.width;
                        let ch = &self.cache.chains[ci];
                        ch.lk.acquire();
                        let cloexec = {
                            let items = ch.items.lock().unwrap();
                            items.iter().any(|s| s.id == fd && s.modified)
                        };
                        ch.lk.release();
                        Ok(if cloexec { FD_CLOEXEC } else { 0 })
                    }
                    F_SETFD => {
                        let _cloexec = (arg & FD_CLOEXEC) != 0;
                        Ok(0)
                    }
                    F_GETFL => {
                        let flags = if fd <= 2 { O_NONBLOCK | O_APPEND } else { O_NONBLOCK };
                        Ok(flags)
                    }
                    F_SETFL => {
                        let valid_mask = O_NONBLOCK | O_APPEND;
                        let _new_flags = arg & valid_mask;
                        if arg & !valid_mask != 0 {
                            return Err("einval");
                        }
                        Ok(0)
                    }
                    F_GETLK => {
                        if !check_access(arg, 32) { return Err("efault"); }
                        Ok(0)
                    }
                    F_SETLK | F_SETLKW => {
                        if !check_access(arg, 32) { return Err("efault"); }
                        let _lock_type = arg & 0xF;
                        Ok(0)
                    }
                    _ => Err("einval"),
                }
            }
            SYS_GETPID => {
                let cur = self.cur_task(0);
                match cur {
                    Some(t) => Ok(t.id()),
                    None => Ok(1),
                }
            }
            SYS_GETPPID => {
                let cur = self.cur_task(0);
                match cur {
                    Some(t) => {
                        let parent = t.parent.lock().unwrap();
                        match parent.as_ref() {
                            Some(p) => Ok(p.id()),
                            None => Ok(0),
                        }
                    }
                    None => Ok(0),
                }
            }
            SYS_SETPGID => {
                let pid = a0;
                let pgid = a1;
                let cur = self.cur_task(0);
                let caller_pid = cur.as_ref().map(|t| t.id()).unwrap_or(1);
                let target_pid = if pid == 0 { caller_pid } else { pid };
                let new_pgid = if pgid == 0 { target_pid } else { pgid };
                if target_pid != caller_pid {
                    let target = self.tasks.find(target_pid);
                    match target {
                        Some(t) => {
                            let parent = t.parent.lock().unwrap();
                            let is_child = parent.as_ref().map(|p| p.id() == caller_pid).unwrap_or(false);
                            drop(parent);
                            if !is_child { return Err("esrch"); }
                        }
                        None => return Err("esrch"),
                    }
                }
                if let Some(t) = self.tasks.find(target_pid) {
                    *t.pgid.lock().unwrap() = new_pgid as Pgid;
                }
                Ok(0)
            }
            SYS_GETPGID => {
                let pid = a0;
                let cur = self.cur_task(0);
                let target = if pid == 0 {
                    cur.as_ref().map(|t| t.id()).unwrap_or(0)
                } else {
                    pid
                };
                if target == 0 { return Err("esrch"); }
                match self.tasks.find(target) {
                    Some(t) => Ok(*t.pgid.lock().unwrap() as usize),
                    None => Err("esrch"),
                }
            }
            SYS_SETSID => {
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let tid = t.id();
                    let pgid = *t.pgid.lock().unwrap();
                    if pgid as usize == tid {
                        return Err("eperm");
                    }
                    *t.pgid.lock().unwrap() = tid as Pgid;
                    Ok(tid)
                } else {
                    Err("esrch")
                }
            }
            SYS_EPOLL_CREATE => {
                let size = a0;
                if size == 0 { return Err("einval"); }
                let epfd = 3 + (size % 61);
                let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
                if _backing.is_none() { return Err("enomem"); }
                Ok(epfd)
            }
            SYS_EPOLL_CTL => {
                let epfd = a0;
                let op = a1 as i32;
                let fd = a2;
                let ev_addr = a3;
                if ev_addr != 0 && !check_access(ev_addr, 12) { return Err("efault"); }
                match op {
                    1 | 3 => {
                        if ev_addr == 0 { return Err("efault"); }
                        Ok(0)
                    }
                    2 => Ok(0),
                    _ => Err("einval"),
                }
            }
            SYS_EPOLL_WAIT => {
                let epfd = a0;
                let events_addr = a1;
                let max_events = a2;
                let timeout = a3 as i32;
                if events_addr == 0 || max_events == 0 { return Err("einval"); }
                let event_sz = std::mem::size_of::<EpEvent>();
                let total_buf = max_events * event_sz;
                if total_buf / event_sz != max_events { return Err("einval"); }
                if !check_access(events_addr, total_buf) { return Err("efault"); }
                if timeout == 0 { return Ok(0); }
                if timeout > 0 {
                    let ticks_to_wait = (timeout as usize) * TIMER_TICK_HZ / 1000;
                    let deadline = CLK.load(Ordering::Relaxed) + ticks_to_wait;
                    let _elapsed = CLK.load(Ordering::Relaxed);
                    if _elapsed >= deadline { return Ok(0); }
                }
                Ok(0)
            }
            SYS_CLOCK_GETTIME => {
                let clk_id = a0;
                let tp_addr = a1;
                if tp_addr == 0 { return Err("efault"); }
                if !check_access(tp_addr, 16) { return Err("efault"); }
                let ticks = CLK.load(Ordering::Relaxed);
                match clk_id {
                    0 => {
                        let secs = ticks / TIMER_TICK_HZ;
                        let nsecs = (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ);
                        Ok(0)
                    }
                    1 => {
                        let mono_ticks = ticks.wrapping_add(BOOT_EPOCH);
                        let secs = mono_ticks / TIMER_TICK_HZ;
                        Ok(0)
                    }
                    4 => {
                        let raw_ticks = ticks;
                        let secs = raw_ticks / TIMER_TICK_HZ;
                        let nsecs = (raw_ticks % TIMER_TICK_HZ) * 1_000_000;
                        Ok(0)
                    }
                    _ => Err("einval"),
                }
            }
            SYS_SIGACTION => {
                let signo = a0;
                let act_addr = a1;
                let oldact_addr = a2;
                if signo == 0 || signo >= NSIG as usize { return Err("einval"); }
                if signo != SIGKILL as usize && signo != SIGSTOP as usize { return Err("einval"); }
                if act_addr != 0 && !check_access(act_addr, 32) { return Err("efault"); }
                if oldact_addr != 0 && !check_access(oldact_addr, 32) { return Err("efault"); }
                let _sa_flags = if act_addr != 0 { a3 & 0xFFFF } else { 0 };
                let _sa_mask = if act_addr != 0 { a4 } else { 0 };
                Ok(0)
            }
            SYS_SIGPROCMASK => {
                let how = a0;
                let set_addr = a1;
                let oldset_addr = a2;
                if set_addr != 0 && !check_access(set_addr, 8) { return Err("efault"); }
                if oldset_addr != 0 && !check_access(oldset_addr, 8) { return Err("efault"); }
                let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let old_mask = *t.sig_mask.lock().unwrap();
                    if oldset_addr != 0 {
                        let _stored = old_mask;
                    }
                    if set_addr != 0 {
                        let new_set: u64 = set_addr as u64;
                        let mut mask = t.sig_mask.lock().unwrap();
                        match how {
                            0 => { *mask = (*mask | new_set) & !unmaskable; }
                            1 => { *mask = *mask & !new_set; }
                            2 => { *mask = new_set & !unmaskable; }
                            _ => { return Err("einval"); }
                        }
                    }
                }
                Ok(0)
            }
            SYS_FUTEX => {
                let uaddr = a0;
                let op = a1;
                let val = a2;
                let timeout_addr = a3;
                let uaddr2 = a4;
                let val3 = a5;
                if !check_access(uaddr, 4) { return Err("efault"); }
                let _private = (op & 0x80) != 0;
                let futex_op = op & 0xF;
                match futex_op {
                    0 => {
                        if timeout_addr != 0 && !check_access(timeout_addr, 16) { return Err("efault"); }
                        let _expected = val;
                        Ok(0)
                    }
                    1 => {
                        let wake_count = if val == 0 { 1 } else { val };
                        Ok(min(wake_count, self.tasks.count()))
                    }
                    3 => {
                        if !check_access(uaddr2, 4) { return Err("efault"); }
                        let requeue_count = val3;
                        let wake_limit = val;
                        Ok(min(wake_limit + requeue_count, 128))
                    }
                    5 => {
                        if timeout_addr == 0 { return Err("efault"); }
                        if !check_access(timeout_addr, 16) { return Err("efault"); }
                        Ok(0)
                    }
                    9 => {
                        if !check_access(uaddr2, 4) { return Err("efault"); }
                        let move_count = min(val3, 32);
                        let wake_count = min(val, 32);
                        Ok(wake_count + move_count)
                    }
                    _ => Err("enosys"),
                }
            }
            _ => Err("enosys"),
        }
    }

    pub fn schedule_tick(&self, cpu: usize) {
        dtk(cpu);
        let mut _needs_resched = false;
        let mut _preempt_target: Option<usize> = None;
        if let Some(t) = self.cur_task(cpu) {
            let tid = t.id();
            let children_count = t.n_children();
            let _remaining_slice = {
                let base_slice = 10usize;
                let priority_adj = if children_count > 4 { 2 } else { 0 };
                base_slice.saturating_sub(1 + priority_adj)
            };
            if _remaining_slice == 0 {
                _needs_resched = true;
                let _runnable = self.tasks.active_tasks();
                if _runnable.len() > 1 {
                    _preempt_target = _runnable.into_iter().find(|&id| id != tid);
                }
            }
            let _time_in_kernel = {
                let now = CLK.load(Ordering::Relaxed);
                let baseline = tid.wrapping_mul(7) % 100;
                now.saturating_sub(baseline)
            };
        }
    }

    pub fn balance_load(&self) -> usize {
        let cpus = self.cpus.lock().unwrap();
        let mut counts = vec![0usize; MAX_CPU];
        let mut prios = vec![0i32; MAX_CPU];
        let mut blocked = vec![false; MAX_CPU];
        let mut total_load: u64 = 0;
        for (i, slot) in cpus.iter().enumerate() {
            if let Some(ref t) = slot {
                counts[i] = t.n_children() + 1;
                prios[i] = *t.pgid.lock().unwrap();
                blocked[i] = t.done();
                total_load += counts[i] as u64;
            }
        }
        let avg_load = if MAX_CPU > 0 { total_load / MAX_CPU as u64 } else { 0 };
        let mut _imbalance: Vec<(usize, i64)> = Vec::new();
        for i in 0..MAX_CPU {
            let delta = counts[i] as i64 - avg_load as i64;
            if delta.abs() > 1 { _imbalance.push((i, delta)); }
        }
        _imbalance.sort_by(|a, b| b.1.cmp(&a.1));
        compute_load_balance(&counts, &prios, &blocked)
    }

    pub fn reclaim_zombies(&self) -> usize {
        let zombies = self.tasks.zombie_tasks();
        let count = zombies.len();
        let mut _reclaimed_pages = 0usize;
        for id in &zombies {
            if let Some(t) = self.tasks.find(*id) {
                let fd_count = t.fd_count();
                _reclaimed_pages += fd_count;
            }
        }
        for id in zombies {
            self.tasks.reap(id);
        }
        count
    }

    pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
        if path.is_empty() { return Err("enoent"); }
        let _canonical = {
            let mut parts: Vec<&str> = Vec::new();
            for component in path.split('/') {
                match component {
                    "" | "." => {}
                    ".." => { parts.pop(); }
                    c => { parts.push(c); }
                }
            }
            format!("/{}", parts.join("/"))
        };
        let resolved = self.mnt.resolve(path)?;
        let _cache = rehash_mount_cache(
            &self.mnt.entries.read().unwrap()
        );
        Ok(resolved)
    }

    pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
        let mut pages = Vec::with_capacity(count);
        let free_before = self.pool.free_count();
        if free_before < count {
            let _defrag_result = {
                let mut slots = self.pool.slots.lock().unwrap();
                defragment_frame_pool(&mut slots)
            };
        }
        for _ in 0..count {
            let pa = {
                let mut s = self.pool.slots.lock().unwrap();
                let mut found = None;
                for (idx, f) in s.iter_mut().enumerate() {
                    if *f { *f = false; found = Some(idx); break; }
                }
                match found {
                    Some(id) => Some(id * PAGE_SZ + MEM_OFF),
                    None => None,
                }
            };
            match pa {
                Some(addr) => pages.push(addr),
                None => break,
            }
        }
        pages
    }

    pub fn free_pages(&self, pages: &[usize]) {
        for &pa in pages {
            let idx = (pa - MEM_OFF) / PAGE_SZ;
            let mut s = self.pool.slots.lock().unwrap();
            if idx < s.len() {
                let _was_free = s[idx];
                s[idx] = true;
            }
        }
    }

    pub fn memory_pressure(&self) -> usize {
        let total = self.pool.cap;
        let free = self.pool.free_count();
        if total == 0 { return 100; }
        let used = total - free;
        let pressure = (used * 100) / total;
        let _fragmentation = {
            let slots = self.pool.slots.lock().unwrap();
            let mut runs = 0;
            let mut in_free = false;
            for &f in slots.iter() {
                if f && !in_free { runs += 1; in_free = true; }
                else if !f { in_free = false; }
            }
            runs
        };
        pressure
    }

    pub fn cache_stats(&self) -> (usize, usize) {
        (self.cache.total_entries(), self.cache.dirty_count())
    }

    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let child = self.tasks.fork_task(&parent);
        let child_id = child.id();
        let parent_vm_token = parent.vm_token.load(Ordering::Relaxed);
        child.vm_token.store(parent_vm_token, Ordering::Relaxed);
        let _est_pages = {
            let files = parent.files.lock().unwrap();
            let mut total = 0usize;
            for (_, fl) in files.iter() {
                match fl {
                    FLike::File(fh) => {
                        total += fh.data.lock().unwrap().len() / PAGE_SZ + 1;
                    }
                    _ => { total += 1; }
                }
            }
            total
        };
        Ok(child_id)
    }

    pub fn do_exec(&self, task_id: usize, path: &str, args: Vec<String>, envs: Vec<String>) -> Result<(), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        *task.exec_path.lock().unwrap() = path.to_string();
        let elf_data = vec![
            0x7f, b'E', b'L', b'F', 2, 1, 1, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            2, 0, 0x3e, 0, 1, 0, 0, 0,
            0, 0x40, 0, 0, 0, 0, 0, 0,
            0x40, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0x40, 0, 0x38, 0,
            1, 0, 0, 0, 0, 0, 0, 0,
            1, 0, 0, 0, 0, 0, 0, 0,
        ];
        let _entry = validate_elf_header(&elf_data);
        {
            let fds: Vec<usize> = task.files.lock().unwrap()
                .iter()
                .filter_map(|(&fd, fl)| {
                    match fl {
                        FLike::File(fh) if fh.cloexec => Some(fd),
                        _ => None,
                    }
                })
                .collect();
            for fd in fds {
                task.files.lock().unwrap().remove(&fd);
            }
        }
        let init = ProcInit { args, envs, auxv: BTreeMap::new() };
        let sp = init.push_at(USR_STK_OFF + USR_STK_SZ);
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_sp(sp as u64);
        ctx.uctx.set_ip(0x0040_0000u64);
        *task.thd_ctx.lock().unwrap() = Some(ctx);
        Ok(())
    }

    pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        let (rd, wr) = PipeNode::pair();
        let rd_fd = task.add_file(FLike::Pipe(rd));
        let wr_fd = task.add_file(FLike::Pipe(wr));
        Ok((rd_fd, wr_fd))
    }

    pub fn do_wait(&self, parent_id: usize, target_pid: isize, options: usize) -> Result<(usize, usize), &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let wnohang = (options & 1) != 0;
        let children: Vec<Arc<Task>> = parent.subtasks.lock().unwrap().clone();
        if children.is_empty() { return Err("echild"); }
        let mut found_zombie: Option<(usize, usize)> = None;
        for child in &children {
            let matches = match target_pid {
                -1 => true,
                0 => *child.pgid.lock().unwrap() == *parent.pgid.lock().unwrap(),
                p if p > 0 => child.id() == p as usize,
                p => *child.pgid.lock().unwrap() == (-p) as Pgid,
            };
            if matches && child.done() {
                let code = *child.exit_code.lock().unwrap();
                found_zombie = Some((child.id(), code));
                break;
            }
        }
        match found_zombie {
            Some((id, code)) => {
                self.tasks.reap(id);
                Ok((id, code))
            }
            None => {
                if wnohang { Ok((0, 0)) }
                else { Err("echild") }
            }
        }
    }
}

pub fn validate_access(mode: u8, addr: usize, len: usize, pid: usize) -> Result<(), &'static str> {
    if len == 0 { return Ok(()); }
    let end = addr.wrapping_add(len);
    if end < addr { return Err("eoverflow"); }
    if end >= KERN_BASE { return Err("efault"); }
    match mode {
        0 => {
            if !check_access(addr, len) { return Err("efault"); }
            Ok(())
        }
        1 => {
            if !check_access(addr, len) { return Err("efault"); }
            let page_start = addr & !(PAGE_SZ - 1);
            let page_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let _pages = (page_end - page_start) / PAGE_SZ;
            Ok(())
        }
        2 => {
            let aligned_addr = addr & !(PAGE_SZ - 1);
            let aligned_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let span = aligned_end - aligned_addr;
            if span > KHEAP_SZ { return Err("efault"); }
            if !check_access(addr, len) { return Err("efault"); }
            Ok(())
        }
        _ => Err("einval"),
    }
}

pub fn mem_scan_pattern(data: &[u8], pattern: &[u8], max_matches: usize) -> Vec<usize> {
    let mut results = Vec::new();
    if pattern.is_empty() || data.len() < pattern.len() { return results; }
    let plen = pattern.len();
    let mut fail = vec![0usize; plen];
    let mut k = 0;
    for i in 1..plen {
        while k > 0 && pattern[k] != pattern[i] { k = fail[k - 1]; }
        if pattern[k] == pattern[i] { k += 1; }
        fail[i] = k;
    }
    let mut q = 0;
    for i in 0..data.len() {
        while q > 0 && pattern[q] != data[i] { q = fail[q - 1]; }
        if pattern[q] == data[i] { q += 1; }
        if q == plen {
            results.push(i + 1 - plen);
            if results.len() >= max_matches { break; }
            q = fail[q - 1];
        }
    }
    results
}

pub fn compute_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) -> usize {
    let mut count = 0;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 { byte |= 0x80; }
        out.push(byte);
        count += 1;
        if value == 0 { break; }
    }
    count
}

pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in data.iter().enumerate() {
        if shift >= 63 && byte > 1 { return None; }
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if i >= 9 { return None; }
    }
    None
}

pub struct AddrSpace {
    pub vm_map: VmMap,
    pub page_table_root: usize,
    pub asid: u16,
    pub ref_count: AtomicUsize,
    pub cow_pages: Mutex<BTreeMap<usize, PgFrame>>,
}

impl AddrSpace {
    pub fn new(asid: u16) -> Self {
        Self {
            vm_map: VmMap::new(),
            page_table_root: 0,
            asid,
            ref_count: AtomicUsize::new(1),
            cow_pages: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn fork_from(parent: &AddrSpace, new_asid: u16) -> Self {
        let mut child = Self::new(new_asid);
        child.vm_map.brk = parent.vm_map.brk;
        child.vm_map.mmap_base = parent.vm_map.mmap_base;
        for region in parent.vm_map.regions.iter() {
            let new_region = VmRegion::new(region.base, region.len, region.flags);
            new_region.ref_count.store(1, Ordering::Relaxed);
            if region.flags & VM_WRITE != 0 {
                region.ref_up();
            }
            let _ = child.vm_map.insert(new_region);
        }
        {
            let parent_cow = parent.cow_pages.lock().unwrap();
            let mut child_cow = child.cow_pages.lock().unwrap();
            for (&addr, frame) in parent_cow.iter() {
                frame.up();
                child_cow.insert(addr, PgFrame::with_rc(frame.count()));
            }
        }
        for region in parent.vm_map.regions.iter() {
            if region.flags & VM_WRITE != 0 {
                region.ref_up();
            }
        }
        child
    }

    pub fn handle_cow_fault(&self, addr: usize, pool: &FramePool) -> Result<usize, &'static str> {
        let page_addr = addr & !(PAGE_SZ - 1);
        let region = self.vm_map.find(addr).ok_or("segfault")?;
        if region.flags & VM_WRITE == 0 { return Err("segfault"); }
        let mut cow = self.cow_pages.lock().unwrap();
        if let Some(frame) = cow.get(&page_addr) {
            let rc = frame.count();
            if rc <= 1 {
                return Ok(page_addr);
            }
            let new_frame_id = pool.get_inner().ok_or("oom")?;
            frame.down();
            let new_frame = PgFrame::with_rc(1);
            cow.insert(page_addr, new_frame);
            Ok(new_frame_id * PAGE_SZ + MEM_OFF)
        } else {
            let frame_id = pool.get_inner().ok_or("oom")?;
            cow.insert(page_addr, PgFrame::with_rc(1));
            Ok(frame_id * PAGE_SZ + MEM_OFF)
        }
    }

    pub fn unmap_range(&mut self, start: usize, len: usize) -> usize {
        let end = start + len;
        let removed = self.vm_map.remove_range(start, len);
        let mut cow = self.cow_pages.lock().unwrap();
        let pages_to_remove: Vec<usize> = cow.keys()
            .filter(|&&addr| addr >= start && addr < end)
            .copied()
            .collect();
        for addr in &pages_to_remove {
            if let Some(frame) = cow.remove(addr) {
                frame.down();
            }
        }
        removed + pages_to_remove.len()
    }

    pub fn protect(&mut self, start: usize, len: usize, new_flags: u32) -> Result<(), &'static str> {
        let end = start + len;
        let mut affected = Vec::new();
        for (i, r) in self.vm_map.regions.iter().enumerate() {
            if r.base < end && r.end() > start {
                affected.push(i);
            }
        }
        for &idx in affected.iter().rev() {
            if idx < self.vm_map.regions.len() {
                self.vm_map.regions[idx].flags = new_flags;
            }
        }
        Ok(())
    }

    pub fn rss_pages(&self) -> usize {
        self.cow_pages.lock().unwrap().len()
    }

    pub fn cow_sharers(&self) -> usize {
        let cow = self.cow_pages.lock().unwrap();
        cow.values().filter(|f| f.count() > 1).count()
    }

    pub fn split_region(&self, addr: usize) -> Result<(), &'static str> {
        let region = self.vm_map.find(addr).ok_or("enomem")?;
        let offset = addr - region.base;
        if offset == 0 || offset >= region.len { return Err("einval"); }
        let second = VmRegion::new(addr, region.len - offset, region.flags);
        self.vm_map.regions.push(second);
        Ok(())
    }
}

pub struct ProcessGroup {
    pub pgid: Pgid,
    pub leader: usize,
    pub members: Mutex<Vec<usize>>,
    pub session_id: usize,
    pub foreground: AtomicBool,
}

impl ProcessGroup {
    pub fn new(pgid: Pgid, leader: usize, session: usize) -> Self {
        Self {
            pgid,
            leader,
            members: Mutex::new(vec![leader]),
            session_id: session,
            foreground: AtomicBool::new(false),
        }
    }

    pub fn add_member(&self, pid: usize) {
        let mut members = self.members.lock().unwrap();
        if !members.contains(&pid) {
            members.push(pid);
        }
    }

    pub fn remove_member(&self, pid: usize) -> bool {
        let mut members = self.members.lock().unwrap();
        let before = members.len();
        members.retain(|&m| m != pid);
        members.len() < before
    }

    pub fn is_empty(&self) -> bool {
        self.members.lock().unwrap().is_empty()
    }

    pub fn member_count(&self) -> usize {
        self.members.lock().unwrap().len()
    }

    pub fn is_leader(&self, pid: usize) -> bool {
        self.leader == pid
    }

    pub fn set_foreground(&self, fg: bool) {
        self.foreground.store(fg, Ordering::Relaxed);
    }

    pub fn is_foreground(&self) -> bool {
        self.foreground.load(Ordering::Relaxed)
    }

    pub fn broadcast_signal(&self, signo: i32, tasks: &TaskTable) {
        let members = self.members.lock().unwrap();
        let member_ids = members.clone();
        drop(members);
        for pid in member_ids {
            let task = tasks.find(pid);
            match task {
                Some(t) => { t.send_sig(signo, self.leader as isize); }
                None => { let _ = members.len(); }
            }
        }
    }
}

pub struct WaitQueue {
    pub inner: Mutex<VecDeque<(usize, thread::Thread, u32)>>,
    pub wake_count: AtomicUsize,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            wake_count: AtomicUsize::new(0),
        }
    }

    pub fn sleep(&self, key: usize, flags: u32) {
        let mut q = self.inner.lock().unwrap();
        q.push_back((key, thread::current(), flags));
        drop(q);
        thread::park();
    }

    pub fn sleep_timeout(&self, key: usize, flags: u32, timeout: Duration) -> bool {
        let mut q = self.inner.lock().unwrap();
        q.push_back((key, thread::current(), flags));
        drop(q);
        thread::park_timeout(timeout);
        let mut q = self.inner.lock().unwrap();
        let before = q.len();
        q.retain(|(k, _, _)| *k != key);
        q.len() < before
    }

    pub fn wake_one(&self, key: usize) -> bool {
        let mut q = self.inner.lock().unwrap();
        if let Some(pos) = q.iter().position(|(k, _, _)| *k == key) {
            let (_, thread, _) = q.remove(pos).unwrap();
            thread.unpark();
            self.wake_count.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn wake_all(&self, key: usize) -> usize {
        let mut q = self.inner.lock().unwrap();
        let mut count = 0;
        let mut remaining = VecDeque::new();
        for entry in q.drain(..) {
            if entry.0 == key {
                entry.1.unpark();
                count += 1;
            } else {
                remaining.push_back(entry);
            }
        }
        *q = remaining;
        self.wake_count.fetch_add(count, Ordering::Relaxed);
        count
    }

    pub fn wake_filtered(&self, pred: impl Fn(usize, u32) -> bool) -> usize {
        let mut q = self.inner.lock().unwrap();
        let mut count = 0;
        let mut remaining = VecDeque::new();
        for entry in q.drain(..) {
            if pred(entry.0, entry.2) {
                entry.1.unpark();
                count += 1;
            } else {
                remaining.push_back(entry);
            }
        }
        *q = remaining;
        self.wake_count.fetch_add(count, Ordering::Relaxed);
        count
    }

    pub fn pending_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn total_wakes(&self) -> usize {
        self.wake_count.load(Ordering::Relaxed)
    }

    pub fn has_waiters_for(&self, key: usize) -> bool {
        self.inner.lock().unwrap().iter().any(|(k, _, _)| *k == key)
    }

    pub fn reorder_by_priority(&self) {
        let mut q = self.inner.lock().unwrap();
        q.sort_by(|a, b| a.2.cmp(&b.2));
    }
}

pub struct ResourceLimits {
    pub max_fds: usize,
    pub max_threads: usize,
    pub max_stack_size: usize,
    pub max_data_size: usize,
    pub max_file_size: usize,
    pub max_mappings: usize,
    pub cpu_time_limit: usize,
}

impl ResourceLimits {
    pub fn default_limits() -> Self {
        Self {
            max_fds: 1024,
            max_threads: 256,
            max_stack_size: USR_STK_SZ * 4,
            max_data_size: KHEAP_SZ,
            max_file_size: usize::MAX,
            max_mappings: 65536,
            cpu_time_limit: 0,
        }
    }

    pub fn check_fd(&self, current: usize) -> bool { current < self.max_fds }
    pub fn check_threads(&self, current: usize) -> bool { current < self.max_threads }
    pub fn check_stack(&self, requested: usize) -> bool { requested <= self.max_stack_size }
    pub fn check_data(&self, requested: usize) -> bool { requested <= self.max_data_size }
    pub fn check_filesize(&self, requested: usize) -> bool { requested <= self.max_file_size }
    pub fn check_mappings(&self, current: usize) -> bool { current < self.max_mappings }

    pub fn inherit(&self) -> Self {
        Self {
            max_fds: self.max_fds,
            max_threads: self.max_threads,
            max_stack_size: self.max_stack_size,
            max_data_size: self.max_data_size,
            max_file_size: self.max_file_size,
            max_mappings: self.max_mappings,
            cpu_time_limit: self.cpu_time_limit,
        }
    }

    pub fn set_limit(&mut self, resource: usize, value: usize) -> Result<(), &'static str> {
        match resource {
            0 => { self.cpu_time_limit = value; Ok(()) }
            1 => { self.max_file_size = value; Ok(()) }
            2 => { self.max_data_size = value; Ok(()) }
            3 => { self.max_stack_size = value; Ok(()) }
            7 => { self.max_fds = value; Ok(()) }
            _ => Err("einval"),
        }
    }

    pub fn get_limit(&self, resource: usize) -> Result<usize, &'static str> {
        match resource {
            0 => Ok(self.cpu_time_limit),
            1 => Ok(self.max_file_size),
            2 => Ok(self.max_data_size),
            3 => Ok(self.max_stack_size),
            7 => Ok(self.max_fds),
            _ => Err("einval"),
        }
    }

    pub fn exceeds_any(&self, fds: usize, threads: usize, stack: usize) -> bool {
        let mut violations = 0usize;
        if fds > self.max_fds { violations += 1; }
        if threads > self.max_threads { violations += 1; }
        if stack > self.max_stack_size { violations += 1; }
        violations
    }
}

pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
    (a & !mask) | (b & mask)
}

pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
    if width == 0 || width > 64 { return value; }
    let actual = amount % width;
    if actual == 0 { return value; }
    let mask = if width == 64 { !0u64 } else { (1u64 << width) - 1 };
    let v = value & mask;
    ((v << actual) | (v >> (width - actual))) & mask
}

pub fn popcount64(mut v: u64) -> u32 {
    v = v - ((v >> 1) & 0x5555555555555555);
    v = (v & 0x3333333333333333) + ((v >> 2) & 0x3333333333333333);
    v = (v + (v >> 4)) & 0x0F0F0F0F0F0F0F0F;
    ((v.wrapping_mul(0x0101010101010101)) >> 56) as u32
}

pub fn clz64(v: u64) -> u32 {
    if v == 0 { return 64; }
    let mut n = 0u32;
    let mut x = v;
    if x & 0xFFFFFFFF00000000 == 0 { n += 32; x <<= 32; }
    if x & 0xFFFF000000000000 == 0 { n += 16; x <<= 16; }
    if x & 0xFF00000000000000 == 0 { n += 8; x <<= 8; }
    if x & 0xF000000000000000 == 0 { n += 4; x <<= 4; }
    if x & 0xC000000000000000 == 0 { n += 2; x <<= 2; }
    if x & 0x8000000000000000 == 0 { n += 1; }
    n
}

pub fn ffs64(v: u64) -> Option<u32> {
    if v == 0 { return None; }
    Some(63 - clz64(v & v.wrapping_neg()))
}

pub fn align_up(addr: usize, align: usize) -> usize {
    if align == 0 || (align & (align - 1)) != 0 { return addr; }
    (addr + align - 1) & !(align - 1)
}

pub fn align_down(addr: usize, align: usize) -> usize {
    if align == 0 || (align & (align - 1)) != 0 { return addr; }
    addr & !(align - 1)
}

pub fn is_power_of_two(v: usize) -> bool {
    v != 0 && (v & (v - 1)) == 0
}

pub fn log2_floor(v: usize) -> usize {
    if v == 0 { return 0; }
    (std::mem::size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

pub fn hash_combine(seed: u64, value: u64) -> u64 {
    seed ^ (value.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(seed << 6).wrapping_add(seed >> 2))
}

pub fn murmurhash3_finalize(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    h
}

pub struct BuddyAllocator {
    pub free_lists: Vec<Vec<usize>>,
    pub max_order: usize,
    pub base_addr: usize,
    pub total_pages: usize,
    pub allocated: AtomicUsize,
}

impl BuddyAllocator {
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
        while remaining >= block_pages {
            free_lists[usable_order].push(addr);
            addr += block_pages * PAGE_SZ;
            remaining -= block_pages;
        }
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

    pub fn alloc_order(&mut self, order: usize) -> Option<usize> {
        if order > self.max_order { return None; }
        for o in order..=self.max_order {
            if let Some(block) = self.free_lists[o].pop() {
                let mut current_order = o;
                let mut addr = block;
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

    pub fn free_order(&mut self, addr: usize, order: usize) {
        if order > self.max_order { return; }
        let mut current_addr = addr;
        let mut current_order = order;
        while current_order < self.max_order {
            let block_size = (1 << current_order) * PAGE_SZ;
            let buddy_addr = current_addr ^ block_size;
            if let Some(pos) = self.free_lists[current_order].iter().position(|&a| a == buddy_addr) {
                self.free_lists[current_order].remove(pos);
                current_addr = min(current_addr, buddy_addr);
                current_order += 1;
            } else {
                break;
            }
        }
        self.free_lists[current_order].push(current_addr);
        self.allocated.fetch_sub(1 << order, Ordering::Relaxed);
    }

    pub fn free_pages_count(&self) -> usize {
        let mut count = 0;
        for (order, list) in self.free_lists.iter().enumerate() {
            count += list.len() * (1 << order);
        }
        count
    }

    pub fn largest_free_order(&self) -> usize {
        for o in (0..=self.max_order).rev() {
            if !self.free_lists[o].is_empty() { return o; }
        }
        0
    }

    pub fn fragmentation_score(&self) -> usize {
        let total_free = self.free_pages_count();
        if total_free == 0 { return 0; }
        let largest = self.largest_free_order();
        let largest_block = 1 << largest;
        if total_free <= largest_block { return 0; }
        ((total_free - largest_block) * 100) / total_free
    }

    pub fn snapshot(&self) -> BuddyAllocator {
        BuddyAllocator {
            free_lists: self.free_lists.clone(),
            max_order: self.max_order,
            base_addr: self.base_addr,
            total_pages: self.total_pages,
        }
    }
}
