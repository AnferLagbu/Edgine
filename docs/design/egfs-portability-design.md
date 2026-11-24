# EGFS 可移植性与快照控制面设计

> 本文回答 **EGFS 要演化成什么**：如何从 Edgine 内部文件系统变成可脱离 Edgine 单独存在、
> 不依赖 EG 特有功能、同时保持可靠的文件系统；快照为什么不再走 `EG_SNAPSHOT_*` 私有
> syscall；盘上格式、控制面、崩溃恢复和平台适配各自负责什么。
>
> 本文不是现状说明，也不是任务清单。现状证据见正文表格；可执行分解见
> [egfs-portability-project.md](../plan/egfs-portability-project.md)；私有 ABI 退役口径见
> [private-abi-retirement.md](../plan/private-abi-retirement.md)。

---

## 一、结论与边界

### 1.1 目标

1. **可移植**：同一份 EGFS 盘镜像能在 Edgine 和 Linux 上挂载、读写、快照、回滚。
2. **可脱离 Edgine**：核心不引用 Edgine 内核类型、驱动、SGEG、FREG；OS 适配器是薄层。
3. **不依赖 EG 特有功能**：权限用 POSIX uid/gid/mode/xattr，不用 PWM/域标签作为正确性前提。
4. **好用可靠**：快照语义清楚，崩溃后可恢复，能离线 `fsck`，能在线 `scrub`，有跨 OS 测试。
5. **控制面可移植**：快照是 EGFS 的文件系统操作，不是 Edgine syscall；外部 Linux 程序零私有号依赖。

### 1.2 非目标

- 不在第一波重写 SPA/DMU/TXG/ZIL/ARC/metaslab；它们是现有资产，先做接口解耦，不做推倒重来。
- 不在第一波做 ZFS 级 pool/RAID/加密/远程复制；RAIDZ、压缩、去重保留为可选能力，不作为可移植性门槛。
- 不把 FUSE 当最终性能形态；FUSE 是跨 OS 验证腿，内核模块是后续性能选项。
- 不承诺第三方稳定 EG 私有 ABI；私有号只服务 EG 原生工具。

### 1.3 关键决策

| 编号 | 决策 | 理由 |
|---|---|---|
| D1 | 核心与 OS 适配分离，核心只依赖 `core` + `alloc` + 纯逻辑依赖 | ZFS 的 SPA/DMU/ZPL + SPL 分层证明这条路可行；核心一旦引用 `privileged::driver::block` 就无法在 Linux 编译 |
| D2 | 快照控制面走**目录 fd 上的 `ioctl`**，不新增/保留私有 syscall | Btrfs 用 `BTRFS_IOC_SNAP_CREATE_V2` 在父目录 fd 上操作；`ioctl` 在 Edgine 与 Linux 都能表达 FS 私有操作，且不占 syscall 编号 |
| D3 | 快照表必须持久化，和数据集元数据在同一 txg 提交 | 当前 `EgfsSnapshotManager` 只在内存，重启即丢；快照是文件系统对象，不是会话状态 |
| D4 | ZIL 必须真正写盘并在挂载时重放 | `zil_persist.rs` 已有磁盘布局但内核侧零调用者；不重放就没有崩溃一致性 |
| D5 | Linux 适配器先做 FUSE，内核模块后置 | FUSE 成本最低，能最快证明“同一镜像、同一核心、跨 OS”；性能阶段再换适配器 |
| D6 | SGEG / FREG 只做可选适配，不做正确性依赖 | 可移植性要求 EGFS 在 Linux 上独立 mount；FREG 可增强 Edgine 侧恢复，但不能是恢复前提 |
| D7 | v1 盘只读挂载 + 离线升级到 v2，不静默改写 | 现有 `HVM1` 无版本/特性位；直接原地改格式会带来不可逆的数据风险 |

---

## 二、现状与缺口

| 缺口 | 现状证据 | 对可移植性的影响 |
|---|---|---|
| 全局单例 | `egfs_data.rs` 的 `static EGFS_DATA: OnceCell<EgfsData>`，`get_egfs()` 是全局入口；`init()` 扫描所有块设备并自动挂载/格式化 | 核心不能多实例，也不能在 Linux 用户态以“打开一个镜像”的方式运行 |
| 块设备硬耦合 | `spa.rs`、`vdev.rs` 直接 `use crate::privileged::driver::block`，调用 `hdd_read_sector` / `hdd_write_sector` / `hdd_is_present` | 核心离开 Edgine 就编译不过；FUSE/host 测试无法注入文件后端 |
| 快照只在内存 | `snapshot.rs` 的 `snapshots: Mutex<Vec<EgfsSnapshot>>`；`egfs_data.rs` 的 `snap_mgr` 没有序列化/反序列化路径 | 重启后快照全丢，无法作为持久文件系统能力 |
| 文件块索引未实现 | `EgfsDmuObject.bp` 只有单个根；`EgfsBlockPointer.level` 定义但未被遍历；`write()` 每次 `cow_bp(new_bp)` 直接替换根 | 多块文件会丢前一块；当前只能算“单块文件 + 大 size 字段”，不是可用文件系统 |
| 元数据格式缺版本/特性位 | `encode_metadata` 的 `HVM1` 只有 4 字节 magic + 计数 + `next_obj_id`；没有 format version、feature flags、endianness 标记 | 无法安全演进，也无法让 Linux 适配器判断兼容性 |
| ZIL 未接线 | `zil_persist.rs` 有 block/record 布局与 CRC，但 `rg zil_persist` 在内核侧零调用者；`mount_drive` 只 `zil.init()`，不重放 | 掉电后可能停在半写状态；`fsck` 也没有可依赖的 WAL |
| xattr 不可逆 | `sgeg-design.md` 已记录：`setxattr` 只接受 4 个名字槽位，值压成 `u64` 哈希存 `data_hash`，`getxattr` 返回哈希 | 不符合可移植 POSIX xattr 语义；Linux 侧无法互操作 |
| 控制面是私有 syscall | `EG_SNAPSHOT_*` 895–898 走 `types.rs` / dispatch / 用户库 | 外部 Linux 程序无法使用；EGFS 离开 Edgine 就没有快照入口 |
| 热插拔/FREG 耦合 | `egfs.rs` 热插拔监听直接 `get_egfs()`；`egfs_data.rs` `init()` 注册 FREG recovery domain | 这些应留在 Edgine 适配器；核心不应知道热插拔或 FREG |

结论：**当前 EGFS 是“ZFS 风格内核文件系统”，不是“可移植文件系统实现”。** 好消息是 SPA/DMU/TXG/ZIL/ARC/metaslab/ZAP 这些核心件已经存在，问题主要在边界、持久化和控制面，不需要从零重写。

---

## 三、目标架构

```text
┌──────────────────────────────────────────────────────────────┐
│ egfs-tools        mkfs / fsck / scrub / snapshot CLI          │
├──────────────────────────────────────────────────────────────┤
│ egfs-uapi         ioctl 结构、错误码、版本号、特性位          │
├──────────────────────────────────────────────────────────────┤
│ egfs-core         SPA / DMU / TXG / ZIL / ARC / metaslab / ZAP│
│                   snapshot / checksum / compress / dedup      │
│                   no_std + alloc，不引用任何 OS 内核接口      │
├───────────────┬───────────────────────┬──────────────────────┤
│ Edgine adapter│ Linux FUSE adapter    │ host test adapter    │
│ VFS / hotplug │ libfuse / /dev/fuse   │ mem/file vdev        │
│ block driver  │ 用户态 block backend  │ std harness          │
│ SGEG/FREG opt │ 无 SGEG/FREG           │ 无 OS 依赖           │
└───────────────┴───────────────────────┴──────────────────────┘
```

### 3.1 核心边界

核心对外暴露实例化 API，不暴露全局单例：

```rust
pub trait EgfsBlockDevice: Send + Sync {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<(), EgfsIoError>;
    fn write(&self, offset: u64, buf: &[u8]) -> Result<(), EgfsIoError>;
    fn flush(&self) -> Result<(), EgfsIoError>;
    fn size_bytes(&self) -> u64;
}

pub struct Egfs { /* SPA、数据集、ZIL、快照表、配置 */ }

impl Egfs {
    pub fn open(dev: Arc<dyn EgfsBlockDevice>, opts: MountOptions) -> Result<Self, EgfsError>;
    pub fn format(dev: Arc<dyn EgfsBlockDevice>, opts: FormatOptions) -> Result<Self, EgfsError>;
    pub fn snapshot_create(&self, ds: DatasetId, name: &[u8]) -> Result<SnapshotId, EgfsError>;
    pub fn snapshot_list(&self, ds: DatasetId) -> Result<Vec<SnapshotInfo>, EgfsError>;
    // ...
}
```

- 核心不接收 `EgfsData` 全局引用，不调用 `get_egfs()`。
- 核心不接收 `drive: u8`、不调用 `hdd_*`；块设备由 `EgfsBlockDevice` 注入。
- 核心不调用 `pwm_has_capability`、`freg::*`、`hotplug::*`；这些在适配器。
- 错误类型是 OS 无关的 `EgfsError`；适配器映射到 Edgine `KernelError` 或 Linux `errno`。

### 3.2 Edgine 适配器

- 保留 `get_egfs()` 作为**适配器 API**，内部持有 `OnceCell<EgfsService>`，`EgfsService` 再持有 `Egfs` 实例。
- 块设备适配器把 `drive + partition_start` 包成 `EgfsBlockDevice`。
- 热插拔监听留在适配器：`hotplug_add_disk` 调 `EgfsService`，不直接调核心全局。
- FREG recovery domain 留在适配器；FREG 不可用时 EGFS 仍能正常 mount。
- SGEG 只通过 VFS 权限/标签适配器参与；核心不知道 PWM/域标签。

### 3.3 Linux FUSE 适配器

- 第一阶段：用户态 `egfs-fuse` 进程，`libfuse` 低层 API + 文件/loop 块后端。
- 读路径：`lookup/getattr/readdir/read/open` 映射到核心 inode/目录 API。
- 写路径：`write/create/unlink/rename/setattr` 映射到核心事务 API。
- 控制面：`FUSE_IOCTL` 转发 `EGFS_IOC_SNAPSHOT_*`；若运行环境的 FUSE 实现不支持 ioctl，则 CLI 通过本地控制 socket 调 daemon。
- 不要求第一阶段支持 mmap/直接 I/O/复杂锁；先保证 mount、读写、快照、xattr 正确。

### 3.4 host 测试适配器

- 内存后端 `MemVdev`：单元测试、格式/元数据/快照 roundtrip。
- 文件后端 `FileVdev`：可生成盘镜像，做掉电/部分写注入、跨镜像测试。
- 目标是让 `egfs-core` 在普通 `cargo test` 下跑，不依赖 QEMU。

---

## 四、盘上格式 v2

### 4.1 版本与特性位

- v1 = 现有 `HVM1` 元数据 + 现有 uberblock；**只读挂载**，不原地改写。
- v2 = 新 magic `HVM2` / `EGFS`，带：
  - `format_version: u16`
  - `endianness: u32`（固定 little-endian，写入标记用于诊断）
  - `feature_compat: u64`
  - `feature_ro_compat: u64`
  - `feature_incompat: u64`
  - `checksum_type: u16`
  - `header_checksum: u32`
- 挂载规则：未知 `incompat` → 拒绝；未知 `ro_compat` → 只读；未知 `compat` → 忽略。
- 升级工具 `egfs-upgrade` 离线复制 v1 → v2；升级前要求 `fsck` 通过，升级后保留 v1 镜像备份。

### 4.2 Uberblock

- 保留 128 槽环形；每槽 512 字节以内。
- v2 字段：`magic`、`version`、`txg`、`root_bp`、`snap_table_bp`、`zil_head_bp`、`pool_guid`、`timestamp`、`feature_*`、`checksum`。
- 挂载扫描全部槽位，取“magic/version/checksum 均有效且 txg 最大”的槽；不取“最后一个写入”，防止 torn write。
- 写入顺序：先写数据/元数据/ZIL，flush，再写 uberblock，flush；uberblock 是唯一提交点。

### 4.3 数据集元数据

- 保留现有 object record（222 字节）与目录条目编码，但 v2 头部增加：
  - `snapshot_table_bp`
  - `xattr_table_bp`
  - `feature_*`
  - `format_version`
- 对象/目录计数上限继续保留；解析时先校验长度与计数，再遍历。
- xattr 改为**真实值存储**：名字最大 255 字节，值最大 64 KiB，独立 xattr record/对象；不再把值压成 `u64` 哈希。`getxattr` 必须可逆。

### 4.4 快照表

快照表是数据集级持久对象，不塞进目录 ZAP（现有 ZAP 名字上限 64 字节、值上限 128 字节，不适合 127 字节快照名和多字段记录）。

| 字段 | 类型 | 说明 |
|---|---|---|
| `snap_id` | u64 | 数据集内单调递增，永不复用 |
| `name` | [u8;128] | NUL 结尾，最长 127 字节；数据集内唯一 |
| `ds_id` | u64 | 所属数据集 |
| `root_bp` | EgfsBlockPointer | 快照根，创建后不可变 |
| `birth_txg` | u64 | 创建时事务组 |
| `used_space` | u64 | 创建时统计，允许近似 |
| `ref_count` | u32 | 克隆/打开引用 |
| `origin_snap` | u64 | 克隆来源；0 表示非克隆 |
| `flags` | u32 | READONLY / DELETING / CLONE |
| `state` | u8 | LIVE / DELETING |
| `checksum` | u32 | 记录校验 |

表头另存 `next_snap_id` 与 `entry_count`。快照表与数据集元数据在同一 txg 提交；挂载时先读表，再校验每条记录的 `root_bp`。

### 4.5 ZIL

- 复用 `zil_persist.rs` 的 4 KiB block：64B header + 256B record + trailer；block CRC 覆盖 record 区。
- v2 记录增加 `seq` 与 `txg` 的显式排序；记录类型保持现有 1..=13。
- `zil_head_bp` 指向最近一个有效 block；block 内 `next_block` 形成链；回放从 head 向后扫描，遇到 magic/CRC 失败即停。
- 只回放 `txg > uberblock.txg` 的记录；`txg <= uberblock.txg` 的已进入根元数据，跳过。
- 回放必须幂等：记录描述“结果状态”，不是“操作过程”；重复回放同一记录不产生第二次副作用。ext4 fast commit 的幂等性原则可借鉴。

### 4.6 索引结构

**结论：完工后的 EGFS 应该是索引式结构，但不是“所有层都用同一棵 B-tree”。**
这里说的“索引式”指：元数据通过键定位，不靠线性扫描、单链表或单根块指针。采用 ZFS 式混合索引：

| 索引 | 键 → 值 | 目标结构 | 现状 |
|---|---|---|---|
| 对象索引 | `obj_id` → dnode | 稀疏对象表 / radix tree / 数组 + 空闲链 | `EgfsObjSet.objects: Mutex<Vec<EgfsDmuObject>>`，按 `obj_id` 线性查找 |
| 目录索引 | 名字 → `obj_id` | 可增长的 ZAP 式哈希索引，支持变长名字与冲突链 | `EgfsZap` 固定 256 项、名字 64B、值 128B；超限截断/拒绝 |
| 文件块索引 | 逻辑块号 → `EgfsBlockPointer` | 多级间接块树，复用 `EgfsBlockPointer.level` | 只有 `obj.bp` 单根；`level` 字段存在但未遍历；写入直接替换根指针，多块文件实际不成立 |
| 快照索引 | `snap_id` / 名字 → 快照记录 | 持久快照表（独立 ZAP/记录表） | 只有内存 `Vec`，无持久化 |
| xattr 索引 | `(obj_id, name)` → value | 独立 xattr 表，变长值 | 值压成 `u64` 哈希，不可逆 |
| 空闲空间索引 | 位置 → extent/metaslab | metaslab + 空闲树/位图 | 已有 metaslab 骨架 |

具体选择：

1. **对象索引**：优先做稀疏对象表。对象号不一定连续，不能用无界 `Vec` 下标当唯一索引；至少要有 `obj_id → dnode` 的哈希/radix 查找，并保留 `next_obj_id` 单调分配。第一波可以先做内存 radix + 盘上对象表，B-tree 留到稀疏度真正成为瓶颈时。
2. **目录索引**：使用 **ZAP 式哈希索引**，不是全 B-tree。ZFS 的目录就是 ZAP，现有 `EgfsZap` 已有哈希与冲突槽位，只需把“固定 256 项 / 64B 名字 / 128B 值”改成可增长、变长名字、可落盘。目录不要求有序遍历，`readdir` 顺序不承诺；若要 `seekdir` 稳定顺序，再评估 B-tree。
3. **文件块索引**：使用 **多级间接块树**，这是当前 `EgfsBlockPointer.level` 的正确用法：
   - `level = 0` 是数据块；`level > 0` 是间接元数据块，内容为 `EgfsBlockPointer` 数组。
   - 逻辑块号从根按 fanout 逐级下降；空洞用 null BP 表示；写时分配新块并 COW 整条路径，最后更新 dnode 根。
   - fanout = `block_size / sizeof(EgfsBlockPointer)`；按 4 KiB 块估算可支撑多级、大文件；具体上限在实现时用常量断言钉住。
   - 这条索引是“文件可大于一个块”的前提；当前 `write()` 每次替换 `obj.bp`，会把前一块丢掉，必须在 E1.5 修掉。
4. **快照/xattr 索引**：用独立记录表/ZAP，不塞进目录 ZAP；快照名 127B、xattr 值 64KiB，现有 ZAP 槽位不够。
5. **空闲空间**：继续用 metaslab；不在第一波换成全局 B-tree。

为什么不是“全 B-tree”：

- ZFS 的 object set + ZAP + 间接块树本身就是索引式，且与现有 SPA/DMU/BP 结构同源；改造成本低。
- 全盘统一 COW B-tree 是另一个大工程（分裂/合并、锁、崩溃一致性都要重做）；在 EGFS 当前阶段属于范围膨胀。
- 只有当“目录需要稳定有序遍历”或“对象/快照规模超过 ZAP 设计上限”时，才把对应索引升级为 B-tree；不要为了概念统一先造一棵通用 B-tree。

当前必须承认：**代码还不是索引式。** `Vec` 对象表、固定 ZAP、单 `bp` 文件，这三件事决定了它现在只能算“ZFS 风格外壳”；E1.5 不补，E2 的 v2 元数据和快照表都会建在错误的地基上。

---

## 五、快照语义

| 操作 | 语义 | 原子性/失败 |
|---|---|---|
| `create` | 冻结数据集当前根，写一条新快照记录；返回 `snap_id` | 与元数据同 txg；`ENOSPC` 不留下半条记录 |
| `list` | 返回 id/name/txg/used/readonly/origin，不暴露内部块指针 | 读快照表；表损坏返回 `EIO` |
| `destroy` | 标记 `DELETING`；`ref_count>1` 返回 `EBUSY`；引用归零后释放块 | 两阶段：先摘表项，后异步回收；崩溃后 `fsck` 收尾 |
| `rollback` | 把数据集根切换到目标快照根；快照本身保留 | 有打开的可写句柄返回 `EBUSY`；不删除更新的快照 |
| `clone` | 以快照根创建新可写数据集；源快照 `ref_count++` | 新数据集独立 `ds_id`；写时 COW |

补充约束：

- 快照名按字节比较，不强制 UTF-8；大小写敏感；同一数据集内唯一。
- 快照不是备份：共享块损坏会影响快照与原卷；文档和 CLI 必须明确这一点。
- 默认每数据集最多 64 个快照（沿用 `HV_SNAP_MAX`）；可配置但要有上限。
- `used_space` 是创建时统计，允许近似；精确空间统计放 `scrub`/`fsck`，不阻塞快照创建。

---

## 六、控制面 UAPI

### 6.1 ioctl 接口

控制面定义在共享 UAPI 头（`egfs_uapi.h` 或等价 Rust `repr(C)` 结构）：

```c
#define EGFS_IOCTL_MAGIC 0xE6

struct egfs_snapshot_create {
    uint32_t version;      /* = 1 */
    uint32_t flags;        /* READONLY 等 */
    char     name[128];
    uint64_t snap_id;      /* out */
};

struct egfs_snapshot_id {
    uint32_t version;
    uint32_t flags;
    uint64_t snap_id;
};

struct egfs_snapshot_list {
    uint32_t version;
    uint32_t count;        /* in: 容量；out: 实际数量 */
    uint64_t ids[];        /* 变长数组，按 id 排序 */
};
```

| ioctl | 作用对象 | 语义 |
|---|---|---|
| `EGFS_IOC_SNAPSHOT_CREATE` | 目录 fd | 创建快照；`name` 为空/重复返回 `EINVAL`/`EEXIST` |
| `EGFS_IOC_SNAPSHOT_DESTROY` | 目录 fd | 删除指定 `snap_id`；有克隆引用返回 `EBUSY` |
| `EGFS_IOC_SNAPSHOT_ROLLBACK` | 目录 fd | 回滚到 `snap_id`；有可写打开句柄返回 `EBUSY` |
| `EGFS_IOC_SNAPSHOT_CLONE` | 目录 fd | 从 `snap_id` 创建克隆数据集；返回新 `ds_id` |
| `EGFS_IOC_SNAPSHOT_LIST` | 目录 fd | 返回 id 数组；缓冲区不足返回 `ENOBUFS` |

- 所有结构以 `version` 开头；后续扩展用 `flags` + 尾部可变区域，不改变已有字段偏移。
- `ioctl` 在目录 fd 上执行，权限按目录的写权限检查；`rollback` 额外要求 `CAP_SYS_ADMIN` 或数据集 owner。
- 错误码映射：`EEXIST`、`ENOENT`、`ENOSPC`、`EBUSY`、`EINVAL`、`EPERM`、`EIO`、`EOPNOTSUPP`。

### 6.2 CLI

`egfs snapshot create|list|destroy|rollback|clone <mountpoint>` 作为 `ioctl` 的薄封装；FUSE 场景下 CLI 与 daemon 共用同一 UAPI，必要时通过控制 socket 转发。

### 6.3 与旧 syscall 的关系

- `EG_SNAPSHOT_*` 不再保留为“第二套入口”；迁移期只允许作为**临时兼容包装**，且必须调用同一核心方法。
- 所有 EG 原生调用方（`eash`、用户库）切到 ioctl/CLI 后，删除 syscall 常量、dispatch、用户库 wrapper 与测试。
- 外部 Linux 程序不得依赖任何 EG 私有号；快照能力只能通过 ioctl/CLI 发现。

---

## 七、崩溃一致性与恢复

### 7.1 提交顺序

1. 写数据块（COW 到新位置）。
2. 写元数据块（对象表、目录、快照表、xattr）。
3. 追加 ZIL 记录并写盘。
4. `flush` 设备。
5. 写 uberblock 到下一个环形槽。
6. `flush` 设备；返回成功。

任何一步失败：uberblock 不推进，旧根仍有效；未提交的 COW 块在 `fsck` 中回收。

### 7.2 挂载回放

1. 扫描 uberblock 环，选最高有效 txg。
2. 读 `zil_head_bp`，链式扫描有效 ZIL block。
3. 校验 block CRC、记录 CRC、`seq` 单调性、`txg` 范围。
4. 按 `seq` 回放 `txg > uberblock.txg` 的记录；重复记录跳过。
5. 回放完成后提交新 uberblock，推进 `zil_head`；失败则拒绝挂载或只读挂载。

### 7.3 fsck / scrub

- `fsck.egfs` 离线：检查 uberblock 环、元数据块校验、对象表、目录项、快照表、引用计数、ZIL 尾部；`--repair` 才修改；修复前生成元数据镜像。
- `scrub` 在线：遍历已分配块，校验 checksum；RAIDZ 可用时尝试重建；报告坏块，不静默丢弃。
- `fsck` 与 `scrub` 都复用核心的块读取/校验逻辑，不复制一套解析器。

---

## 八、平台适配与依赖规则

### 8.1 依赖规则

核心允许依赖：`core`、`alloc`、纯算法库、内部 `EgfsError`/`EgfsBlockDevice`。
核心禁止依赖：`crate::privileged::*`、`crate::functions::driver::*`、`crate::functions::sync::irq_lock`、`crate::functions::proc`、`crate::functions::sgeg`、`crate::privileged::freg`。

> 第一波可以先在 `functions/fs/egfs/portable` 子模块内做逻辑分层，不必立即物理拆 crate；
> 但 `portable` 子模块必须能在 host 上单独编译/测试，作为“无 OS 依赖”的证明。

### 8.2 Edgine 适配器

- `EgfsService` 持有 `Egfs` 实例；`get_egfs()` 只做服务定位，不再直接暴露核心全局状态。
- 块设备适配：`drive + partition_start` → `EgfsBlockDevice`。
- 热插拔：适配器解析 `HotplugEvent`，调 `EgfsService::add/remove_device`。
- FREG：适配器注册 recovery domain；核心恢复只依赖盘上 uberblock/ZIL/fsck。
- SGEG：适配器做权限/标签映射；核心用 POSIX 权限。

### 8.3 Linux 适配器

- FUSE 进程用 `libfuse` 低层 API；块后端用文件/loop 设备。
- 同一盘镜像可在 Edgine 与 FUSE 之间来回挂载，作为首要可移植性证据。
- 性能基线只记录，不作为第一波门槛；后续内核模块复用同一核心 API。

---

## 九、验证矩阵

| 层 | 验证 | 通过标准 |
|---|---|---|
| host 核心 | 格式化、mount、对象/目录、xattr、快照表 roundtrip | `cargo test` 全绿；无内核依赖 |
| 索引 | 10 万目录项、100 万稀疏对象号、多 GB 文件、稀疏文件 | 查找不全表扫描；多块文件读写正确；间接树 COW 路径正确 |
| 盘上格式 | v1 只读挂载、v2 读写、特性位拒绝未知 incompat | 不静默改写 v1；未知特性不让 mount 写 |
| 快照 | create/list/destroy/rollback/clone + 重启 | 重启后快照仍在；错误码符合 UAPI |
| 崩溃 | 在数据/元数据/ZIL/uberblock 各点注入掉电 | 1000 个随机点后 mount 可回放，`fsck` 干净 |
| 跨 OS | Edgine 创建 → Linux FUSE 挂载；反向 | 同一镜像双向读写、快照可见 |
| 控制面 | ioctl 矩阵；旧 syscall 零调用 | 外部 Linux 程序零私有号；`EG_SNAPSHOT_*` 已删 |
| Edgine 回归 | `./ci/build.sh all`、`make test-host`、`make test-kernel-host`、`bash ci/audit.sh full` | 全绿；ABI 边界测试更新 |
| 可靠性 | `fsck` 检测构造损坏；`scrub` 报告坏块 | 不静默丢数据；修复前有镜像 |

---

## 十、迁移与退役

1. **v1 → v2**：先做只读挂载 + `egfs-upgrade` 离线升级；升级前 `fsck`，升级后保留备份。
2. **快照控制面**：先落 `EGFS_IOC_*` 与 CLI；再迁 `eash`/用户库；最后删 `EG_SNAPSHOT_*`。
3. **全局单例**：`get_egfs()` 留在 Edgine 适配器；核心改为 `Egfs` 实例；热插拔/FREG 同步迁移。
4. **ZIL**：先 wire `zil_persist` 写入与回放；再让 `fsck` 消费 ZIL 尾部；最后补掉电测试。
5. **文档**：`private-abi-retirement.md`、`progress-active-tasks.md`、`syscall-followup.md` 与代码同批更新；文档/代码分离提交。

---

## 十一、开放问题

1. 快照形态选 **Btrfs 式根树 COW** 还是 **APFS 式容器/对象映射冻结**？建议第一波根树 COW，容器化留到多卷共享。
2. Linux 适配器先 FUSE 还是内核模块？建议 FUSE 先证明可移植性，内核模块留给性能阶段。
3. xattr 值上限 64 KiB、ACL 存储位置、SGEG 标签是否走 xattr，需要与 SGEG 设计对齐。
4. RAIDZ/压缩/去重在 v2 中保留到什么程度？建议保留实现，但不作为 v2 首批验收门槛。
5. 是否支持在线 `scrub` 与后台快照回收并发？第一波可以只做离线 `fsck` + 同步回收。
