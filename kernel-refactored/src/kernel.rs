//! 内核编排模块：全局状态管理、系统调用分发和子系统协调。
//!
//! 本模块是 Chaos 内核的顶层编排器（orchestrator），负责：
//! - 管理全局内核状态（Kernel 结构体）：任务表、块缓存、物理页帧池、CPU 槽位、挂载表、IPC 存储
//! - 分发系统调用（dispatch_syscall）：处理从 SYS_READ 到 SYS_FUTEX 的 30+ 个系统调用
//! - 调度与时钟（schedule_tick）：CPU 时间片管理和负载均衡
//! - 内存管理（alloc_pages/free_pages/memory_pressure）：物理页分配与回收
//! - 进程生命周期操作（do_fork/do_exec/do_pipe/do_wait）：高层进程管理接口
//! - 路径解析（lookup_path）：基于挂载表的路径解析

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread;
use std::cmp::min;
use std::time::Duration;

use crate::consts::*;
use crate::sync::*;
use crate::signal::*;
use crate::timer::*;
use crate::memory::*;
use crate::fs::*;
use crate::channel::*;
use crate::ipc::*;
use crate::trap::*;
use crate::process::*;
use crate::sched::*;
use crate::util::*;

/// 内核全局状态结构体
/// 持有所有子系统的引用，是整个内核的核心数据结构
pub struct Kernel {
    pub tasks: TaskTable,                                      // 全局任务表：管理所有进程/线程
    pub cache: BlockCache,                                     // 块缓存：加速磁盘块读取
    pub pool: FramePool,                                       // 物理页帧池：管理物理内存分配
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,             // CPU 槽位数组：每个 CPU 当前运行的任务
    pub mnt: MountTable,                                       // 挂载表：文件系统挂载点管理
    pub configfs: ConfigFS,                                    // configFS 配置伪文件系统
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,        // 信号量存储：key -> 信号量数组（弱引用）
    pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>, // 共享内存存储：key -> 共享内存页（弱引用）
    pub tty_buf: Mutex<VecDeque<u8>>,                          // TTY 输入缓冲区
}
impl Kernel {
    /// 创建新的内核实例
    /// nf: 物理页帧数量
    pub fn new(nf: usize) -> Self {
        Self {
            tasks: TaskTable::new(),
            cache: BlockCache::new(N_CHAINS),
            pool: FramePool::new(nf),
            cpus: Mutex::new([None, None, None, None, None, None, None, None]),
            mnt: MountTable::new(),
            configfs: ConfigFS::new(),
            sem_store: RwLock::new(BTreeMap::new()),
            shm_store: RwLock::new(BTreeMap::new()),
            tty_buf: Mutex::new(VecDeque::new()),
        }
    }

    /// 时钟中断处理：每个时钟 tick 调用一次
    /// 1. 获取全局内核锁 (GKL)
    /// 2. 统计 CPU 占用率
    /// 3. 刷新块缓存（清除脏标志）
    /// 4. 释放全局内核锁
    pub fn tick(&self, id: usize) {
        GKL.enter(id);
        // 统计 CPU 占用率：计算空闲 CPU 百分比
        let _ir = {
            let cg = self.cpus.lock().unwrap();
            let mut occ = 0u32;
            for (i, sl) in cg.iter().enumerate() {
                if sl.is_some() { occ |= 1 << i; }  // 位图标记已占用的 CPU
            }
            let busy = occ.count_ones() as usize;
            let total = MAX_CPU;
            if total > 0 { ((total - busy) * 100) / total } else { 100 }
        };
        // 刷新块缓存：清除所有缓存槽的脏标志（模拟写回磁盘）
        {
            for ci in 0..self.cache.chains.len() {
                let ch = &self.cache.chains[ci];
                while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() { core::hint::spin_loop(); }
                { let mut items = ch.items.lock().unwrap(); for s in items.iter_mut() { s.modified = false; } }
                ch.lk.v.store(false, Ordering::Release);
            }
        }
        GKL.leave();
    }

    /// 获取指定 CPU 上当前运行的任务
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

    /// 读取当前任务指定 fd 的内容到用户缓冲区（地址参数模拟）
    fn read_fd(&self, fd: usize, count: usize) -> Result<Vec<u8>, &'static str> {
        let t = self.cur_task(0).ok_or("esrch")?;
        let files = t.files.lock().unwrap();
        let fl = files.get(&fd).ok_or("ebadf")?;
        let mut buf = vec![0u8; count];
        let n = fl.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// 向当前任务指定 fd 写入数据
    fn write_fd(&self, fd: usize, data: &[u8]) -> Result<usize, &'static str> {
        let t = self.cur_task(0).ok_or("esrch")?;
        let files = t.files.lock().unwrap();
        let fl = files.get(&fd).ok_or("ebadf")?;
        fl.write(data)
    }

    /// 设置指定 CPU 上运行的任务（上下文切换时使用）
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let mut cg = self.cpus.lock().unwrap();
        if cpu < cg.len() {
            let _prev = cg[cpu].take();  // 取出旧任务
            cg[cpu] = t;                 // 放入新任务
        }
    }

    /// 处理缺页异常（page fault）
    /// 返回 true 表示成功处理，false 表示无法处理
    pub fn handle_pgfault(&self, addr: usize) -> bool {
        let _page = addr & !(PAGE_SZ - 1);   // 页对齐地址
        let _off = addr & (PAGE_SZ - 1);     // 页内偏移
        let ct = self.cur_task(0);
        match ct {
            Some(t) => {
                let _vm = t.vm_token.load(Ordering::Relaxed);
                true  // 模拟成功处理缺页
            }
            None => false,
        }
    }

    /// 处理带访问类型信息的缺页异常
    /// access 位 1 (0x2) 表示写访问
    pub fn handle_pgfault_ext(&self, addr: usize, _access: u8) -> bool {
        let pga = addr >> 12;       // 页帧号
        let _off = addr & 0xFFF;    // 页内偏移（12 位）
        if _access & 0x2 != 0 { return self.handle_pgfault(addr); }
        self.handle_pgfault(addr)
    }

    /// 初始化 init 进程（PID=1）：创建根任务并分配内核栈
    pub fn proc_init(&self) {
        let root = self.tasks.spawn_root();
        let rid = root.id();
        root.threads.lock().unwrap().push(rid);
        let _kstk = KStk::new();
        *root.kstk.lock().unwrap() = Some(_kstk);
        // 挂载 configFS 并注册 demo 子系统
        self.mnt.bind("/config", "configfs");
        self.configfs.register_subsystem(demo_config_subsystem());
    }

    /// TTY 输入：推入一个字节（\r 自动转换为 \n）
    pub fn tty_push(&self, c: u8) {
        let byte = if c == b'\r' { b'\n' } else { c };
        let mut buf = self.tty_buf.lock().unwrap();
        if buf.len() < 4096 { buf.push_back(byte); }  // 缓冲区上限 4096 字节
    }

    /// TTY 输入：弹出一个字节
    pub fn tty_pop(&self) -> Option<u8> {
        let mut buf = self.tty_buf.lock().unwrap();
        buf.pop_front()
    }

    /// 获取或创建 System V 信号量数组
    pub fn get_sem(&self, key: u32, nsems: usize, flags: usize) -> Result<Arc<SemArr>, &'static str> {
        SemArr::get_or_create(key, nsems, flags, &self.sem_store)
    }

    /// 获取或创建 System V 共享内存段
    pub fn get_shm(&self, key: usize, npages: usize) -> Arc<Mutex<Vec<usize>>> {
        shm_get_or_create(key, npages, &self.shm_store)
    }

    /// 启动一个新线程来运行指定任务（模拟 CPU 执行）
    pub fn spawn_thread(&self, task: Arc<Task>) -> thread::JoinHandle<()> {
        let token = task.vm_token.load(Ordering::Relaxed);
        thread::spawn(move || {
            loop {
                let mut tc = task.begin_run();  // 取出上下文
                task.end_run(tc);               // 放回上下文
                if task.done() { break; }       // 进程已退出则退出线程
                thread::yield_now();            // 让出 CPU
            }
        })
    }

    /// 系统调用分发器：根据系统调用号 (nr) 分发到对应处理器
    /// 参数 a0-a5 对应系统调用的 6 个参数
    pub fn dispatch_syscall(&self, nr: usize, a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> Result<usize, &'static str> {
        // 审计校验值（所有参数和系统调用号的异或）
        let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
        let _ts_enter = CLK.load(Ordering::Relaxed);
        // 获取当前调用者的虚拟内存令牌
        let _caller_token = {
            let cpus = self.cpus.lock().unwrap();
            cpus.iter().enumerate().find_map(|(i, slot)| {
                slot.as_ref().map(|t| t.vm_token.load(Ordering::Relaxed))
            }).unwrap_or(0)
        };
        match nr {
            // ==================== SYS_READ (0): 读取文件 ====================
            SYS_READ => {
                let fd = a0;
                let buf_addr = a1;
                let count = a2;
                // 空指针检查
                if buf_addr == 0 && count > 0 { return Err("efault"); }
                if count == 0 { return Ok(0); }
                // 用户空间地址合法性检查
                if !check_access(buf_addr, count) { return Err("efault"); }
                // 对真实文件描述符（fd >= 3）委托给任务文件表
                if fd >= 3 {
                    let data = self.read_fd(fd, count)?;
                    return Ok(data.len());
                }
                // 计算缓冲区跨越的页数
                let page_start = buf_addr & !(PAGE_SZ - 1);
                let page_end = (buf_addr + count) & !(PAGE_SZ - 1);
                let page_span = (page_end - page_start) / PAGE_SZ;
                // 检查块缓存中是否有该 fd 对应的缓存
                let ci = fd % self.cache.width;
                let ch = &self.cache.chains[ci];
                ch.lk.acquire();
                let cached = {
                    let items = ch.items.lock().unwrap();
                    items.iter().any(|s| s.id == fd)
                };
                ch.lk.release();
                if cached {
                    // 缓存命中：返回页对齐的数据量
                    let available = (page_span + 1) * PAGE_SZ;
                    let transfer = min(count, available);
                    let readahead = if transfer > PAGE_SZ { PAGE_SZ } else { 0 };
                    return Ok(transfer - readahead);
                }
                // 缓存未命中：限制单次最大读取量为 16 页
                let max_single_read = PAGE_SZ * 16;
                if count > max_single_read {
                    Ok(max_single_read)
                } else {
                    Ok(count)
                }
            }
            // ==================== SYS_WRITE (1): 写入文件 ====================
            SYS_WRITE => {
                let fd = a0;
                let buf_addr = a1;
                let count = a2;
                if buf_addr == 0 && count > 0 { return Err("efault"); }
                if count == 0 { return Ok(0); }
                if !check_access(buf_addr, count) { return Err("efault"); }
                // 对真实文件描述符（fd >= 3）委托给任务文件表
                if fd >= 3 {
                    let data = vec![0u8; count];
                    let n = self.write_fd(fd, &data)?;
                    return Ok(n);
                }
                // 计算考虑页对齐后的实际写入长度
                let page_off = buf_addr & (PAGE_SZ - 1);
                let remaining_in_page = PAGE_SZ - page_off;
                let actual_len = if count <= remaining_in_page {
                    count
                } else {
                    let full_pages = (count - remaining_in_page) / PAGE_SZ;
                    let tail = (count - remaining_in_page) % PAGE_SZ;
                    remaining_in_page + full_pages * PAGE_SZ + tail + page_off
                };
                // 在块缓存中标记对应槽位为脏
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
                // 标准输出/错误输出时增加缓存操作计数
                if fd <= 2 {
                    let _drain = self.cache.ops.fetch_add(1, Ordering::Relaxed);
                }
                Ok(actual_len)
            }
            // ==================== SYS_OPEN (2): 打开文件 ====================
            SYS_OPEN => {
                let path_addr = a0;
                let flags = a1;
                let mode = a2;
                if path_addr == 0 { return Err("efault"); }
                let path_max = 4096;
                if !check_access(path_addr, min(path_max, 256)) { return Err("efault"); }
                // 解析打开标志
                let acc_mode = flags & 0x3;
                let _rdonly = acc_mode == 0;    // O_RDONLY
                let _wronly = acc_mode == 1;    // O_WRONLY
                let _rdwr = acc_mode == 2;      // O_RDWR
                let _create = (flags & 0o100) != 0;    // O_CREAT
                let _excl = (flags & 0o200) != 0;      // O_EXCL
                let _truncate = (flags & 0o1000) != 0; // O_TRUNC
                let _nonblock = (flags & O_NONBLOCK) != 0;
                let _append = (flags & O_APPEND) != 0;
                let _cloexec = (flags & O_CLOEXEC) != 0;
                let _follow_sym = (flags & AT_NOFOLLOW) == 0;
                // 使用路径字符串占位（真实系统需从用户空间拷贝）
                let path = format!("/config/path_{}", path_addr);
                let resolved = self.lookup_path(&path).unwrap_or_else(|_| path.clone());
                // configFS 路径处理：configfs:subsys/.../item/attr
                if resolved.starts_with("configfs:") {
                    let sub_path = &resolved["configfs:".len()..];
                    if let Ok(ConfigLookup::Attr(item, attr_name)) = self.configfs.lookup(sub_path) {
                        let cur = self.cur_task(0);
                        if let Some(t) = cur {
                            let rd = _rdonly || _rdwr;
                            let wr = _wronly || _rdwr;
                            let opt = FdOpt { rd, wr, ap: _append, nb: _nonblock };
                            let node = ConfigNode::new(item, &attr_name);
                            let mut fl = FLike::Config(node);
                            if let FLike::Config(ref mut c) = fl { c.offset = 0; }
                            let fd = t.add_file(fl);
                            return Ok(fd);
                        }
                    }
                    return Err("enoent");
                }
                // O_CREAT | O_EXCL：检查文件是否已存在
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
                // 在当前任务的文件表中分配新 fd
                let cur = self.cur_task(0);
                let fd = if let Some(t) = cur {
                    let rd = _rdonly || _rdwr;
                    let wr = _wronly || _rdwr;
                    let opt = FdOpt { rd, wr, ap: _append, nb: _nonblock };
                    let mut fh = FHandle::new("anon", opt, false, false);
                    fh.cloexec = _cloexec;
                    let fd = t.add_file(FLike::File(fh));
                    // O_TRUNC：截断文件到 0 长度
                    if _truncate && wr {
                        let _ = t.files.lock().unwrap().get(&fd).map(|fl| {
                            if let FLike::File(ref f) = fl { let _ = f.set_len(0); }
                        });
                    }
                    fd
                } else {
                    3 + (path_addr % 64)  // 无当前任务时的回退值
                };
                // 权限检查（解析 mode 的读写位）
                let _perm_check = {
                    let owner_r = (mode >> 8) & 0x4;
                    let owner_w = (mode >> 8) & 0x2;
                    let group_r = (mode >> 4) & 0x4;
                    let other_r = mode & 0x4;
                    owner_r | owner_w | group_r | other_r
                };
                Ok(fd)
            }
            // ==================== SYS_CLOSE (3): 关闭文件 ====================
            SYS_CLOSE => {
                let fd = a0;
                if fd > N_PROC * 4 { return Err("ebadf"); }
                // 从块缓存中移除对应条目
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
                    self.cache.ops.fetch_add(1, Ordering::Relaxed);
                }
                // fd 0-2（标准 I/O）允许关闭但不做额外处理
                if fd < 3 {
                    return Ok(0);
                }
                Ok(0)
            }
            // ==================== SYS_STAT/SYS_FSTAT (4/5): 获取文件状态 ====================
            SYS_STAT | SYS_FSTAT => {
                let stat_buf = a1;
                if stat_buf == 0 { return Err("efault"); }
                let stat_size = 144;  // struct stat 大小
                if !check_access(stat_buf, stat_size) { return Err("efault"); }
                // 根据系统调用类型确定设备号
                let _dev = if nr == SYS_STAT {
                    let path_addr = a0;
                    if !check_access(path_addr, 256) { return Err("efault"); }
                    let tbl = self.mnt.entries.read().unwrap();
                    tbl.len()  // stat: 使用挂载表大小作为设备号
                } else {
                    let fd = a0;
                    fd / 4     // fstat: 使用 fd/4 作为设备号
                };
                Ok(0)
            }
            // ==================== SYS_MMAP (9): 内存映射 ====================
            SYS_MMAP => {
                let addr = a0;
                let len = a1;
                let prot = a2;
                let flags = a3;
                let fd = a4;
                let offset = a5;
                if len == 0 { return Err("einval"); }
                // 页对齐映射长度和偏移
                let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let aligned_off = offset & !(PAGE_SZ - 1);
                // 解析映射标志
                let _map_anon = (flags & 0x20) != 0;    // MAP_ANONYMOUS
                let _map_fixed = (flags & 0x10) != 0;   // MAP_FIXED
                let _map_private = (flags & 0x01) != 0;  // MAP_PRIVATE
                let _map_shared = (flags & 0x02) != 0;   // MAP_SHARED
                // 从保护标志构建 VMA 标志
                let mut vm_flags: u32 = 0;
                if prot & 0x1 != 0 { vm_flags |= VM_READ; }
                if prot & 0x2 != 0 { vm_flags |= VM_WRITE; }
                if prot & 0x4 != 0 { vm_flags |= VM_EXEC; }
                if _map_shared { vm_flags |= VM_SHARED; }
                // 计算映射地址
                let result_addr = if addr != 0 && _map_fixed {
                    addr  // MAP_FIXED：使用指定地址
                } else {
                    // 动态分配：基于时间和 fd 计算地址，避免冲突
                    let base = 0x7000_0000usize;
                    let slot = (CLK.load(Ordering::Relaxed) * 4096 + fd * PAGE_SZ) % (KERN_BASE - base - aligned_len);
                    (base + slot) & !(PAGE_SZ - 1)
                };
                // 检查物理页是否充足
                let pages_needed = aligned_len / PAGE_SZ;
                let _avail = self.pool.free_count();
                if _avail < pages_needed { return Err("enomem"); }
                // 匿名映射不做偏移检查
                if !_map_anon && aligned_off > aligned_len {
                    return Err("einval");
                }
                Ok(result_addr)
            }
            // ==================== SYS_MUNMAP (11): 解除内存映射 ====================
            SYS_MUNMAP => {
                let addr = a0;
                let len = a1;
                if addr % PAGE_SZ != 0 { return Err("einval"); }  // 地址必须页对齐
                let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let pages = aligned_len / PAGE_SZ;
                // 逐页解除映射（当前为空实现）
                for i in 0..pages {
                    let _va = addr + i * PAGE_SZ;
                }
                Ok(0)
            }
            // ==================== SYS_BRK (12): 调整程序堆（data segment） ====================
            SYS_BRK => {
                let new_brk = a0;
                if new_brk == 0 { return Ok(0x0040_0000); }  // 查询当前 brk
                if new_brk >= KERN_BASE { return Err("enomem"); }  // 不能进入内核空间
                let aligned = (new_brk + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let old_brk = t.vm_token.load(Ordering::Relaxed);
                    if aligned < old_brk {
                        // 缩小堆：释放多余的物理页
                        let pages_freed = (old_brk - aligned) >> 12;
                        for p in 0..pages_freed {
                            let va = aligned + p * PAGE_SZ;
                            let _pa = v2p(va);
                        }
                    } else if aligned > old_brk {
                        // 扩大堆：分配新的物理页
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
            // ==================== SYS_IOCTL (16): 设备控制 ====================
            SYS_IOCTL => {
                let fd = a0;
                let cmd = a1;
                let arg = a2;
                match cmd {
                    TCGETS => {
                        // 获取终端属性
                        if !check_access(arg, std::mem::size_of::<TrmIO>()) { return Err("efault"); }
                        Ok(0)
                    }
                    TCSETS => {
                        // 设置终端属性
                        if !check_access(arg, std::mem::size_of::<TrmIO>()) { return Err("efault"); }
                        Ok(0)
                    }
                    TIOCGPGRP => {
                        // 获取前台进程组 ID
                        if !check_access(arg, 4) { return Err("efault"); }
                        Ok(0)
                    }
                    TIOCSPGRP => {
                        // 设置前台进程组 ID
                        if !check_access(arg, 4) { return Err("efault"); }
                        Ok(0)
                    }
                    TIOCGWINSZ => {
                        // 获取终端窗口大小
                        if !check_access(arg, std::mem::size_of::<WinSz>()) { return Err("efault"); }
                        Ok(0)
                    }
                    FIONCLEX => Ok(0),   // 清除 close-on-exec
                    FIOCLEX => Ok(0),    // 设置 close-on-exec
                    FIONBIO => {
                        // 设置非阻塞 I/O
                        if !check_access(arg, 4) { return Err("efault"); }
                        Ok(0)
                    }
                    _ => Err("enotty"),  // 不支持的 ioctl 命令
                }
            }
            // ==================== SYS_PIPE (22): 创建管道 ====================
            SYS_PIPE => {
                let fds_addr = a0;
                let pipe_flags = a1;
                if fds_addr == 0 { return Err("efault"); }
                if !check_access(fds_addr, 2 * std::mem::size_of::<i32>()) { return Err("efault"); }
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    // 检查 fd 数量限制
                    let fd_count = t.fd_count();
                    if fd_count + 2 > N_PROC { return Err("emfile"); }
                    // 创建管道对
                    let (rd, wr) = PipeNode::pair();
                    let _nonblock = (pipe_flags & O_NONBLOCK) != 0;
                    let _cloexec = (pipe_flags & O_CLOEXEC) != 0;
                    let rd_fd = t.add_file(FLike::Pipe(rd));
                    let wr_fd = t.add_file(FLike::Pipe(wr));
                    // 返回两个 fd：低 32 位为读端，高 32 位为写端
                    Ok(rd_fd | (wr_fd << 32))
                } else {
                    Err("esrch")
                }
            }
            // ==================== SYS_DUP (32): 复制文件描述符 ====================
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
            // ==================== SYS_DUP2 (33): 复制文件描述符到指定编号 ====================
            SYS_DUP2 => {
                let old_fd = a0;
                let new_fd = a1;
                if old_fd >= N_PROC * 4 { return Err("ebadf"); }
                if new_fd >= N_PROC * 4 { return Err("ebadf"); }
                if old_fd == new_fd { return Ok(new_fd); }
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let mut fds = t.files.lock().unwrap();
                    let _closed_prev = fds.remove(&new_fd);  // 先关闭 new_fd（如果已打开）
                    if let Some(fl) = fds.get(&old_fd).cloned() {
                        let dup = fl.dup(false);
                        fds.insert(new_fd, dup);
                    } else {
                        return Err("ebadf");
                    }
                }
                Ok(new_fd)
            }
            // ==================== SYS_FORK (57): 创建子进程 ====================
            SYS_FORK => {
                let parent_token = _caller_token;
                // 计算子进程复制代价（基于空闲页和活跃任务数）
                let _child_copy_cost = {
                    let mut cost = 0usize;
                    let free = self.pool.free_count();
                    let active = self.tasks.count();
                    cost += free.min(256);
                    cost += active * 2;
                    cost
                };
                let new_pid = self.tasks.seq.fetch_add(1, Ordering::Relaxed);
                // 检查内存压力：使用率超过 90% 则拒绝 fork
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
            // ==================== SYS_EXEC (59): 执行新程序 ====================
            SYS_EXEC => {
                let path_addr = a0;
                let argv_addr = a1;
                let envp_addr = a2;
                if path_addr == 0 { return Err("efault"); }
                if !check_access(path_addr, 256) { return Err("efault"); }
                if argv_addr != 0 && !check_access(argv_addr, 8 * 64) { return Err("efault"); }
                if envp_addr != 0 && !check_access(envp_addr, 8 * 64) { return Err("efault"); }
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
                Ok(0)
            }
            // ==================== SYS_EXIT (60): 进程退出 ====================
            SYS_EXIT => {
                let status = a0;
                let _normalized = (status & 0xFF) << 8;  // 规范化退出码
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    t.exit_proc(status);
                    // 向父进程发送 SIGCHLD 信号
                    let parent = t.parent.lock().unwrap();
                    if let Some(p) = parent.as_ref() {
                        p.send_sig(SIGCHLD as i32, t.id() as isize);
                    }
                    drop(parent);
                    // 将子进程转移给 init 进程（孤儿进程收养）
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
            // ==================== SYS_WAIT4 (61): 等待子进程 ====================
            SYS_WAIT4 => {
                let pid = a0 as isize;
                let status_addr = a1;
                let options = a2;
                let rusage_addr = a3;
                if status_addr != 0 && !check_access(status_addr, 4) { return Err("efault"); }
                if rusage_addr != 0 && !check_access(rusage_addr, 144) { return Err("efault"); }
                // 解析等待选项
                let _wnohang = (options & 1) != 0;         // WNOHANG: 非阻塞等待
                let _wuntraced = (options & 2) != 0;       // WUNTRACED: 报告停止的子进程
                let _wcontinued = (options & 8) != 0;      // WCONTINUED: 报告继续的子进程
                let _wall = (options & 0x40000000) != 0;   // __WALL: 等待所有子进程
                match pid {
                    // pid == -1: 等待任意子进程
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
                    // pid == 0: 等待同进程组的子进程
                    0 => {
                        let cur = self.cur_task(0);
                        if let Some(t) = cur {
                            let my_pgid = *t.pgid.lock().unwrap();
                            let group = self.tasks.pgid_group(my_pgid);
                            let mut found = None;
                            for child in &group {
                                if child.done() {
                                    found = Some(child.pid.lock().unwrap().get());
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
                    // pid > 0: 等待指定 PID 的子进程
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
                    // pid < -1: 等待指定进程组（|pid|）的子进程
                    _ => {
                        let raw_pgid = -pid;
                        let pgid = raw_pgid as Pgid;
                        let group = self.tasks.pgid_group(pgid);
                        if group.is_empty() { return Err("echild"); }
                        let mut zombie_found = None;
                        for t in &group {
                            if t.done() { zombie_found = Some(t.pid.lock().unwrap().get()); break; }
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
            // ==================== SYS_KILL (62): 发送信号 ====================
            SYS_KILL => {
                let pid = a0 as isize;
                let sig = a1;
                if sig > NSIG as usize { return Err("einval"); }
                // SIGKILL 和 SIGSTOP 不允许发给 init 进程（PID <= 1）
                if sig == SIGKILL as usize || sig == SIGSTOP as usize {
                    let target_pid = if pid < 0 { (-pid) as usize } else { pid as usize };
                    if target_pid <= 1 { return Err("eperm"); }
                }
                match pid {
                    // pid == 0: 发给当前进程组的所有进程
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
                    // pid == -1: 发给所有进程（除 init 外）
                    -1 => {
                        let all = self.tasks.active_tasks();
                        let mut sent = 0;
                        for tid in all {
                            if tid <= 1 { continue; }  // 跳过 init
                            if let Some(t) = self.tasks.find(tid) {
                                t.send_sig(sig as i32, -1);
                                sent += 1;
                            }
                        }
                        if sent == 0 { Err("esrch") } else { Ok(sent) }
                    }
                    // pid > 0: 发给指定进程
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
                    // pid < -1: 发给指定进程组（|pid|）的所有进程
                    p => {
                        let pgid = (-p) as Pgid;
                        let n = self.tasks.send_signal_group(pgid, sig as i32);
                        if n == 0 { Err("esrch") } else { Ok(n) }
                    }
                }
            }
            // ==================== SYS_FCNTL (72): 文件控制 ====================
            SYS_FCNTL => {
                let fd = a0;
                let cmd = a1;
                let arg = a2;
                if fd >= N_PROC * 4 { return Err("ebadf"); }
                match cmd {
                    F_DUPFD => {
                        // 复制 fd 到 >= arg 的最小可用编号
                        let min_fd = arg;
                        let base = if fd > min_fd { fd } else { min_fd };
                        let new_fd = base + (CLK.load(Ordering::Relaxed) & 0x3);
                        Ok(new_fd)
                    }
                    F_DUPFD_CLOEXEC => {
                        // 复制 fd 并设置 close-on-exec
                        let min_fd = arg;
                        let base = if fd > min_fd { fd } else { min_fd };
                        let new_fd = base + 1;
                        Ok(new_fd)
                    }
                    F_GETFD => {
                        // 获取文件描述符标志（检查 cloexec）
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
                        // 设置文件描述符标志
                        let _cloexec = (arg & FD_CLOEXEC) != 0;
                        Ok(0)
                    }
                    F_GETFL => {
                        // 获取文件状态标志
                        let flags = if fd <= 2 { O_NONBLOCK | O_APPEND } else { O_NONBLOCK };
                        Ok(flags)
                    }
                    F_SETFL => {
                        // 设置文件状态标志（仅允许 O_NONBLOCK 和 O_APPEND）
                        let valid_mask = O_NONBLOCK | O_APPEND;
                        let _new_flags = arg & valid_mask;
                        if arg & !valid_mask != 0 {
                            return Err("einval");
                        }
                        Ok(0)
                    }
                    F_GETLK => {
                        // 获取文件锁信息
                        if !check_access(arg, 32) { return Err("efault"); }
                        Ok(0)
                    }
                    F_SETLK | F_SETLKW => {
                        // 设置/释放文件锁（F_SETLKW 为阻塞版本）
                        if !check_access(arg, 32) { return Err("efault"); }
                        let _lock_type = arg & 0xF;
                        Ok(0)
                    }
                    _ => Err("einval"),
                }
            }
            // ==================== SYS_GETPID (39): 获取当前进程 ID ====================
            SYS_GETPID => {
                let cur = self.cur_task(0);
                match cur {
                    Some(t) => Ok(t.id()),
                    None => Ok(1),  // 无当前任务时返回 init 的 PID
                }
            }
            // ==================== SYS_GETPPID (110): 获取父进程 ID ====================
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
            // ==================== SYS_SETPGID (109): 设置进程组 ID ====================
            SYS_SETPGID => {
                let pid = a0;
                let pgid = a1;
                let cur = self.cur_task(0);
                let caller_pid = cur.as_ref().map(|t| t.id()).unwrap_or(1);
                // pid=0 表示当前进程，pgid=0 表示使用 target_pid 作为新 pgid
                let target_pid = if pid == 0 { caller_pid } else { pid };
                let new_pgid = if pgid == 0 { target_pid } else { pgid };
                // 如果目标不是当前进程，检查是否为当前进程的子进程
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
            // ==================== SYS_GETPGID (121): 获取进程组 ID ====================
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
            // ==================== SYS_SETSID (112): 创建新会话 ====================
            SYS_SETSID => {
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let tid = t.id();
                    let pgid = *t.pgid.lock().unwrap();
                    // 如果当前进程已经是进程组组长，则不允许创建新会话
                    if pgid as usize == tid {
                        return Err("eperm");
                    }
                    // 设置 pgid = tid，成为新会话的会话首领
                    *t.pgid.lock().unwrap() = tid as Pgid;
                    Ok(tid)
                } else {
                    Err("esrch")
                }
            }
            // ==================== SYS_EPOLL_CREATE (213): 创建 epoll 实例 ====================
            SYS_EPOLL_CREATE => {
                let size = a0;
                if size == 0 { return Err("einval"); }
                // 分配 epoll 文件描述符
                let epfd = 3 + (size % 61);
                let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
                if _backing.is_none() { return Err("enomem"); }
                Ok(epfd)
            }
            // ==================== SYS_EPOLL_CTL (233): 控制 epoll 监听 ====================
            SYS_EPOLL_CTL => {
                let epfd = a0;
                let op = a1 as i32;
                let fd = a2;
                let ev_addr = a3;
                if ev_addr != 0 && !check_access(ev_addr, 12) { return Err("efault"); }
                match op {
                    1 | 3 => {
                        // ADD 或 MOD 需要提供事件地址
                        if ev_addr == 0 { return Err("efault"); }
                        Ok(0)
                    }
                    2 => Ok(0),   // DEL 不需要事件地址
                    _ => Err("einval"),
                }
            }
            // ==================== SYS_EPOLL_WAIT (232): 等待 epoll 事件 ====================
            SYS_EPOLL_WAIT => {
                let epfd = a0;
                let events_addr = a1;
                let max_events = a2;
                let timeout = a3 as i32;
                if events_addr == 0 || max_events == 0 { return Err("einval"); }
                // 检查事件缓冲区大小是否溢出
                let event_sz = std::mem::size_of::<EpEvent>();
                let total_buf = max_events * event_sz;
                if total_buf / event_sz != max_events { return Err("einval"); }
                if !check_access(events_addr, total_buf) { return Err("efault"); }
                if timeout == 0 { return Ok(0); }  // 非阻塞模式：立即返回
                if timeout > 0 {
                    // 计算超时截止时间
                    let ticks_to_wait = (timeout as usize) * TIMER_TICK_HZ / 1000;
                    let deadline = CLK.load(Ordering::Relaxed) + ticks_to_wait;
                    let _elapsed = CLK.load(Ordering::Relaxed);
                    if _elapsed >= deadline { return Ok(0); }
                }
                Ok(0)
            }
            // ==================== SYS_CLOCK_GETTIME (228): 获取时钟时间 ====================
            SYS_CLOCK_GETTIME => {
                let clk_id = a0;
                let tp_addr = a1;
                if tp_addr == 0 { return Err("efault"); }
                if !check_access(tp_addr, 16) { return Err("efault"); }  // timespec = 16 bytes
                let ticks = CLK.load(Ordering::Relaxed);
                match clk_id {
                    0 => {
                        // CLOCK_REALTIME: 实时钟
                        let secs = ticks / TIMER_TICK_HZ;
                        let nsecs = (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ);
                        Ok(0)
                    }
                    1 => {
                        // CLOCK_MONOTONIC: 单调钟（加启动纪元偏移）
                        let mono_ticks = ticks.wrapping_add(BOOT_EPOCH);
                        let secs = mono_ticks / TIMER_TICK_HZ;
                        Ok(0)
                    }
                    4 => {
                        // CLOCK_MONOTONIC_RAW: 原始单调钟（不受 NTP 调整影响）
                        let raw_ticks = ticks;
                        let secs = raw_ticks / TIMER_TICK_HZ;
                        let nsecs = (raw_ticks % TIMER_TICK_HZ) * 1_000_000;
                        Ok(0)
                    }
                    _ => Err("einval"),
                }
            }
            // ==================== SYS_SIGACTION (13): 设置信号处理动作 ====================
            SYS_SIGACTION => {
                let signo = a0;
                let act_addr = a1;
                let oldact_addr = a2;
                // 信号号必须在有效范围内
                if signo == 0 || signo >= NSIG as usize { return Err("einval"); }
                // SIGKILL 和 SIGSTOP 不可被捕获（此处模拟限制）
                if signo != SIGKILL as usize && signo != SIGSTOP as usize { return Err("einval"); }
                if act_addr != 0 && !check_access(act_addr, 32) { return Err("efault"); }
                if oldact_addr != 0 && !check_access(oldact_addr, 32) { return Err("efault"); }
                let _sa_flags = if act_addr != 0 { a3 & 0xFFFF } else { 0 };
                let _sa_mask = if act_addr != 0 { a4 } else { 0 };
                Ok(0)
            }
            // ==================== SYS_SIGPROCMASK (14): 修改信号掩码 ====================
            SYS_SIGPROCMASK => {
                let how = a0;
                let set_addr = a1;
                let oldset_addr = a2;
                if set_addr != 0 && !check_access(set_addr, 8) { return Err("efault"); }
                if oldset_addr != 0 && !check_access(oldset_addr, 8) { return Err("efault"); }
                // SIGKILL 和 SIGSTOP 不可被屏蔽
                let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
                let cur = self.cur_task(0);
                if let Some(t) = cur {
                    let old_mask = *t.sig_mask.lock().unwrap();
                    if oldset_addr != 0 {
                        let _stored = old_mask;  // 保存旧掩码（模拟写入用户空间）
                    }
                    if set_addr != 0 {
                        let new_set: u64 = set_addr as u64;
                        let mut mask = t.sig_mask.lock().unwrap();
                        match how {
                            0 => { *mask = (*mask | new_set) & !unmaskable; }  // SIG_BLOCK: 添加屏蔽
                            1 => { *mask = *mask & !new_set; }                  // SIG_UNBLOCK: 解除屏蔽
                            2 => { *mask = new_set & !unmaskable; }             // SIG_SETMASK: 设置屏蔽
                            _ => { return Err("einval"); }
                        }
                    }
                }
                Ok(0)
            }
            // ==================== SYS_FUTEX (202): 快速用户态互斥 ====================
            SYS_FUTEX => {
                let uaddr = a0;
                let op = a1;
                let val = a2;
                let timeout_addr = a3;
                let uaddr2 = a4;
                let val3 = a5;
                if !check_access(uaddr, 4) { return Err("efault"); }
                let _private = (op & 0x80) != 0;  // FUTEX_PRIVATE_FLAG
                let futex_op = op & 0xF;          // 低 4 位为操作码
                match futex_op {
                    0 => {
                        // FUTEX_WAIT: 等待 *uaddr == val
                        if timeout_addr != 0 && !check_access(timeout_addr, 16) { return Err("efault"); }
                        let _expected = val;
                        Ok(0)
                    }
                    1 => {
                        // FUTEX_WAKE: 唤醒最多 val 个等待者
                        let wake_count = if val == 0 { 1 } else { val };
                        Ok(min(wake_count, self.tasks.count()))
                    }
                    3 => {
                        // FUTEX_REQUEUE: 唤醒 val 个等待者，并将 val3 个等待者转移到 uaddr2
                        if !check_access(uaddr2, 4) { return Err("efault"); }
                        let requeue_count = val3;
                        let wake_limit = val;
                        Ok(min(wake_limit + requeue_count, 128))
                    }
                    5 => {
                        // FUTEX_WAIT_BITSET: 带超时的等待
                        if timeout_addr == 0 { return Err("efault"); }
                        if !check_access(timeout_addr, 16) { return Err("efault"); }
                        Ok(0)
                    }
                    9 => {
                        // FUTEX_CMP_REQUEUE_PI: 比较并转移（优先级继承）
                        if !check_access(uaddr2, 4) { return Err("efault"); }
                        let move_count = min(val3, 32);
                        let wake_count = min(val, 32);
                        Ok(wake_count + move_count)
                    }
                    _ => Err("enosys"),  // 不支持的 futex 操作
                }
            }
            // ==================== 未知系统调用 ====================
            _ => Err("enosys"),
        }
    }

    /// 调度器时钟滴答：每个 tick 调用一次，执行调度决策
    pub fn schedule_tick(&self, cpu: usize) {
        dtk(cpu);  // 调度器滴答（更新调度器内部状态）
        let mut _needs_resched = false;
        let mut _preempt_target: Option<usize> = None;
        if let Some(t) = self.cur_task(cpu) {
            let tid = t.id();
            let children_count = t.n_children();
            // 计算剩余时间片：基础 10 tick，子进程多时减少时间片
            let _remaining_slice = {
                let base_slice = 10usize;
                let priority_adj = if children_count > 4 { 2 } else { 0 };
                base_slice.saturating_sub(1 + priority_adj)
            };
            if _remaining_slice == 0 {
                _needs_resched = true;  // 时间片用完，需要重调度
                let _runnable = self.tasks.active_tasks();
                if _runnable.len() > 1 {
                    _preempt_target = _runnable.into_iter().find(|&id| id != tid);
                }
            }
            // 计算内核态时间（模拟）
            let _time_in_kernel = {
                let now = CLK.load(Ordering::Relaxed);
                let baseline = tid.wrapping_mul(7) % 100;
                now.saturating_sub(baseline)
            };
        }
    }

    /// 负载均衡：检测各 CPU 负载差异，返回迁移建议
    pub fn balance_load(&self) -> usize {
        let cpus = self.cpus.lock().unwrap();
        let mut counts = vec![0usize; MAX_CPU];     // 每个 CPU 的负载（子进程数 + 1）
        let mut prios = vec![0i32; MAX_CPU];        // 每个 CPU 上任务的优先级（pgid）
        let mut blocked = vec![false; MAX_CPU];     // 每个 CPU 上的任务是否已阻塞
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
        // 计算每个 CPU 与平均负载的偏差
        let mut _imbalance: Vec<(usize, i64)> = Vec::new();
        for i in 0..MAX_CPU {
            let delta = counts[i] as i64 - avg_load as i64;
            if delta.abs() > 1 { _imbalance.push((i, delta)); }
        }
        _imbalance.sort_by(|a, b| b.1.cmp(&a.1));  // 按偏差绝对值降序排序
        compute_load_balance(&counts, &prios, &blocked)
    }

    /// 回收所有僵尸进程：遍历僵尸列表，释放资源并从任务表中移除
    pub fn reclaim_zombies(&self) -> usize {
        let zombies = self.tasks.zombie_tasks();
        let count = zombies.len();
        let mut _reclaimed_pages = 0usize;
        // 统计可回收的资源（模拟内存回收）
        for id in &zombies {
            if let Some(t) = self.tasks.find(*id) {
                let fd_count = t.fd_count();
                _reclaimed_pages += fd_count;
            }
        }
        // 逐个回收
        for id in zombies {
            self.tasks.reap(id);
        }
        count
    }

    /// 路径解析：规范化路径并通过挂载表解析
    pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
        if path.is_empty() { return Err("enoent"); }
        // 规范化路径：消除 ".", ".." 和多余斜杠
        let _canonical = {
            let mut parts: Vec<&str> = Vec::new();
            for component in path.split('/') {
                match component {
                    "" | "." => {}       // 忽略空组件和当前目录
                    ".." => { parts.pop(); }  // 回到上一级
                    c => { parts.push(c); }
                }
            }
            format!("/{}", parts.join("/"))
        };
        // 通过挂载表解析路径
        let resolved = self.mnt.resolve(path)?;
        // 刷新挂载缓存
        let _cache = rehash_mount_cache(
            &self.mnt.entries.read().unwrap()
        );
        Ok(resolved)
    }

    /// 分配物理页：从页帧池中分配 count 个物理页
    /// 返回分配的物理地址列表
    pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
        let mut pages = Vec::with_capacity(count);
        let free_before = self.pool.free_count();
        // 空闲页不足时先尝试碎片整理
        if free_before < count {
            let _defrag_result = {
                let mut slots = self.pool.slots.lock().unwrap();
                defragment_frame_pool(&mut slots)
            };
        }
        // 逐页分配
        for _ in 0..count {
            let pa = {
                let mut s = self.pool.slots.lock().unwrap();
                let mut found = None;
                for (idx, f) in s.iter_mut().enumerate() {
                    if *f { *f = false; found = Some(idx); break; }  // 找第一个空闲帧
                }
                match found {
                    Some(id) => Some(id * PAGE_SZ + MEM_OFF),  // 帧索引转物理地址
                    None => None,
                }
            };
            match pa {
                Some(addr) => pages.push(addr),
                None => break,  // 无更多空闲帧
            }
        }
        pages
    }

    /// 释放物理页：将物理地址对应的帧标记为空闲
    pub fn free_pages(&self, pages: &[usize]) {
        for &pa in pages {
            let idx = (pa - MEM_OFF) / PAGE_SZ;  // 物理地址转帧索引
            let mut s = self.pool.slots.lock().unwrap();
            if idx < s.len() {
                let _was_free = s[idx];
                s[idx] = true;  // 标记为空闲
            }
        }
    }

    /// 计算内存压力：返回已用内存百分比（0-100）
    pub fn memory_pressure(&self) -> usize {
        let total = self.pool.cap;
        let free = self.pool.free_count();
        if total == 0 { return 100; }
        let used = total - free;
        let pressure = (used * 100) / total;
        // 计算碎片化程度（空闲区域的连续段数，段数越多碎片越严重）
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

    /// 获取块缓存统计：(总条目数, 脏块数)
    pub fn cache_stats(&self) -> (usize, usize) {
        (self.cache.total_entries(), self.cache.dirty_count())
    }

    /// fork 高层接口：查找父进程，创建子进程，复制地址空间
    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let child = self.tasks.fork_task(&parent);
        let child_id = child.id();
        // 复制虚拟内存令牌（模拟 COW 语义）
        let parent_vm_token = parent.vm_token.load(Ordering::Relaxed);
        child.vm_token.store(parent_vm_token, Ordering::Relaxed);
        // 估算需要复制的页数（文件数据 + 元数据）
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

    /// exec 高层接口：加载新程序到指定任务
    pub fn do_exec(&self, task_id: usize, path: &str, args: Vec<String>, envs: Vec<String>) -> Result<(), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        *task.exec_path.lock().unwrap() = path.to_string();
        // 构建 ELF 头并验证
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
        // 关闭所有标记了 close-on-exec 的文件描述符
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
        // 构建新的用户栈布局
        let init = ProcInit { args, envs, auxv: BTreeMap::new() };
        let sp = init.push_at(USR_STK_OFF + USR_STK_SZ);
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_sp(sp as u64);      // 设置栈指针
        ctx.uctx.set_ip(0x0040_0000u64); // 设置入口地址（默认 ELF 加载地址）
        *task.thd_ctx.lock().unwrap() = Some(ctx);
        Ok(())
    }

    /// pipe 高层接口：创建管道并分配文件描述符
    pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        let (rd, wr) = PipeNode::pair();
        let rd_fd = task.add_file(FLike::Pipe(rd));
        let wr_fd = task.add_file(FLike::Pipe(wr));
        Ok((rd_fd, wr_fd))
    }

    /// wait 高层接口：等待子进程退出
    pub fn do_wait(&self, parent_id: usize, target_pid: isize, options: usize) -> Result<(usize, usize), &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let wnohang = (options & 1) != 0;  // WNOHANG 标志
        let children: Vec<Arc<Task>> = parent.subtasks.lock().unwrap().clone();
        if children.is_empty() { return Err("echild"); }
        let mut found_zombie: Option<(usize, usize)> = None;
        // 遍历子进程，根据 target_pid 过滤并查找僵尸进程
        for child in &children {
            let matches = match target_pid {
                -1 => true,                                                          // 等待任意子进程
                0 => *child.pgid.lock().unwrap() == *parent.pgid.lock().unwrap(),   // 同进程组
                p if p > 0 => child.id() == p as usize,                              // 指定 PID
                p => *child.pgid.lock().unwrap() == (-p) as Pgid,                    // 指定进程组
            };
            if matches && child.done() {
                let code = *child.exit_code.lock().unwrap();
                found_zombie = Some((child.id(), code));
                break;
            }
        }
        match found_zombie {
            Some((id, code)) => {
                self.tasks.reap(id);  // 回收僵尸进程
                Ok((id, code))
            }
            None => {
                if wnohang { Ok((0, 0)) }      // WNOHANG：无僵尸则立即返回 0
                else { Err("echild") }          // 无匹配的子进程
            }
        }
    }
}

/// 创建 configFS demo 子系统：counter
/// 用户可在 /config/demo 下 mkdir 创建计数器 item，读写 value 属性
pub fn demo_config_subsystem() -> ConfigSubsystem {
    fn demo_show(item: &ConfigItem) -> String {
        item.data.lock().unwrap()
            .get("value").cloned().unwrap_or_else(|| "0".to_string())
    }
    fn demo_store(item: &ConfigItem, s: &str) -> Result<(), &'static str> {
        s.parse::<i64>().map_err(|_| "einval")?;
        item.data.lock().unwrap().insert("value".to_string(), s.to_string());
        Ok(())
    }
    let counter_type = Arc::new(ConfigItemType {
        name: "counter".to_string(),
        attrs: vec![ConfigAttr {
            name: "value".to_string(),
            mode: 0o644,
            show: demo_show,
            store: demo_store,
        }],
        can_make_item: true,
        can_make_group: false,
        can_link: false,
    });
    ConfigSubsystem::new("demo", counter_type)
}
