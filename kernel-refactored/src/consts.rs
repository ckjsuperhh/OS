//! 内核常量、系统参数和系统调用号定义模块。
//!
//! 本模块是整个内核的"常量中心"，集中管理所有子系统共享的数值常量。
//! 按子系统分为：内存管理、文件控制、终端控制、ELF 加载、调度器、
//! 信号处理、定时器、网络通信和系统调用编号等类别。
//!
//! 将常量集中管理的好处：
//! 1. 避免在各模块中重复定义魔法数字
//! 2. 修改一处即可全局生效
//! 3. 阅读代码时能快速定位某个常量的含义

// ==================== 内存与体系结构 ====================

/// 内存页标准大小（4KB），x86-64 和 RISC-V 通用
pub const PAGE_SZ: usize = 4096;
/// 系统最大支持进程数
pub const N_PROC: usize = 256;
/// 可用物理页框总数（65536 页 = 256MB）
pub const N_FRAMES: usize = 65536;
/// 内核虚拟地址空间基址（64 位高半内核典型值）
pub const KERN_BASE: usize = 0xFFFF_FFFF_8000_0000;
/// 物理内存线性映射区的虚拟地址偏移
pub const PHYS_OFF: usize = 0xFFFF_FFFF_0000_0000;
/// 板级物理内存起始偏移（RISC-V QEMU virt 机器为 0x80000000）
pub const MEM_OFF: usize = 0x8000_0000;
/// 内核动态堆总大小（8MB）
pub const KHEAP_SZ: usize = 0x800000;
/// 哈希表/就绪队列链数（用于进程哈希表和调度器就绪队列）
pub const N_CHAINS: usize = 64;
/// 环形缓冲区单缓冲最大容量（Channel / Pipe 默认大小）
pub const RBUF_CAP: usize = 256;
/// 进程上下文保存的通用寄存器数量（RISC-V callee-saved 寄存器）
pub const N_REGS: usize = 16;
/// 文件系统挂载嵌套最大深度（防止循环挂载）
pub const MNT_DEPTH: usize = 8;
/// 最大支持 CPU 核心数
pub const MAX_CPU: usize = 8;
/// 每线程内核栈大小（16KB）
pub const KSTK_SZ: usize = 0x4000;
/// 用户进程栈虚拟地址起始偏移
pub const USR_STK_OFF: usize = 0x7FFF_0000;
/// 用户进程栈大小（64KB）
pub const USR_STK_SZ: usize = 0x10000;
/// 系统时钟 tick 单位（微秒），1 tick = 1ms = 1000us
pub const USEC_TICK: usize = 1000;
/// 符号链接/进程嵌套 follow 限制（防止无限循环解析）
pub const FOLLOW_LIM: usize = 3;

// ==================== fcntl 文件控制命令 ====================
// 这些常量用于 sys_fcntl() 系统调用中的命令分发

/// 复制文件描述符（新 fd >= arg）
pub const F_DUPFD: usize = 0;
/// 获取文件描述符标志
pub const F_GETFD: usize = 1;
/// 设置文件描述符标志
pub const F_SETFD: usize = 2;
/// 获取文件状态标志（如 O_NONBLOCK、O_APPEND）
pub const F_GETFL: usize = 3;
/// 设置文件状态标志
pub const F_SETFL: usize = 4;
/// 获取文件锁信息（查询是否有冲突锁）
pub const F_GETLK: usize = 5;
/// 设置文件锁（非阻塞）
pub const F_SETLK: usize = 6;
/// 设置文件锁（阻塞等待直到可用）
pub const F_SETLKW: usize = 7;
/// close-on-exec 标志位值
pub const FD_CLOEXEC: usize = 1;
/// 复制 fd 并同时设置 CLOEXEC 标志
pub const F_DUPFD_CLOEXEC: usize = 1030;
/// 非阻塞 I/O 模式标志
pub const O_NONBLOCK: usize = 0o4000;
/// 追加写模式（每次 write 前将偏移量移到文件末尾）
pub const O_APPEND: usize = 0o2000;
/// 打开文件时自动设置 CLOEXEC 标志
pub const O_CLOEXEC: usize = 0o2000000;
/// 路径解析时不跟随符号链接
pub const AT_NOFOLLOW: usize = 0x100;

// ==================== 终端 / IOCTL 命令 ====================
// 这些值与 Linux ioctl 编号兼容，确保用户态程序能正确调用

/// 获取终端属性（termios 结构体）
pub const TCGETS: usize = 0x5401;
/// 设置终端属性
pub const TCSETS: usize = 0x5402;
/// 获取终端前台进程组 ID
pub const TIOCGPGRP: usize = 0x540F;
/// 设置终端前台进程组 ID
pub const TIOCSPGRP: usize = 0x5410;
/// 获取终端窗口大小（行数和列数）
pub const TIOCGWINSZ: usize = 0x5413;
/// 清除文件描述符的 CLOEXEC 标志
pub const FIONCLEX: usize = 0x5450;
/// 设置文件描述符的 CLOEXEC 标志
pub const FIOCLEX: usize = 0x5451;
/// 设置/清除文件描述符的非阻塞模式
pub const FIONBIO: usize = 0x5421;

// ==================== ELF 辅助向量 ====================
// exec() 加载 ELF 时，将这些信息通过用户栈传递给动态链接器

/// 程序头表的虚拟地址
pub const AT_PHDR: u8 = 3;
/// 单个程序头条目的大小
pub const AT_PHENT: u8 = 4;
/// 程序头表中条目的数量
pub const AT_PHNUM: u8 = 5;
/// 系统页大小
pub const AT_PAGESZ: u8 = 6;
/// 动态链接器（解释器）的加载基址
pub const AT_BASE: u8 = 7;
/// 程序的入口点地址
pub const AT_ENTRY: u8 = 9;

// ==================== 终端行模式标志 ====================
// 控制终端的行为模式，通过位或组合使用

/// 启用信号生成（Ctrl+C → SIGINT，Ctrl+Z → SIGTSTP）
pub const LM_ISIG: u32 = 0o000001;
/// 规范模式（行缓冲，支持退格、行编辑等特殊字符处理）
pub const LM_ICANON: u32 = 0o000002;
/// 回显输入字符到终端
pub const LM_ECHO: u32 = 0o000010;
/// 擦除字符时回显为 BS-SP-BS（退格-空格-退格）
pub const LM_ECHOE: u32 = 0o000020;
/// kill 字符（通常是 Ctrl+U）后回显换行
pub const LM_ECHOK: u32 = 0o000040;
/// 即使未设 ECHO 也回显换行符
pub const LM_ECHONL: u32 = 0o000100;
/// 信号产生后不刷新输入/输出队列
pub const LM_NOFLSH: u32 = 0o000200;
/// 后台进程组写终端时发送 SIGTTOU 信号
pub const LM_TOSTOP: u32 = 0o000400;
/// 启用扩展输入处理（如 IEXTEN 相关功能）
pub const LM_IEXTEN: u32 = 0o100000;
/// 规范大小写表示（历史遗留，已废弃）
pub const LM_XCASE: u32 = 0o000004;
/// 将控制字符回显为 ^X 格式（如 Ctrl+A 显示为 ^A）
pub const LM_ECHOCTL: u32 = 0o001000;
/// 可视擦除模式（打印被擦除的字符）
pub const LM_ECHOPRT: u32 = 0o002000;
/// kill 行时回显 BS-SP-BS 擦除整行
pub const LM_ECHOKE: u32 = 0o004000;
/// 输出正在被刷新（与 DISCARD 字符相关）
pub const LM_FLUSHO: u32 = 0o010000;
/// 重读未处理的输入（REPRINT 字符后使用）
pub const LM_PENDIN: u32 = 0o040000;
/// 外部处理模式（由外部进程处理终端输入）
pub const LM_EXTPROC: u32 = 0o200000;

// ==================== 虚拟内存标志 ====================
// 用于 mmap 系统调用中标识内存区域的权限和属性

/// 可读权限
pub const VM_READ: u32 = 0x01;
/// 可写权限
pub const VM_WRITE: u32 = 0x02;
/// 可执行权限
pub const VM_EXEC: u32 = 0x04;
/// 共享映射（修改对其他映射同一文件的进程可见）
pub const VM_SHARED: u32 = 0x08;
/// 向下增长（用于栈区域，地址向低地址扩展）
pub const VM_GROWSDOWN: u32 = 0x10;
/// fork 时不复制此区域（用于 vDSO 等特殊映射）
pub const VM_DONTCOPY: u32 = 0x20;
/// 使用大页（HugeTLB）映射
pub const VM_HUGETLB: u32 = 0x40;
/// 直接页框号映射（用于设备内存等非 RAM 区域）
pub const VM_PFNMAP: u32 = 0x80;

// ==================== 进程能力 (Capabilities) ====================
// Linux 风格的细粒度权限控制，将超级用户权限拆分为多个独立能力位

/// 修改文件所有者的能力
pub const CAP_CHOWN: u32 = 0;
/// 向任意进程发送信号的能力
pub const CAP_KILL: u32 = 5;
/// 修改 GID 的能力
pub const CAP_SETGID: u32 = 6;
/// 修改 UID 的能力
pub const CAP_SETUID: u32 = 7;
/// 绑定特权端口（< 1024）的能力
pub const CAP_NET_BIND: u32 = 10;
/// 使用原始套接字的能力
pub const CAP_NET_RAW: u32 = 13;
/// 跟踪任意进程（ptrace）的能力
pub const CAP_SYS_PTRACE: u32 = 19;
/// 系统管理操作能力（挂载、模块加载等，最强大的能力）
pub const CAP_SYS_ADMIN: u32 = 21;
/// 可继承能力掩码（exec 后可传递给子进程的能力位范围）
pub const INHERITABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF;

// ==================== 内存区域 (Zones) ====================
// 物理内存按用途分为不同区域

/// DMA 区域（低 16MB，供旧式 ISA 设备 DMA 访问使用）
pub const ZONE_DMA: usize = 0;
/// 常规区域（16MB-896MB，内核可直接映射访问）
pub const ZONE_NORMAL: usize = 1;
/// 高端区域（896MB 以上，32 位系统上需要临时映射才能访问）
pub const ZONE_HIGH: usize = 2;
/// 内存区域总数
pub const N_ZONES: usize = 3;

// ==================== 调度器 ====================
// 进程优先级范围和调度策略编号

/// 最高优先级（Nice 值 -20，获得最多 CPU 时间）
pub const PRIO_MIN: i32 = -20;
/// 最低优先级（Nice 值 19，获得最少 CPU 时间）
pub const PRIO_MAX: i32 = 19;
/// 默认优先级（Nice 值 0）
pub const PRIO_DEFAULT: i32 = 0;
/// 普通分时调度策略（类似 Linux CFS）
pub const SCHED_NORMAL: u8 = 0;
/// 实时先进先出调度策略（一旦运行就持续占用 CPU 直到主动让出）
pub const SCHED_FIFO: u8 = 1;
/// 实时轮转调度策略（同优先级的实时进程轮流运行）
pub const SCHED_RR: u8 = 2;
/// 批处理调度策略（低优先级后台任务，不要求交互响应性）
pub const SCHED_BATCH: u8 = 3;

// ==================== Slab 分配器 ====================
// 内核对象分配器的参数约束

/// 最小可分配对象大小（8 字节）
pub const SLAB_OBJ_MIN: usize = 8;
/// 最大可分配对象大小（2KB，超过则直接从页分配器分配）
pub const SLAB_OBJ_MAX: usize = 2048;
/// 对象对齐要求（8 字节对齐，满足大多数数据结构的对齐需求）
pub const SLAB_ALIGN: usize = 8;

// ==================== 信号 ====================
// POSIX 信号相关的常量和特殊信号编号

/// 支持的最大信号数量
pub const NSIG: u32 = 64;
/// 默认处理动作（由内核根据信号类型决定：终止、忽略、停止等）
pub const SIG_DFL: usize = 0;
/// 忽略信号（信号到达后不做任何处理）
pub const SIG_IGN: usize = 1;
/// 强制终止信号（不可被捕获或忽略）
pub const SIGKILL: u32 = 9;
/// 强制停止信号（不可被捕获或忽略）
pub const SIGSTOP: u32 = 19;
/// 子进程状态变化信号（子进程退出、停止、继续时发送给父进程）
pub const SIGCHLD: u32 = 17;
/// 用户自定义信号 1
pub const SIGUSR1: u32 = 10;
/// 用户自定义信号 2
pub const SIGUSR2: u32 = 12;
/// 定时器闹钟信号（由 alarm() / setitimer() 触发）
pub const SIGALRM: u32 = 14;

// ==================== 定时器 ====================
// 时间轮调度器的参数

/// 时间轮槽数（256，2 的幂便于取模运算）
pub const TIMER_WHEEL_SIZE: usize = 256;
/// 时钟中断频率（100Hz，即每 10ms 一次 tick）
pub const TIMER_TICK_HZ: usize = 100;
/// 启动纪元时间（时间计算的起点）
pub const BOOT_EPOCH: usize = 0;

// ==================== 套接字 / 网络 ====================
// socket 类型和地址族常量，与 Linux 兼容

/// 流式套接字（面向连接，可靠字节流，如 TCP）
pub const SOCK_STREAM: u32 = 1;
/// 数据报套接字（无连接，不可靠消息，如 UDP）
pub const SOCK_DGRAM: u32 = 2;
/// 原始套接字（直接操作网络层协议）
pub const SOCK_RAW: u32 = 3;
/// IPv4 地址族
pub const AF_INET: u32 = 2;
/// IPv6 地址族
pub const AF_INET6: u32 = 10;
/// Unix 域套接字（本地进程间通信）
pub const AF_UNIX: u32 = 1;

// ==================== 系统调用号 ====================
// 与 Linux x86-64 ABI 兼容的系统调用编号
// 用户态通过 rax 寄存器传入这些编号来发起系统调用

/// read(fd, buf, count) — 从文件描述符读取
pub const SYS_READ: usize = 0;
/// write(fd, buf, count) — 向文件描述符写入
pub const SYS_WRITE: usize = 1;
/// open(path, flags, mode) — 打开文件
pub const SYS_OPEN: usize = 2;
/// close(fd) — 关闭文件描述符
pub const SYS_CLOSE: usize = 3;
/// stat(path, buf) — 获取文件状态（通过路径）
pub const SYS_STAT: usize = 4;
/// fstat(fd, buf) — 获取文件状态（通过 fd）
pub const SYS_FSTAT: usize = 5;
/// mmap(addr, len, prot, flags, fd, off) — 内存映射
pub const SYS_MMAP: usize = 9;
/// munmap(addr, len) — 解除内存映射
pub const SYS_MUNMAP: usize = 11;
/// brk(addr) — 修改数据段大小
pub const SYS_BRK: usize = 12;
/// rt_sigaction(signo, act, oldact, sigsetsize) — 注册信号处理函数
pub const SYS_SIGACTION: usize = 13;
/// rt_sigprocmask(how, set, oldset, sigsetsize) — 修改信号屏蔽掩码
pub const SYS_SIGPROCMASK: usize = 14;
/// ioctl(fd, cmd, arg) — 设备控制
pub const SYS_IOCTL: usize = 16;
/// pipe(pipefd) — 创建管道
pub const SYS_PIPE: usize = 22;
/// dup(oldfd) — 复制文件描述符
pub const SYS_DUP: usize = 32;
/// dup2(oldfd, newfd) — 复制文件描述符到指定编号
pub const SYS_DUP2: usize = 33;
/// getpid() — 获取当前进程 ID
pub const SYS_GETPID: usize = 39;
/// fork() — 创建子进程（写时复制）
pub const SYS_FORK: usize = 57;
/// execve(path, argv, envp) — 加载并执行新程序
pub const SYS_EXEC: usize = 59;
/// exit(status) — 终止当前进程
pub const SYS_EXIT: usize = 60;
/// wait4(pid, status, options, rusage) — 等待子进程状态变化
pub const SYS_WAIT4: usize = 61;
/// kill(pid, sig) — 向进程发送信号
pub const SYS_KILL: usize = 62;
/// fcntl(fd, cmd, arg) — 文件描述符控制
pub const SYS_FCNTL: usize = 72;
/// setpgid(pid, pgid) — 设置进程组 ID
pub const SYS_SETPGID: usize = 109;
/// getppid() — 获取父进程 ID
pub const SYS_GETPPID: usize = 110;
/// setsid() — 创建新会话
pub const SYS_SETSID: usize = 112;
/// getpgid(pid) — 获取进程组 ID
pub const SYS_GETPGID: usize = 121;
/// futex(uaddr, op, val, timeout, uaddr2, val3) — 快速用户态互斥锁
pub const SYS_FUTEX: usize = 202;
/// epoll_create1(flags) — 创建 epoll 实例
pub const SYS_EPOLL_CREATE: usize = 213;
/// clock_gettime(clk_id, tp) — 获取时钟时间
pub const SYS_CLOCK_GETTIME: usize = 228;
/// epoll_wait(epfd, events, maxevents, timeout) — 等待 epoll 事件
pub const SYS_EPOLL_WAIT: usize = 232;
/// epoll_ctl(epfd, op, fd, event) — 控制 epoll 实例
pub const SYS_EPOLL_CTL: usize = 233;

// ==================== IO 队列 ====================

/// I/O 请求队列深度（最大并发 I/O 操作数）
pub const IOQUEUE_DEPTH: usize = 128;
