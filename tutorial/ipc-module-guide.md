# IPC 模块阅读指南

> 文件路径: `kernel-refactored/src/ipc.rs`
> 代码量: 185 行 | 6 个核心结构体 | 依赖: `consts`, `sync`

---

## 一、模块概述

`ipc.rs` 实现了内核中的 **System V IPC（进程间通信）** 机制，包含两个核心子系统：

| 子系统 | 结构体 | 用途 |
|---|---|---|
| SysV 信号量 | `IpcPerm`, `SemDs`, `SemArr`, `SemCtx` | 进程间同步与互斥 |
| 共享内存 | `ShmTag`, `ShmCtx`, `shm_get_or_create` | 进程间高速数据共享 |

**设计定位：** IPC 模块在内核中扮演 "进程间协作" 的角色——信号量用于进程同步（如生产者-消费者），共享内存用于进程间大数据交换。它类似于 Linux 的 `ipc/sem.c` 和 `ipc/shm.c`，但做了大幅简化。

**类型别名：**

```rust
type SemId = usize;   // 信号量数组 ID（进程内局部编号）
type SemNum = u16;    // 信号量编号（数组内索引）
type SemOp = i16;     // 信号量操作值（正数 = 释放，负数 = 获取）

type ShmId = usize;   // 共享内存段 ID
```

---

## 二、IpcPerm — SysV 权限结构

### 2.1 结构体定义

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    pub key: u32,    // IPC 键值（用户空间通过 key 查找/创建 IPC 对象）
    pub uid: u32,    // 当前所有者的用户 ID
    pub gid: u32,    // 当前所有者的组 ID
    pub cuid: u32,   // 创建者的用户 ID
    pub cgid: u32,   // 创建者的组 ID
    pub mode: u32,   // 访问权限（低 9 位，类似 Unix 文件权限 rwxrwxrwx）
    pub seq: u32,    // 序列号（用于区分同一 key 的不同实例）
    pub pad1: usize, // 填充字段（对齐到 C 结构体布局）
    pub pad2: usize, // 填充字段
}
```

**设计要点：**
- `#[repr(C)]` 确保与 C 语言的 `struct ipc_perm` 内存布局兼容
- `key` 是用户空间通过 `ftok()` 生成的标识符，内核用它在全局 store 中查找已有的 IPC 对象
- `mode` 的低 9 位编码权限（`0x1ff = 0o777`），与 Unix 文件权限一致
- `pad1`/`pad2` 用于结构体对齐，兼容 Linux 的 `ipc_perm` 布局

---

## 三、SemDs — 信号量描述符

### 3.1 结构体定义

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemDs {
    pub perm: IpcPerm,  // 权限信息（key、uid、mode 等）
    pub otime: usize,   // 最后一次 semop 操作的时间戳
    _p1: usize,         // 填充（对齐 C 结构体）
    pub ctime: usize,   // 最后一次修改时间戳
    _p2: usize,         // 填充
    pub nsems: usize,   // 该信号量数组中的信号量数量
}
```

**对应 Linux 的 `struct semid_ds`：**
- `otime`（operation time）：每次 semop 成功时更新
- `ctime`（change time）：信号量属性变更时更新
- `nsems`：信号量数组大小，创建后不可变

---

## 四、SemArr — 信号量数组

### 4.1 结构体定义

```rust
pub struct SemArr {
    pub ds: Mutex<SemDs>,     // 信号量描述符（受互斥锁保护）
    pub sems: Vec<Sema>,      // 信号量实例数组（每个元素是一个计数信号量）
}
```

**设计说明：**
- `SemArr` 对应 `semget()` 创建的"信号量集合"——一个 key 对应一组信号量
- `sems` 中的每个 `Sema` 来自 `sync.rs`，复用其计数信号量实现
- `ds` 用 `Mutex` 保护，因为描述符字段可能被并发修改（如 `set_ds`、`otime_now`）

### 4.2 Index trait 实现

```rust
/// 支持 arr[i] 语法直接访问第 i 个信号量
impl Index<usize> for SemArr {
    type Output = Sema;
    fn index(&self, i: usize) -> &Sema { &self.sems[i] }
}
```

### 4.3 方法

```rust
/// 销毁数组中所有信号量（标记为已移除，唤醒等待者）
pub fn remove(&self) {
    for s in &self.sems { s.remove(); }
}

/// 更新 otime 为当前时间（简化为 0 占位）
pub fn otime_now(&self) { self.ds.lock().unwrap().otime = 0; }

/// 更新 ctime 为当前时间
pub fn ctime_now(&self) { self.ds.lock().unwrap().ctime = 0; }

/// 更新描述符的权限字段（uid、gid、mode）
/// mode 被掩码 0x1ff 限制为低 9 位
pub fn set_ds(&self, new: &SemDs) {
    let mut l = self.ds.lock().unwrap();
    l.perm.uid = new.perm.uid;
    l.perm.gid = new.perm.gid;
    l.perm.mode = new.perm.mode & 0x1ff;  // 只保留权限位
}
```

### 4.4 工厂方法 — `get_or_create()`

这是 SemArr 最复杂的方法，实现了 `semget(key, nsems, flags)` 的核心逻辑：

```rust
pub fn get_or_create(
    key: u32,                                                  // IPC 键值
    nsems: usize,                                              // 信号量数量
    flags: usize,                                              // 创建标志
    store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,              // 全局存储（弱引用）
) -> Result<Arc<Self>, &'static str> {
    let mut m = store.write().unwrap();
    let mut k = key;

    // === 情况 1：key == 0，分配一个新的私有键 ===
    if k == 0 {
        k = (1u32..).find(|i| m.get(i).is_none()).unwrap();  // 找最小未使用键
    }
    // === 情况 2：key 已存在 ===
    else if let Some(w) = m.get(&k) {
        if let Some(a) = w.upgrade() {  // 弱引用升级为强引用
            // IPC_EXCL | IPC_CREAT → 已存在则报错
            if (flags & (1 << 9)) != 0 && (flags & (1 << 10)) != 0 {
                return Err("eexist");
            }
            return Ok(a);  // 返回已有的信号量数组
        }
    }

    // === 情况 3：创建新的信号量数组 ===
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

    // 用弱引用存储，避免全局 store 阻止 SemArr 被释放
    m.insert(k, Arc::downgrade(&arr));
    Ok(arr)
}
```

**Weak 引用设计：**

```
全局 store: BTreeMap<u32, Weak<SemArr>>
    │
    ├── key=1 → Weak → Arc<SemArr> (被进程 A 的 SemCtx 持有)
    │                    Arc<SemArr> (被进程 B 的 SemCtx 持有)
    │
    └── key=2 → Weak → (已失效，所有 Arc 均被 drop)
```

当所有进程都释放了对某个 `SemArr` 的引用（Arc 计数归零），`Weak::upgrade()` 返回 `None`，下次 `get_or_create` 时可以重新创建。

---

## 五、SemCtx — 进程信号量上下文

### 5.1 结构体定义

```rust
#[derive(Default)]
pub struct SemCtx {
    /// 该进程已关联的信号量数组映射：SemId → Arc<SemArr>
    pub arrays: BTreeMap<SemId, Arc<SemArr>>,
    /// 撤销操作表：(SemId, SemNum) → SemOp
    /// 记录进程对信号量的操作，进程退出时自动撤销
    pub undos: BTreeMap<(SemId, SemNum), SemOp>,
}
```

**设计要点：**
- 每个进程拥有一个 `SemCtx`，记录它关联的所有信号量
- `undos` 实现 `SEM_UNDO` 语义：进程异常退出时自动释放已获取的信号量，防止死锁

### 5.2 方法

```rust
/// 将信号量数组加入进程上下文，返回分配的局部 ID
pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
    let id = (0..).find(|i| !self.arrays.contains_key(i)).unwrap();  // 找最小空闲 ID
    self.arrays.insert(id, arr);
    id
}

/// 从进程上下文中移除信号量数组
pub fn remove(&mut self, id: SemId) { self.arrays.remove(&id); }

/// 查找最小空闲 ID（内部方法）
fn free_id(&self) -> SemId { (0..).find(|i| self.arrays.get(i).is_none()).unwrap() }

/// 根据 ID 获取信号量数组的强引用
pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> { self.arrays.get(&id).cloned() }

/// 记录一次信号量操作到撤销表
/// op > 0 表示释放（undo 为负），op < 0 表示获取（undo 为正）
pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) {
    let old = *self.undos.get(&(id, num)).unwrap_or(&0);
    self.undos.insert((id, num), old - op);  // 累加撤销操作
}
```

### 5.3 Drop 实现 — 进程退出时的自动撤销

```rust
impl Drop for SemCtx {
    fn drop(&mut self) {
        // 遍历撤销表，对每个信号量执行反向操作
        for (&(id, num), &op) in &self.undos {
            if let Some(arr) = self.arrays.get(&id) {
                match op {
                    1 => arr[num as usize].release(),  // 撤销"获取"→ 释放信号量
                    _ => {}  // 其他操作暂不处理
                }
            }
        }
    }
}
```

**撤销流程图：**

```
进程 A 执行 semop(sem_id, -1, SEM_UNDO)
    │
    ├── sems[0].acquire()      → cnt: 1 → 0
    ├── add_undo(id, 0, -1)    → undos[(id,0)] = 0 - (-1) = 1
    │
    ▼
进程 A 异常退出（SemCtx 被 drop）
    │
    ├── 遍历 undos: (id, 0) → op = 1
    ├── arr[0].release()       → cnt: 0 → 1（自动释放！）
    └── 其他等待该信号量的进程可以被唤醒
```

### 5.4 Clone 实现

```rust
/// fork 时子进程继承信号量上下文，但不继承撤销表
/// （子进程有独立的操作历史，不应撤销父进程的操作）
impl Clone for SemCtx {
    fn clone(&self) -> Self {
        SemCtx { arrays: self.arrays.clone(), undos: BTreeMap::new() }
    }
}
```

---

## 六、ShmTag — 共享内存标记

### 6.1 结构体定义

```rust
#[derive(Clone)]
pub struct ShmTag {
    pub addr: usize,                       // 映射到进程地址空间的虚拟地址
    pub pages: Arc<Mutex<Vec<usize>>>,     // 共享内存的页面数据（简化为 usize 数组）
}
```

**设计说明：**
- `pages` 使用 `Arc<Mutex<Vec<usize>>>` 表示——多个进程通过 Arc 共享同一块内存
- `addr` 记录该段共享内存在当前进程中的映射地址（不同进程可以映射到不同地址）
- 实际内核中 `pages` 应该是物理页面帧号的数组，这里简化为 `usize` 数组

### 6.2 方法

```rust
/// 设置映射地址（shmat 系统调用后更新）
pub fn set_addr(&mut self, a: usize) { self.addr = a; }
```

---

## 七、shm_get_or_create — 共享内存工厂函数

```rust
/// 获取或创建共享内存段
/// key: IPC 键值
/// npages: 页面数量
/// store: 全局共享内存存储（弱引用）
pub fn shm_get_or_create(
    key: usize,
    npages: usize,
    store: &RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
) -> Arc<Mutex<Vec<usize>>> {
    let mut m = store.write().unwrap();
    // 如果 key 已存在且弱引用有效，返回已有的共享内存
    if let Some(w) = m.get(&key) {
        if let Some(g) = w.upgrade() { return g; }
    }
    // 否则创建新的共享内存段
    let g = Arc::new(Mutex::new(vec![0usize; npages]));
    m.insert(key, Arc::downgrade(&g));
    g
}
```

**与 SemArr::get_or_create 的设计一致性：**
- 都使用 `Weak` 弱引用存储在全局 `BTreeMap` 中
- 都通过 `upgrade()` 判断已有对象是否仍然存活
- 当所有引用被释放后，下次调用可以重新创建

---

## 八、ShmCtx — 进程共享内存上下文

### 8.1 结构体定义

```rust
#[derive(Default)]
pub struct ShmCtx {
    /// 该进程已关联的共享内存段：ShmId → ShmTag
    pub ids: BTreeMap<ShmId, ShmTag>
}
```

### 8.2 方法

```rust
/// 将共享内存段加入进程上下文，返回分配的局部 ID
pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
    let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
    self.ids.insert(id, ShmTag { addr: 0, pages: g });
    id
}

/// 根据 ID 获取共享内存标记
pub fn get(&self, id: ShmId) -> Option<ShmTag> { self.ids.get(&id).cloned() }

/// 更新指定 ID 的标记（如修改映射地址）
pub fn set(&mut self, id: ShmId, tag: ShmTag) { self.ids.insert(id, tag); }

/// 根据映射地址反查 ShmId（shmdt 系统调用时使用）
pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
    self.ids.iter().find(|(_, v)| v.addr == addr).map(|(k, _)| *k)
}

/// 从进程上下文中移除共享内存段
pub fn pop(&mut self, id: ShmId) { self.ids.remove(&id); }
```

### 8.3 Clone 实现

```rust
/// fork 时子进程继承共享内存映射（与 SemCtx 不同，共享内存需要继承）
impl Clone for ShmCtx {
    fn clone(&self) -> Self { ShmCtx { ids: self.ids.clone() } }
}
```

---

## 九、使用场景

### 9.1 信号量 IPC 流程

```
进程 A (生产者)                           进程 B (消费者)
     │                                        │
     ▼                                        ▼
semget(key, 1, IPC_CREAT)               semget(key, 1, 0)
     │                                        │
     ▼                                        ▼
SemArr::get_or_create(key)              SemArr::get_or_create(key)
     │                                   → Weak::upgrade() 获取已有数组
     ▼                                        │
sems[0].release()  → cnt: 0→1           sems[0].acquire_spin()
(add_undo 记录撤销)                      → cnt: 1→0，继续执行
```

### 9.2 共享内存 IPC 流程

```
进程 A                                    进程 B
     │                                        │
     ▼                                        ▼
shm_get_or_create(key, 4)               shm_get_or_create(key, 4)
     │                                   → 返回同一块 Arc<Mutex<Vec>>
     ▼                                        ▼
ShmCtx::add(pages) → id=0               ShmCtx::add(pages) → id=0
     │                                        │
     ▼                                        ▼
pages.lock() → 写入数据                   pages.lock() → 读取数据
```

### 9.3 进程退出时的信号量撤销

```
进程退出 → SemCtx::drop()
    │
    ├── undos = {(0, 0): 1, (0, 1): 1}
    │
    ├── arr[0][0].release()  → 自动释放信号量 0
    └── arr[0][1].release()  → 自动释放信号量 1
```

---

## 十、跨模块连接

```
ipc.rs
├── sync::Sema
│   └── SemArr.sems 中每个元素都是 sync.rs 的 Sema
│       SemCtx::drop() 中调用 Sema::release() 实现撤销
│       SemArr::remove() 中调用 Sema::remove() 销毁信号量
│
├── consts::*
│   └── 使用常量定义（如 IPC_CREAT 等标志位）
│
├── 被 kernel.rs 调用
│   └── semget/semop/semctl 系统调用的底层实现
│       shmget/shmat/shmdt 系统调用的底层实现
│
└── Weak 引用模式
    └── 与 sched.rs 无直接关系，但共享相同的全局 store 模式
```

---

## 十一、与原版 kernel.rs 的对应

| ipc.rs 内容 | 原版 kernel.rs 位置 |
|---|---|
| `IpcPerm` | 约第 2600-2620 行 |
| `SemDs` | 约第 2620-2640 行 |
| `SemArr` + `get_or_create` | 约第 2640-2720 行 |
| `SemCtx` + Drop | 约第 2720-2790 行 |
| `ShmTag` / `ShmCtx` | 约第 2790-2850 行 |
| `shm_get_or_create` | 约第 2850-2870 行 |

---

## 十二、潜在的改进方向

1. **SemCtx::drop() 只处理 op == 1 的撤销**：当前只对值为 1 的 undo 操作执行 `release()`，其他值（如批量获取的 undo）被忽略。应改为通用逻辑 `if op > 0 { for _ in 0..op { arr[n].release(); } }`
2. **SemArr::get_or_create 的 key=0 处理**：`key=0` 在 SysV IPC 中表示"私有"信号量，应返回一个新的不冲突的 key，当前实现从 1 开始搜索，可能与用户显式创建的 key 冲突
3. **共享内存缺少权限检查**：`shm_get_or_create` 不检查 `mode` 权限，任何进程都可以读写任意共享内存段
4. **SemDs 时间戳硬编码为 0**：`otime_now()` 和 `ctime_now()` 将时间设为 0 而非真实时间戳
5. **缺少 IPC_RMID 的完整实现**：`SemArr::remove()` 只标记信号量为移除，但没有从全局 store 中清理对应的 Weak 条目
6. **Fork 时 SemCtx 的 undos 不继承**：虽然设计上子进程不应撤销父进程的操作，但 POSIX 规范中 fork 后子进程确实不应继承 SEM_UNDO 调整——这一点当前实现是正确的
