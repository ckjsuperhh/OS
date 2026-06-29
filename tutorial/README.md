# Chaos Kernel 阅读总指南

> 本指南帮助你从零开始，系统性地理解 Chaos 内核的完整运行逻辑。
> 推荐按照下方顺序逐模块阅读，每个模块都配有对应的 tutorial 文档和注释后的源码。

---

## 阅读路线图

```
                        ┌─────────────────────────────────────────┐
                        │          第一阶段：基础常量与数据         │
                        │                                         │
                        │   ① consts ──► ② signal ──► ③ timer    │
                        └──────────────┬──────────────────────────┘
                                       │
                        ┌──────────────▼──────────────────────────┐
                        │         第二阶段：同步与通信原语          │
                        │                                         │
                        │   ④ sync ──► ⑤ channel ──► ⑥ ipc       │
                        └──────────────┬──────────────────────────┘
                                       │
                        ┌──────────────▼──────────────────────────┐
                        │         第三阶段：底层硬件抽象            │
                        │                                         │
                        │   ⑦ memory ──► ⑧ trap ──► ⑨ util       │
                        └──────────────┬──────────────────────────┘
                                       │
                        ┌──────────────▼──────────────────────────┐
                        │         第四阶段：子系统与资源管理        │
                        │                                         │
                        │   ⑩ fs ──► ⑪ process ──► ⑫ sched       │
                        └──────────────┬──────────────────────────┘
                                       │
                        ┌──────────────▼──────────────────────────┐
                        │         第五阶段：内核总调度              │
                        │                                         │
                        │              ⑬ kernel                   │
                        └─────────────────────────────────────────┘
```

---

## 第一阶段：基础常量与数据

### ① `consts.rs` — 内核常量中心

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/consts-module-guide.md` |
| 源码 | `src/consts.rs` (208 行) |
| 依赖 | 无 |

**为什么第一个读它：** 所有其他模块都依赖这里定义的常量。先建立全局概念——内存布局（`PAGE_SZ`、`KERN_BASE`）、系统调用编号（`SYS_READ`~`SYS_FUTEX`）、信号编号（`SIGKILL`=9）、调度策略（`SCHED_NORMAL`/`FIFO`/`RR`）等。

**核心要点：**
- 地址空间：用户空间 < `KERN_BASE` (0xFFFF_FFFF_8000_0000)
- 65 个系统调用编号，覆盖文件/进程/内存/信号/IPC/futex
- `RBUF_CAP = 256`：管道默认缓冲大小

---

### ② `signal.rs` — 信号处理

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/signal-module-guide.md` |
| 源码 | `src/signal.rs` (108 行) |
| 依赖 | `consts` |

**为什么第二个读它：** 结构最简单（只有 2 个结构体），是理解内核异步事件处理的好入口。

**核心要点：**
- `SigAction`：信号处理动作（handler 地址 + 标志 + 临时屏蔽集）
- `SigSet`：64 位位图管理 pending/blocked 信号
- POSIX 规则：`SIGKILL`/`SIGSTOP` 永远不可屏蔽、不可自定义处理函数
- `deliverable()` 返回下一个可投递的信号编号

---

### ③ `timer.rs` — 定时器系统

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/timer-module-guide.md` |
| 源码 | `src/timer.rs` (99 行) |
| 依赖 | `consts`, `util::CLK` |

**为什么第三个读它：** 时间轮算法是内核定时机制的核心，代码短小但概念重要。

**核心要点：**
- `TimerEntry`：单次或周期性定时器（deadline + interval + callback_id）
- `TimerWheel`：256 槽时间轮，`advance()` 每次推进一个槽，触发到期定时器
- 周期性定时器触发后自动重新入队到新的槽位

---

## 第二阶段：同步与通信原语

### ④ `sync.rs` — 同步原语集合

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/sync-module-guide.md` |
| 源码 | `src/sync.rs` (439 行) |
| 依赖 | `consts` |
| 测试 | `group_01` (GKL), `group_02` (Spin), `group_03` (SyncQueue) |

**为什么重要：** 这是内核中**使用最广泛**的模块——几乎每个其他模块都依赖它的同步原语。必须先理解锁和等待队列的语义。

**阅读顺序建议（模块内部）：**
1. `KernLock` → 理解可重入自旋锁 + 全局内核锁 `GKL`
2. `Spin` → 简单自旋锁
3. `EvBus` / `EvFlag` → 事件总线（位掩码 + 回调订阅）
4. `Sema` / `SemaGuard` → 计数信号量 + RAII 守卫
5. `FutexBucket` / `FutexTable` → 用户态 futex 的内核实现
6. `SyncQueue` → 条件变量式等待队列（最复杂，最后读）

**关键设计模式：**
- `GKL` 是全局大内核锁，几乎所有关键路径都要先获取它
- `EvBus` 用回调订阅模式实现事件通知，被管道、进程、信号量共用
- `SyncQueue.park_on()` 实现了类似 `pthread_cond_wait` 的语义

---

### ⑤ `channel.rs` — 字节流通道

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/channel-module-guide.md` |
| 源码 | `src/channel.rs` (277 行) |
| 依赖 | `consts`, `sync` |
| 测试 | `group_02` (Spin 不持有), `group_08` (CircBuf), `group_11` (Channel IPC) |

**承上启下：** 使用 `sync.rs` 中的 `Spin` + `SyncQueue` 构建线程安全的字节通道。是理解 `fs.rs` 管道实现的前置知识。

**核心要点：**
- `CircBuf`：纯环形缓冲区（FIFO 字节队列）
- `Channel`：阻塞式通道（send 非阻塞，recv 阻塞等待）
- `recv()` 的五阶段流程：获取 guard → 尝试读 → 检查关闭 → 休眠等待 → 唤醒后再读

---

### ⑥ `ipc.rs` — System V IPC

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/ipc-module-guide.md` |
| 源码 | `src/ipc.rs` (185 行) |
| 依赖 | `consts`, `sync::Sema` |

**为什么此时读：** 使用 `sync.rs` 的信号量构建更高层的 System V IPC 接口。

**核心要点：**
- `SemArr`：信号量数组，`get_or_create()` 使用 `Weak` 引用实现全局存储
- `SemCtx`：进程级信号量上下文，`Drop` 时自动执行 `SEM_UNDO`（进程死亡时释放信号量）
- `ShmCtx`：共享内存上下文，管理进程间的共享页映射

---

## 第三阶段：底层硬件抽象

### ⑦ `memory.rs` — 内存管理

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/memory-module-guide.md` |
| 源码 | `src/memory.rs` (895 行) |
| 依赖 | `consts`, `util::CLK` |
| 测试 | `group_01` (PgFrame/CoW), `group_04` (FramePool+GKL), `group_11` (帧分配) |

**为什么重要：** 内存管理是内核的核心子系统。本模块包含 12 个组件，从物理页帧到虚存区域到伙伴分配器。

**阅读顺序建议（模块内部）：**
1. `p2v`/`v2p` → 理解物理/虚拟地址转换
2. `PgFrame` → 原子引用计数的物理页帧
3. `FramePool` → 位图式页帧分配器
4. `ZoneInfo` → 分区水位线管理
5. `VmRegion` + `VmMap` → 虚存区域管理
6. `SharedPage` → COW（写时复制）缺页处理
7. `KStk` → 内核栈
8. `SlabEntry` → Slab 分配器
9. `BuddyAllocator` → 伙伴分配器（最复杂）

**关键概念：**
- `FramePool` 是内核中 `Kernel.pool` 的类型，管理所有物理页帧
- `SharedPage.fault()` 实现了 fork 后的写时复制
- `BuddyAllocator` 支持 2^n 页的分配和合并

---

### ⑧ `trap.rs` — 中断与陷阱

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/trap-module-guide.md` |
| 源码 | `src/trap.rs` (368 行) |
| 依赖 | `consts`, `util::{CLK, check_access}` |
| 测试 | `group_09` (Context 保存恢复、中断掩码、缺页) |

**承上启下：** 理解 CPU 上下文保存/恢复，以及中断控制器的设计。`Context` 是每个进程都持有的寄存器快照。

**核心要点：**
- `Context`：16 个通用寄存器 + IP + Flags 的快照，支持 capture/apply/transform
- `TrapCtl`：中断控制器，硬件掩码(hw_mask)/软件掩码(sw_mask)，支持嵌套计数
- `dispatch_vector()`：根据向量号使用不同掩码分发中断
- `validate_access()`：用户态内存访问合法性验证

---

### ⑨ `util.rs` — 工具函数集

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/util-module-guide.md` |
| 源码 | `src/util.rs` (288 行) |
| 依赖 | `consts`, `fs` |
| 测试 | `group_10` (check_access) |

**为什么此时读：** 这是"胶水"模块，提供时钟、地址校验、ELF 解析、负载均衡等被多处使用的工具函数。

**核心要点：**
- `CLK` / `CLK_ALL`：全局原子时钟（每 tick = 1ms）
- `check_access()`：验证用户地址不越界到内核空间
- `cfu()` / `ctu()`：用户态/内核态之间的数据拷贝
- `validate_elf_header()`：解析 64 位 ELF 文件头，返回入口地址
- `compute_load_balance()`：多核负载均衡评分算法

---

## 第四阶段：子系统与资源管理

### ⑩ `fs.rs` — 文件系统与 VFS

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/fs-module-guide.md` |
| 源码 | `src/fs.rs` (1372 行) |
| 依赖 | `consts`, `sync`, `channel::CircBuf`, `util::CLK` |
| 测试 | `group_06` (Disk), `group_07` (MountTable) |

**本模块是最大的模块**，包含内核的文件系统虚拟化层。

**阅读顺序建议（模块内部）：**
1. `FdOpt` / `FHandle` → 文件描述符和文件句柄
2. `PipeBuf` / `PipeNode` → 管道（使用 EvBus 事件通知）
3. `FLike` → VFS 统一分发枚举（File/Pipe/Ep）
4. `EpInst` → epoll 实例
5. `PageCache` → 页缓存（LRU 淘汰 + 脏页回写）
6. `BlockCache` → 块缓存（组相联 + 自旋锁）
7. `MountTable` → 挂载表（最长前缀匹配）
8. `IoQueue` → I/O 调度（SCAN 电梯算法）
9. `Disk` → 块设备（故障注入 + 日志设备）
10. `KObjRegistry` → 内核对象注册表

**VFS 架构图：**
```
进程 fd 表 ──► FLike (统一接口)
                ├── File(FHandle)  ──► 内存文件 (Arc<Mutex<Vec<u8>>>)
                ├── Pipe(PipeNode) ──► 管道缓冲 (VecDeque + EvBus)
                └── Ep(EpInst)     ──► epoll 实例
```

---

### ⑪ `process.rs` — 进程管理

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/process-module-guide.md` |
| 源码 | `src/process.rs` (616 行) |
| 依赖 | `consts`, `sync`, `signal`, `memory`, `fs`, `ipc`, `trap`, `util`, `timer` |
| 测试 | `group_05` (创建/退出/弱引用), `group_10` (zombie), `group_11` (fork) |

**为什么此时读：** `Task` 结构体是内核中字段最多的类型（18 个 Mutex 字段），它聚合了前面所有模块的概念。必须先理解 sync/fs/memory/signal 才能读懂它。

**核心要点：**
- `Task`：一个进程/线程的完整状态——文件表、futex、信号量、共享内存、信号队列、epoll、内核栈、虚存令牌
- `TaskTable`：进程表，支持 spawn、fork（深拷贝）、clone_thread（共享进程）、reap（回收）
- `exit_proc()` 的 7 阶段流程：关闭文件 → 触发 PROC_QUIT → 通知父进程 CHILD_QUIT → 设置退出码 → 清空线程 → 标记状态
- `fork_task()` 复制所有进程状态（文件 dup、信号量/共享内存 clone、pgid、信号掩码）
- `CapSet`：Linux capabilities 权限模型

---

### ⑫ `sched.rs` — 调度器

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/sched-module-guide.md` |
| 源码 | `src/sched.rs` (211 行) |
| 依赖 | `consts`, `util::CLK` |

**为什么最后读它（子系统层）：** 调度器依赖进程管理提供的 task 信息，但它本身的算法相对独立。

**核心要点：**
- `SchedulePolicy`：CFS 风格的调度策略（权重/nice/虚拟运行时间）
- `RunQueue`：就绪队列，`enqueue()` 按综合评分排序，`pick_next()` 选最优任务
- 评分公式：`score = prio * 1000 - nice * 50 + vruntime - weight`
- `rebalance()`：根据时钟推进 vruntime，实现公平调度

---

## 第五阶段：内核总调度

### ⑬ `kernel.rs` — 内核主体与系统调用分发

| 项目 | 内容 |
|---|---|
| 教程 | `tutorial/kernel-module-guide.md` |
| 源码 | `src/kernel.rs` (1204 行) |
| 依赖 | **所有其他模块** |
| 测试 | `group_11` (fork/exec/pipe/mmap 综合测试) |

**为什么最后读：** 这是整个内核的"总入口"和"调度中枢"。它创建并管理所有子系统，处理所有系统调用。只有在理解了前面 12 个模块之后，才能完整地读懂它。

**阅读顺序建议（模块内部）：**

1. `Kernel` 结构体 → 了解所有子系统的组合方式
2. `new()` → 内核初始化流程
3. `proc_init()` → 创建 init 进程
4. `dispatch_syscall()` → **核心：逐一阅读每个系统调用的处理逻辑**
   - 文件 I/O：`SYS_READ`/`SYS_WRITE`/`SYS_OPEN`/`SYS_CLOSE`/`SYS_STAT`/`SYS_IOCTL`
   - 内存：`SYS_MMAP`/`SYS_MUNMAP`/`SYS_BRK`
   - 进程：`SYS_FORK`/`SYS_EXEC`/`SYS_EXIT`/`SYS_WAIT4`/`SYS_KILL`
   - IPC：`SYS_PIPE`/`SYS_DUP`/`SYS_DUP2`/`SYS_FCNTL`
   - 信号：`SYS_SIGACTION`/`SYS_SIGPROCMASK`
   - 高级：`SYS_EPOLL_*`/`SYS_FUTEX`/`SYS_CLOCK_GETTIME`
5. `do_fork()` / `do_exec()` / `do_pipe()` / `do_wait()` → 高级操作的实现
6. `schedule_tick()` / `balance_load()` → 调度和负载均衡

---

## 模块依赖总图

```
                          consts.rs
                         (所有常量)
                        ╱    │    ╲
                       ╱     │     ╲
               signal.rs  timer.rs  (被多处引用)
                  │         │
                  ▼         ▼
               sync.rs ◄────┘
              ╱  │  ╲  ╲
             ╱   │   ╲  ╲
     channel.rs  │   ipc.rs
        │        │      │
        ▼        ▼      ▼
      memory.rs  │   process.rs ◄── trap.rs
        │        │      │              │
        ▼        ▼      ▼              │
       fs.rs ──► │ ◄────┘              │
        │        │                     │
        ▼        ▼                     │
      util.rs ◄──┘                     │
        │                              │
        ▼                              │
      sched.rs                         │
        │                              │
        ▼                              │
    kernel.rs ◄────────────────────────┘
    (总调度 + 所有系统调用)
```

---

## 测试覆盖速查表

| 测试组 | 覆盖模块 | 关键测试 |
|---|---|---|
| `group_01` | `sync` (GKL), `memory` (FramePool) | BKL 重入/释放、跨模块锁序 |
| `group_02` | `sync` (Spin), `channel` (Channel) | 自旋锁数据保护、休眠不持锁 |
| `group_03` | `sync` (SyncQueue) | 条件变量信号/虚假唤醒 |
| `group_04` | `memory` (PgFrame, SharedPage) | 引用计数、COW 缺页 |
| `group_05` | `process` (Task, TaskTable) | 进程创建/退出、弱引用 |
| `group_06` | `fs` (Disk) | 块读取、重试、故障注入 |
| `group_07` | `fs` (MountTable) | 路径解析、并发挂载 |
| `group_08` | `channel` (CircBuf) | 环形缓冲区读写、满拒绝、wrap-around |
| `group_09` | `trap` (Context, TrapCtl) | 上下文保存恢复、中断掩码、缺页 |
| `group_10` | `util` (check_access), `process` | 地址溢出校验、zombie 回收 |
| `group_11` | `kernel` + 全模块集成 | fork/exec、pipe IPC、mmap 文件 I/O |

---

## 文件索引

### 教程文档 (`tutorial/`)

| # | 文件 | 对应源码 |
|---|---|---|
| 1 | `consts-module-guide.md` | `src/consts.rs` |
| 2 | `signal-module-guide.md` | `src/signal.rs` |
| 3 | `timer-module-guide.md` | `src/timer.rs` |
| 4 | `sync-module-guide.md` | `src/sync.rs` |
| 5 | `channel-module-guide.md` | `src/channel.rs` |
| 6 | `ipc-module-guide.md` | `src/ipc.rs` |
| 7 | `memory-module-guide.md` | `src/memory.rs` |
| 8 | `trap-module-guide.md` | `src/trap.rs` |
| 9 | `util-module-guide.md` | `src/util.rs` |
| 10 | `fs-module-guide.md` | `src/fs.rs` |
| 11 | `process-module-guide.md` | `src/process.rs` |
| 12 | `sched-module-guide.md` | `src/sched.rs` |
| 13 | `kernel-module-guide.md` | `src/kernel.rs` |

### 源码文件 (`kernel-refactored/src/`)

| 文件 | 行数 | 核心内容 |
|---|---|---|
| `lib.rs` | 36 | 模块声明 + `pub use` 重导出 |
| `consts.rs` | 208 | 内核常量 |
| `signal.rs` | 108 | 信号动作与集合 |
| `timer.rs` | 99 | 时间轮定时器 |
| `sync.rs` | 439 | 同步原语（锁/信号量/futex/等待队列） |
| `channel.rs` | 277 | 环形缓冲区 + 阻塞通道 |
| `ipc.rs` | 185 | System V 信号量/共享内存 |
| `memory.rs` | 895 | 内存管理（帧池/虚存/COW/buddy/slab） |
| `trap.rs` | 368 | 中断控制 + CPU 上下文 |
| `util.rs` | 288 | 工具函数（时钟/校验/ELF/负载均衡） |
| `fs.rs` | 1372 | 文件系统 VFS（文件/管道/epoll/缓存/磁盘） |
| `process.rs` | 616 | 进程管理（Task/TaskTable/fork/exit） |
| `sched.rs` | 211 | 调度器（CFS 策略/就绪队列） |
| `kernel.rs` | 1204 | 内核主体 + 系统调用分发 |

**总计：14 个源文件，约 6,268 行内核代码**
