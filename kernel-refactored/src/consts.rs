//! Kernel constants, system parameters, and syscall numbers.
//!
//! Organized by subsystem: memory, file control, terminal, ELF, scheduling,
//! signals, timers, networking, and syscall identifiers.

// ==================== Memory & Architecture ====================

/// Memory page standard size (4KB)
pub const PAGE_SZ: usize = 4096;
/// Maximum supported process count
pub const N_PROC: usize = 256;
/// Total available physical page frames
pub const N_FRAMES: usize = 65536;
/// Kernel virtual address space base (64-bit)
pub const KERN_BASE: usize = 0xFFFF_FFFF_8000_0000;
/// Physical memory linear mapping virtual address offset
pub const PHYS_OFF: usize = 0xFFFF_FFFF_0000_0000;
/// Board physical memory start offset (RISC-V)
pub const MEM_OFF: usize = 0x8000_0000;
/// Kernel dynamic heap total size
pub const KHEAP_SZ: usize = 0x800000;
/// Hash table / ready queue chain count
pub const N_CHAINS: usize = 64;
/// Ring buffer single-buffer max capacity
pub const RBUF_CAP: usize = 256;
/// General-purpose registers saved in process context
pub const N_REGS: usize = 16;
/// Maximum filesystem mount nesting depth
pub const MNT_DEPTH: usize = 8;
/// Maximum supported CPU cores
pub const MAX_CPU: usize = 8;
/// Kernel stack size per thread
pub const KSTK_SZ: usize = 0x4000;
/// User process stack virtual address start offset
pub const USR_STK_OFF: usize = 0x7FFF_0000;
/// User process stack size
pub const USR_STK_SZ: usize = 0x10000;
/// System clock tick unit in microseconds (1 tick = 1ms)
pub const USEC_TICK: usize = 1000;
/// Symlink / process nesting follow limit
pub const FOLLOW_LIM: usize = 3;

// ==================== fcntl Commands ====================

pub const F_DUPFD: usize = 0;
pub const F_GETFD: usize = 1;
pub const F_SETFD: usize = 2;
pub const F_GETFL: usize = 3;
pub const F_SETFL: usize = 4;
pub const F_GETLK: usize = 5;
pub const F_SETLK: usize = 6;
pub const F_SETLKW: usize = 7;
pub const FD_CLOEXEC: usize = 1;
pub const F_DUPFD_CLOEXEC: usize = 1030;
pub const O_NONBLOCK: usize = 0o4000;
pub const O_APPEND: usize = 0o2000;
pub const O_CLOEXEC: usize = 0o2000000;
pub const AT_NOFOLLOW: usize = 0x100;

// ==================== Terminal / IOCTL ====================

pub const TCGETS: usize = 0x5401;
pub const TCSETS: usize = 0x5402;
pub const TIOCGPGRP: usize = 0x540F;
pub const TIOCSPGRP: usize = 0x5410;
pub const TIOCGWINSZ: usize = 0x5413;
pub const FIONCLEX: usize = 0x5450;
pub const FIOCLEX: usize = 0x5451;
pub const FIONBIO: usize = 0x5421;

// ==================== ELF Auxiliary Vector ====================

pub const AT_PHDR: u8 = 3;
pub const AT_PHENT: u8 = 4;
pub const AT_PHNUM: u8 = 5;
pub const AT_PAGESZ: u8 = 6;
pub const AT_BASE: u8 = 7;
pub const AT_ENTRY: u8 = 9;

// ==================== Terminal Line Mode Flags ====================

pub const LM_ISIG: u32 = 0o000001;
pub const LM_ICANON: u32 = 0o000002;
pub const LM_ECHO: u32 = 0o000010;
pub const LM_ECHOE: u32 = 0o000020;
pub const LM_ECHOK: u32 = 0o000040;
pub const LM_ECHONL: u32 = 0o000100;
pub const LM_NOFLSH: u32 = 0o000200;
pub const LM_TOSTOP: u32 = 0o000400;
pub const LM_IEXTEN: u32 = 0o100000;
pub const LM_XCASE: u32 = 0o000004;
pub const LM_ECHOCTL: u32 = 0o001000;
pub const LM_ECHOPRT: u32 = 0o002000;
pub const LM_ECHOKE: u32 = 0o004000;
pub const LM_FLUSHO: u32 = 0o010000;
pub const LM_PENDIN: u32 = 0o040000;
pub const LM_EXTPROC: u32 = 0o200000;

// ==================== Virtual Memory Flags ====================

pub const VM_READ: u32 = 0x01;
pub const VM_WRITE: u32 = 0x02;
pub const VM_EXEC: u32 = 0x04;
pub const VM_SHARED: u32 = 0x08;
pub const VM_GROWSDOWN: u32 = 0x10;
pub const VM_DONTCOPY: u32 = 0x20;
pub const VM_HUGETLB: u32 = 0x40;
pub const VM_PFNMAP: u32 = 0x80;

// ==================== Capabilities ====================

pub const CAP_CHOWN: u32 = 0;
pub const CAP_KILL: u32 = 5;
pub const CAP_SETUID: u32 = 7;
pub const CAP_SETGID: u32 = 6;
pub const CAP_NET_BIND: u32 = 10;
pub const CAP_NET_RAW: u32 = 13;
pub const CAP_SYS_ADMIN: u32 = 21;
pub const CAP_SYS_PTRACE: u32 = 19;
pub const INHERITABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF;

// ==================== Memory Zones ====================

pub const ZONE_DMA: usize = 0;
pub const ZONE_NORMAL: usize = 1;
pub const ZONE_HIGH: usize = 2;
pub const N_ZONES: usize = 3;

// ==================== Scheduling ====================

pub const PRIO_MIN: i32 = -20;
pub const PRIO_MAX: i32 = 19;
pub const PRIO_DEFAULT: i32 = 0;
pub const SCHED_NORMAL: u8 = 0;
pub const SCHED_FIFO: u8 = 1;
pub const SCHED_RR: u8 = 2;
pub const SCHED_BATCH: u8 = 3;

// ==================== Slab Allocator ====================

pub const SLAB_OBJ_MIN: usize = 8;
pub const SLAB_OBJ_MAX: usize = 2048;
pub const SLAB_ALIGN: usize = 8;

// ==================== Signals ====================

pub const NSIG: u32 = 64;
pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;
pub const SIGKILL: u32 = 9;
pub const SIGSTOP: u32 = 19;
pub const SIGCHLD: u32 = 17;
pub const SIGUSR1: u32 = 10;
pub const SIGUSR2: u32 = 12;
pub const SIGALRM: u32 = 14;

// ==================== Timer ====================

pub const TIMER_WHEEL_SIZE: usize = 256;
pub const TIMER_TICK_HZ: usize = 100;
pub const BOOT_EPOCH: usize = 0;

// ==================== Socket / Networking ====================

pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_RAW: u32 = 3;
pub const AF_INET: u32 = 2;
pub const AF_INET6: u32 = 10;
pub const AF_UNIX: u32 = 1;

// ==================== Syscall Numbers ====================

pub const SYS_READ: usize = 0;
pub const SYS_WRITE: usize = 1;
pub const SYS_OPEN: usize = 2;
pub const SYS_CLOSE: usize = 3;
pub const SYS_STAT: usize = 4;
pub const SYS_FSTAT: usize = 5;
pub const SYS_MMAP: usize = 9;
pub const SYS_MUNMAP: usize = 11;
pub const SYS_BRK: usize = 12;
pub const SYS_IOCTL: usize = 16;
pub const SYS_PIPE: usize = 22;
pub const SYS_DUP: usize = 32;
pub const SYS_DUP2: usize = 33;
pub const SYS_SIGACTION: usize = 13;
pub const SYS_SIGPROCMASK: usize = 14;
pub const SYS_FORK: usize = 57;
pub const SYS_EXEC: usize = 59;
pub const SYS_EXIT: usize = 60;
pub const SYS_WAIT4: usize = 61;
pub const SYS_KILL: usize = 62;
pub const SYS_FCNTL: usize = 72;
pub const SYS_GETPID: usize = 39;
pub const SYS_GETPPID: usize = 110;
pub const SYS_SETPGID: usize = 109;
pub const SYS_GETPGID: usize = 121;
pub const SYS_SETSID: usize = 112;
pub const SYS_EPOLL_CREATE: usize = 213;
pub const SYS_EPOLL_CTL: usize = 233;
pub const SYS_EPOLL_WAIT: usize = 232;
pub const SYS_CLOCK_GETTIME: usize = 228;
pub const SYS_FUTEX: usize = 202;

// ==================== IO Queue ====================

pub const IOQUEUE_DEPTH: usize = 128;
