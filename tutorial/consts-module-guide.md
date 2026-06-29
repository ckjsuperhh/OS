# Consts 模块阅读指南

> 文件路径: `kernel-refactored/src/consts.rs`
> 代码量: 208 行 | 0 个结构体 | 纯常量定义，无外部依赖

---

## 一、模块概述

`consts.rs` 是整个内核的 **常量中心**，集中定义了所有子系统共享的数值常量。它不包含任何结构体或函数逻辑，纯粹是 `pub const` 声明的集合。

| 分类 | 用途 | 典型消费者 |
|---|---|---|
| 内存与体系结构 | 页大小、物理/虚拟地址布局、栈大小 | `proc.rs`, `fs.rs`, `vm.rs` |
| 文件控制 (fcntl) | 文件描述符操作命令、打开标志 | `fs.rs` |
| 终端 / IOCTL | 终端控制命令码 | `fs.rs`, `channel.rs` |
| ELF 辅助向量 | 加载 ELF 可执行文件时传递给用户程序的参数 | `proc.rs` |
| 终端行模式标志 | 控制终端的回显、信号、编辑行为 | `fs.rs` |
| 虚拟内存标志 | mmap 区域的读写执行权限 | `vm.rs` |
| 能力 (Capabilities) | Linux 风格的权限控制位 | `proc.rs` |
| 内存区域 (Zones) | DMA/Normal/High 内存分区 | `vm.rs` |
| 调度 | 优先级范围与调度策略编号 | `proc.rs` |
| Slab 分配器 | 对象大小和对齐约束 | `vm.rs` |
| 信号 | 信号编号和默认动作 | `signal.rs`, `proc.rs` |
| 定时器 | 时间轮大小和时钟频率 | `timer.rs` |
| 套接字/网络 | socket 类型和地址族 | `net.rs`（如有） |
| 系统调用号 | x86-64 兼容的系统调用编号 | 系统调用分发器 |
| IO 队列 | 异步 I/O 请求队列深度 | `fs.rs` |

**设计定位：** Consts 模块在内核中扮演"字典"的角色——所有数值型配置、协议编号、硬件参数都在这里查找。将常量集中管理的好处是：
1. 避免在各模块中重复定义魔法数字
2. 修改一处即可全局生效
3. 阅读代码时能快速定位某个常量的含义

---

## 二、常量分类详解

### 2.1 内存与体系结构常量

```rust
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
/// 进程上下文保存的通用寄存器数量（RISC-V 为 16 个 callee-saved 寄存器）
pub const N_REGS: usize = 16;
/// 文件系统挂载嵌套最大深度
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
/// 符号链接/进程嵌套 follow 限制（防止无限循环）
pub const FOLLOW_LIM: usize = 3;
```

**地址空间布局示意：**

```
  虚拟地址空间 (64-bit)
  ┌──────────────────────────┐ 0xFFFF_FFFF_FFFF_FFFF
  │     内核空间             │
  │     KERN_BASE ──────────►│ 0xFFFF_FFFF_8000_0000
  │     PHYS_OFF ──────────► │ 0xFFFF_FFFF_0000_0000
  │                          │
  ├──────────────────────────┤
  │     用户空间             │
  │     USR_STK_OFF ───────► │ 0x7FFF_0000
  │                          │
  └──────────────────────────┘ 0x0000_0000_0000_0000
```

### 2.2 fcntl 命令常量

```rust
/// 复制文件描述符（到新 fd >= arg）
pub const F_DUPFD: usize = 0;
/// 获取文件描述符标志
pub const F_GETFD: usize = 1;
/// 设置文件描述符标志
pub const F_SETFD: usize = 2;
/// 获取文件状态标志
pub const F_GETFL: usize = 3;
/// 设置文件状态标志
pub const F_SETFL: usize = 4;
/// 获取文件锁信息
pub const F_GETLK: usize = 5;
/// 设置文件锁（非阻塞）
pub const F_SETLK: usize = 6;
/// 设置文件锁（阻塞等待）
pub const F_SETLKW: usize = 7;
/// close-on-exec 标志
pub const FD_CLOEXEC: usize = 1;
/// 复制 fd 并设置 CLOEXEC
pub const F_DUPFD_CLOEXEC: usize = 1030;
/// 非阻塞 I/O 标志
pub const O_NONBLOCK: usize = 0o4000;
/// 追加写模式
pub const O_APPEND: usize = 0o2000;
/// 打开时设置 CLOEXEC
pub const O_CLOEXEC: usize = 0o2000000;
/// 不跟随符号链接
pub const AT_NOFOLLOW: usize = 0x100;
```

**使用场景：** 这些常量在 `fs.rs` 的 `sys_fcntl()` 实现中被用作命令分发依据。

### 2.3 终端 / IOCTL 常量

```rust
pub const TCGETS: usize = 0x5401;    // 获取终端属性
pub const TCSETS: usize = 0x5402;    // 设置终端属性
pub const TIOCGPGRP: usize = 0x540F; // 获取终端前台进程组
pub const TIOCSPGRP: usize = 0x5410; // 设置终端前台进程组
pub const TIOCGWINSZ: usize = 0x5413;// 获取终端窗口大小
pub const FIONCLEX: usize = 0x5450;  // 清除 CLOEXEC 标志
pub const FIOCLEX: usize = 0x5451;   // 设置 CLOEXEC 标志
pub const FIONBIO: usize = 0x5421;   // 设置/清除非阻塞模式
```

**与 Linux 的兼容性：** 这些值直接取自 Linux 的 `ioctl` 编号，确保用户态程序（如 shell）能正确调用终端控制接口。

### 2.4 ELF 辅助向量

```rust
pub const AT_PHDR: u8 = 3;     // 程序头表地址
pub const AT_PHENT: u8 = 4;    // 单个程序头大小
pub const AT_PHNUM: u8 = 5;    // 程序头数量
pub const AT_PAGESZ: u8 = 6;   // 系统页大小
pub const AT_BASE: u8 = 7;     // 动态链接器基址
pub const AT_ENTRY: u8 = 9;    // 程序入口地址
```

**用途：** 当 `exec()` 加载 ELF 可执行文件时，内核会在用户栈上构建辅助向量数组，将这些信息传递给动态链接器（如 `ld-linux.so`）或程序本身。

### 2.5 终端行模式标志

```rust
pub const LM_ISIG: u32 = 0o000001;     // 启用信号（Ctrl+C → SIGINT）
pub const LM_ICANON: u32 = 0o000002;   // 规范模式（行缓冲，支持退格等编辑）
pub const LM_ECHO: u32 = 0o000010;     // 回显输入字符
pub const LM_ECHOE: u32 = 0o000020;    // 回显擦除字符为 BS-SP-BS
pub const LM_ECHOK: u32 = 0o000040;    // kill 字符后回显换行
pub const LM_ECHONL: u32 = 0o000100;   // 即使未设 ECHO 也回显换行
pub const LM_NOFLSH: u32 = 0o000200;   // 信号后不刷新输入/输出队列
pub const LM_TOSTOP: u32 = 0o000400;   // 后台进程写终端时发 SIGTTOU
pub const LM_IEXTEN: u32 = 0o100000;   // 启用扩展输入处理
pub const LM_XCASE: u32 = 0o000004;    // 规范大小写表示（已废弃）
pub const LM_ECHOCTL: u32 = 0o001000;  // 回显控制字符为 ^X
pub const LM_ECHOPRT: u32 = 0o002000;  // 可视擦除模式
pub const LM_ECHOKE: u32 = 0o004000;   // kill 行时回显 BS-SP-BS
pub const LM_FLUSHO: u32 = 0o010000;   // 输出被刷新
pub const LM_PENDIN: u32 = 0o040000;   // 重读输入
pub const LM_EXTPROC: u32 = 0o200000;  // 外部处理模式
```

**组合使用：** 这些标志通过位或组合控制终端行为。典型的交互式 shell 终端设置为 `ISIG | ICANON | ECHO | ECHOE | ECHOK`。

### 2.6 虚拟内存标志

```rust
pub const VM_READ: u32 = 0x01;      // 可读
pub const VM_WRITE: u32 = 0x02;     // 可写
pub const VM_EXEC: u32 = 0x04;      // 可执行
pub const VM_SHARED: u32 = 0x08;    // 共享映射（修改对其他进程可见）
pub const VM_GROWSDOWN: u32 = 0x10; // 向下增长（用于栈区域）
pub const VM_DONTCOPY: u32 = 0x20;  // fork 时不复制
pub const VM_HUGETLB: u32 = 0x40;   // 大页映射
pub const VM_PFNMAP: u32 = 0x80;    // 直接页框号映射（设备内存）
```

**典型组合：**
- 代码段: `VM_READ | VM_EXEC`
- 数据段: `VM_READ | VM_WRITE`
- 栈区域: `VM_READ | VM_WRITE | VM_GROWSDOWN`
- 共享库: `VM_READ | VM_EXEC | VM_SHARED`

### 2.7 能力 (Capabilities)

```rust
pub const CAP_CHOWN: u32 = 0;          // 修改文件所有者
pub const CAP_KILL: u32 = 5;           // 发送信号给任意进程
pub const CAP_SETGID: u32 = 6;         // 修改 GID
pub const CAP_SETUID: u32 = 7;         // 修改 UID
pub const CAP_NET_BIND: u32 = 10;      // 绑定特权端口 (<1024)
pub const CAP_NET_RAW: u32 = 13;       // 使用原始套接字
pub const CAP_SYS_PTRACE: u32 = 19;    // 跟踪任意进程
pub const CAP_SYS_ADMIN: u32 = 21;     // 系统管理操作（挂载等）
pub const INHERABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF; // 可继承能力掩码
```

### 2.8 内存区域 (Zones)

```rust
pub const ZONE_DMA: usize = 0;     // DMA 区域（低 16MB，供旧式 ISA 设备使用）
pub const ZONE_NORMAL: usize = 1;  // 常规区域（16MB-896MB，内核直接映射）
pub const ZONE_HIGH: usize = 2;    // 高端区域（896MB 以上，需临时映射）
pub const N_ZONES: usize = 3;      // 区域总数
```

### 2.9 调度常量

```rust
pub const PRIO_MIN: i32 = -20;     // 最高优先级（Nice 值）
pub const PRIO_MAX: i32 = 19;      // 最低优先级
pub const PRIO_DEFAULT: i32 = 0;   // 默认优先级
pub const SCHED_NORMAL: u8 = 0;    // 普通分时调度（CFS 类似）
pub const SCHED_FIFO: u8 = 1;      // 实时先进先出调度
pub const SCHED_RR: u8 = 2;        // 实时轮转调度
pub const SCHED_BATCH: u8 = 3;     // 批处理调度（低优先级后台任务）
```

### 2.10 Slab 分配器常量

```rust
pub const SLAB_OBJ_MIN: usize = 8;    // 最小对象大小（8 字节）
pub const SLAB_OBJ_MAX: usize = 2048; // 最大对象大小（2KB）
pub const SLAB_ALIGN: usize = 8;      // 对象对齐要求（8 字节）
```

### 2.11 信号常量

```rust
pub const NSIG: u32 = 64;        // 支持的最大信号数
pub const SIG_DFL: usize = 0;    // 默认处理（终止进程等）
pub const SIG_IGN: usize = 1;    // 忽略信号
pub const SIGKILL: u32 = 9;      // 强制终止（不可捕获/忽略）
pub const SIGSTOP: u32 = 19;     // 强制停止（不可捕获/忽略）
pub const SIGCHLD: u32 = 17;     // 子进程状态变化
pub const SIGUSR1: u32 = 10;     // 用户自定义信号 1
pub const SIGUSR2: u32 = 12;     // 用户自定义信号 2
pub const SIGALRM: u32 = 14;     // 定时器闹钟信号
```

**跨模块关系：** 这些常量被 `signal.rs` 用于信号掩码操作，被 `proc.rs` 用于进程信号传递。

### 2.12 定时器常量

```rust
pub const TIMER_WHEEL_SIZE: usize = 256; // 时间轮槽数（2 的幂便于取模）
pub const TIMER_TICK_HZ: usize = 100;    // 时钟中断频率（100Hz = 10ms/tick）
pub const BOOT_EPOCH: usize = 0;         // 启动纪元（时间起点）
```

### 2.13 套接字/网络常量

```rust
pub const SOCK_STREAM: u32 = 1;  // 流式套接字（TCP）
pub const SOCK_DGRAM: u32 = 2;   // 数据报套接字（UDP）
pub const SOCK_RAW: u32 = 3;     // 原始套接字
pub const AF_INET: u32 = 2;      // IPv4 地址族
pub const AF_INET6: u32 = 10;    // IPv6 地址族
pub const AF_UNIX: u32 = 1;      // Unix 域套接字
```

### 2.14 系统调用号

```rust
pub const SYS_READ: usize = 0;           // read(fd, buf, count)
pub const SYS_WRITE: usize = 1;          // write(fd, buf, count)
pub const SYS_OPEN: usize = 2;           // open(path, flags, mode)
pub const SYS_CLOSE: usize = 3;          // close(fd)
pub const SYS_STAT: usize = 4;           // stat(path, buf)
pub const SYS_FSTAT: usize = 5;          // fstat(fd, buf)
pub const SYS_MMAP: usize = 9;           // mmap(addr, len, prot, flags, fd, off)
pub const SYS_MUNMAP: usize = 11;        // munmap(addr, len)
pub const SYS_BRK: usize = 12;           // brk(addr)
pub const SYS_SIGACTION: usize = 13;     // rt_sigaction(...)
pub const SYS_SIGPROCMASK: usize = 14;   // rt_sigprocmask(...)
pub const SYS_IOCTL: usize = 16;         // ioctl(fd, cmd, arg)
pub const SYS_PIPE: usize = 22;          // pipe(pipefd)
pub const SYS_DUP: usize = 32;           // dup(oldfd)
pub const SYS_DUP2: usize = 33;          // dup2(oldfd, newfd)
pub const SYS_GETPID: usize = 39;        // getpid()
pub const SYS_FORK: usize = 57;          // fork()
pub const SYS_EXEC: usize = 59;          // execve(path, argv, envp)
pub const SYS_EXIT: usize = 60;          // exit(status)
pub const SYS_WAIT4: usize = 61;         // wait4(pid, status, options, rusage)
pub const SYS_KILL: usize = 62;          // kill(pid, sig)
pub const SYS_FCNTL: usize = 72;         // fcntl(fd, cmd, arg)
pub const SYS_SETPGID: usize = 109;      // setpgid(pid, pgid)
pub const SYS_GETPPID: usize = 110;      // getppid()
pub const SYS_SETSID: usize = 112;       // setsid()
pub const SYS_GETPGID: usize = 121;      // getpgid(pid)
pub const SYS_FUTEX: usize = 202;        // futex(uaddr, op, val, ...)
pub const SYS_EPOLL_CREATE: usize = 213; // epoll_create1(flags)
pub const SYS_CLOCK_GETTIME: usize = 228;// clock_gettime(clk_id, tp)
pub const SYS_EPOLL_WAIT: usize = 232;   // epoll_wait(epfd, events, max, timeout)
pub const SYS_EPOLL_CTL: usize = 233;    // epoll_ctl(epfd, op, fd, event)
```

**编号兼容性：** 这些编号与 Linux x86-64 ABI 完全一致，确保编译好的用户态二进制文件能正确发起系统调用。

### 2.15 IO 队列常量

```rust
pub const IOQUEUE_DEPTH: usize = 128; // I/O 请求队列深度（最大并发 I/O 数）
```

---

## 三、使用场景

### 3.1 进程管理中的使用

```rust
// proc.rs 中使用 PAGE_SZ 对齐栈分配
let stack_addr = USR_STK_OFF;  // 用户栈起始虚拟地址
let stack_size = USR_STK_SZ;   // 64KB 栈空间

// 进程表大小由 N_PROC 控制
let procs = Vec::with_capacity(N_PROC);
```

### 3.2 Channel/Pipe 中的使用

```rust
// channel.rs 中使用 RBUF_CAP 作为默认环形缓冲区大小
let ch = Channel::new(RBUF_CAP);  // 256 字节

// fs.rs 中 Pipe 实现引用同样的常量
```

### 3.3 系统调用分发中的使用

```rust
// 系统调用入口根据 SYS_* 常量分发
match syscall_no {
    SYS_READ => sys_read(fd, buf, count),
    SYS_WRITE => sys_write(fd, buf, count),
    SYS_FORK => sys_fork(),
    // ...
}
```

---

## 四、跨模块连接

```
consts.rs
├──► signal.rs  — NSIG, SIG_DFL, SIG_IGN, SIGKILL, SIGSTOP 等信号常量
├──► timer.rs   — TIMER_WHEEL_SIZE, TIMER_TICK_HZ, BOOT_EPOCH
├──► channel.rs — RBUF_CAP（环形缓冲区默认容量）
├──► proc.rs    — N_PROC, N_REGS, PAGE_SZ, KSTK_SZ, USR_STK_OFF/SZ
│                 SCHED_*, PRIO_*, 信号常量, SYS_* 系统调用号
├──► fs.rs      — fcntl 命令, IOCTL 命令, 终端模式标志, FOLLOW_LIM
├──► vm.rs      — VM_* 内存标志, SLAB_*, ZONE_*, PAGE_SZ, N_FRAMES
└──► 系统调用入口 — 所有 SYS_* 常量
```

---

## 五、与原版 kernel.rs 的对应

| consts.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| 内存常量 (PAGE_SZ, KERN_BASE 等) | 散布在文件开头的全局常量区 |
| 系统调用号 (SYS_*) | `syscall_dispatch()` 函数中的 match 臂 |
| 信号常量 | 信号处理相关函数附近 |
| 终端标志 | TTY 设备驱动的 ioctl 分支中 |

---

## 六、潜在的改进方向

1. **常量类型不统一**：部分用 `usize`，部分用 `u32`，部分用 `u8`。应统一为语义最合适的类型，或使用 `newtype` 模式封装
2. **缺少 const 断言**：例如应保证 `PAGE_SZ` 是 2 的幂、`TIMER_WHEEL_SIZE` 是 2 的幂（用于取模优化），可用 `const _: () = assert!(...)` 添加编译期检查
3. **命名风格不一致**：终端模式标志用 `LM_` 前缀，而 Linux 内核用 `ECHO`, `ICANON` 等。可考虑统一命名风格
4. **缺少按模块分文件**：随着内核功能扩展，常量会越来越多，可考虑拆分为 `consts/memory.rs`, `consts/signal.rs` 等子模块
5. **部分常量缺少文档注释**：如 `AT_NOFOLLOW`、`BOOT_EPOCH` 等含义不够直观的常量应补充注释
