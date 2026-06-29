//! System V IPC 模块：信号量（semaphore）和共享内存（shared memory）。
//!
//! 本模块实现内核中的进程间通信（IPC）机制：
//! - **IpcPerm**：SysV IPC 权限结构（key、uid、gid、mode 等）
//! - **SemDs**：信号量数组描述符（权限 + 时间戳 + 数量）
//! - **SemArr**：信号量数组，带 Index trait 和 get_or_create 工厂方法
//! - **SemCtx**：每进程信号量上下文，带 SEM_UNDO 撤销机制（Drop 时自动释放）
//! - **ShmTag / ShmCtx**：共享内存标记与每进程共享内存上下文
//! - **shm_get_or_create**：共享内存段的全局工厂函数
//!
//! 依赖 sync::Sema 作为信号量的底层实现，使用 Weak 引用模式管理全局 IPC 对象生命周期。

use std::collections::BTreeMap;
use std::ops::Index;
use std::sync::{Arc, Mutex, RwLock, Weak};

use crate::consts::*;
use crate::sync::Sema;

// ==================== 类型别名 ====================

type SemId = usize;   // 信号量数组 ID（进程内局部编号）
type SemNum = u16;    // 信号量编号（数组内索引，u16 足够）
type SemOp = i16;     // 信号量操作值（正数 = 释放/V 操作，负数 = 获取/P 操作）

type ShmId = usize;   // 共享内存段 ID

// ==================== IpcPerm — SysV IPC 权限结构 ====================

/// SysV IPC 权限结构，对应 Linux 的 struct ipc_perm。
/// 使用 #[repr(C)] 保证与 C 语言的内存布局兼容。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    pub key: u32,    // IPC 键值（用户空间通过 key 查找/创建 IPC 对象）
    pub uid: u32,    // 当前所有者的用户 ID
    pub gid: u32,    // 当前所有者的组 ID
    pub cuid: u32,   // 创建者的用户 ID
    pub cgid: u32,   // 创建者的组 ID
    pub mode: u32,   // 访问权限（低 9 位编码 rwxrwxrwx，类似 Unix 文件权限）
    pub seq: u32,    // 序列号（区分同一 key 的不同实例）
    pub pad1: usize, // 填充字段（对齐 C 结构体布局）
    pub pad2: usize, // 填充字段
}

// ==================== SemDs — 信号量数组描述符 ====================

/// 信号量数组描述符，对应 Linux 的 struct semid_ds。
/// 包含权限信息、时间戳和信号量数量。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemDs {
    pub perm: IpcPerm,  // 权限信息（key、uid、mode 等）
    pub otime: usize,   // 最后一次 semop 操作的时间戳
    _p1: usize,         // 填充（对齐 C 结构体布局）
    pub ctime: usize,   // 最后一次属性修改的时间戳
    _p2: usize,         // 填充
    pub nsems: usize,   // 该信号量数组中的信号量数量（创建后不可变）
}

// ==================== SemArr — 信号量数组 ====================

/// 信号量数组：一个 key 对应一组信号量。
/// ds 存储描述符（权限+时间戳），sems 存储实际的信号量实例。
/// sems 中的每个 Sema 来自 sync.rs，复用其计数信号量实现。
pub struct SemArr {
    pub ds: Mutex<SemDs>,     // 信号量描述符（受互斥锁保护）
    pub sems: Vec<Sema>,      // 信号量实例数组
}

/// 支持 arr[i] 语法直接访问第 i 个信号量
impl Index<usize> for SemArr {
    type Output = Sema;
    fn index(&self, i: usize) -> &Sema { &self.sems[i] }
}
impl SemArr {
    /// 销毁数组中所有信号量（标记为已移除，唤醒所有等待者）
    pub fn remove(&self) { for s in &self.sems { s.remove(); } }
    /// 更新 otime 为当前时间（简化实现：设为 0 占位）
    pub fn otime_now(&self) { self.ds.lock().unwrap().otime = 0; }
    /// 更新 ctime 为当前时间（简化实现：设为 0 占位）
    pub fn ctime_now(&self) { self.ds.lock().unwrap().ctime = 0; }
    /// 更新描述符的权限字段（uid、gid、mode），mode 被掩码限制为低 9 位
    pub fn set_ds(&self, new: &SemDs) {
        let mut l = self.ds.lock().unwrap();
        l.perm.uid = new.perm.uid;
        l.perm.gid = new.perm.gid;
        l.perm.mode = new.perm.mode & 0x1ff;  // 0x1ff = 0o777，只保留权限位
    }
    /// 工厂方法：获取或创建信号量数组（实现 semget 系统调用的核心逻辑）
    ///
    /// - key=0：分配新的私有键（最小未使用的正整数）
    /// - key 已存在：通过 Weak::upgrade 尝试获取已有数组
    ///   - IPC_EXCL | IPC_CREAT 同时设置且已存在：返回 Err("eexist")
    ///   - 否则返回已有数组
    /// - key 不存在：创建新的信号量数组，初始计数均为 0
    ///
    /// 全局 store 使用 Weak 引用，避免阻止 SemArr 被释放
    pub fn get_or_create(
        key: u32,
        nsems: usize,
        flags: usize,
        store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,
    ) -> Result<Arc<Self>, &'static str> {
        let mut m = store.write().unwrap();
        let mut k = key;
        if k == 0 {
            // key=0 表示私有信号量，分配最小未使用的正整数键
            k = (1u32..).find(|i| m.get(i).is_none()).unwrap();
        } else if let Some(w) = m.get(&k) {
            if let Some(a) = w.upgrade() {
                // IPC_EXCL (bit 9) | IPC_CREAT (bit 10)：要求独占创建
                if (flags & (1 << 9)) != 0 && (flags & (1 << 10)) != 0 { return Err("eexist"); }
                return Ok(a);  // 返回已有的信号量数组
            }
        }
        // 创建新的信号量数组
        let mut sv = Vec::new();
        for _ in 0..nsems { sv.push(Sema::new(0)); }  // 初始计数均为 0
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
        // 用弱引用存入全局 store，不阻止 SemArr 被释放
        m.insert(k, Arc::downgrade(&arr));
        Ok(arr)
    }
}

// ==================== SemCtx — 每进程信号量上下文 ====================

/// 每进程的信号量上下文，记录进程关联的所有信号量数组和撤销操作表。
/// undos 实现 SEM_UNDO 语义：进程退出时自动撤销（释放）已获取的信号量，防止死锁。
#[derive(Default)]
pub struct SemCtx {
    /// 该进程已关联的信号量数组：SemId → Arc<SemArr>
    pub arrays: BTreeMap<SemId, Arc<SemArr>>,
    /// 撤销操作表：(SemId, SemNum) → SemOp
    /// 记录累积的撤销值（与实际操作值符号相反）
    pub undos: BTreeMap<(SemId, SemNum), SemOp>,
}
impl SemCtx {
    /// 将信号量数组加入进程上下文，返回分配的最小空闲局部 ID
    pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
        let id = (0..).find(|i| !self.arrays.contains_key(i)).unwrap();
        self.arrays.insert(id, arr);
        id
    }
    /// 从进程上下文中移除指定 ID 的信号量数组
    pub fn remove(&mut self, id: SemId) { self.arrays.remove(&id); }
    /// 查找最小空闲 ID（内部辅助方法）
    fn free_id(&self) -> SemId { (0..).find(|i| self.arrays.get(i).is_none()).unwrap() }
    /// 根据 ID 获取信号量数组的强引用（克隆 Arc）
    pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> { self.arrays.get(&id).cloned() }
    /// 记录一次信号量操作到撤销表（用于 SEM_UNDO）
    /// op 为实际操作值，undo 值为 -op（反向操作）
    pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) {
        let old = *self.undos.get(&(id, num)).unwrap_or(&0);
        self.undos.insert((id, num), old - op);  // 累加撤销操作（符号相反）
    }
}

/// fork 时子进程继承信号量数组关联，但不继承撤销表。
/// 子进程有独立的操作历史，不应撤销父进程的操作（符合 POSIX 规范）。
impl Clone for SemCtx {
    fn clone(&self) -> Self {
        SemCtx { arrays: self.arrays.clone(), undos: BTreeMap::new() }
    }
}

/// 进程退出时自动撤销信号量操作（SEM_UNDO 语义的核心实现）。
/// 遍历撤销表，对每个信号量执行反向操作（如释放已获取的信号量）。
impl Drop for SemCtx {
    fn drop(&mut self) {
        for (&(id, num), &op) in &self.undos {
            if let Some(arr) = self.arrays.get(&id) {
                match op {
                    1 => arr[num as usize].release(),  // 撤销"获取"操作 → 释放信号量
                    _ => {}  // 其他操作暂不处理（简化实现）
                }
            }
        }
    }
}

// ==================== ShmTag — 共享内存标记 ====================

/// 共享内存标记：记录一段共享内存在进程中的映射信息。
/// pages 通过 Arc 共享——多个进程的 ShmTag 指向同一块物理内存。
#[derive(Clone)]
pub struct ShmTag {
    pub addr: usize,                       // 映射到进程地址空间的虚拟地址
    pub pages: Arc<Mutex<Vec<usize>>>,     // 共享内存的页面数据（简化为 usize 数组）
}
impl ShmTag {
    /// 设置映射地址（shmat 系统调用后更新）
    pub fn set_addr(&mut self, a: usize) { self.addr = a; }
}

// ==================== shm_get_or_create — 共享内存工厂函数 ====================

/// 获取或创建共享内存段（实现 shmget 系统调用的核心逻辑）。
/// - key 已存在且弱引用有效：返回已有的共享内存
/// - key 不存在或弱引用失效：创建新的共享内存段（初始化为 0）
/// 全局 store 使用 Weak 引用，与 SemArr::get_or_create 设计一致。
pub fn shm_get_or_create(
    key: usize,
    npages: usize,
    store: &RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
) -> Arc<Mutex<Vec<usize>>> {
    let mut m = store.write().unwrap();
    // 尝试复用已有的共享内存
    if let Some(w) = m.get(&key) {
        if let Some(g) = w.upgrade() { return g; }
    }
    // 创建新的共享内存段（初始化为 0）
    let g = Arc::new(Mutex::new(vec![0usize; npages]));
    m.insert(key, Arc::downgrade(&g));  // 弱引用存入全局 store
    g
}

// ==================== ShmCtx — 每进程共享内存上下文 ====================

/// 每进程的共享内存上下文，记录进程关联的所有共享内存段。
#[derive(Default)]
pub struct ShmCtx {
    /// 该进程已关联的共享内存段：ShmId → ShmTag
    pub ids: BTreeMap<ShmId, ShmTag>
}
impl ShmCtx {
    /// 将共享内存段加入进程上下文，返回分配的最小空闲局部 ID
    pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
        let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
        self.ids.insert(id, ShmTag { addr: 0, pages: g });
        id
    }
    /// 根据 ID 获取共享内存标记的克隆
    pub fn get(&self, id: ShmId) -> Option<ShmTag> { self.ids.get(&id).cloned() }
    /// 更新指定 ID 的共享内存标记（如修改映射地址）
    pub fn set(&mut self, id: ShmId, tag: ShmTag) { self.ids.insert(id, tag); }
    /// 根据映射地址反查 ShmId（shmdt 系统调用时通过地址查找对应的共享内存段）
    pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
        self.ids.iter().find(|(_, v)| v.addr == addr).map(|(k, _)| *k)
    }
    /// 从进程上下文中移除共享内存段（shmdt 分离操作）
    pub fn pop(&mut self, id: ShmId) { self.ids.remove(&id); }
}

/// fork 时子进程继承共享内存映射（与 SemCtx 不同，共享内存需要完整继承）。
/// 子进程可以访问与父进程相同的共享内存段。
impl Clone for ShmCtx {
    fn clone(&self) -> Self { ShmCtx { ids: self.ids.clone() } }
}
