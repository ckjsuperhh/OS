# Kernel 模块阅读指南

> 文件路径: `kernel-refactored/src/kernel.rs`
> 代码量: 1204 行 | 1 个核心结构体 | 依赖: `consts`, `sync`, `signal`, `timer`, `memory`, `fs`, `channel`, `ipc`, `trap`, `process`, `sched`, `util`

---

## 一、模块概述

`kernel.rs` 是内核的 **顶层编排器（orchestrator）**，它持有所有子系统的引用，并协调它们完成系统调用分发、进程调度、内存管理等核心功能。

| 功能 | 方法 | 用途 |
|---|---|---|
| 初始化 | `new`, `proc_init` | 创建内核实例、启动 init 进程 |
| 系统调用 | `dispatch_syscall` | 分发 30+ 个系统调用 |
| 调度 | `schedule_tick`, `balance_load` | CPU 调度与负载均衡 |
| 内存 | `alloc_pages`, `free_pages`, `memory_pressure` | 物理页分配与回收 |
| 进程管理 | `do_fork`, `do_exec`, `do_pipe`, `do_wait` | 高层进程操作 |
| 路径解析 | `lookup_path` | 挂载点路径解析 |
| 维护 | `reclaim_zombies`, `tick` | 僵尸回收、缓存刷新 |

**设计定位：** `kernel.rs` 不包含底层数据结构的实现——它通过组合 `process.rs`、`fs.rs`、`memory.rs` 等模块来提供完整的内核功能。可以将它理解为内核的 `main.rs`，所有模块在这里汇合。

---

## 二、Kernel 结构体

### 2.1 定义

```rust
pub struct Kernel {
    pub tasks: TaskTable,                                // 全局任务表
    pub cache: BlockCache,                               // 块缓存
    pub pool: FramePool,                                 // 物理页帧池
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,       // CPU 槽位（每个 CPU 当前运行的任务）
    pub mnt: MountTable,                                 // 挂载表
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,  // 信号量存储（key -> 弱引用）
    pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>, // 共享内存存储
    pub tty_buf: Mutex<VecDeque<u8>>,                    // TTY 输入缓冲区
}
```

**字段说明：**
- `tasks`: 来自 `process.rs`，管理所有进程和线程
- `cache`: 来自 `fs.rs`，组相联块缓存
- `pool`: 来自 `memory.rs`，物理页帧分配器
- `cpus`: 模拟多核 CPU，每个槽位存放当前正在执行的任务
- `sem_store` / `shm_store`: 使用 `Weak` 引用，允许 IPC 对象在所有使用者退出后自动回收
- `tty_buf`: 终端输入缓冲，最多 4096 字节

### 2.2 构造函数

```rust
pub fn new(nf: usize) -> Self
```

创建内核实例，初始化所有子系统：
- `BlockCache::new(N_CHAINS)` — 创建 N_CHAINS 条缓存链
- `FramePool::new(nf)` — 创建 nf 个物理页帧
- `cpus` 初始化为 `[None; MAX_CPU]` — 所有 CPU 空闲

---

## 三、初始化与基础操作

### 3.1 proc_init — 启动 init 进程

```rust
pub fn proc_init(&self) {
    let root = self.tasks.spawn_root();        // 创建 init 进程（ID=1）
    let rid = root.id();
    root.threads.lock().unwrap().push(rid);    // 注册主线程
    let _kstk = KStk::new();                  // 分配内核栈
    *root.kstk.lock().unwrap() = Some(_kstk);
}
```

### 3.2 TTY 操作

```rust
/// 推入一个字节到 TTY 缓冲（\r 自动转 \n）
pub fn tty_push(&self, c: u8)

/// 弹出 TTY 缓冲的一个字节
pub fn tty_pop(&self) -> Option<u8>
```

### 3.3 CPU 任务管理

```rust
/// 获取指定 CPU 上当前运行的任务
pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>>

/// 设置指定 CPU 上运行的任务
pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>)
```

### 3.4 tick — 时钟中断处理

```rust
pub fn tick(&self, id: usize)
```

tick 方法模拟时钟中断：
1. 获取全局内核锁 (GKL)
2. 统计 CPU 占用率
3. 刷新块缓存（清除所有脏块标志）
4. 释放全局内核锁

### 3.5 页错误处理

```rust
/// 处理缺页异常
pub fn handle_pgfault(&self, addr: usize) -> bool

/// 处理带访问类型的缺页异常
pub fn handle_pgfault_ext(&self, addr: usize, _access: u8) -> bool
```

---

## 四、dispatch_syscall — 系统调用分发

这是内核中最长的方法（约 800 行），通过 `match nr` 分发到各个系统调用处理器。

### 4.1 入口处理

```rust
pub fn dispatch_syscall(&self, nr: usize, a0-a5: usize) -> Result<usize, &'static str>
```

每个系统调用入口都有统一的审计逻辑：
- 计算参数异或校验 (`_audit`)
- 记录进入时间戳 (`_ts_enter`)
- 获取调用者的 vm_token

### 4.2 SYS_READ (0)

```
SYS_READ(fd, buf_addr, count)
    │
    ├── 空指针检查 → "efault"
    ├── 地址合法性检查 check_access()
    ├── 计算页跨度 page_span
    │
    ├── 块缓存命中 → 返回缓存数据量
    │
    └── 缓存未命中 → 返回 min(count, PAGE_SZ * 16)
```

### 4.3 SYS_WRITE (1)

```
SYS_WRITE(fd, buf_addr, count)
    │
    ├── 空指针和地址检查
    ├── 计算实际写入长度（考虑页对齐）
    ├── 在块缓存中标记对应槽位为脏
    └── 返回写入长度
```

### 4.4 SYS_OPEN (2)

```
SYS_OPEN(path_addr, flags, mode)
    │
    ├── 解析 flags（rdonly/wronly/rdwr/create/excl/truncate/nonblock/append/cloexec）
    ├── 挂载表查询最佳匹配前缀
    ├── O_CREAT|O_EXCL 时检查文件是否已存在
    ├── 在当前任务中分配 fd 并创建 FHandle
    ├── O_TRUNC 时截断文件
    └── 返回 fd
```

### 4.5 SYS_CLOSE (3)

```
SYS_CLOSE(fd)
    │
    ├── fd 范围检查
    ├── 从块缓存中移除对应条目
    └── 返回 0
```

### 4.6 SYS_STAT / SYS_FSTAT (4/5)

```
SYS_STAT(path, stat_buf) / SYS_FSTAT(fd, stat_buf)
    │
    ├── stat_buf 地址检查
    ├── 计算 dev（stat: 挂载表大小, fstat: fd/4）
    └── 返回 0
```

### 4.7 SYS_MMAP (9)

```
SYS_MMAP(addr, len, prot, flags, fd, offset)
    │
    ├── len == 0 → "einval"
    ├── 页对齐 len 和 offset
    ├── 解析 flags（ANON/FIXED/PRIVATE/SHARED）
    ├── 从 prot 构建 vm_flags（READ/WRITE/EXEC/SHARED）
    │
    ├── FIXED → 返回指定 addr
    └── 非 FIXED → 基于时间和 fd 计算映射地址
    │
    ├── 检查物理页是否充足 → "enomem"
    └── 返回映射地址
```

### 4.8 SYS_MUNMAP (11)

```
SYS_MUNMAP(addr, len)
    │
    ├── 地址必须页对齐
    ├── 计算页数
    └── 逐页解除映射（当前为空操作）
```

### 4.9 SYS_BRK (12)

```
SYS_BRK(new_brk)
    │
    ├── new_brk == 0 → 返回当前 brk (0x0040_0000)
    ├── new_brk >= KERN_BASE → "enomem"
    ├── 页对齐
    │
    ├── 缩小 (aligned < old_brk)
    │   └── 逐页释放
    │
    ├── 扩大 (aligned > old_brk)
    │   └── 检查可用页 → 逐页分配
    │
    └── 更新 vm_token
```

### 4.10 SYS_IOCTL (16)

支持的控制命令：

| 命令 | 常量 | 功能 |
|---|---|---|
| TCGETS | 0x5401 | 获取终端属性 |
| TCSETS | 0x5402 | 设置终端属性 |
| TIOCGPGRP | 0x540F | 获取前台进程组 |
| TIOCSPGRP | 0x5410 | 设置前台进程组 |
| TIOCGWINSZ | 0x5413 | 获取窗口大小 |
| FIONCLEX | 0x5450 | 清除 close-on-exec |
| FIOCLEX | 0x5451 | 设置 close-on-exec |
| FIONBIO | 0x5421 | 设置非阻塞 |

### 4.11 SYS_PIPE (22)

```
SYS_PIPE(fds_addr, pipe_flags)
    │
    ├── 地址检查
    ├── 检查当前任务的 fd 容量
    ├── 创建 PipeNode 对
    ├── 解析 flags（NONBLOCK/CLOEXEC）
    ├── 分配读/写 fd
    └── 返回 rd_fd | (wr_fd << 32)
```

### 4.12 SYS_DUP / SYS_DUP2 (32/33)

```
SYS_DUP(old_fd)
    └── 在当前任务中找最小可用 fd

SYS_DUP2(old_fd, new_fd)
    ├── old_fd == new_fd → 直接返回
    ├── 关闭 new_fd（如果已打开）
    ├── 复制 old_fd 的文件到 new_fd
    └── 返回 new_fd
```

### 4.13 SYS_FORK (57)

```
SYS_FORK()
    │
    ├── 计算子进程复制代价
    ├── 检查内存压力 (>90% → "enomem")
    ├── 检查剩余物理页是否足够
    └── 分配新 PID 并返回
```

### 4.14 SYS_EXEC (59)

```
SYS_EXEC(path_addr, argv_addr, envp_addr)
    │
    ├── 地址合法性检查
    ├── 验证 ELF 头
    └── 返回 0
```

### 4.15 SYS_EXIT (60)

```
SYS_EXIT(status)
    │
    ├── 规范化退出码: (status & 0xFF) << 8
    ├── 调用当前任务的 exit_proc()
    ├── 向父进程发送 SIGCHLD
    └── 将子进程转移给 init 进程
```

### 4.16 SYS_WAIT4 (61)

根据 pid 参数分四种情况：

| pid 值 | 含义 |
|---|---|
| -1 | 等待任意子进程 |
| 0 | 等待同进程组的子进程 |
| >0 | 等待指定 PID 的子进程 |
| <-1 | 等待指定进程组的子进程 |

```
SYS_WAIT4(pid, status_addr, options, rusage_addr)
    │
    ├── 检查 WNOHANG 选项
    ├── 根据 pid 查找匹配的僵尸进程
    │   ├── 找到 → 返回其 PID
    │   ├── WNOHANG → 返回 0
    │   └── 无僵尸 → "echild"
```

### 4.17 SYS_KILL (62)

```
SYS_KILL(pid, sig)
    │
    ├── 信号号合法性检查
    ├── SIGKILL/SIGSTOP 不允许发给 PID<=1
    │
    ├── pid == 0 → 发给当前进程组
    ├── pid == -1 → 发给所有进程（除 init）
    ├── pid > 0 → 发给指定进程
    └── pid < -1 → 发给指定进程组
```

### 4.18 SYS_FCNTL (72)

支持的 fcntl 命令：

| 命令 | 常量 | 功能 |
|---|---|---|
| F_DUPFD | 0 | 复制 fd（>= arg） |
| F_DUPFD_CLOEXEC | 1030 | 复制 fd 并设 cloexec |
| F_GETFD | 1 | 获取 fd 标志 |
| F_SETFD | 2 | 设置 fd 标志 |
| F_GETFL | 3 | 获取文件状态标志 |
| F_SETFL | 4 | 设置文件状态标志 |
| F_GETLK | 5 | 获取文件锁 |
| F_SETLK/F_SETLKW | 6/7 | 设置文件锁 |

### 4.19 SYS_GETPID / SYS_GETPPID (39/110)

返回当前进程 ID 或父进程 ID。

### 4.20 SYS_SETPGID / SYS_GETPGID (109/121)

```
SYS_SETPGID(pid, pgid)
    ├── pid=0 → 当前进程
    ├── pgid=0 → 使用 target_pid 作为新 pgid
    ├── 检查目标进程是否为调用者的子进程
    └── 设置 pgid

SYS_GETPGID(pid)
    └── 返回指定进程的 pgid
```

### 4.21 SYS_SETSID (112)

```
SYS_SETSID()
    ├── 如果 pgid == tid → "eperm"（已经是组长）
    ├── 设置 pgid = tid（创建新会话）
    └── 返回新的 sid（= tid）
```

### 4.22 SYS_EPOLL_CREATE / SYS_EPOLL_CTL / SYS_EPOLL_WAIT (213/233/232)

```
SYS_EPOLL_CREATE(size)
    └── 分配 epfd = 3 + (size % 61)

SYS_EPOLL_CTL(epfd, op, fd, ev_addr)
    ├── ADD/MOD → 需要 ev_addr
    └── DEL → 不需要 ev_addr

SYS_EPOLL_WAIT(epfd, events_addr, max_events, timeout)
    ├── 参数合法性检查
    ├── timeout == 0 → 立即返回 0
    ├── timeout > 0 → 计算截止时间
    └── 返回就绪事件数
```

### 4.23 SYS_CLOCK_GETTIME (228)

支持的时钟：
- `CLOCK_REALTIME (0)` — 实时钟
- `CLOCK_MONOTONIC (1)` — 单调钟（加 BOOT_EPOCH）
- `CLOCK_MONOTONIC_RAW (4)` — 原始单调钟

### 4.24 SYS_SIGACTION (13) / SYS_SIGPROCMASK (14)

```
SYS_SIGACTION(signo, act_addr, oldact_addr)
    ├── 信号号合法性（排除 SIGKILL/SIGSTOP）
    └── 地址检查

SYS_SIGPROCMASK(how, set_addr, oldset_addr)
    ├── how=0 (BLOCK) → mask |= new_set
    ├── how=1 (UNBLOCK) → mask &= !new_set
    └── how=2 (SETMASK) → mask = new_set
    （SIGKILL 和 SIGSTOP 不可屏蔽）
```

### 4.25 SYS_FUTEX (202)

```
SYS_FUTEX(uaddr, op, val, timeout_addr, uaddr2, val3)
    │
    ├── 解析 op（低 4 位为操作码，bit 7 为 PRIVATE 标志）
    │
    ├── op=0 (WAIT) → 检查 uaddr 值是否等于 val，等待
    ├── op=1 (WAKE) → 唤醒 val 个等待者
    ├── op=3 (REQUEUE) → 唤醒 val 个并移动 val3 个到 uaddr2
    ├── op=5 (WAIT_BITSET) → 带超时的等待
    └── op=9 (CMP_REQUEUE_PI) → 比较并移动
```

---

## 五、schedule_tick — 调度时钟

```rust
pub fn schedule_tick(&self, cpu: usize)
```

每次时钟中断时调用，执行调度决策：

```
schedule_tick(cpu)
    │
    ▼
[1] 调用 dtk(cpu)（调度器滴答）
    │
    ▼
[2] 获取当前任务
    ├── 计算子进程数量
    ├── 计算剩余时间片
    │   base_slice = 10
    │   如果子进程 > 4：减少 2
    │
    ├── 时间片用完 → 标记需要重调度
    │   └── 从活跃任务中找替代者
    │
    └── 计算内核态时间
```

---

## 六、balance_load — 负载均衡

```rust
pub fn balance_load(&self) -> usize
```

```
balance_load()
    │
    ▼
[1] 遍历所有 CPU 槽位
    收集: counts（负载）, prios（优先级）, blocked（阻塞状态）
    │
    ▼
[2] 计算平均负载 avg_load = total / MAX_CPU
    │
    ▼
[3] 计算每个 CPU 的偏差 delta
    偏差 > 1 的 CPU 标记为不平衡
    │
    ▼
[4] 调用 compute_load_balance() 返回迁移建议
```

---

## 七、reclaim_zombies — 僵尸回收

```rust
pub fn reclaim_zombies(&self) -> usize
```

```
reclaim_zombies()
    │
    ▼
[1] 获取所有僵尸任务列表
    │
    ▼
[2] 对每个僵尸统计 fd 数量（模拟回收页数）
    │
    ▼
[3] 逐个调用 tasks.reap() 回收
    │
    ▼
[4] 返回回收的僵尸数量
```

---

## 八、lookup_path — 路径解析

```rust
pub fn lookup_path(&self, path: &str) -> Result<String, &'static str>
```

```
lookup_path("/usr/bin/sh")
    │
    ▼
[1] 规范化路径（消除 ".", "..", 重复 "/"）
    │
    ▼
[2] 调用 mnt.resolve() 做挂载点解析
    │
    ▼
[3] 调用 rehash_mount_cache() 刷新缓存
    │
    ▼
[4] 返回解析后的路径
```

---

## 九、内存管理

### 9.1 alloc_pages — 页分配

```rust
pub fn alloc_pages(&self, count: usize) -> Vec<usize>
```

```
alloc_pages(count)
    │
    ├── 空闲页不足 → 调用 defragment_frame_pool() 碎片整理
    │
    ▼
[1] 逐页分配
    遍历 pool.slots，找第一个 free=true 的帧
    标记为 false（已分配）
    计算物理地址 = idx * PAGE_SZ + MEM_OFF
    │
    ▼
[2] 返回分配的物理地址列表
```

### 9.2 free_pages — 页释放

```rust
pub fn free_pages(&self, pages: &[usize])
```

将物理地址转回帧索引，标记为 free。

### 9.3 memory_pressure — 内存压力

```rust
pub fn memory_pressure(&self) -> usize
```

返回已用内存百分比（0-100）。同时计算碎片化程度（空闲区域的连续段数）。

---

## 十、高层进程操作

### 10.1 do_fork — 创建子进程

```rust
pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str>
```

```
do_fork(parent_id)
    │
    ▼
[1] 查找父进程
    │
    ▼
[2] 调用 tasks.fork_task() 创建子进程
    （复制文件描述符、cwd、exec_path、pgid、IPC 上下文、信号掩码）
    │
    ▼
[3] 复制 vm_token（虚拟内存状态）
    │
    ▼
[4] 估算需要复制的页数
    │
    ▼
[5] 返回子进程 ID
```

### 10.2 do_exec — 执行程序

```rust
pub fn do_exec(&self, task_id: usize, path: &str, args: Vec<String>, envs: Vec<String>) -> Result<(), &'static str>
```

```
do_exec(task_id, path, args, envs)
    │
    ▼
[1] 设置 exec_path
    │
    ▼
[2] 验证 ELF 头
    │
    ▼
[3] 关闭所有 cloexec 文件描述符
    │
    ▼
[4] 构建 ProcInit 栈布局
    │
    ▼
[5] 设置新的线程上下文
    sp = push_at(USR_STK_OFF + USR_STK_SZ)
    ip = 0x0040_0000
```

### 10.3 do_pipe — 创建管道

```rust
pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str>
```

创建 PipeNode 对，分配读/写 fd，返回 `(rd_fd, wr_fd)`。

### 10.4 do_wait — 等待子进程

```rust
pub fn do_wait(&self, parent_id: usize, target_pid: isize, options: usize) -> Result<(usize, usize), &'static str>
```

```
do_wait(parent_id, target_pid, options)
    │
    ▼
[1] 获取父进程的子进程列表
    │
    ▼
[2] 根据 target_pid 过滤
    -1: 任意子进程
     0: 同进程组
    >0: 指定 PID
    <0: 指定进程组
    │
    ▼
[3] 查找僵尸子进程
    ├── 找到 → reap 并返回 (id, exit_code)
    ├── WNOHANG → 返回 (0, 0)
    └── 无僵尸 → "echild"
```

---

## 十一、使用场景

### 11.1 完整进程生命周期

```rust
let kern = Kernel::new(1024);
kern.proc_init();                           // 启动 init

let pid = kern.do_fork(1)?;                 // fork 子进程
kern.do_exec(pid, "/bin/ls", args, envs)?;  // exec 新程序
let (child, code) = kern.do_wait(1, -1, 0)?;// 等待子进程退出
```

### 11.2 系统调用链

```rust
// open → write → close
let fd = kern.dispatch_syscall(SYS_OPEN, path_addr, flags, mode, 0, 0, 0)?;
kern.dispatch_syscall(SYS_WRITE, fd, buf_addr, count, 0, 0, 0)?;
kern.dispatch_syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0)?;
```

### 11.3 测试引用

- `group_01` - 系统调用分发、进程创建
- `group_02` - fork/exec/wait 完整流程
- `group_03` - 文件读写系统调用
- `group_04` - 管道系统调用
- `group_05` - mmap/brk 内存管理
- `group_06` - epoll 事件系统调用
- `group_07` - 信号系统调用
- `group_08` - futex 系统调用
- `group_09` - 进程组和会话
- `group_10` - 调度和负载均衡

---

## 十二、跨模块连接

```
kernel.rs (编排层)
├── process.rs: TaskTable, Task, Pid, ThdCtx, ProcInit, CapSet
├── fs.rs: FHandle, FLike, PipeNode, EpInst, BlockCache, MountTable, TrmIO, WinSz, IoQueue, Disk
├── memory.rs: FramePool, KStk, frame_alloc, defragment_frame_pool
├── ipc.rs: SemArr, SemCtx, ShmCtx, shm_get_or_create
├── sync.rs: GKL (全局内核锁), Spin, EvBus, FutexBucket
├── signal.rs: SigSet, SIGCHLD, SIGKILL, SIGSTOP, NSIG
├── trap.rs: Context
├── sched.rs: dtk, compute_load_balance
├── timer.rs: CLK, TIMER_TICK_HZ, BOOT_EPOCH
├── channel.rs: CircBuf, Channel
├── util.rs: check_access, validate_elf_header, v2p, rehash_mount_cache
└── consts.rs: SYS_READ..SYS_FUTEX, PAGE_SZ, MAX_CPU, N_FRAMES, KERN_BASE 等
```

---

## 十三、系统调用编号总览

| 编号 | 常量 | 系统调用 | 功能 |
|---|---|---|---|
| 0 | SYS_READ | read | 读文件 |
| 1 | SYS_WRITE | write | 写文件 |
| 2 | SYS_OPEN | open | 打开文件 |
| 3 | SYS_CLOSE | close | 关闭文件 |
| 4 | SYS_STAT | stat | 文件状态（路径） |
| 5 | SYS_FSTAT | fstat | 文件状态（fd） |
| 9 | SYS_MMAP | mmap | 内存映射 |
| 11 | SYS_MUNMAP | munmap | 解除映射 |
| 12 | SYS_BRK | brk | 调整堆 |
| 13 | SYS_SIGACTION | sigaction | 信号处理 |
| 14 | SYS_SIGPROCMASK | sigprocmask | 信号掩码 |
| 16 | SYS_IOCTL | ioctl | 设备控制 |
| 22 | SYS_PIPE | pipe | 创建管道 |
| 32 | SYS_DUP | dup | 复制fd |
| 33 | SYS_DUP2 | dup2 | 复制fd到指定 |
| 39 | SYS_GETPID | getpid | 获取PID |
| 57 | SYS_FORK | fork | 创建子进程 |
| 59 | SYS_EXEC | execve | 执行程序 |
| 60 | SYS_EXIT | exit | 退出进程 |
| 61 | SYS_WAIT4 | wait4 | 等待子进程 |
| 62 | SYS_KILL | kill | 发送信号 |
| 72 | SYS_FCNTL | fcntl | 文件控制 |
| 109 | SYS_SETPGID | setpgid | 设置进程组 |
| 110 | SYS_GETPPID | getppid | 获取父PID |
| 112 | SYS_SETSID | setsid | 创建会话 |
| 121 | SYS_GETPGID | getpgid | 获取进程组 |
| 202 | SYS_FUTEX | futex | 快速互斥 |
| 213 | SYS_EPOLL_CREATE | epoll_create | 创建epoll |
| 228 | SYS_CLOCK_GETTIME | clock_gettime | 获取时间 |
| 232 | SYS_EPOLL_WAIT | epoll_wait | 等待事件 |
| 233 | SYS_EPOLL_CTL | epoll_ctl | 控制epoll |

---

## 十四、潜在的改进方向

1. **dispatch_syscall 方法过长**：800 行的 match 块难以维护，可拆分为独立模块（如 `syscall_fs.rs`、`syscall_proc.rs`、`syscall_mem.rs`）
2. **缺少真正的数据操作**：SYS_READ/SYS_WRITE 只检查地址和返回长度，不实际移动数据——这是因为内核在用户态模拟，数据搬运由测试框架完成
3. **cpus 数组固定为 8 个**：`MAX_CPU = 8` 硬编码在 `consts.rs` 中，无法动态调整
4. **GKL 全局锁粒度过大**：`tick()` 方法持有 GKL 期间遍历所有缓存链，可能导致其他 CPU 长时间等待
5. **do_fork 不复制内存**：当前 fork 只复制了 vm_token（brk 地址），没有实际复制物理页——这不符合写时复制 (COW) 语义
6. **SYS_FORK 的内存检查不完整**：检查了 `_mem_pressure` 和 `_child_copy_cost`，但没有实际分配内存给子进程
7. **信号处理简化**：`SYS_SIGACTION` 只允许 SIGKILL 和 SIGSTOP 的 sigaction，其他信号都返回 "einval"

---

## 十五、宏观视角：kernel.rs 的整体架构

kernel.rs 是混沌内核的**中枢调度器**，所有系统调用在此汇聚，由 `Kernel` 结构体统一协调各子系统完成任务。

### 15.1 整体分层图

```
                   ┌─────────────────────────────────────────┐
   用户态 trap →   │  Kernel.tick()  时钟滴答 + GKL 守护     │
                   └──────────────────┬──────────────────────┘
                                      │
         ┌────────────────────────────┴─────────────────────────────┐
         │                 dispatch_syscall (核心分发器)              │
         │  按编号路由到 30+ 个系统调用的实现逻辑                       │
         └────┬───────┬───────┬─────────┬──────────┬───────────┬───┘
              │       │       │         │          │           │
    ┌─────────┴─┐ ┌───┴───┐ ┌─┴────┐ ┌──┴────┐ ┌───┴────┐ ┌───┴─────┐
    │  文件/IO  │ │ 内存  │ │ 进程 │ │ 信号  │ │  IPC   │ │ Epoll   │
    │ read/write│ │ mmap  │ │ fork │ │signal │ │ sem/shm│ │ epoll_* │
    │ open/close│ │ munmap│ │ exec │ │sigproc│ │        │ │         │
    │ stat/fcntl│ │ brk   │ │ wait │ │mask   │ │        │ │         │
    │ dup/pipe  │ │ alloc │ │ exit │ │       │ │        │ │         │
    │ ioctl     │ │ pages │ │ kill │ │       │ │        │ │         │
    └───────────┘ └───────┘ └──────┘ └───────┘ └────────┘ └─────────┘
         │           │        │          │          │          │
         ▼           ▼        ▼          ▼          ▼          ▼
    ┌──────────────────────────────────────────────────────────────┐
    │                    子系统层 (kernel-refactored/src)            │
    │  fs.rs │ memory.rs │ process.rs │ signal.rs │ ipc.rs │ …     │
    └──────────────────────────────────────────────────────────────┘
```

### 15.2 核心子系统职责一览

| 子系统 | 对应 syscall | 核心职责 |
|--------|--------------|----------|
| **文件 / IO** | read, write, open, close, stat, fstat, dup, dup2, pipe, ioctl, fcntl | 文件描述符操作、路径解析、终端 I/O |
| **内存管理** | mmap, munmap, brk, alloc_pages, free_pages | 虚拟/物理内存映射、堆扩展、页帧分配 |
| **进程生命周期** | fork, exec, exit, wait4, kill, getpid, getppid | 创建/替换/终止进程，等待子进程 |
| **进程组 / 会话** | setpgid, getpgid, setsid | 进程组与会话管理 |
| **信号** | sigaction, sigprocmask | 信号注册与屏蔽集管理 |
| **IPC** | (内部) get_sem, get_shm | System V 信号量与共享内存访问 |
| **Epoll** | epoll_create, epoll_ctl, epoll_wait | 高效 I/O 多路复用 |
| **Futex** | futex | 用户态快速互斥锁支持 |
| **时钟** | clock_gettime | 系统时钟查询 |

### 15.3 核心接口清单（按职责分组）

**系统调用入口**
- `dispatch_syscall(nr, args) -> Result<...>` — 唯一的 syscall 分发点，按编号路由

**进程生命周期（高层操作）**
- `do_fork(cur_pid) -> Result<Pid>` — 创建子进程（复制 Task 结构）
- `do_exec(pid, path) -> Result<()>` — 替换进程映像（关闭 cloexec fd）
- `do_wait(pid, status_ptr) -> Result<Pid>` — 等待子进程退出，回收僵尸
- `do_pipe(pid) -> Result<(fd_r, fd_w)>` — 创建匿名管道

**内存管理**
- `alloc_pages(pid, npages) -> Result<usize>` — 为进程分配物理页
- `free_pages(pid, addr, npages) -> Result<()>` — 释放进程物理页
- `memory_pressure() -> usize` — 查询当前内存压力水位
- `cache_stats() -> (usize, usize)` — 查询页缓存命中率统计

**调度与回收**
- `tick()` — 时钟中断入口，驱动调度器 + GKL 守护 + 僵尸回收
- `schedule_tick()` — 单步调度：选出下一个可运行任务并切换
- `balance_load()` — 跨 CPU 负载均衡（迁移任务到空闲核）
- `reclaim_zombies()` — 扫描并回收已退出的僵尸进程

**路径与文件系统**
- `lookup_path(pid, path) -> Result<INode>` — 从进程 cwd 出发解析绝对/相对路径

**终端 I/O**
- `tty_push(data)` / `tty_pop() -> Option<u8>` — 内核 TTY 缓冲区读写

**中断与异常**
- `handle_pgfault(addr) -> Result<()>` — 缺页异常处理（触发 COW / 分配）
- `handle_pgfault_ext(pid, addr) -> Result<()>` — 带进程上下文的缺页处理

**IPC 访问**
- `get_sem(key) -> Result<&SemCtx>` — 获取 System V 信号量上下文
- `get_shm(key) -> Result<&ShmCtx>` — 获取 System V 共享内存上下文

**线程**
- `spawn_thread(pid, entry) -> Result<Tid>` — 在指定进程内创建新线程

### 15.4 核心数据流：一次 read 系统调用

```
用户态 SYS_READ(fd, buf, len)
    │
    ▼
dispatch_syscall(SYS_READ, ...)
    │
    ├─ 检查 buf 地址合法性（用户空间范围）
    │
    ├─ cur_task() → 获取当前 Task
    │
    ├─ task.fdt.lock() → 查 FDT 找到 FHandle
    │
    ├─ fhandle.read(buf, len)
    │     └─ 内部：VFS → InodeTable → 对应 Inode 的 read()
    │
    └─ 返回实际读取字节数 (isize)
```

### 15.5 设计亮点

1. **单 struct 集中调度**：Kernel 是唯一的中枢，所有 syscall 在此路由，避免了分散的多入口设计
2. **GKL（全局内核锁）保护**：`tick()` 持有 GKL 期间完成调度、缓存清理和僵尸回收，简化了并发正确性
3. **路径解析与 VFS 解耦**：`lookup_path` 将路径字符串解析为 Inode，与底层文件系统实现分离
4. **内存压力感知**：`memory_pressure()` 让调度器在内存紧张时可以触发回收或拒绝新分配
5. **do_fork / do_exec / do_wait 三件套**：清晰分离了"创建-替换-回收"三个阶段，符合 Unix 进程模型

### 15.6 建议阅读顺序

1. `Kernel::new()` — 了解内核初始化时注册了哪些子系统
2. `tick()` / `schedule_tick()` — 理解时间片驱动的调度循环
3. `dispatch_syscall` 中的 `SYS_READ` / `SYS_WRITE` — 最简单的 syscall 路径
4. `do_fork` → `do_exec` → `do_wait` — 进程完整生命周期
5. `handle_pgfault` — 缺页异常如何触发内存分配
6. `balance_load` / `reclaim_zombies` — 后台维护机制
