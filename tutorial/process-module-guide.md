# Process 模块阅读指南

> 文件路径: `kernel-refactored/src/process.rs`
> 代码量: 616 行 | 6 个核心结构体 | 依赖: `consts`, `sync`, `signal`, `memory`, `fs`, `ipc`, `trap`, `util`, `timer`

---

## 一、模块概述

`process.rs` 实现了内核的 **进程与任务管理** 子系统，涵盖从 PID 分配到进程创建、线程克隆、信号传递的完整生命周期管理。

| 层次 | 结构体 | 用途 |
|---|---|---|
| 标识 | `Pid`, `Tid`, `Pgid` | 进程/线程/进程组标识 |
| 信息 | `TaskInfo` | 任务元数据（ID、名称、状态、fd列表） |
| 上下文 | `ThdCtx` | 线程执行上下文（寄存器、信号掩码、clear_tid） |
| 核心 | `Task` | 进程/线程的完整描述（文件、信号、futex、IPC、内存等） |
| 管理 | `TaskTable` | 全局任务表（创建、查找、fork、clone、回收） |
| 权限 | `CapSet` | 进程能力集（capabilities） |
| 初始化 | `ProcInit` | 进程启动时的栈布局（argc/argv/envp/auxv） |

**设计定位：** `process.rs` 是内核中"进程"概念的完整实现。`kernel.rs` 中的 `do_fork`、`do_exec`、`do_wait` 等高层操作都委托给 `TaskTable` 和 `Task` 的方法。它通过 `fs.rs` 的 `FLike` 管理文件描述符，通过 `ipc.rs` 管理信号量和共享内存，通过 `sync.rs` 管理 futex 和事件总线。

---

## 二、Pid — 进程标识

### 2.1 结构体定义

```rust
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub usize);
```

### 2.2 方法

```rust
impl Pid {
    pub const INIT: usize = 1;          // init 进程的 PID 常量
    pub fn new() -> Self { Pid(0) }     // 创建未分配的 PID
    pub fn get(&self) -> usize { self.0 } // 获取 PID 数值
    pub fn is_init(&self) -> bool { self.0 == Self::INIT } // 是否为 init 进程
}
```

`Pid` 实现了 `Display` trait，可直接格式化输出。同时实现了 `Ord`，支持排序比较。

---

## 三、TaskInfo — 任务元信息

```rust
#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,            // 任务 ID（全局唯一）
    pub tag: String,          // 任务标签（通常是可执行文件路径）
    pub status: Option<i32>,  // 退出状态（None=运行中, Some(code)=已退出）
    pub fds: Vec<String>,     // 打开的文件描述符名称列表
}
```

`status` 字段是判断进程是否为僵尸进程的关键：`status.is_some()` 表示进程已退出。

---

## 四、ThdCtx — 线程上下文

```rust
pub struct ThdCtx {
    pub uctx: Context,    // 用户态寄存器上下文（来自 trap.rs）
    pub clear_tid: usize, // futex clear_tid 地址（线程退出时唤醒等待者）
    pub smask: u64,       // 信号掩码
}
```

`ThdCtx` 保存了线程被调度出去时的完整状态。当线程重新被调度时，通过 `Task::begin_run()` 取出上下文，执行完毕后通过 `Task::end_run()` 放回。

**Context 结构（来自 trap.rs）：**
- `r: [u64; N_REGS]` — 通用寄存器
- `ip: u64` — 指令指针
- `flags: u64` — 标志寄存器

---

## 五、Task — 核心任务结构

### 5.1 结构体定义

```rust
pub struct Task {
    pub info: Mutex<TaskInfo>,                    // 任务元信息
    pub parent: Mutex<Option<Arc<Task>>>,         // 父进程
    pub subtasks: Mutex<Vec<Arc<Task>>>,          // 子进程列表
    pub files: Mutex<BTreeMap<usize, FLike>>,     // 文件描述符表（fd -> FLike）
    pub cwd: Mutex<String>,                       // 当前工作目录
    pub exec_path: Mutex<String>,                 // 可执行文件路径
    pub futexes: Mutex<BTreeMap<usize, Arc<FutexBucket>>>, // futex 表
    pub sem_ctx: Mutex<SemCtx>,                   // 信号量上下文
    pub shm_ctx: Mutex<ShmCtx>,                   // 共享内存上下文
    pub pid: Mutex<Pid>,                          // 进程 PID
    pub pgid: Mutex<Pgid>,                        // 进程组 ID
    pub threads: Mutex<Vec<Tid>>,                 // 线程 ID 列表
    pub ev: Arc<Mutex<EvBus>>,                    // 事件总线
    pub exit_code: Mutex<usize>,                  // 退出码
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>, // 信号队列 (信号号, 发送者TID)
    pub sig_mask: Mutex<u64>,                     // 信号掩码
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,  // epoll 实例表
    pub kstk: Mutex<Option<KStk>>,                // 内核栈
    pub thd_ctx: Mutex<Option<ThdCtx>>,           // 线程上下文
    pub vm_token: AtomicUsize,                    // 虚拟内存令牌（brk 地址）
}
```

**设计要点：**
- Task 同时代表进程和线程——进程包含多个线程，共享 `files`、`pid` 等资源
- 几乎所有字段都用 `Mutex` 包装，允许并发访问不同字段
- `ev` 事件总线用于进程间通知（子进程退出、信号接收等）

### 5.2 文件描述符管理

```rust
/// 获取最小可用 fd（从 0 开始）
pub fn get_free_fd(&self) -> usize

/// 获取从 arg 开始的最小可用 fd
pub fn get_free_fd_from(&self, arg: usize) -> usize

/// 添加文件并返回分配的 fd
pub fn add_file(&self, fl: FLike) -> usize

/// 获取指定 fd 的文件
pub fn get_file(&self, fd: usize) -> Option<FLike>

/// 关闭文件描述符
pub fn close_fd(&self, fd: usize) -> Result<(), &'static str>

/// 复制文件描述符（分配新 fd）
pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str>

/// dup2：将 old_fd 复制到指定的 new_fd
pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str>

/// 统计打开的 fd 数量
pub fn fd_count(&self) -> usize

/// 设置 close-on-exec 标志
pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str>
```

### 5.3 进程退出 — `exit_proc()`

```rust
pub fn exit_proc(&self, code: usize)
```

**退出流程：**

```
exit_proc(code) 调用
    │
    ▼
[1] 关闭所有文件描述符
    遍历 files 表，逐个 remove
    │
    ▼
[2] 触发 PROC_QUIT 事件
    设置自身 ev 总线的 PROC_QUIT 标志
    通知所有等待此进程的线程
    │
    ▼
[3] 通知父进程
    设置父进程的 CHILD_QUIT 事件标志
    │
    ▼
[4] 记录退出码
    exit_code = (code & 0xFF) | ((code >> 8) << 8)
    │
    ▼
[5] 清理线程列表
    threads.clear()
    │
    ▼
[6] 设置退出状态
    info.status = Some(code & 0xFF)
```

### 5.4 信号处理

```rust
/// 检查是否有未屏蔽的信号
pub fn has_sig(&self) -> bool

/// 发送信号给此任务
pub fn send_sig(&self, signo: i32, sender_tid: isize)
```

`send_sig` 在入队信号后，设置 `EvFlag::RECV_SIG` 事件，唤醒等待信号的线程。

### 5.5 运行上下文管理

```rust
/// 取出线程上下文（准备执行）
pub fn begin_run(&self) -> ThdCtx

/// 放回线程上下文（执行完毕）
pub fn end_run(&self, cx: ThdCtx)
```

`begin_run` 使用 `Option::take()` 原子性地取出上下文，确保同一时刻只有一个 CPU 在执行此线程。

### 5.6 Epoll 管理

```rust
/// 获取 epoll 实例（克隆）
pub fn get_ep_mut(&self, fd: usize) -> Result<EpInst, &'static str>

/// 设置 epoll 实例
pub fn set_ep(&self, fd: usize, inst: EpInst)
```

### 5.7 Futex 管理

```rust
/// 获取或创建 futex 桶
pub fn get_futex(&self, uaddr: usize) -> Arc<FutexBucket>
```

---

## 六、TaskTable — 全局任务表

### 6.1 结构体定义

```rust
pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,  // ID -> Task 映射
    pub seq: AtomicUsize,                         // ID 序列号（原子递增）
    pub root: Mutex<Option<Arc<Task>>>,           // init 进程（PID=1）
}
```

### 6.2 创建与查找

```rust
/// 创建新任务并注册到表中
pub fn spawn(&self, tag: &str) -> Arc<Task>

/// 创建 root（init）进程
pub fn spawn_root(&self) -> Arc<Task>

/// 按 ID 查找任务
pub fn find(&self, id: usize) -> Option<Arc<Task>>

/// 按标签名查找（可能返回多个）
pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>>

/// 按线程 ID 查找所属进程
pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>>

/// 按进程组 ID 查找组内所有进程
pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>>

/// 注册任务（指定 PID）
pub fn register(&self, task: &Arc<Task>, pid: Pid)
```

### 6.3 fork 操作 — `fork_task()`

```rust
pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task>
```

**fork 流程：**
```
fork_task(src)
    │
    ▼
[1] 分配新 ID
    │
    ▼
[2] 创建新 Task（空壳）
    │
    ▼
[3] 复制 cwd（当前工作目录）
    │
    ▼
[4] 复制 exec_path（可执行文件路径）
    │
    ▼
[5] 复制文件描述符表（每个 FLike 都 dup）
    │
    ▼
[6] 复制 pgid（进程组）
    │
    ▼
[7] 复制 sem_ctx / shm_ctx（IPC 上下文）
    │
    ▼
[8] 复制 sig_mask（信号掩码）
    │
    ▼
[9] 建立父子关系
    child.parent = src
    src.subtasks.push(child)
    │
    ▼
[10] 注册到任务表
```

### 6.4 线程克隆 — `clone_thread()`

```rust
pub fn clone_thread(&self, src: &Arc<Task>, stack_top: u64, tls: u64, clear_tid: usize) -> Arc<Task>
```

与 `fork_task` 不同，`clone_thread` 创建的新任务**共享**原进程的地址空间和资源：
- 设置新栈顶 (`set_sp`)
- 设置 TLS 指针 (`set_tls`)
- 设置 clear_tid（线程退出时 futex wake 的地址）
- 复制 vm_token（虚拟内存令牌）
- 新 TID 加入 src 的 threads 列表

### 6.5 用户态任务创建 — `new_user_task()`

```rust
pub fn new_user_task(&self, path: &str, args: Vec<String>, envs: Vec<String>) -> Arc<Task>
```

创建完整的用户态进程：
1. 调用 `spawn` 创建任务
2. 验证 ELF 头
3. 通过 `ProcInit` 构建用户栈
4. 打开标准文件描述符（stdin=0, stdout=1, stderr=2）
5. 注册 PID 并添加主线程

### 6.6 进程回收 — `reap()`

```rust
pub fn reap(&self, id: usize)
```

**回收流程：**
```
reap(id)
    │
    ▼
[1] 设置 status = Some(0)
    │
    ▼
[2] 取走所有子进程
    │
    ▼
[3] 将孤儿进程交给 init（root）
    每个子进程: link_parent(init), init.link_child(child)
    │
    ▼
[4] 从任务表中移除
```

这符合 POSIX 语义：父进程退出后，子进程被 init 收养。

### 6.7 辅助方法

```rust
/// 终止并回收进程
pub fn terminate_and_collect(&self, id: usize, code: usize) -> bool

/// 列出所有活跃（未退出）的任务 ID
pub fn active_tasks(&self) -> Vec<usize>

/// 列出所有僵尸（已退出未回收）的任务 ID
pub fn zombie_tasks(&self) -> Vec<usize>

/// 向进程组发送信号
pub fn send_signal_group(&self, pgid: Pgid, signo: i32) -> usize
```

---

## 七、CapSet — 进程能力集

### 7.1 结构体定义

```rust
pub struct CapSet {
    pub bits: u64,       // 允许拥有的能力全集（permitted）
    pub effective: u64,  // 当前生效的能力（effective）
    pub ambient: u64,    // 可继承给子进程的能力（ambient）
}
```

### 7.2 方法

```rust
/// 空能力集
pub fn new() -> Self

/// 全部能力（root 权限）
pub fn full() -> Self

/// 检查是否拥有指定能力
pub fn check(&self, cap: u32) -> bool

/// 授予能力
pub fn grant(&mut self, cap: u32)

/// 撤销能力
pub fn drop_cap(&mut self, cap: u32)

/// 从父进程继承能力（过滤掉不可继承的位）
pub fn inherit(parent: &CapSet) -> CapSet

/// 检查是否拥有掩码中的任意能力
pub fn has_any(&self, mask: u64) -> bool

/// 清除 ambient 集
pub fn clear_ambient(&mut self)

/// 提升能力到 ambient 集
pub fn raise_ambient(&mut self, cap: u32) -> bool
```

**继承规则：** 使用 `INHERITABLE_MASK`（在 `consts.rs` 中定义）过滤父进程的能力，只有不在 mask 中的能力位才能被继承。

---

## 八、ProcInit — 进程初始化栈布局

### 8.1 结构体定义

```rust
pub struct ProcInit {
    pub args: Vec<String>,              // 命令行参数（argv）
    pub envs: Vec<String>,              // 环境变量（envp）
    pub auxv: BTreeMap<u8, usize>,      // 辅助向量（auxiliary vector）
}
```

### 8.2 栈布局构建

```rust
pub fn push_at(&self, top: usize) -> usize
```

`push_at` 模拟 Linux 内核在 exec 时构建用户栈的过程：

```
高地址 (top = USR_STK_OFF + USR_STK_SZ)
    ┌─────────────────────────────────────┐
    │  对齐填充                            │
    ├─── argc (8 bytes) ──────────────────┤
    ├─── argv[0] 指针                     │
    ├─── argv[1] 指针                     │
    ├─── ...                              │
    ├─── NULL (argv 结束)                  │
    ├─── envp[0] 指针                     │
    ├─── envp[1] 指针                     │
    ├─── ...                              │
    ├─── NULL (envp 结束)                  │
    ├─── auxv[0].type, auxv[0].value      │
    ├─── ...                              │
    ├─── AT_NULL, 0 (auxv 结束)            │
    ├─── argv 字符串数据                   │
    ├─── envp 字符串数据                   │
    └─────────────────────────────────────┘
低地址 (返回的 sp)
```

### 8.3 辅助方法

```rust
/// 计算栈上数据的总大小
pub fn total_size(&self) -> usize
```

---

## 九、使用场景

### 9.1 创建并运行进程

```rust
let table = TaskTable::new();
let task = table.new_user_task("/bin/sh", vec!["sh".into()], vec!["PATH=/bin".into()]);
// task 现在拥有:
//   - fd 0,1,2 指向 /dev/tty
//   - 用户栈已构建
//   - PID 已注册
```

### 9.2 fork + exec

```rust
let child = table.fork_task(&parent);
// child 拥有 parent 的文件描述符副本、相同的 cwd、pgid、信号掩码
// 但独立的线程上下文和退出码
```

### 9.3 线程创建

```rust
let thread = table.clone_thread(&task, stack_top, tls_addr, clear_tid_addr);
// thread 共享 task 的 vm_token，但有独立的栈和 TLS
```

### 9.4 信号传递

```rust
// 发送信号
task.send_sig(15, -1);  // SIGTERM from kernel

// 检查信号
if task.has_sig() { /* 有待处理信号 */ }

// 组播信号
table.send_signal_group(pgid, 9);  // SIGKILL to process group
```

### 9.5 测试引用

- `group_01` - 进程创建、PID 分配
- `group_03` - fork 后文件描述符共享
- `group_07` - 信号发送与接收
- `group_09` - 线程克隆
- `group_12` - 进程回收、孤儿进程

---

## 十、跨模块连接

```
process.rs
├── fs.rs: FLike/FHandle（文件描述符管理）、EpInst（epoll）、PipeNode（管道）
├── ipc.rs: SemCtx/ShmCtx（信号量和共享内存上下文）
├── sync.rs: EvBus/EvFlag（事件总线）、FutexBucket（futex）、Spin
├── signal.rs: SigSet（信号集定义）
├── memory.rs: KStk（内核栈）
├── trap.rs: Context（寄存器上下文）
├── kernel.rs: Kernel 持有 TaskTable，调度器通过 TaskTable 管理任务
├── sched.rs: 调度算法引用 Task 的优先级和状态
└── consts.rs: N_PROC, USR_STK_OFF, USR_STK_SZ, INHERITABLE_MASK 等
```

---

## 十一、潜在的改进方向

1. **Task 字段过多**：18 个 Mutex 字段导致每次操作都需要获取不同的锁，可考虑分组（如把文件相关字段放入 `FileCtx` 子结构）
2. **fork_task 的双重 push**：`src.subtasks.lock().unwrap().push(tgt.clone())` 被调用了两次（第 409 行和第 413 行），导致子进程列表中出现重复引用
3. **Pid 类型不一致**：`Pid(usize)` 但 `Pgid` 是 `i32`，在 setpgid 等系统调用中需要频繁类型转换
4. **exit_proc 中的 FDT 审计代码**：退出时计算 fd 间隙数的代码（`_fdt_audit`）仅用于调试，在生产环境可移除
5. **ProcInit::push_at 使用 wrapping_sub**：栈地址计算使用 `wrapping_sub` 可能在溢出时产生错误结果
6. **缺少 exec 后的 cloexec 自动关闭**：虽然 `do_exec` 中有关闭 cloexec fd 的逻辑，但 `fork_task` 不处理 cloexec
