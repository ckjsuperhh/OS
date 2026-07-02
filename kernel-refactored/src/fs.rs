//! 文件系统模块：文件句柄、管道、块缓存、磁盘 I/O、挂载表、epoll 和页缓存。
//!
//! 本模块是内核的文件系统子系统，提供以下能力：
//! - FHandle: 文件句柄，支持读写、seek、dup 等 POSIX 语义
//! - PipeNode: 基于事件总线的管道（pipe() 系统调用的后端）
//! - FLike: VFS 统一分发层，将 File/Pipe/Ep 三种文件类型统一到同一接口
//! - EpInst: epoll 事件多路复用
//! - PageCache: LRU 页缓存，加速文件读取
//! - BlockCache: 组相联块缓存，加速磁盘块读取
//! - MountTable: 挂载表，支持最长前缀匹配的路径解析
//! - IoQueue: SCAN 电梯 I/O 调度器
//! - Disk: 块设备驱动（含故障注入和日志设备支持）
//! - KObjRegistry: 内核对象注册表，追踪和回收内核对象

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::fmt;
use std::cmp::min;
use std::thread;
use std::time::Duration;

use crate::consts::*;
use crate::sync::{Spin, SyncQueue, EvBus, EvFlag, EvCb, GKL};
use crate::channel::CircBuf;
use crate::util::CLK;

// ==================== configFS 伪文件系统 ==================== [BUG-20]
// configFS 是 Linux 风格的用户态驱动伪文件系统：
//   - 对象生命周期由用户态控制：mkdir 创建、rmdir 销毁，区别于 procfs/sysfs 的内核驱动。
//   - 属性内容由回调动态生成/接受，适合内核子系统的运行时配置（如内核模块参数、驱动配置）。
//   - 挂载于 /config，子系统注册后在其子目录下暴露对象类型。
//
// 目录树结构：
//   /config/
//     <subsystem>/          ← ConfigSubsystem（内核注册）
//       <item 或 group>/    ← ConfigChild（用户 mkdir 创建）
//         <attr>            ← ConfigAttr（read=show回调, write=store回调）

/// configFS 属性描述符
/// 一个属性对应 item 目录下的一个普通文件（如 /config/demo/counter0/value）。
/// - `mode`：权限位（0o644 可读写，0o444 只读）
/// - `show`：读取时调用，将 item 的内部状态格式化为字符串（类比 sysfs show）
/// - `store`：写入时调用，将字符串解析后更新 item 的内部状态（类比 sysfs store）
pub struct ConfigAttr {
    pub name: String,                                       // 属性文件名（如 "value"）
    pub mode: u16,                                          // 权限位（0o644 / 0o444）
    pub show: fn(&ConfigItem) -> String,                    // 读取回调：item → 字符串
    pub store: fn(&ConfigItem, &str) -> Result<(), &'static str>, // 写入回调：字符串 → item
}

/// configFS 项目类型：一类可创建对象的模板
/// 类比 Linux `config_item_type`：决定该组下可以 mkdir 什么类型的孩子，以及孩子有哪些属性。
/// - `can_make_item`：mkdir 时创建 ConfigItem（叶节点，有属性，无子对象）
/// - `can_make_group`：mkdir 时创建 ConfigGroup（可再嵌套子对象）
/// - `can_link`：是否允许符号链接（本实现暂不启用）
pub struct ConfigItemType {
    pub name: String,           // 类型名（如 "counter"）
    pub attrs: Vec<ConfigAttr>, // 该类型暴露的属性列表
    pub can_make_item: bool,    // 允许在此组下 mkdir 创建叶节点
    pub can_make_group: bool,   // 允许在此组下 mkdir 创建嵌套组
    pub can_link: bool,         // 允许在此组下建立符号链接（暂未实现）
}

/// configFS 配置项（叶节点）：对应用户 mkdir 创建的目录
/// 包含 item_type 定义的所有属性，数据存在 `data` 键值表中，由 show/store 回调读写。
pub struct ConfigItem {
    pub name: String,                           // item 目录名（如 "counter0"）
    pub item_type: Arc<ConfigItemType>,         // 所属类型（决定有哪些属性）
    pub data: Mutex<BTreeMap<String, String>>,  // 运行时数据：属性名 → 当前值
}

impl ConfigItem {
    /// 创建新 item，data 初始为空（属性由 show 回调提供默认值）
    pub fn new(name: &str, item_type: Arc<ConfigItemType>) -> Self {
        Self {
            name: name.to_string(),
            item_type,
            data: Mutex::new(BTreeMap::new()),
        }
    }
}

/// configFS 子节点：item（叶节点）或 group（中间节点）
/// mkdir 时根据 item_type.can_make_item / can_make_group 决定创建哪种类型。
#[derive(Clone)]
pub enum ConfigChild {
    Item(Arc<ConfigItem>),   // 叶节点：有属性文件，不能继续 mkdir
    Group(Arc<ConfigGroup>), // 中间节点：可以继续 mkdir 创建子对象
}

/// configFS 配置组（中间节点）：可以包含子 item / 子 group 的目录
/// 内部维护一把 Mutex 保护 children 表，支持并发 mkdir/rmdir。
pub struct ConfigGroup {
    pub item: ConfigItem,                               // 本组自身作为一个 item（有名称和类型）
    pub children: Mutex<BTreeMap<String, ConfigChild>>, // 子对象表：名称 → 子节点
}

impl ConfigGroup {
    /// 创建新的配置组，children 初始为空
    pub fn new(name: &str, item_type: Arc<ConfigItemType>) -> Self {
        Self {
            item: ConfigItem::new(name, item_type),
            children: Mutex::new(BTreeMap::new()),
        }
    }
}

/// configFS 子系统：内核模块向 /config 下注册的顶层目录
/// 一个子系统对应 /config/<name>，其 root group 决定了用户可以在其下创建什么对象。
pub struct ConfigSubsystem {
    pub name: String,           // 子系统名，挂载为 /config/<name>
    pub root: Arc<ConfigGroup>, // 根 group，mkdir 时从这里创建子对象
}

impl ConfigSubsystem {
    /// 创建新子系统，root group 使用 root_type 定义的类型
    pub fn new(name: &str, root_type: Arc<ConfigItemType>) -> Self {
        Self {
            name: name.to_string(),
            root: Arc::new(ConfigGroup::new(name, root_type)),
        }
    }
}

/// configFS 全局管理器：持有所有已注册的子系统
/// 挂载于 Kernel.configfs，由 proc_init() 初始化并注册子系统。
pub struct ConfigFS {
    pub subsystems: Mutex<BTreeMap<String, Arc<ConfigSubsystem>>>, // 子系统表：名称 → 子系统
}

impl ConfigFS {
    /// 创建空的 configFS 实例（无任何子系统）
    pub fn new() -> Self {
        Self { subsystems: Mutex::new(BTreeMap::new()) }
    }

    /// 注册一个内核子系统，使其出现在 /config/<name> 下
    pub fn register_subsystem(&self, subsys: ConfigSubsystem) {
        let mut s = self.subsystems.lock().unwrap();
        s.insert(subsys.name.clone(), Arc::new(subsys));
    }

    /// 解析路径，返回找到的属性、item 或 group 节点
    /// 路径格式：`subsys[/group...]item/attr` 或 `subsys[/group...]`
    ///
    /// 实现策略：按 '/' 切分路径，第一段定位子系统，
    /// 后续段沿 group.children 树往下走；遇到 item 后看下一段是否是已知属性。
    /// 借用规则：每次在持有 children 锁的同时 clone 出目标 Arc，出锁后再切换 current，
    /// 避免跨循环持有 MutexGuard 引起借用冲突。
    pub fn lookup(&self, path: &str) -> Result<ConfigLookup, &'static str> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() { return Err("enoent"); }
        let subsys_name = parts[0];
        // 在锁作用域内取出 Arc，出锁后再使用，避免 subsystems 锁跨越整个遍历
        let subsys = {
            let s = self.subsystems.lock().unwrap();
            s.get(subsys_name).cloned()
        };
        let subsys = subsys.ok_or("enoent")?;
        let mut current = subsys.root.clone(); // 从子系统根 group 开始遍历
        let mut i = 1;
        while i < parts.len() {
            let name = parts[i];
            // 在锁内 clone 出下一跳节点，立即出锁，避免借用冲突
            let next = {
                let children = current.children.lock().unwrap();
                match children.get(name) {
                    Some(ConfigChild::Group(g)) => Some(ConfigChild::Group(g.clone())),
                    Some(ConfigChild::Item(item)) => {
                        if i + 1 < parts.len() {
                            // item 后面还有一段，必须是属性名，且必须是最后一段
                            // 若 i+2 < parts.len()，说明属性名后还有多余路径段（如 .../attr/extra），属非法路径
                            if i + 2 < parts.len() {
                                return Err("enoent"); // 属性名后不允许再有子路径
                            }
                            let attr_name = parts[i + 1];
                            for attr in &item.item_type.attrs {
                                if attr.name == attr_name {
                                    return Ok(ConfigLookup::Attr(item.clone(), attr_name.to_string()));
                                }
                            }
                            return Err("enoent"); // 属性不存在
                        } else {
                            return Ok(ConfigLookup::Item(item.clone())); // 路径终止于 item
                        }
                    }
                    None => return Err("enoent"),
                }
            };
            match next {
                Some(ConfigChild::Group(g)) => { current = g; i += 1; }
                _ => return Err("enoent"),
            }
        }
        Ok(ConfigLookup::Group(current)) // 路径终止于 group
    }

    /// 在指定路径的 group 下创建新的子对象（用户态 mkdir 触发）
    /// 根据 item_type 决定创建 ConfigItem（叶）还是 ConfigGroup（中间节点）。
    pub fn mkdir(&self, path: &str, name: &str) -> Result<(), &'static str> {
        let group = self.resolve_group(path)?;
        let item_type = &group.item.item_type;
        if item_type.can_make_group {
            let new_group = Arc::new(ConfigGroup::new(name, item_type.clone()));
            group.children.lock().unwrap().insert(name.to_string(), ConfigChild::Group(new_group));
        } else if item_type.can_make_item {
            let new_item = Arc::new(ConfigItem::new(name, item_type.clone()));
            group.children.lock().unwrap().insert(name.to_string(), ConfigChild::Item(new_item));
        } else {
            return Err("eperm"); // 该 group 的类型不允许创建子对象
        }
        Ok(())
    }

    /// 在指定路径的 group 下删除子对象（用户态 rmdir 触发）
    /// 将 name 从 children map 中移除，Arc 引用计数减一。
    /// 若此时无其他持有者则立即释放；若有打开的 ConfigNode 持有同一 Arc，
    /// 则由 Arc 的引用计数自然管理，ConfigNode drop 时才真正释放——此处无需额外处理。
    pub fn rmdir(&self, path: &str, name: &str) -> Result<(), &'static str> {
        let group = self.resolve_group(path)?;
        let mut children = group.children.lock().unwrap();
        if children.remove(name).is_some() { Ok(()) } else { Err("enoent") }
    }

    /// 内部辅助：将路径解析为 group；若路径指向非 group 节点则返回 notdir
    fn resolve_group(&self, path: &str) -> Result<Arc<ConfigGroup>, &'static str> {
        match self.lookup(path)? {
            ConfigLookup::Group(g) => Ok(g),
            _ => Err("notdir"),
        }
    }
}

/// configFS 路径查找结果
/// SYS_OPEN 和内部 API 用此枚举区分路径终止于哪种节点。
#[derive(Clone)]
pub enum ConfigLookup {
    Group(Arc<ConfigGroup>),         // 路径终止于目录（group）
    Item(Arc<ConfigItem>),           // 路径终止于对象目录（item）
    Attr(Arc<ConfigItem>, String),   // 路径终止于属性文件（item + 属性名）
}

/// configFS 属性文件句柄：打开一个属性文件后产生的读写游标
/// 持有对 item 的 Arc 引用（rmdir 后 item 依然有效，直到此句柄关闭）。
/// `offset` 记录下一次 read 从字符串的哪个字节开始，实现分段读取。
#[derive(Clone)]
pub struct ConfigNode {
    pub item: Arc<ConfigItem>,  // 所属 item（跨 rmdir 存活，Arc 保证）
    pub attr_name: String,      // 对应的属性名
    pub offset: usize,          // 读取游标（write 后需调用方重置为 0 才能重读）
}

impl ConfigNode {
    /// 创建新的属性文件句柄，读取游标从 0 开始
    pub fn new(item: Arc<ConfigItem>, attr_name: &str) -> Self {
        Self { item, attr_name: attr_name.to_string(), offset: 0 }
    }

    /// 读取属性内容（从 offset 开始，最多填满 buf）
    /// 调用 show 回调生成当前值字符串，按 offset 切片写入 buf，并推进 offset。
    /// 返回 0 表示 EOF；offset > bytes.len() 属防御（两次 read 之间值被缩短），同样返回 0。
    /// 若要重新从头读，需重新 open（新建 ConfigNode），与 FHandle 语义一致。
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, &'static str> {
        for attr in &self.item.item_type.attrs {
            if attr.name == self.attr_name {
                let content = (attr.show)(&self.item);
                let bytes = content.as_bytes();
                if self.offset >= bytes.len() { return Ok(0); }
                let n = min(bytes.len() - self.offset, buf.len());
                buf[..n].copy_from_slice(&bytes[self.offset..self.offset + n]);
                self.offset += n;
                return Ok(n);
            }
        }
        Err("enoent")
    }

    /// 写入属性内容（将整个 buf 作为一次完整写入，不修改 offset）
    /// 先将字节流解析为 UTF-8 字符串，去除首尾空白后传入 store 回调。
    /// store 回调负责格式校验并更新 item.data。offset 不变，与 FHandle::write 语义一致。
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, &'static str> {
        let s = std::str::from_utf8(buf).map_err(|_| "utf8")?;
        for attr in &self.item.item_type.attrs {
            if attr.name == self.attr_name {
                (attr.store)(&self.item, s.trim())?;
                return Ok(buf.len());
            }
        }
        Err("enoent")
    }

    /// poll 状态：config 属性文件总是可读可写，不会阻塞
    pub fn poll(&self) -> (bool, bool, bool) { (true, true, false) }
}

// ==================== 文件描述符选项 ====================

/// 文件打开选项，对应 Linux open() 的 flags
#[derive(Clone, Copy)]
pub struct FdOpt {
    pub rd: bool,   // 可读标志 (O_RDONLY / O_RDWR)
    pub wr: bool,   // 可写标志 (O_WRONLY / O_RDWR)
    pub ap: bool,   // 追加模式 (O_APPEND): 每次写入自动追加到文件末尾
    pub nb: bool,   // 非阻塞模式 (O_NONBLOCK): 读写操作不阻塞
}
impl Default for FdOpt {
    /// 默认选项：只读、非追加、阻塞
    fn default() -> Self { Self { rd: true, wr: false, ap: false, nb: false } }
}

// ==================== 文件描述符状态 ====================

/// 文件描述符的可变状态（偏移量、选项、锁状态）
/// 用 Arc<RwLock<>> 包装后，dup 产生的多个 FHandle 共享同一份状态
struct FdState {
    off: u64,    // 当前文件偏移量（下一次读写的位置）
    opt: FdOpt,  // 文件打开选项
    flk: u8,     // 文件锁状态（保留字段）
}
impl FdState {
    /// 创建新的文件描述符状态，初始偏移量为 0
    fn create(opt: FdOpt) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(FdState { off: 0, opt, flk: 0 }))
    }
}

// ==================== 文件句柄 ====================

/// 文件句柄，代表一个打开的文件
/// data 和 desc 均使用 Arc，dup 后新旧句柄共享数据和偏移量（符合 POSIX 语义）
#[derive(Clone)]
pub struct FHandle {
    pub path: String,                     // 文件路径名
    pub data: Arc<Mutex<Vec<u8>>>,        // 文件数据（Arc 共享，clone 后指向同一份数据）
    desc: Arc<RwLock<FdState>>,           // 描述符状态（偏移量、选项，Arc 共享）
    pub pipe: bool,                       // 是否为管道文件
    pub cloexec: bool,                    // close-on-exec 标志（exec 时自动关闭）
}

/// 文件 seek 定位方式，对应 lseek() 的 whence 参数
#[derive(Debug)]
pub enum FSeek {
    Start(u64),  // SEEK_SET: 从文件开头定位
    End(i64),    // SEEK_END: 从文件末尾定位
    Cur(i64),    // SEEK_CUR: 从当前位置定位
}

impl FHandle {
    /// 创建空文件句柄
    pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self {
        Self {
            path: path.to_string(),
            data: Arc::new(Mutex::new(Vec::new())),
            desc: FdState::create(opt),
            pipe,
            cloexec,
        }
    }

    /// 用已有数据创建文件句柄（用于预加载文件内容）
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        Self {
            path: path.to_string(),
            data: Arc::new(Mutex::new(d)),
            desc: FdState::create(opt),
            pipe: false,
            cloexec: false,
        }
    }

    /// 复制文件句柄（共享 data 和 desc，可指定新的 cloexec 标志）
    /// dup 后新旧句柄共享偏移量，写入一端会反映到另一端
    pub fn dup(&self, cloexec: bool) -> Self {
        FHandle {
            path: self.path.clone(),
            data: self.data.clone(),
            desc: self.desc.clone(),
            pipe: self.pipe,
            cloexec,
        }
    }

    /// 设置文件选项（目前仅支持设置 O_NONBLOCK）
    pub fn set_opt(&self, arg: usize) {
        let mut d = self.desc.write().unwrap();
        d.opt.nb = (arg & O_NONBLOCK) != 0;
    }

    /// 获取当前文件选项
    pub fn get_opt(&self) -> FdOpt { self.desc.read().unwrap().opt }

    /// 从当前偏移量读取数据，读取后自动推进偏移量
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let off = self.desc.read().unwrap().off as usize;
        let len = self.read_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }

    /// 从指定偏移量读取数据（不修改 desc 中的偏移量）
    /// nb 模式下如果无数据则立即返回 0
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        // 检查是否有读权限
        if !self.desc.read().unwrap().opt.rd { return Err("ebadf"); }
        // 非阻塞模式：直接读取，无数据返回 0
        if self.desc.read().unwrap().opt.nb {
            let d = self.data.lock().unwrap();
            if off >= d.len() { return Ok(0); }
            let n = min(buf.len(), d.len() - off);
            buf[..n].copy_from_slice(&d[off..off + n]);
            return Ok(n);
        }
        // 阻塞模式（当前实现与 nb 相同，因为内核模拟不需要真正阻塞）
        let d = self.data.lock().unwrap();
        if off >= d.len() { return Ok(0); }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        Ok(n)
    }

    /// 写入数据，追加模式下自动写入到文件末尾
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        // 追加模式：偏移量设为文件末尾
        let off = {
            let d = self.desc.read().unwrap();
            if d.opt.ap { self.data.lock().unwrap().len() as u64 } else { d.off }
        } as usize;
        let len = self.write_at(off, buf)?;
        self.desc.write().unwrap().off += len as u64;
        Ok(len)
    }

    /// 从指定偏移量写入数据（自动扩展文件大小）
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        // 检查是否有写权限
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        // 如果写入位置超出文件末尾，自动扩展（填充 0）
        if off + buf.len() > d.len() { d.resize(off + buf.len(), 0); }
        d[off..off + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }

    /// 移动文件偏移量（seek）
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let mut d = self.desc.write().unwrap();
        d.off = match pos {
            FSeek::Start(o) => o,                                    // 从开头偏移
            FSeek::End(o) => (self.data.lock().unwrap().len() as i64 + o) as u64, // 从末尾偏移
            FSeek::Cur(o) => (d.off as i64 + o) as u64,              // 从当前位置偏移
        };
        Ok(d.off)
    }

    /// 统一传输接口：支持读/写、带偏移/不带偏移的组合
    /// dir & 1 != 0 表示读操作，否则为写操作
    pub fn transfer(&self, dir: u8, offset: Option<usize>, buf_rd: Option<&mut [u8]>, buf_wr: Option<&[u8]>) -> Result<usize, &'static str> {
        // 计算路径哈希（用于缓存/调试，当前未实际使用）
        let _path_hash = {
            let mut h: u64 = 0x811c9dc5;  // FNV-1a 初始值
            for b in self.path.bytes() { h ^= b as u64; h = h.wrapping_mul(0x01000193); }
            h
        };
        if dir & 1 != 0 {
            // 读操作
            match (offset, buf_rd) {
                (Some(off), Some(buf)) => self.read_at(off, buf),
                (None, Some(buf)) => self.read(buf),
                _ => Err("einval"),
            }
        } else {
            // 写操作
            match (offset, buf_wr) {
                (Some(off), Some(buf)) => self.write_at(off, buf),
                (None, Some(buf)) => self.write(buf),
                _ => Err("einval"),
            }
        }
    }

    /// 设置文件长度（截断或扩展）
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        self.data.lock().unwrap().resize(len as usize, 0);
        Ok(())
    }

    /// 同步文件数据到磁盘（当前为空实现，因为数据在内存中）
    pub fn sync_all(&self) -> Result<(), &'static str> { Ok(()) }

    /// 同步文件数据（不含元数据）到磁盘
    pub fn sync_data(&self) -> Result<(), &'static str> { Ok(()) }

    /// 获取文件大小（字节数）
    pub fn metadata_sz(&self) -> usize { self.data.lock().unwrap().len() }

    /// 目录查找（当前为空实现）
    pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> { Ok(()) }

    /// 读取目录条目（模拟实现：返回 "entry_N"）
    pub fn read_entry(&self) -> Result<String, &'static str> {
        let mut d = self.desc.write().unwrap();
        if !d.opt.rd { return Err("ebadf"); }
        let off = d.off;
        d.off += 1;
        Ok(format!("entry_{}", off))
    }

    /// 查询 poll 状态：返回 (可读, 可写, 错误)
    pub fn poll_status(&self) -> (bool, bool, bool) { (true, true, false) }

    /// ioctl 设备控制（当前为空实现）
    pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> { Ok(0) }

    /// 内存映射（当前为空实现）
    pub fn mmap(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> { Ok(()) }

    /// 获取底层数据的 Arc 引用（用于 inode 级别的数据共享）
    pub fn inode_ref(&self) -> Arc<Mutex<Vec<u8>>> { self.data.clone() }

    /// 预读建议：提示内核预加载指定范围的数据到缓存
    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        let d = self.data.lock().unwrap();
        let actual_end = min(offset + len, d.len());
        // 计算需要预读的页数（当前为空实现）
        let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
        Ok(())
    }

    /// 预分配文件空间（fallocate）：确保 offset+len 范围内的空间已分配
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if !self.desc.read().unwrap().opt.wr { return Err("ebadf"); }
        let mut d = self.data.lock().unwrap();
        let needed = offset + len;
        if needed > d.len() {
            d.resize(needed, 0);
        }
        Ok(())
    }

    /// splice 零拷贝传输：从 self 读取 count 字节并写入 dst
    /// 直接在内核空间搬运数据，不经过用户缓冲区
    pub fn splice_to(&self, dst: &FHandle, count: usize) -> Result<usize, &'static str> {
        let src_off = self.desc.read().unwrap().off;
        let sd = self.data.lock().unwrap();
        if src_off as usize >= sd.len() { return Ok(0); }
        let avail = sd.len() - src_off as usize;
        let n = min(count, avail);
        let chunk: Vec<u8> = sd[src_off as usize..src_off as usize + n].to_vec();
        drop(sd);  // 先释放源文件锁，避免死锁
        self.desc.write().unwrap().off += n as u64;
        dst.write(&chunk)
    }
}

impl fmt::Debug for FHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let d = self.desc.read().unwrap();
        f.debug_struct("FH").field("off", &d.off).field("path", &self.path).finish()
    }
}

// ==================== 管道 ====================

/// 管道方向：读端或写端
#[derive(Clone, PartialEq)]
pub enum PipeDir { Rd, Wr }

/// 管道内部缓冲区（读写两端共享）
pub struct PipeBuf {
    pub buf: VecDeque<u8>,   // 双端队列存储管道数据
    pub bus: EvBus,          // 事件总线（通知可读/关闭等事件）
    pub ends: i32,           // 存活端数（初始为 2，每 drop 一个 PipeNode -1）
}

/// 管道节点：持有共享缓冲区的一端（读端或写端）
#[derive(Clone)]
pub struct PipeNode {
    data: Arc<Mutex<PipeBuf>>,   // 共享缓冲区（Arc 引用计数）
    dir: PipeDir,                // 本端方向
}

/// Drop 时减少存活端数并触发 CLOSED 事件
/// 这确保了当一端关闭时，另一端能检测到并返回 EOF/EPIPE
impl Drop for PipeNode {
    fn drop(&mut self) {
        let mut d = self.data.lock().unwrap();
        d.ends -= 1;                    // 存活端数减一
        d.bus.set(EvFlag::CLOSED);      // 通知对端：本端已关闭
    }
}

impl PipeNode {
    /// 创建管道对：返回 (读端, 写端)，共享同一个 PipeBuf
    pub fn pair() -> (PipeNode, PipeNode) {
        let inner = PipeBuf { buf: VecDeque::new(), bus: EvBus::default(), ends: 2 };
        let d = Arc::new(Mutex::new(inner));
        (
            PipeNode { data: d.clone(), dir: PipeDir::Rd },
            PipeNode { data: d, dir: PipeDir::Wr },
        )
    }

    /// 检查读端是否可以读取（有数据或写端已关闭）
    pub fn can_read(&self) -> bool {
        if self.dir != PipeDir::Rd { return false; }
        let d = self.data.lock().unwrap();
        d.buf.len() > 0 || d.ends < 2
    }

    /// 检查写端是否可以写入（读端仍在）
    pub fn can_write(&self) -> bool {
        if self.dir != PipeDir::Wr { return false; }
        self.data.lock().unwrap().ends == 2
    }

    /// 从读端读取数据
    /// 空且两端都在时返回 "again"（EAGAIN，非阻塞模式下应重试）
    pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        if self.dir != PipeDir::Rd { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        // 缓冲区空且写端仍在：阻塞（返回 EAGAIN）
        if d.buf.is_empty() && d.ends == 2 { return Err("again"); }
        let n = min(buf.len(), d.buf.len());
        for i in 0..n { buf[i] = d.buf.pop_front().unwrap(); }
        // 读完后如果缓冲区空了，清除可读事件
        if d.buf.is_empty() { d.bus.clear(EvFlag::READABLE); }
        Ok(n)
    }

    /// 向写端写入数据，写入后触发 READABLE 事件通知读端
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if self.dir != PipeDir::Wr { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        for &c in buf { d.buf.push_back(c); }
        d.bus.set(EvFlag::READABLE);  // 设置可读事件，唤醒等待的读者
        Ok(buf.len())
    }

    /// 查询 poll 状态
    pub fn poll(&self) -> (bool, bool, bool) {
        (self.can_read(), self.can_write(), false)
    }
}

// ==================== 文件类统一抽象（VFS 分发层） ====================

/// FLike 是内核中的 VFS（虚拟文件系统）层
/// 每个文件描述符都存储为 FLike，读写时通过 match 分发到对应实现
/// [BUG-20] 新增 Config 分支以支持 configFS 属性文件
#[derive(Clone)]
pub enum FLike {
    File(FHandle),      // 普通文件
    Pipe(PipeNode),     // 管道（pipe() 创建）
    Ep(EpInst),         // epoll 实例（epoll_create() 创建）
    Config(ConfigNode), // configFS 属性文件
}

impl FLike {
    /// 复制文件描述符（共享底层数据，可指定新的 cloexec）
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
            FLike::Config(c) => {
                FLike::Config(c.clone())
            }
        }
    }

    /// 统一读取接口：根据文件类型分发到不同的读取实现
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        let _pre_tick = CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                // 普通文件读取：检查权限 → 获取偏移 → 复制数据 → 推进偏移
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
                // 管道读取：从双端队列中弹出数据
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
                // 读完后如果缓冲区空了，清除可读事件并触发回调
                if d.buf.is_empty() {
                    d.bus.clear(EvFlag::READABLE);
                }
                Ok(take)
            }
            FLike::Ep(_) => Err("enosys"),  // epoll 实例不可读
            FLike::Config(c) => {
                let mut node = c.clone();
                node.read(buf)
            }
        }
    }

    /// 统一写入接口：根据文件类型分发到不同的写入实现
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        match self {
            FLike::File(f) => {
                // 普通文件写入：检查权限 → 计算偏移 → 扩展文件 → 复制数据
                let (off, is_append) = {
                    let desc = f.desc.read().unwrap();
                    if !desc.opt.wr { return Err("ebadf"); }
                    let o = if desc.opt.ap {
                        f.data.lock().unwrap().len() as u64  // 追加模式：写入到末尾
                    } else {
                        desc.off
                    };
                    (o as usize, desc.opt.ap)
                };
                let mut d = f.data.lock().unwrap();
                let end = off + buf.len();
                // 如果写入范围超出文件大小，扩展文件
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
                // 管道写入：push 到双端队列并触发可读事件
                if p.dir != PipeDir::Wr { return Ok(0); }
                let mut d = p.data.lock().unwrap();
                let mut written = 0;
                for &c in buf {
                    d.buf.push_back(c);
                    written += 1;
                }
                if written > 0 {
                    d.bus.set(EvFlag::READABLE);
                }
                Ok(written)
            }
            FLike::Ep(_) => Err("enosys"),  // epoll 实例不可写
            FLike::Config(c) => {
                let mut node = c.clone();
                node.write(buf)
            }
        }
    }

    /// ioctl 分发：将设备控制命令转发到对应的底层实现
    /// ioctl(IO control)：设备 / 特殊资源的配置、查询、开关、控制命令，比如改非阻塞、获取终端尺寸、网卡配置、设置缓冲区大小、锁文件
    pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str> {
        match self {
            FLike::File(f) => {
                let _opt = f.desc.read().unwrap().opt;
                match req as u32 {
                    0..=0xFF => Ok(0),   // 低字节命令：通用返回 0
                    _ => f.io_ctl(req as u32, a1),
                }
            }
            FLike::Pipe(_) => {
                match req {
                    0x5421 => Ok(0),     // FIONBIO: 管道忽略非阻塞设置
                    _ => Err("enotty"),  // 管道不支持其他 ioctl
                }
            }
            FLike::Ep(_) => Err("enosys"),
            FLike::Config(_) => Err("enotty"),
        }
    }

    /// 内存映射：仅普通文件支持 mmap
    /// 把文件的一段内容，直接映射到进程虚拟地址空间里一块内存区间。程序像读写普通数组一样读写文件，不用 read/write；CPU 缺页时内核自动加载磁盘数据，修改后内核自动回写。
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
            _ => Err("enosys"),  // 管道、epoll 和 configFS 不支持 mmap
        }
    }

    /// poll 状态查询：返回 (可读, 可写, 错误)
    pub fn poll(&self) -> (bool, bool, bool) {
        match self {
            FLike::File(f) => {
                let desc = f.desc.read().unwrap();
                let readable = desc.opt.rd;
                let writable = desc.opt.wr;
                let _off = desc.off;
                drop(desc);
                // 错误条件：路径和数据均为空（无效文件）
                let error = f.path.is_empty() && f.data.lock().unwrap().is_empty();
                (readable, writable, error)
            }
            FLike::Pipe(p) => {
                let d = p.data.lock().unwrap();
                let has_data = !d.buf.is_empty();
                let closed = d.ends < 2;
                // 读端可读条件：有数据或写端已关闭
                let can_rd = (p.dir == PipeDir::Rd) && (has_data || closed);
                // 写端可写条件：读端仍在
                let can_wr = (p.dir == PipeDir::Wr) && !closed;
                // 错误条件：已关闭但有残留数据且本端是写端
                let err = closed && has_data && p.dir == PipeDir::Wr;
                (can_rd, can_wr, err)
            }
            FLike::Ep(e) => {
                let ready = e.ready.lock().unwrap();
                let has_ready = !ready.is_empty();
                // epoll 可读条件：就绪队列非空
                (has_ready, false, false)
            }
            FLike::Config(c) => c.poll(),
        }
    }
}

impl fmt::Debug for FLike {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FLike::File(h) => write!(f, "F({:?})", h),
            FLike::Pipe(_) => write!(f, "P"),
            FLike::Ep(_) => write!(f, "E"),
            FLike::Config(_) => write!(f, "C"),
        }
    }
}

// ==================== 伪文件系统节点 ====================

/// 伪文件系统节点（procfs 风格的只读伪文件）
/// 用于实现 /proc/self/status 等虚拟文件
pub struct PseudoNode {
    pub content: Vec<u8>,   // 文件内容（只读数据）
    pub ftype: u8,          // 文件类型标记
}
impl PseudoNode {
    /// 从字符串创建伪文件节点
    pub fn new(s: &str, ft: u8) -> Self { Self { content: s.as_bytes().to_vec(), ftype: ft } }

    /// 从指定偏移量读取数据
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> usize {
        if off >= self.content.len() { return 0; }
        let n = min(self.content.len() - off, buf.len());
        buf[..n].copy_from_slice(&self.content[off..off + n]);
        n
    }

    /// 写入（不支持，返回错误）
    pub fn write_at(&self, _off: usize, _buf: &[u8]) -> Result<usize, &'static str> { Err("nosup") }

    /// 获取文件大小
    pub fn metadata_sz(&self) -> usize { self.content.len() }
}

// ==================== 独立辅助函数 ====================

/// 将字节切片复制为 Vec（用于读取操作的便捷转换）
pub fn read_as_vec(data: &[u8]) -> Vec<u8> { data.to_vec() }

// ==================== Epoll 事件多路复性子系统 ====================

// IO 多路复用工具：一个进程可以同时监听成千上万个 fd（文件、管道、socket），哪个 fd 有可读 / 可写 / 异常事件，内核就主动通知进程，不用循环挨个轮询所有 fd，大幅节省 CPU。
/// epoll 用户数据（通常存放 fd 或自定义指针）
#[derive(Clone, Copy)]
pub struct EpData { pub ptr: u64 }

/// epoll 事件：事件类型位掩码 + 用户数据
#[derive(Clone)]
pub struct EpEvent { pub events: u32, pub data: EpData }
impl EpEvent {
    // 事件类型常量（对应 Linux epoll 的事件标志）
    pub const IN: u32 = 0x001;       // EPOLLIN: 可读
    pub const OUT: u32 = 0x004;      // EPOLLOUT: 可写
    pub const ERR: u32 = 0x008;      // EPOLLERR: 错误
    pub const HUP: u32 = 0x010;      // EPOLLHUP: 挂起
    pub const PRI: u32 = 0x002;      // EPOLLPRI: 紧急数据
    pub const RDNORM: u32 = 0x040;   // EPOLLRDNORM: 普通数据可读
    pub const RDBAND: u32 = 0x080;   // EPOLLRDBAND: 带外数据可读
    pub const WRNORM: u32 = 0x100;   // EPOLLWRNORM: 普通数据可写
    pub const WRBAND: u32 = 0x200;   // EPOLLWRBAND: 带外数据可写
    pub const MSG: u32 = 0x400;      // EPOLLMSG: 消息可用
    pub const RDHUP: u32 = 0x2000;   // EPOLLRDHUP: 对端关闭
    pub const EXCL: u32 = 1 << 28;   // EPOLLEXCLUSIVE: 排他唤醒
    pub const WAKEUP: u32 = 1 << 29; // EPOLLWAKEUP: 阻止系统休眠
    pub const ONESHOT: u32 = 1 << 30;// EPOLLONESHOT: 一次性监听
    pub const ET: u32 = 1 << 31;     // EPOLLET: 边缘触发

    /// 检查事件是否包含指定的标志位
    pub fn has(&self, ev: u32) -> bool { (self.events & ev) != 0 }
}

/// epoll 控制操作常量
pub struct EpCtlOp;
impl EpCtlOp {
    pub const ADD: i32 = 1;   // EPOLL_CTL_ADD: 添加 fd 到监听列表
    pub const DEL: i32 = 2;   // EPOLL_CTL_DEL: 从监听列表移除 fd
    pub const MOD: i32 = 3;   // EPOLL_CTL_MOD: 修改 fd 的监听事件
}

/// epoll 实例：管理一组被监听的 fd 及其事件
#[derive(Clone)]
pub struct EpInst {
    pub events: BTreeMap<usize, EpEvent>,    // fd -> 注册的事件映射
    pub ready: Arc<Mutex<BTreeSet<usize>>>,  // 就绪的 fd 集合（有事件发生的 fd）
    pub new_ctl: Arc<Mutex<BTreeSet<usize>>>,// 新注册的控制 fd（用于增量更新）
}
impl EpInst {
    /// 创建空的 epoll 实例
    pub fn new() -> Self {
        EpInst {
            events: BTreeMap::new(),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
            new_ctl: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    /// 控制操作：添加、删除、修改 fd 的监听事件
    pub fn control(&mut self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
        match op {
            1 => {
                // ADD: 插入事件映射并记录到 new_ctl
                self.events.insert(fd, ev.clone());
                self.new_ctl.lock().unwrap().insert(fd);
                Ok(())
            }
            3 => {
                // MOD: 更新已有映射（fd 必须已注册）
                if self.events.contains_key(&fd) {
                    self.events.insert(fd, ev.clone());
                    self.new_ctl.lock().unwrap().insert(fd);
                    Ok(())
                } else {
                    Err("eperm")
                }
            }
            2 => {
                // DEL: 移除事件映射
                if self.events.remove(&fd).is_some() { Ok(()) } else { Err("eperm") }
            }
            _ => Err("eperm"),
        }
    }
}

// ==================== 终端 I/O ====================

/// termios 结构体：终端属性配置
/// 对应 Linux 的 struct termios，通过 ioctl 的 TCGETS/TCSETS 命令操作
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrmIO {
    pub iflag: u32,    // 输入模式标志（如 IGNCR, ICRNL, IXON）
    pub oflag: u32,    // 输出模式标志（如 OPOST, ONLCR）
    pub cflag: u32,    // 控制模式标志（如 CS8, CREAD）
    pub lflag: u32,    // 本地模式标志（如 ECHO, ICANON, ISIG）
    pub line: u8,      // 行规程（通常为 0 = N_TTY）
    pub cc: [u8; 32],  // 控制字符数组（VINTR=3, VEOF=4, VERASE=127 等）
    pub ispeed: u32,   // 输入波特率
    pub ospeed: u32,   // 输出波特率
}
impl Default for TrmIO {
    /// 默认终端属性：启用 canonical 模式、echo、信号处理
    fn default() -> Self {
        TrmIO {
            iflag: 0o66402,
            oflag: 0o5,
            cflag: 0o2277,
            lflag: 0o105073,
            line: 0,
            // 控制字符默认值：VINTR=3(Ctrl-C), VEOF=4(Ctrl-D), VERASE=127(Backspace) 等
            cc: [3,28,127,21,4,0,1,0,17,19,26,255,18,15,23,22,255,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            ispeed: 0,
            ospeed: 0,
        }
    }
}

/// 窗口大小结构体：终端窗口尺寸
/// 通过 ioctl 的 TIOCGWINSZ/TIOCSWINSZ 命令操作
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WinSz {
    pub row: u16,      // 行数
    pub col: u16,      // 列数
    pub xpx: u16,      // 水平像素宽度
    pub ypx: u16,      // 垂直像素高度
}

// ==================== 页缓存（LRU 策略） ====================

/// 页缓存条目：缓存一个磁盘页的数据
pub struct PageCacheEntry {
    pub page_id: usize,       // 页标识（通常是块号或文件内偏移/页大小）
    pub data: Vec<u8>,        // 页数据
    pub dirty: bool,          // 脏页标志（被修改过，需要写回磁盘）
    pub access_tick: usize,   // 最后访问时间戳（用于 LRU 判定）
    pub pin_count: usize,     // 钉住计数（>0 时不可被驱逐，正在被使用）
}

/// 页缓存管理器：基于 LRU（最近最少使用）策略管理缓存页
pub struct PageCache {
    pub entries: HashMap<usize, PageCacheEntry>,  // 页表（page_id -> 条目）
    pub capacity: usize,                          // 最大缓存页数
    pub hits: AtomicUsize,                        // 缓存命中计数
    pub misses: AtomicUsize,                      // 缓存未命中计数
    pub evictions: AtomicUsize,                   // 驱逐计数,淘汰页面总次数
    pub lru_order: VecDeque<usize>,               // LRU 顺序（队头最旧，队尾最新）
}

impl PageCache {
    /// 创建指定容量的页缓存
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

    /// 查找页：命中则更新 LRU 顺序（移到队尾）和访问时间
    pub fn lookup(&mut self, page_id: usize) -> Option<&[u8]> {
        if self.entries.contains_key(&page_id) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            // 移到 LRU 队尾（标记为最近使用）
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

    /// 插入新页：缓存满时先驱逐 LRU 页
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

    /// LRU 驱逐：从队头开始找第一个未被钉住的页进行驱逐
    pub fn evict_lru(&mut self) -> bool {
        let mut victim = None;
        // 从 LRU 队头（最旧）开始扫描
        for &id in self.lru_order.iter() {
            if let Some(e) = self.entries.get(&id) {
                if e.pin_count == 0 {  // 未被钉住的页才能驱逐
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
            false  // 所有页都被钉住，无法驱逐
        }
    }

    /// 标记页为脏页（需要写回磁盘）
    pub fn mark_dirty(&mut self, page_id: usize) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    /// 写回所有脏页并清除脏标志，返回写回的页数（没有磁盘当然没有写写回）
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

    /// 获取缓存统计信息：(命中数, 未命中数, 驱逐数)
    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.evictions.load(Ordering::Relaxed),
        )
    }

    /// 钉住页（增加引用计数，防止被驱逐）
    pub fn pin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pin_count += 1;
            true
        } else {
            false
        }
    }

    /// 解除钉住（减少引用计数）
    pub fn unpin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            if e.pin_count > 0 { e.pin_count -= 1; }
            true
        } else {
            false
        }
    }

    /// 使指定页失效（从缓存中移除）
    pub fn invalidate(&mut self, page_id: usize) -> bool {
        if self.entries.remove(&page_id).is_some() {
            self.lru_order.retain(|&x| x != page_id);
            true
        } else {
            false
        }
    }

    /// 批量刷写指定页范围内的脏页，返回刷写的页数
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

// ==================== 内核对象注册表 ====================

/// 内核对象条目：追踪一个内核对象的元信息
/// 记录内核所有资源对象：管道、epoll、信号量、共享内存、打开文件、设备、线程等。
/// 给每一类内核资源分配全局唯一 ID；记录归属进程、父子层级、引用计数；支持按类型 / 进程快速检索资源；自动垃圾回收 GC：没人用（ref_count=0）的资源统一清理；导出对象依赖关系图，方便调试内核泄露。
pub struct KObjEntry {
    pub obj_id: usize,          // 对象 ID（全局唯一，递增分配）
    pub type_tag: u32,          // 类型标签（区分文件/管道/信号量等不同类型）
    pub owner_pid: usize,       // 所属进程的 PID
    pub created_tick: usize,    // 创建时的时间戳
    pub ref_count: usize,       // 引用计数（为 0 时可被 GC 回收）
    pub parent_id: Option<usize>, // 父对象 ID（用于构建对象层级关系树）
}

/// 内核对象注册表：全局追踪所有内核对象
/// 支持按类型索引、对象关系图导出、引用计数管理和 GC 回收
pub struct KObjRegistry {
    pub objects: Mutex<BTreeMap<usize, KObjEntry>>,   // ID -> 条目映射
    pub seq: AtomicUsize,                             // ID 序列号（原子递增分配）
    pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>, // 类型标签 -> ID 列表（按类型快速查找）
}

impl KObjRegistry {
    /// 创建空的注册表，ID 从 1 开始分配
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            type_index: Mutex::new(BTreeMap::new()),
        }
    }

    /// 注册新对象，返回分配的 ID
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
        // 更新类型索引
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    /// 注册子对象（带 parent_id，用于构建对象树）
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

    /// 注销对象：从注册表和类型索引中移除
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

    /// 按类型标签查找所有对象 ID
    pub fn find_by_type(&self, tag: u32) -> Vec<usize> {
        self.type_index.lock().unwrap().get(&tag).cloned().unwrap_or_default()
    }

    /// 导出对象关系图：返回所有 (parent_id, child_id) 边
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

    /// GC 扫描：回收所有引用计数为 0 的对象，返回回收数量
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

    /// 增加引用计数
    pub fn ref_up(&self, id: usize) -> bool {
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count += 1;
            true
        } else {
            false
        }
    }

    /// 减少引用计数（使用 saturating_sub 防止下溢）
    pub fn ref_down(&self, id: usize) -> bool {
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count = e.ref_count.saturating_sub(1);
            true
        } else {
            false
        }
    }

    /// 获取注册表中的对象总数
    pub fn count(&self) -> usize {
        self.objects.lock().unwrap().len()
    }

    /// 查找指定进程拥有的所有对象 ID
    pub fn owner_objects(&self, pid: usize) -> Vec<usize> {
        self.objects.lock().unwrap().iter()
            .filter(|(_, e)| e.owner_pid == pid)
            .map(|(id, _)| *id)
            .collect()
    }
}

// ==================== 块缓存（组相联结构） ====================

/// 缓存槽：存储一个磁盘块的数据
pub struct CacheSlot {
    pub id: usize,           // 块 ID（对应磁盘块号）
    pub payload: Vec<u8>,    // 块数据（通常 512 字节）
    pub modified: bool,      // 脏块标志（被修改过，需要写回磁盘）
}

/// 缓存链：一组缓存槽，用自旋锁保护并发访问
pub struct CacheChain {
    pub lk: Spin,                    // 自旋锁（轻量级，适合短时间持有）
    pub items: Mutex<Vec<CacheSlot>> // 槽位列表
}
impl CacheChain {
    pub fn new() -> Self { Self { lk: Spin::new(), items: Mutex::new(Vec::new()) } }
}

/// 块缓存：组相联结构，由多条缓存链组成
/// 块 ID 通过哈希映射到对应的链，减少锁竞争
pub struct BlockCache {
    pub chains: Vec<CacheChain>,  // 缓存链数组
    pub width: usize,             // 链数量（组数）
    pub ops: AtomicUsize,         // 操作计数器
}
impl BlockCache {
    /// 创建指定链数量的块缓存
    pub fn new(w: usize) -> Self {
        let mut c = Vec::with_capacity(w);
        for _ in 0..w { c.push(CacheChain::new()); }
        Self { chains: c, width: w, ops: AtomicUsize::new(0) }
    }

    /// 计算块 ID 对应的链索引（简单取模）
    pub fn idx(&self, k: usize) -> usize { k % self.width }

    /// 获取块数据：先查缓存，未命中则模拟磁盘读取并插入缓存
    pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>> {
        // 使用位混合哈希减少冲突
        let ci = {
            let raw = k;
            let mixed = raw ^ (raw >> 7);
            mixed % self.width
        };
        let ch = &self.chains[ci];
        // 获取链的自旋锁
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        // 在链中查找缓存命中
        let cached_data = {
            let e = ch.items.lock().unwrap();
            let mut found: Option<Vec<u8>> = None;
            for slot in e.iter() {
                if slot.id == k {
                    // 命中：克隆数据返回
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
        // 未命中：模拟磁盘延迟
        let tick_before = CLK.load(Ordering::Relaxed);
        if lat.as_nanos() > 0 { thread::sleep(lat); }
        // 生成确定性的块数据（基于块号和时间的伪随机）
        let block_data = {
            let mut payload = Vec::with_capacity(512);
            let seed = k.wrapping_mul(0x9E3779B9) ^ tick_before;
            for i in 0..512 {
                payload.push(((seed.wrapping_add(i)) & 0xFF) as u8);
            }
            payload
        };
        let result = block_data.clone();
        // 插入新缓存槽
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

    /// 同步所有脏块：遍历所有链，清除脏标志
    /// 需要获取全局内核锁 (GKL) 以确保同步操作的原子性
    pub fn sync_all(&self, id: usize) {
        // 获取全局内核锁（支持可重入）
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
        // 遍历所有缓存链
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
                core::hint::spin_loop();
            }
            {
                let mut items = ch.items.lock().unwrap();
                for slot in items.iter_mut() {
                    if slot.modified {
                        slot.modified = false;  // 清除脏标志（模拟写回磁盘）
                        synced += 1;
                    }
                }
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        // 释放全局内核锁
        GKL.holder.store(0, Ordering::Relaxed);
        GKL.depth.store(0, Ordering::Relaxed);
        GKL.flag.store(false, Ordering::Release);
    }

    /// 使指定块号的缓存条目失效
    pub fn invalidate(&self, k: usize) {
        let ci = k % self.width;
        let ch = &self.chains[ci];
        while ch.lk.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        {
            let mut items = ch.items.lock().unwrap();
            let mut idx = 0;
            // 移除所有匹配块号的条目
            while idx < items.len() {
                if items[idx].id == k { items.remove(idx); }
                else { idx += 1; }
            }
        }
        ch.lk.v.store(false, Ordering::Release);
    }

    /// 统计所有链中的缓存条目总数
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

    /// 统计所有链中的脏块数量
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

    /// 驱逐冷数据：移除超过 max_age 的脏块
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
                // 保留条件：未修改 或 年龄小于 max_age
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

// ==================== 挂载表 ====================
/// VFS 路径解析核心层，实现多文件系统挂载、最长前缀匹配，把用户字符串路径翻译成「设备 + 子路径」
/// 挂载条目：一个挂载点前缀到目标设备的映射
#[derive(Clone, Debug)]
pub struct MountEntry {
    pub prefix: String,   // 挂载点前缀（如 "/dev"、"/proc"）
    pub target: String,   // 目标设备或文件系统（如 "sda1"、"procfs"）
}

/// 挂载表：管理所有挂载点，支持最长前缀匹配
/// 条目按前缀长度降序排列，确保最长前缀优先匹配
pub struct MountTable {
    pub entries: RwLock<Vec<MountEntry>>,
}
impl MountTable {
    /// 创建空挂载表
    pub fn new() -> Self { Self { entries: RwLock::new(Vec::new()) } }

    /// 绑定挂载点（自动按前缀长度降序排序）
    pub fn bind(&self, pfx: &str, tgt: &str) {
        let mut e = self.entries.write().unwrap();
        let exists = e.iter().any(|m| m.prefix == pfx && m.target == tgt);
        if !exists {
            // 计算前缀哈希（用于调试/统计，当前未实际使用）
            let _hash = {
                let mut h: u64 = 0x100;
                for b in pfx.bytes() { h = h.wrapping_mul(31).wrapping_add(b as u64); }
                h
            };
            e.push(MountEntry { prefix: pfx.to_string(), target: tgt.to_string() });
            // 按前缀长度降序排序，使最长前缀排在前面
            e.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
        }
    }

    /// 路径解析：使用最长前缀匹配 + 递归解析
    /// 返回格式为 "device:sub_path" 的解析结果
    pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
        let tbl = self.entries.read().unwrap();
        let mut best_match_idx: Option<usize> = None;
        let mut best_prefix_len = 0;
        // 遍历所有挂载条目，找最长匹配前缀
        for (idx, m) in tbl.iter().enumerate() {
            if m.prefix.is_empty() { continue; }
            let plen = m.prefix.len();
            if plen > path.len() { continue; }
            // 逐字节比较前缀
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
                // 匹配到挂载点：递归解析剩余路径
                let m = &tbl[idx];
                let rest = &path[m.prefix.len()..];
                let dev = m.target.clone();
                let _depth_check = tbl.iter().filter(|e| !e.prefix.is_empty()).count();
                drop(tbl);
                let sub = self.resolve(rest)?;
                // 拼接结果: "device:sub_path"
                let mut result = String::with_capacity(dev.len() + 1 + sub.len());
                result.push_str(&dev);
                result.push(':');
                result.push_str(&sub);
                Ok(result)
            }
            None => {
                // 未匹配到挂载点：规范化路径（消除连续斜杠）
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

    /// 卸载指定前缀的所有挂载点
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
        e.len() < before  // 返回是否有挂载点被移除
    }

    /// 列出所有挂载点：返回 (前缀, 目标) 对列表
    pub fn list_mounts(&self) -> Vec<(String, String)> {
        let tbl = self.entries.read().unwrap();
        let mut result = Vec::with_capacity(tbl.len());
        for m in tbl.iter() {
            result.push((m.prefix.clone(), m.target.clone()));
        }
        result
    }

    /// 查找路径对应的最佳挂载点（最长前缀匹配）
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

    /// 获取挂载点数量
    pub fn mount_count(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// 检查指定前缀是否已挂载
    pub fn has_prefix(&self, pfx: &str) -> bool {
        self.entries.read().unwrap().iter().any(|m| {
            m.prefix.as_bytes() == pfx.as_bytes()
        })
    }
}

// ==================== I/O 调度队列（SCAN 电梯算法） ====================

/// I/O 请求：描述一次磁盘块读写操作
/// 磁盘机械寻道很慢，不能按提交顺序读写。
/// 电梯 SCAN 算法：磁头单向扫，到头掉头，优先处理同方向近的块；同时合并相邻 IO，大幅减少磁头来回移动耗时。
/// 属于文件系统横向底层子系统，BlockCache 缺页、刷脏块时提交 IO 到此队列。
pub struct IoRequest {
    pub block: usize,           // 目标块号
    pub write: bool,            // 是否为写操作
    pub priority: u8,           // 优先级（数值越小优先级越高）
    pub submitted_tick: usize,  // 提交时的时间戳
}

/// I/O 调度队列：使用 SCAN（电梯）算法调度磁盘 I/O
/// 磁头沿一个方向移动服务请求，到达端点后反转方向
pub struct IoQueue {
    pub pending: Mutex<VecDeque<IoRequest>>,  // 待处理请求队列
    pub head_pos: AtomicUsize,                // 磁头当前位置
    pub direction_up: AtomicBool,             // 扫描方向（true=向高块号移动）
    pub dispatched: AtomicUsize,              // 已调度的请求计数
    pub merged: AtomicUsize,                  // 已合并的请求计数
}

impl IoQueue {
    /// 创建空的 I/O 调度队列
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            head_pos: AtomicUsize::new(0),
            direction_up: AtomicBool::new(true),
            dispatched: AtomicUsize::new(0),
            merged: AtomicUsize::new(0),
        }
    }

    /// 提交单个 I/O 请求到队列
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

    /// 批量提交 I/O 请求；队列过深时自动触发相邻请求合并
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
        // 队列深度超过阈值时合并相邻请求
        let depth: i32 = q.len() as i32;
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
        count
    }

    /// 调度下一个请求：SCAN 算法选择距磁头最近且方向一致的请求
    pub fn dispatch(&self) -> Option<(usize, bool)> {
        let mut q = self.pending.lock().unwrap();
        if q.is_empty() { return None; }
        let head = self.head_pos.load(Ordering::Relaxed);
        let going_up = self.direction_up.load(Ordering::Relaxed);
        let mut best_idx = 0;
        let mut best_dist = usize::MAX;
        // 遍历队列，找到距磁头最近的请求
        for (i, req) in q.iter().enumerate() {
            let dist = if going_up {
                // 向上扫描：优先选择磁头前方的请求
                if req.block >= head { req.block - head } else { usize::MAX / 2 + req.block }
            } else {
                // 向下扫描：优先选择磁头后方的请求
                if req.block <= head { head - req.block } else { usize::MAX / 2 + head }
            };
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        let req = q.remove(best_idx)?;
        self.head_pos.store(req.block, Ordering::Relaxed);
        // 检查是否需要反转扫描方向
        if going_up && req.block >= head {
            if q.iter().all(|r| r.block < req.block) {
                self.direction_up.store(false, Ordering::Relaxed);  // 反转：向上 → 向下
            }
        } else if !going_up && req.block <= head {
            if q.iter().all(|r| r.block > req.block) {
                self.direction_up.store(true, Ordering::Relaxed);  // 反转：向下 → 向上
            }
        }
        self.dispatched.fetch_add(1, Ordering::Relaxed);
        Some((req.block, req.write))
    }

    /// 合并相邻块号的请求（block+1 且读写方向相同），减少磁盘寻道次数
    pub fn merge_adjacent(&self) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut merged = 0;
        let mut i = 0;
        while i + 1 < q.len() {
            // 相邻块号且同方向的请求可以合并
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

    /// 获取队列当前深度（待处理请求数）
    pub fn depth(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

// ==================== 磁盘 I/O（含故障注入和日志） ====================

/// 磁盘块设备：支持正常读写、故障注入和日志设备
pub struct Disk {
    pub errs: AtomicUsize,                    // 剩余错误次数（0=正常, usize::MAX=永久故障）
    pub ops: AtomicUsize,                     // 操作计数器
    pub label: String,                        // 设备标签/名称
    pub journal: Option<Arc<Disk>>,           // 日志设备（用于崩溃恢复）
}
impl Disk {
    /// 创建正常磁盘（无故障）
    pub fn new(s: &str) -> Self {
        Self { errs: AtomicUsize::new(0), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }

    /// 创建带故障注入的磁盘：前 n 次操作会失败
    pub fn failing(s: &str, n: usize) -> Self {
        Self { errs: AtomicUsize::new(n), ops: AtomicUsize::new(0), label: s.to_string(), journal: None }
    }

    /// 附加日志设备（用于写前日志，支持崩溃恢复）
    pub fn attach_journal(&mut self, d: Arc<Disk>) { self.journal = Some(d); }

    /// 设置剩余错误次数
    pub fn set_errs(&self, n: usize) { self.errs.store(n, Ordering::SeqCst); }

    /// 读块：循环重试直到成功
    /// errs == 0 时正常读取；errs > 0 时消耗一次错误后重试；errs == MAX 时永久失败
    pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str> {
        let sector = blk;
        let buf_len = out.len();
        loop {
            let op_id = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                // 正常：填充 0xAA 模式数据
                let mut i = 0;
                while i < buf_len { out[i] = 0xAA; i += 1; }
                return Ok(());
            }
            let persistent = rem == usize::MAX;  // 永久故障模式
            if !persistent {
                let prev = self.errs.fetch_sub(1, Ordering::SeqCst);
                let _remaining = if prev > 0 { prev - 1 } else { 0 };
            }
            // 故障时尝试从日志设备恢复
            match &self.journal {
                Some(jdev) => {
                    let mut scratch = [0u8; 8];
                    let _jr = jdev.read_block_n(sector, &mut scratch, 5);
                }
                None => {
                    let _backoff = op_id & 0x3;  // 退避策略
                }
            }
        }
    }

    /// 带重试次数限制的读块：最多尝试 lim 次
    pub fn read_block_n(&self, blk: usize, out: &mut [u8], lim: usize) -> Result<usize, &'static str> {
        let mut attempt = 0usize;
        let sector = blk;
        loop {
            attempt += 1;
            let _oid = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                // 正常读取：填充带索引异或的数据
                for (i, b) in out.iter_mut().enumerate() { *b = 0xAA ^ (i as u8); }
                return Ok(attempt);
            }
            if rem != usize::MAX { self.errs.fetch_sub(1, Ordering::SeqCst); }
            // 尝试从日志设备恢复
            if let Some(ref jd) = self.journal {
                let mut tb = [0u8; 8];
                let _ = jd.read_block_n(sector, &mut tb, lim.min(5));
            }
            if lim > 0 && attempt >= lim { return Err("limit"); }
        }
    }

    /// 获取总操作数
    pub fn total_ops(&self) -> usize { self.ops.load(Ordering::SeqCst) }

    /// 重置操作计数器
    pub fn reset_ops(&self) { self.ops.store(0, Ordering::SeqCst); }

    /// 写块：检查故障状态，有错误则返回 io_error
    pub fn write_block(&self, blk: usize, data: &[u8]) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        let rem = self.errs.load(Ordering::SeqCst);
        if rem != 0 {
            if rem != usize::MAX { self.errs.fetch_sub(1, Ordering::SeqCst); }
            return Err("io_error");
        }
        Ok(())
    }

    /// 刷新磁盘缓存（同步日志设备）
    pub fn flush(&self) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        // 如果有日志设备，也刷新日志设备
        if let Some(ref j) = self.journal {
            j.ops.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
