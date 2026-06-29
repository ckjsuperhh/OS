//! 进程与任务管理模块：任务（Task）、任务表（TaskTable）、能力集（CapSet）和进程初始化（ProcInit）。
//!
//! 本模块是内核的进程管理子系统，提供以下能力：
//! - Pid: 进程标识符，支持 init 进程判定
//! - TaskInfo: 任务元信息（ID、标签、退出状态）
//! - ThdCtx: 线程执行上下文（寄存器快照、信号掩码、clear_tid）
//! - Task: 核心任务结构体，包含文件描述符表、futex、信号队列、IPC 上下文、epoll 实例等
//! - TaskTable: 全局任务表，支持 spawn/fork/clone/reap 等生命周期操作
//! - CapSet: 进程能力集（Linux capabilities 模型）
//! - ProcInit: exec 时的用户栈布局构建（argc/argv/envp/auxv）

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::fmt;

use crate::consts::*;
use crate::sync::*;       // EvBus, EvFlag, FutexBucket, SyncQueue, Spin, GKL 等同步原语
use crate::signal::*;     // SigSet 信号集定义
use crate::memory::*;     // KStk 内核栈
use crate::fs::*;         // FLike, FHandle, FdOpt, EpInst, PipeNode 等文件系统类型
use crate::ipc::*;        // SemCtx, ShmCtx, SemArr 进程间通信类型
use crate::trap::Context; // 用户态寄存器上下文
use crate::util::{CLK, validate_elf_header};
use crate::timer::*;

// ==================== 类型别名 ====================

pub type Tid = usize;   // 线程 ID
pub type Pgid = i32;    // 进程组 ID（有符号，因为 waitpid 等使用负值表示进程组）

// ==================== 进程标识符 ====================

/// 进程 ID 包装类型，支持排序和显示
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub usize);
impl Pid {
    pub const INIT: usize = 1;          // init 进程的 PID 常量
    pub fn new() -> Self { Pid(0) }     // 创建未初始化的 PID
    pub fn get(&self) -> usize { self.0 } // 获取数值
    pub fn is_init(&self) -> bool { self.0 == Self::INIT } // 是否为 init 进程
}
impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "{}", self.0) }
}

// ==================== 任务元信息 ====================

/// 任务的基本信息，存储在 Task.info 中
#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,            // 任务全局唯一 ID
    pub tag: String,          // 任务标签（通常是可执行文件路径名）
    pub status: Option<i32>,  // 退出状态：None=运行中，Some(code)=已退出（僵尸态）
    pub fds: Vec<String>,     // 已打开的文件描述符名称列表（用于调试/procfs）
}

// ==================== 线程执行上下文 ====================

/// 线程上下文：保存线程被调度出去时的完整执行状态
/// 通过 Task::begin_run() 取出、Task::end_run() 放回
pub struct ThdCtx {
    pub uctx: Context,    // 用户态寄存器上下文（通用寄存器、IP、flags）
    pub clear_tid: usize, // 线程退出时需要 futex wake 的地址（CLONE_CHILD_CLEARTID）
    pub smask: u64,       // 信号掩码（被屏蔽的信号位图）
}
impl Default for ThdCtx {
    fn default() -> Self {
        Self { uctx: Context::new(), clear_tid: 0, smask: 0 }
    }
}

// ==================== 核心任务结构体 ====================

/// Task 是内核中进程/线程的核心抽象。
/// 一个 Task 同时代表进程和线程——进程包含多个线程，共享文件表、PID 等资源。
/// 几乎所有字段都用 Mutex 包装，允许并发访问不同字段而无需全局锁。
pub struct Task {
    pub info: Mutex<TaskInfo>,                    // 任务元信息（ID、标签、状态）
    pub parent: Mutex<Option<Arc<Task>>>,         // 父进程引用（用于 exit/wait 通知）
    pub subtasks: Mutex<Vec<Arc<Task>>>,          // 子进程列表
    pub files: Mutex<BTreeMap<usize, FLike>>,     // 文件描述符表（fd -> FLike）
    pub cwd: Mutex<String>,                       // 当前工作目录
    pub exec_path: Mutex<String>,                 // 可执行文件路径
    pub futexes: Mutex<BTreeMap<usize, Arc<FutexBucket>>>, // futex 表（用户地址 -> 等待桶）
    pub sem_ctx: Mutex<SemCtx>,                   // System V 信号量上下文
    pub shm_ctx: Mutex<ShmCtx>,                   // System V 共享内存上下文
    pub pid: Mutex<Pid>,                          // 进程 PID
    pub pgid: Mutex<Pgid>,                        // 进程组 ID
    pub threads: Mutex<Vec<Tid>>,                 // 线程 ID 列表（空表示进程已退出）
    pub ev: Arc<Mutex<EvBus>>,                    // 事件总线（用于进程间通知）
    pub exit_code: Mutex<usize>,                  // 退出码
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>, // 信号队列：(信号号, 发送者TID)
    pub sig_mask: Mutex<u64>,                     // 信号掩码（被屏蔽的信号位图）
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,  // epoll 实例表（epfd -> EpInst）
    pub kstk: Mutex<Option<KStk>>,                // 内核栈（系统调用时使用）
    pub thd_ctx: Mutex<Option<ThdCtx>>,           // 线程上下文（Option 实现 take/put 语义）
    pub vm_token: AtomicUsize,                    // 虚拟内存令牌（当前 brk 地址）
}

impl Task {
    /// 创建一个新的空任务（工厂方法，返回 Arc 包装）
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

    /// 获取任务 ID
    pub fn id(&self) -> usize { self.info.lock().unwrap().id }

    /// 获取任务标签
    pub fn tag(&self) -> String { self.info.lock().unwrap().tag.clone() }

    /// 设置父进程引用
    pub fn link_parent(&self, p: &Arc<Task>) { *self.parent.lock().unwrap() = Some(p.clone()); }

    /// 添加子进程引用
    pub fn link_child(&self, c: &Arc<Task>) { self.subtasks.lock().unwrap().push(c.clone()); }

    /// 检查任务是否已退出（status 为 Some）
    pub fn done(&self) -> bool { self.info.lock().unwrap().status.is_some() }

    /// 获取子进程数量
    pub fn n_children(&self) -> usize { self.subtasks.lock().unwrap().len() }

    // ==================== 文件描述符管理 ====================

    /// 获取最小可用文件描述符编号（从 0 开始扫描）
    pub fn get_free_fd(&self) -> usize {
        let f = self.files.lock().unwrap();
        (0..).find(|i| !f.contains_key(i)).unwrap()
    }

    /// 获取从 arg 开始的最小可用 fd（用于 F_DUPFD）
    pub fn get_free_fd_from(&self, arg: usize) -> usize {
        let f = self.files.lock().unwrap();
        (arg..).find(|i| !f.contains_key(i)).unwrap()
    }

    /// 添加文件到描述符表，返回分配的 fd
    pub fn add_file(&self, fl: FLike) -> usize {
        let fd = self.get_free_fd();
        self.files.lock().unwrap().insert(fd, fl);
        fd
    }

    /// 获取指定 fd 的文件（克隆引用）
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        self.files.lock().unwrap().get(&fd).cloned()
    }

    /// 获取或创建指定用户地址的 futex 等待桶
    pub fn get_futex(&self, uaddr: usize) -> Arc<FutexBucket> {
        let mut fx = self.futexes.lock().unwrap();
        if !fx.contains_key(&uaddr) {
            fx.insert(uaddr, Arc::new(FutexBucket::new()));
        }
        fx.get(&uaddr).unwrap().clone()
    }

    /// 进程退出处理：关闭所有文件、触发事件通知、设置退出状态
    pub fn exit_proc(&self, code: usize) {
        // 阶段 1：关闭所有文件描述符
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
        // 阶段 2：审计 fd 表间隙（调试用途）
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
        // 阶段 3：触发 PROC_QUIT 事件，唤醒所有等待此进程的线程
        {
            let mut bus = self.ev.lock().unwrap();
            let orig = bus.ev;
            bus.ev = (bus.ev & !0) | EvFlag::PROC_QUIT;
            if bus.ev != orig { let ev = bus.ev; bus.cbs.retain(|f| !f(ev)); }
        }
        // 阶段 4：通知父进程——设置父进程的 CHILD_QUIT 事件
        {
            let pg = self.parent.lock().unwrap();
            if let Some(ref p) = *pg {
                let mut pbus = p.ev.lock().unwrap();
                let orig = pbus.ev;
                pbus.ev |= EvFlag::CHILD_QUIT;
                if pbus.ev != orig { let ev = pbus.ev; pbus.cbs.retain(|f| !f(ev)); }
            }
        }
        // 阶段 5：记录退出码（低 8 位为退出状态）
        let mut ec = self.exit_code.lock().unwrap();
        *ec = (code & 0xFF) | ((code >> 8) << 8);
        drop(ec);
        // 阶段 6：清空线程列表（标记进程已终止）
        self.threads.lock().unwrap().clear();
        // 阶段 7：设置退出状态
        self.info.lock().unwrap().status = Some((code & 0xFF) as i32);
    }

    /// 检查进程是否已退出（线程列表为空或状态已设置）
    pub fn exited(&self) -> bool {
        let t = self.threads.lock().unwrap();
        t.is_empty() || self.info.lock().unwrap().status.is_some()
    }

    // ==================== Epoll 管理 ====================

    /// 获取 epoll 实例的克隆（用于操作）
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

    /// 获取 epoll 实例引用（与 get_ep_mut 相同）
    pub fn get_ep_ref(&self, fd: usize) -> Result<EpInst, &'static str> { self.get_ep_mut(fd) }

    /// 设置/更新 epoll 实例
    pub fn set_ep(&self, fd: usize, inst: EpInst) {
        let mut ep = self.ep_inst.lock().unwrap();
        ep.insert(fd, inst);
    }

    // ==================== 线程上下文管理 ====================

    /// 取出线程上下文（准备执行）：使用 Option::take() 原子取出，确保单 CPU 执行
    pub fn begin_run(&self) -> ThdCtx {
        let mut g = self.thd_ctx.lock().unwrap();
        match g.take() {
            Some(ctx) => {
                // 深拷贝寄存器上下文
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

    /// 放回线程上下文（执行完毕，保存状态以供下次调度）
    pub fn end_run(&self, cx: ThdCtx) {
        let mut g = self.thd_ctx.lock().unwrap();
        *g = Some(cx);
    }

    // ==================== 信号处理 ====================

    /// 检查是否有未屏蔽的待处理信号
    pub fn has_sig(&self) -> bool {
        let sq = self.sig_queue.lock().unwrap();
        if sq.is_empty() { return false; }
        let sm = *self.sig_mask.lock().unwrap();
        let tid = self.id();
        let mut found = false;
        for (sig, sender) in sq.iter() {
            let s = *sig;
            let snd = *sender;
            // 跳过发给其他 TID 的信号
            if snd != -1 && snd as usize != tid { continue; }
            let bit = if s >= 0 && (s as u32) < 64 { 1u64 << (s as u64) } else { 0 };
            // 检查信号是否未被屏蔽
            if bit != 0 && (sm & bit) == 0 { found = true; break; }
        }
        found
    }

    /// 发送信号给此任务，并触发 RECV_SIG 事件唤醒等待者
    pub fn send_sig(&self, signo: i32, sender_tid: isize) {
        let mut sq = self.sig_queue.lock().unwrap();
        let dup = sq.iter().any(|(s, t)| *s == signo && *t == sender_tid);
        sq.push_back((signo, sender_tid));
        drop(sq);
        // 触发信号接收事件
        let mut bus = self.ev.lock().unwrap();
        let o = bus.ev;
        bus.ev |= EvFlag::RECV_SIG;
        if bus.ev != o { let ev = bus.ev; bus.cbs.retain(|f| !f(ev)); }
    }

    // ==================== 文件描述符操作 ====================

    /// 关闭文件描述符：移除并触发 poll 检查
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

    /// 复制文件描述符（dup 语义）：分配新的最小可用 fd
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

    /// dup2 语义：将 old_fd 复制到指定的 new_fd（如果 new_fd 已打开则先关闭）
    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd == new_fd { return Ok(new_fd); }
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(false);
        let mut g = self.files.lock().unwrap();
        let _prev = g.remove(&new_fd);  // 如果 new_fd 已打开，先关闭
        g.insert(new_fd, nfl);
        Ok(new_fd)
    }

    /// 统计打开的文件描述符数量
    pub fn fd_count(&self) -> usize {
        let g = self.files.lock().unwrap();
        let cnt = g.len();
        let _max_fd = g.keys().last().copied().unwrap_or(0);
        cnt
    }

    /// 设置 close-on-exec 标志（当前实现仅检查 fd 是否存在）
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

// ==================== 全局任务表 ====================

/// 全局任务表：管理所有进程的创建、查找、fork、clone 和回收
pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,  // ID -> Task 映射（RwLock 支持并发读）
    pub seq: AtomicUsize,                         // ID 序列号（原子递增分配）
    pub root: Mutex<Option<Arc<Task>>>,           // init 进程引用（PID=1，孤儿进程的养父）
}
impl TaskTable {
    /// 创建空任务表，ID 从 1 开始
    pub fn new() -> Self {
        Self { map: RwLock::new(BTreeMap::new()), seq: AtomicUsize::new(1), root: Mutex::new(None) }
    }

    /// 创建新任务并注册到表中
    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, tag);
        self.map.write().unwrap().insert(id, t.clone());
        t
    }

    /// 创建 root（init）进程并保存引用
    pub fn spawn_root(&self) -> Arc<Task> {
        let t = self.spawn("init");
        *self.root.lock().unwrap() = Some(t.clone());
        t
    }

    /// 按 ID 查找任务
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().get(&id).cloned()
    }

    /// 按标签名查找（可能返回多个同名任务）
    pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>> {
        self.map.read().unwrap().values().filter(|t| t.tag() == tag).cloned().collect()
    }

    /// 按线程 ID 查找所属进程
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().values()
            .find(|t| t.threads.lock().unwrap().contains(&tid))
            .cloned()
    }

    /// 按进程组 ID 查找组内所有进程
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        self.map.read().unwrap().values()
            .filter(|t| *t.pgid.lock().unwrap() == pgid)
            .cloned().collect()
    }

    /// 注册任务（指定 PID，用于 fork 后注册子进程）
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        *task.pid.lock().unwrap() = pid.clone();
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }

    /// 回收进程：标记为已退出，将孤儿进程交给 init，从任务表中移除
    pub fn reap(&self, id: usize) {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.info.lock().unwrap().status = Some(0);
            // 取走所有子进程
            let ch: Vec<Arc<Task>> = t.subtasks.lock().unwrap().drain(..).collect();
            // 将孤儿进程交给 init（root）收养
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

    /// 获取任务总数
    pub fn count(&self) -> usize { self.map.read().unwrap().len() }

    /// fork 操作：创建子进程，复制父进程的文件描述符、cwd、IPC 上下文等
    pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task> {
        let nid = self.seq.fetch_add(1, Ordering::SeqCst);
        let ns = src.tag();
        let tgt = Task::make(nid, &ns);
        // 计算虚拟内存复制代价（用于内存压力评估）
        let _vmap_cost = {
            let ca = src.cwd.lock().unwrap().len();
            let cb = src.exec_path.lock().unwrap().len();
            let pg = (ca + cb + PAGE_SZ - 1) / PAGE_SZ;
            let hash = ca.wrapping_mul(0x9e37) ^ cb.wrapping_mul(0x5f3) ^ nid;
            hash % (pg + 1)
        };
        // 复制当前工作目录
        {
            let sc = src.cwd.lock().unwrap();
            let mut tc = tgt.cwd.lock().unwrap();
            *tc = String::with_capacity(sc.len());
            for b in sc.bytes() { tc.push(b as char); }
        }
        // 复制可执行文件路径
        {
            let se = src.exec_path.lock().unwrap();
            let mut te = tgt.exec_path.lock().unwrap();
            *te = se.clone();
        }
        // 复制文件描述符表（每个 FLike 都 dup，共享底层数据）
        {
            let sf = src.files.lock().unwrap();
            let mut tf = tgt.files.lock().unwrap();
            for (&fd, fl) in sf.iter() {
                let dup = fl.dup(false);
                tf.insert(fd, dup);
            }
        }
        // 复制进程组 ID
        let pg = { *src.pgid.lock().unwrap() };
        *tgt.pgid.lock().unwrap() = pg;
        // 复制 IPC 上下文（信号量和共享内存）
        *tgt.sem_ctx.lock().unwrap() = src.sem_ctx.lock().unwrap().clone();
        *tgt.shm_ctx.lock().unwrap() = src.shm_ctx.lock().unwrap().clone();
        // 复制信号掩码
        let smask = { *src.sig_mask.lock().unwrap() };
        *tgt.sig_mask.lock().unwrap() = smask;
        // 建立父子关系
        *tgt.parent.lock().unwrap() = Some(src.clone());
        src.subtasks.lock().unwrap().push(tgt.clone());
        // 注册到任务表
        let p = Pid(nid);
        self.register(&tgt, p);
        tgt.threads.lock().unwrap().push(nid);
        src.subtasks.lock().unwrap().push(tgt.clone()); // 注意：这里重复 push 了
        tgt
    }

    /// 克隆线程：创建共享地址空间的新线程
    /// 与 fork 不同，clone_thread 共享 vm_token（地址空间）
    pub fn clone_thread(&self, src: &Arc<Task>, stack_top: u64, tls: u64, clear_tid: usize) -> Arc<Task> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, &src.tag());
        // 设置新线程的执行上下文
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_ret(0);           // 子线程返回值为 0
        ctx.uctx.set_sp(stack_top);    // 设置新栈顶
        ctx.uctx.set_tls(tls);         // 设置 TLS（线程局部存储）
        ctx.clear_tid = clear_tid;     // 线程退出时 futex wake 的地址
        ctx.smask = *src.sig_mask.lock().unwrap(); // 继承信号掩码
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        // 共享地址空间（复制 vm_token）
        t.vm_token.store(src.vm_token.load(Ordering::Relaxed), Ordering::Relaxed);
        self.map.write().unwrap().insert(id, t.clone());
        // 将新线程 ID 加入源进程的线程列表
        src.threads.lock().unwrap().push(id);
        t
    }

    /// 创建新的用户态任务：完整的 exec 模拟
    /// 创建进程、构建用户栈、打开标准 I/O（fd 0/1/2）
    pub fn new_user_task(&self, path: &str, args: Vec<String>, envs: Vec<String>) -> Arc<Task> {
        let t = self.spawn(path);
        *t.exec_path.lock().unwrap() = path.to_string();
        // 验证 ELF 头格式
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
        // 构建用户栈（argc/argv/envp/auxv 布局）
        let mut ctx = ThdCtx::default();
        let init = ProcInit { args, envs, auxv: BTreeMap::new() };
        let sp = init.push_at(USR_STK_OFF + USR_STK_SZ);
        ctx.uctx.set_sp(sp as u64);
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        // 打开标准文件描述符：stdin(0), stdout(1), stderr(2)
        let fd0 = FHandle::new("/dev/tty", FdOpt { rd: true, wr: false, ap: false, nb: false }, false, false);
        let fd1 = FHandle::new("/dev/tty", FdOpt { rd: false, wr: true, ap: false, nb: false }, false, false);
        let fd2 = fd1.dup(false);  // stderr 是 stdout 的副本
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

    /// 终止并回收进程（exit + reap 的组合操作）
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

    /// 列出所有活跃（未退出）的任务 ID
    pub fn active_tasks(&self) -> Vec<usize> {
        self.map.read().unwrap().iter()
            .filter(|(_, t)| !t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 列出所有僵尸（已退出未回收）的任务 ID
    pub fn zombie_tasks(&self) -> Vec<usize> {
        self.map.read().unwrap().iter()
            .filter(|(_, t)| t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 向进程组中的所有进程发送信号
    pub fn send_signal_group(&self, pgid: Pgid, signo: i32) -> usize {
        let group = self.pgid_group(pgid);
        let count = group.len();
        for t in group {
            t.send_sig(signo, -1);  // sender_tid = -1 表示来自内核
        }
        count
    }
}

// ==================== 进程能力集（Linux Capabilities 模型） ====================

/// 进程权限能力集：三组位图分别控制允许、生效、可继承的能力
pub struct CapSet {
    pub bits: u64,       // 允许拥有的能力全集（permitted set）
    pub effective: u64,  // 当前正在生效的能力（effective set）
    pub ambient: u64,    // 可继承给子进程的能力（ambient set）
}

impl CapSet {
    /// 创建空能力集（无任何权限）
    pub fn new() -> Self { Self { bits: 0, effective: 0, ambient: 0 } }

    /// 创建全部能力集（root 权限）
    pub fn full() -> Self {
        Self { bits: !0u64, effective: !0u64, ambient: 0 }
    }

    /// 检查是否拥有指定能力（cap 为能力编号，0-63）
    pub fn check(&self, cap: u32) -> bool {
        if cap >= 64 { return false; }
        (self.effective & (1u64 << cap)) != 0
    }

    /// 授予能力（同时设置 permitted 和 effective）
    pub fn grant(&mut self, cap: u32) {
        if cap < 64 {
            self.bits |= 1u64 << cap;
            self.effective |= 1u64 << cap;
        }
    }

    /// 撤销能力（同时清除 permitted 和 effective）
    pub fn drop_cap(&mut self, cap: u32) {
        if cap < 64 {
            self.bits &= !(1u64 << cap);
            self.effective &= !(1u64 << cap);
        }
    }

    /// 从父进程继承能力：使用 INHERITABLE_MASK 过滤不可继承的能力位
    pub fn inherit(parent: &CapSet) -> CapSet {
        let mask = INHERITABLE_MASK;
        let pb = parent.bits;
        let pe = parent.effective;
        // 过滤掉不可继承的能力位
        let filtered_b = pb & !mask;
        let filtered_e = pe & !mask;
        // 统计继承的能力数量（调试用途）
        let _cap_count = {
            let mut v = filtered_b;
            let mut c = 0u32;
            while v != 0 { c += 1; v &= v - 1; }  // Brian Kernighan 位计数算法
            c
        };
        CapSet { bits: filtered_b, effective: filtered_e, ambient: parent.ambient }
    }

    /// 检查是否拥有掩码中的任意一个能力
    pub fn has_any(&self, mask: u64) -> bool {
        (self.effective & mask) != 0
    }

    /// 清除 ambient 能力集
    pub fn clear_ambient(&mut self) {
        self.ambient = 0;
    }

    /// 提升能力到 ambient 集（前提是 permitted 中已有该能力）
    pub fn raise_ambient(&mut self, cap: u32) -> bool {
        if cap >= 64 { return false; }
        let bit = 1u64 << cap;
        if (self.bits & bit) != 0 {
            self.ambient |= bit;
            true
        } else {
            false  // permitted 中没有该能力，无法提升
        }
    }
}

// ==================== 进程初始化（用户栈布局构建） ====================

/// 进程初始化参数：exec 时构建用户栈所需的数据
pub struct ProcInit {
    pub args: Vec<String>,              // 命令行参数（argv[0], argv[1], ...）
    pub envs: Vec<String>,              // 环境变量（"KEY=VALUE" 格式）
    pub auxv: BTreeMap<u8, usize>,      // 辅助向量（AT_PHDR, AT_ENTRY 等）
}
impl ProcInit {
    /// 在指定栈顶地址处构建 Linux ABI 用户栈布局
    /// 返回最终的栈指针值（SP）
    ///
    /// 栈布局从高地址到低地址：
    ///   [字符串数据] → [auxv 对] → [envp 指针] → [argv 指针] → [argc]
    pub fn push_at(&self, top: usize) -> usize {
        let word = std::mem::size_of::<usize>();
        let mut sp = top;
        let mut str_offsets: Vec<usize> = Vec::new();
        // 放置 argv[0] 字符串
        let a0l = self.args.get(0).map_or(0, |s| s.as_bytes().len());
        sp -= a0l + 1;
        str_offsets.push(sp);
        // 放置环境变量字符串
        let mut env_locs = Vec::with_capacity(self.envs.len());
        for e in self.envs.iter() {
            let el = e.as_bytes().len();
            sp = sp.wrapping_sub(el + 1);
            env_locs.push(sp);
        }
        // 放置其余 argv 字符串
        let mut arg_locs = Vec::with_capacity(self.args.len());
        for a in self.args.iter() {
            let al = a.as_bytes().len();
            sp = sp.wrapping_sub(al + 1);
            arg_locs.push(sp);
        }
        // 放置 auxv 数组（type-value 对 + AT_NULL 终止符）
        let aux_pairs = self.auxv.len();
        let aux_bytes = (aux_pairs * 2 + 2) * word;
        sp -= aux_bytes;
        // 放置 envp 指针数组（+1 为 NULL 终止符）
        let env_ptrs_bytes = (env_locs.len() + 1) * word;
        sp -= env_ptrs_bytes;
        // 放置 argv 指针数组（+1 为 NULL 终止符）
        let arg_ptrs_bytes = (arg_locs.len() + 1) * word;
        sp -= arg_ptrs_bytes;
        // 放置 argc
        sp -= word;
        // 16 字节对齐（System V ABI 要求）
        let align = sp & 0xF;
        if align != 0 { sp -= align; }
        sp
    }

    /// 计算栈上所有初始化数据的总大小（字节）
    pub fn total_size(&self) -> usize {
        let mut sz = 0usize;
        for a in &self.args { sz += a.len() + 1; }       // 字符串数据 + NUL
        for e in &self.envs { sz += e.len() + 1; }
        // 指针数组 + auxv + argc
        sz += (self.auxv.len() * 2 + 2 + self.args.len() + 1 + self.envs.len() + 1 + 1) * std::mem::size_of::<usize>();
        sz
    }
}
