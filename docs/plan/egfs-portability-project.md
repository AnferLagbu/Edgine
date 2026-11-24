# EGFS 可移植性工程计划

> 对应设计：[egfs-portability-design.md](../design/egfs-portability-design.md)。
> 本文是可执行分解：工作包、改动面、依赖、验收与风险。本文不重复设计论证；
> 私有 ABI 退役口径见 [private-abi-retirement.md](./private-abi-retirement.md)。

---

## 一、范围与出口

### 1.1 范围

本轮只做四件事：

1. 把 EGFS 核心与 Edgine 内核适配层解耦，使核心能在 host 与 Linux 上编译/运行。
2. 把快照从内存表变成持久文件系统对象，并把控制面从 `EG_SNAPSHOT_*` 迁到 `ioctl`。
3. 把 ZIL 真正写盘并在挂载时重放，补 `fsck` / `scrub` 与掉电测试。
4. 用 Linux FUSE 适配器证明“同一盘镜像、同一核心、跨 OS”。

### 1.2 非目标

- 不重写 SPA/DMU/TXG/ZIL/ARC/metaslab；只改边界、持久化与控制面。
- 不做 ZFS 级 pool/RAID/加密/远程复制；RAIDZ、压缩、去重保留但不作为首批门槛。
- 不做 Linux 内核模块；FUSE 是第一阶段适配器，内核模块后置。
- 不承诺第三方稳定 EG 私有 ABI。

### 1.3 出口条件

- `egfs-core` 逻辑可在 host 上完成 format/mount/read/write/snapshot/xattr 测试。
- 同一 v2 镜像可在 Edgine 与 Linux FUSE 双向挂载。
- 重启后快照仍存在；掉电注入后 mount 能回放，`fsck` 干净。
- `EG_SNAPSHOT_*` 在代码中零命中；外部 Linux 程序零私有号依赖。
- `./ci/build.sh all`、`make test-host`、`make test-kernel-host`、`bash ci/audit.sh full` 全绿。

---

## 二、工作包

### E0：设计冻结与基线

**目标**：把设计决策、现状和退役口径固定下来，防止边做边改。

**改动点**

- `docs/design/egfs-portability-design.md`（本设计）。
- `docs/plan/egfs-portability-project.md`（本计划）。
- `docs/plan/private-abi-retirement.md`：把 EGFS 快照从“保留”改为“迁移到 ioctl”。
- 建立现状基线：记录 `get_egfs`、`block::hdd_*`、`snap_mgr`、`zil_persist` 的当前调用面。

**测试/证据**

- `rg` 清单：`get_egfs`、`hdd_read_sector`、`hdd_write_sector`、`snapshots`、`zil_persist`、`EG_SNAPSHOT`。
- 文档 diff 与代码 diff 分离提交。

**出口**：设计/计划/退役口径三者一致；不新增私有号；工作树干净。

---

### E1：核心去耦（无行为变更）

**目标**：先把“核心能离开 Edgine 编译”这件事做出来，不改变现有 Edgine 行为。

**改动点**

1. 新增 `functions/fs/egfs/portable/` 子模块：
   - `block.rs`：`EgfsBlockDevice` trait（`read/write/flush/size_bytes`）与 `EgfsIoError`。
   - `error.rs`：OS 无关 `EgfsError`；`KernelError` 映射留在适配器。
   - `lock.rs`：锁抽象；Edgine 用 `IrqSpinLock`，host 用 `std::sync::Mutex`（仅测试）。
2. `vdev.rs`：
   - `EgfsVdev` 持有 `Arc<dyn EgfsBlockDevice>` + `partition_start`，不再持有 `drive: u8` 直接调 `block::hdd_*`。
   - `probe_disk_size` 移到 Edgine 适配器；核心只读 `size_bytes()`。
3. `spa.rs`：
   - `read_sector` / `write_sector` 走 vdev 后端；删除 `use crate::privileged::driver::block`。
4. `egfs_data.rs`：
   - 把 `EgfsData` 的核心状态收敛为 `Egfs` 实例；`OnceCell`/`get_egfs` 移到 `EgfsService`（Edgine 适配器）。
   - 核心 `open(dev, opts)` / `format(dev, opts)` 不扫描全局磁盘。
   - `init()` 的磁盘发现、自动格式化、热插拔注册留在适配器。
5. `egfs.rs`：
   - 热插拔监听改调 `EgfsService::add/remove_device`；不再直接 `get_egfs().hotplug_*`。
6. host 测试：
   - `MemVdev`：格式/mount/读写/目录。
   - `FileVdev`：生成镜像文件，后续崩溃注入复用。
   - 新增 `host-tests/tests/egfs_core_mount_test.rs`。

**测试**

- `rg "privileged::driver::block" src/kernel/functions/fs/egfs/portable` 零命中。
- `cargo test` / `make test-host` 中新增 core mount 测试通过。
- `./ci/build.sh all`、`make test-kernel-host` 通过。
- 现有 EGFS mount/读写行为不变。

**出口**：核心在 host 上可 format/mount/read/write；Edgine 行为不回归；仍保留 `get_egfs` 兼容入口，但只在适配器。

---

### E1.5：索引层最小实现（E2 前置）

**目标**：先把“当前不是索引式”这件事修掉，避免 v2 元数据和快照表建在 `Vec` + 固定 ZAP + 单 BP 上。

**改动点**

1. 对象索引：
   - `EgfsObjSet.objects` 从“按 `obj_id` 线性查找的 `Vec`”改为可按键查找的结构（内存 radix/哈希 + 盘上对象表）。
   - 保留 `next_obj_id` 单调分配；支持稀疏对象号；对象释放后可复用或留空。
2. 目录索引：
   - `EgfsZap` 从固定 256 项、64B 名字、128B 值改为可增长、变长名字、可落盘的 ZAP 式哈希索引。
   - 目录不承诺有序遍历；`readdir` 只保证不重不漏。
3. 文件块索引：
   - 启用 `EgfsBlockPointer.level`：`level=0` 数据块，`level>0` 间接块；按 fanout 逐级下降。
   - `write()` 不再“替换 `obj.bp`”，改为按逻辑块号定位、COW 路径、最后更新 dnode 根。
   - 支持空洞、稀疏文件、多块文件；常量断言 fanout 与最大层级。
4. 测试：
   - 10 万目录项 lookup/insert/delete。
   - 100 万稀疏对象号 alloc/lookup。
   - 多 GB 文件顺序读/写；稀疏文件；跨块边界读写。
   - 间接树 COW 路径的单元测试与镜像 roundtrip。

**测试**

- `rg` 确认核心查询路径不再依赖 `Vec` 线性扫描；目录 ZAP 不再固定 256 项。
- 新增 `host-tests/tests/egfs_index_test.rs`。
- 现有目录/文件语义不回归。

**出口**：文件可大于一个块；目录可超过 256 项；对象号可稀疏；查找不全表扫描。

---

### E2：盘上 v2 与快照持久化

**目标**：快照从“内存会话状态”变成“盘上文件系统对象”；元数据格式可演进。

**改动点**

1. `metadata`（`egfs_data.rs` 的编码/解析路径）：
   - 新 magic `HVM2`，增加 `format_version`、`endianness`、`feature_compat/ro_compat/incompat`、`snapshot_table_bp`、`xattr_table_bp`。
   - v1 `HVM1` 只读挂载；不原地改写。
2. `snapshot.rs`：
   - 新增持久记录结构：`snap_id`、`name[128]`、`ds_id`、`root_bp`、`birth_txg`、`used_space`、`ref_count`、`origin_snap`、`flags`、`state`、`checksum`。
   - 表头持久化 `next_snap_id` / `entry_count`。
   - `create/destroy/rollback/clone/list` 走核心方法；`destroy` 两阶段（DELETING → 回收）。
3. `egfs_data.rs`：
   - `mount_drive` 读取快照表并恢复 `snap_mgr`；`snapshot_create` 与数据集元数据同 txg 提交。
   - `rollback` 原子切换数据集根；有可写打开句柄返回 `EBUSY`。
4. xattr：
   - 真实值存储，不再存 `u64` 哈希；名字最大 255B，值最大 64 KiB；独立 xattr 表/对象。
5. `tools/egfs-upgrade`：
   - 离线 v1 → v2；升级前 `fsck`，升级后保留 v1 备份；不做自动原地升级。

**测试**

- 快照 create/list/destroy/rollback/clone；卸载/重启后仍存在。
- clone 后源快照 `ref_count` 增加；destroy 有克隆引用返回 `EBUSY`。
- v1 镜像只读挂载通过；v2 未知 `incompat` 拒绝挂载。
- xattr set/get 往返，值可逆；大值/长名边界。
- v1→v2 升级工具 roundtrip。

**出口**：快照持久化、可重启恢复；v1 不被静默改写；xattr 可移植。

---

### E3：ZIL 写入、挂载回放、fsck/scrub

**目标**：把 `zil_persist.rs` 从“有布局、无调用者”变成真正的 WAL；补崩溃恢复。

**改动点**

1. `zil_persist.rs` / `zil.rs`：
   - 在 txg 提交路径追加 ZIL block；写 `zil_head_bp` 到 uberblock。
   - 记录 `seq`、`txg`；block CRC + trailer 校验。
2. `egfs_data.rs::mount_drive`：
   - 扫描 uberblock 环，选最高有效 txg。
   - 从 `zil_head_bp` 链式扫描，校验 CRC/seq/txg。
   - 只回放 `txg > uberblock.txg` 的记录；幂等，重复记录跳过。
   - 回放后提交新 uberblock，推进 ZIL head。
3. `tools/egfs-fsck`：
   - 离线检查 uberblock、元数据块、对象表、目录项、快照表、引用计数、ZIL 尾部。
   - `--repair` 前生成元数据镜像；只保证元数据一致性，不做数据恢复。
4. `tools/egfs-scrub`：
   - 遍历已分配块校验 checksum；RAIDZ 可用时尝试重建；报告坏块。
5. 崩溃注入：
   - `FileVdev` 增加 failpoint：数据写后、元数据写后、ZIL 写后、uberblock 写前/后、flush 前后。
   - 1000 个随机掉电点，mount 后校验已提交 txg 不丢、未提交块被回收。

**测试**

- 每个 failpoint 单独跑；随机 1000 点跑一轮。
- 人为破坏 ZIL block CRC / uberblock checksum，mount 拒绝或回退到上一有效槽。
- `fsck` 对构造的目录/快照引用不一致能检测；`--repair` 后复检干净。

**出口**：掉电后能回放；`fsck`/`scrub` 可用；不再出现“WAL 写了但没人重放”。

---

### E4：UAPI、Edgine ioctl 与私有 syscall 退役

**目标**：快照控制面从 `EG_SNAPSHOT_*` 迁到 `EGFS_IOC_*`；外部 Linux 程序零私有号。

**改动点**

1. UAPI：
   - 新增共享 UAPI（建议 `src/rust/egfs-uapi` + 生成 C 头，具体路径按现有 crate 布局定）。
   - 结构以 `version` 开头；`EGFS_IOC_SNAPSHOT_CREATE/DESTROY/ROLLBACK/CLONE/LIST`。
2. Edgine VFS：
   - 目录 fd 的 `ioctl` 分发到 `EgfsService` → `Egfs` 核心方法。
   - 权限：目录写权限；rollback 要求 `CAP_SYS_ADMIN` 或数据集 owner；错误码映射。
3. CLI：
   - `egfs snapshot create|list|destroy|rollback|clone`；FUSE 场景共用同一 UAPI。
4. 迁移调用方：
   - `eash` / 用户库 / 测试从 `EG_SNAPSHOT_*` 切到 ioctl/CLI。
   - 删除 `types.rs`、`dispatch.rs`、`src/user/lib/src/sys.rs`、`edgine-abi` 中的 `EG_SNAPSHOT_*`。
5. 锁边界：
   - `host-tests/tests/syscall_abi_boundary_test.rs` 更新保留私有集合；快照四号不再出现。
   - 文档 `private-abi-retirement.md` 标记 EGFS 快照为“已迁移”。

**测试**

- ioctl 矩阵：create/list/destroy/rollback/clone，正常 + `EEXIST/ENOENT/ENOSPC/EBUSY/EINVAL/EPERM`。
- 目录 fd 权限；非目录 fd 返回 `ENOTDIR`。
- `rg "EG_SNAPSHOT" src host-tests` 零命中。
- 外部 Linux 程序访问 EGFS 快照只走 ioctl/CLI，不触发私有号。

**出口**：快照控制面可移植；旧 syscall 真删；ABI 边界锁更新。

---

### E5：Linux FUSE 适配器

**目标**：用最低成本证明“同一镜像、同一核心、跨 OS”。

**改动点**

1. 新增 `tools/egfs-fuse`：
   - 首选 `libfuse` 低层 API（稳定、可直接控制 inode/fd）；若引入 Rust `fuser`，单独评审依赖与许可。
   - 文件/loop 块后端实现 `EgfsBlockDevice`。
2. VFS 映射：
   - `lookup/getattr/readdir/read/open/write/create/unlink/rename/setattr/xattr`。
   - `FUSE_IOCTL` 转发 `EGFS_IOC_SNAPSHOT_*`；不支持时走本地控制 socket。
3. 跨镜像测试：
   - Edgine 创建 v2 镜像 → Linux FUSE 挂载、读写、快照、回滚。
   - Linux 创建 → Edgine 挂载。
4. 性能基线：
   - 记录顺序读写、随机读、元数据操作、快照创建耗时；不作为第一波门槛。

**测试**

- Linux 上 mount/umount、读写、目录、xattr、快照 CLI。
- 同一镜像双向挂载；v2 特性位识别正确。
- FUSE 断开/重启后镜像仍可由 Edgine 或 fsck 读取。

**出口**：Linux FUSE 可用；跨 OS 镜像成立；性能基线归档。

**E5.5：真实块设备 / U 盘验收（可选手工门禁，不进 CI）**

**目标**：证明同一核心不只跑文件/loop 后端，还能在真实 USB 块设备上 format/mount/读写/掉线恢复。

**前置**

- E5 的 FUSE 可用；`mkfs.egfs` 能对块设备写入 v2 元数据。
- `EgfsBlockDevice` 支持文件、loop、真实块设备三种后端；块大小/逻辑扇区大小可探测。
- 安全门禁：只接受显式 `/dev/disk/by-id/usb-*`，校验 `TRAN=usb`、序列号、容量；拒绝根盘及其分区；任一分区已挂载则拒绝。

**步骤**

1. `lsblk -o NAME,SIZE,MODEL,SERIAL,TRAN,MOUNTPOINTS` 识别目标，记录序列号与容量。
2. `umount` 全部分区；`sync`；确认无挂载。
3. `mkfs.egfs --device /dev/disk/by-id/usb-... --force --expect-serial ...`。
4. `egfs-fuse mount`，写入混合负载（大文件、多级目录、稀疏文件、xattr、快照）。
5. `fsync` + `umount`；重新挂载校验；再做一次“写入中拔盘”的掉线测试。
6. 同一 U 盘接到 Edgine 目标机（或整盘镜像回灌），验证双向挂载。
7. `fsck.egfs`/`scrub` 报告干净；已提交数据不丢，未提交块可回收。

**说明**

- U 盘只做**最终物理验证**；CI 与日常回归仍用镜像文件 + loop，保证可复现。
- USB 闪存有写缓存与磨损问题，不作为掉电一致性的唯一证据；掉电注入仍靠 failpoint。
- “跨 OS”第一波是 **Linux FUSE ↔ Edgine 内核**；macOS/Windows 需要各自适配器，不因 U 盘可插而自动成立。

**对开发机的侵入面**

- 镜像/loop：只写临时目录与 `/dev/loopN`；无系统安装，`umount`/`losetup -d` 可清理。
- FUSE：只多一个用户态进程与临时挂载点；不装内核模块、不改 `/etc/fstab`。
- raw U 盘：只有目标 U 盘被改写；需写块设备权限；by-id + 序列号 + 根盘拒绝。
- 明确不做：EGFS 内核模块、bootloader/分区表改动、系统库替换、全机 `sync`。

**出口**：真实 USB 块设备可格式化、挂载、读写、重挂载；掉线后 `fsck` 干净；镜像与 U 盘结果一致。

---

### E6：退役、文档与收口

**目标**：删旧入口、清台账、把可移植性变成持续门禁。

**改动点**

- 删除 `EG_SNAPSHOT_*` 的残留常量/dispatch/wrapper/测试。
- 更新 `private-abi-retirement.md`、`progress-active-tasks.md`、`syscall-followup.md`。
- 把 host core 测试、FUSE 跨镜像测试、掉电测试接入 CI（能进则进，不能进先做手工门禁）。
- 可选后续：Linux 内核模块复用同一核心 API；不在本轮。

**出口**：代码零 `EG_SNAPSHOT`；文档与台账闭合；CI/手工门禁可复现。

---

## 三、依赖与顺序

```text
E0 设计冻结
  └─ E1 核心去耦
       └─ E1.5 索引层最小实现
            ├─ E2 盘上 v2 + 快照持久化
            │    ├─ E3 ZIL/回放/fsck
            │    └─ E4 UAPI + Edgine ioctl + 私有号退役
            └─ E5 FUSE（可在 E2 的只读 mount 后提前起步）
                 ├─ E5.5 USB/真实块设备门禁（可选）
                 └─ E6 收口
```

- E1 必须先做：不做块设备/实例解耦，E5 无法复用核心。
- E1.5 是 E2 的前提：对象表/目录索引/文件块索引不修，v2 元数据与快照表会建在 `Vec` + 单 BP 上。
- E2 是 E4 的前提：没有持久快照表，ioctl 只是换入口。
- E3 与 E4 可部分并行，但 E3 的 ZIL 回放必须先于 E4 的“删除旧 syscall”发布，避免旧数据无恢复路径。
- E5 可在 E2 完成后先做只读 mount，再补写/快照；不要等 E4 全部完成。
- E5.5 以 E5 + `mkfs.egfs` 为前提，只做最终物理验证，不阻塞 E6。

---

## 四、验收矩阵

| 验收项 | 方法 | 通过标准 |
|---|---|---|
| 核心无 OS 依赖 | `rg` + host 编译 | `portable` 子模块零 `privileged`/`driver`/`sgeg` 引用 |
| host 核心 | `cargo test` / `make test-host` | format/mount/读写/目录/xattr/快照 roundtrip 全绿 |
| 索引规模 | 10 万目录项、100 万稀疏对象号、多 GB 文件 | 查找不全表扫描；多块文件/稀疏文件正确 |
| v1 兼容 | v1 镜像挂载 | 只读成功；不写盘；未知 incompat 拒绝 |
| v2 快照持久化 | create → umount → mount → list | 快照仍在；`snap_id` 不复用；clone refcount 正确 |
| 掉电恢复 | failpoint + 1000 随机点 | 已提交 txg 不丢；未提交块可回收；`fsck` 干净 |
| 控制面 | ioctl 矩阵 | 正常/错误码齐全；目录 fd 权限正确 |
| 私有号退役 | `rg EG_SNAPSHOT` | 代码/测试零命中；外部程序零私有号 |
| 跨 OS | Edgine ↔ Linux FUSE | 同一镜像双向读写、快照可见 |
| 真实块设备 / U 盘 | E5.5 手工门禁 | 显式 by-id 目标可格式化、挂载、读写、拔盘重挂载；`fsck` 干净；根盘误选被拒绝 |
| 回归 | `./ci/build.sh all`、`make test-kernel-host`、`bash ci/audit.sh full` | 全绿；ABI 边界测试更新 |
| 可靠性 | 构造损坏 + `fsck`/`scrub` | 能检测/报告；修复前有镜像；不静默丢数据 |

---

## 五、风险与缓解

| 风险 | 触发点 | 缓解 |
|---|---|---|
| v1→v2 数据损坏 | 原地改写元数据 | 只读挂载 + 离线升级 + 备份；禁止自动升级 |
| 全局单例重构回归 | `get_egfs` 调用方多、热插拔/FREG 耦合 | E1 先适配器包装，逐步切调用方；每步跑 Edgine 回归 |
| ZIL 回放不幂等 | 记录是“操作”而非“结果” | 记录结果状态；`seq` 排序；重复记录跳过；掉电测试 |
| 快照回收竞态 | destroy 与 clone/打开句柄并发 | 两阶段 DELETING + refcount；`fsck` 收尾；rollback `EBUSY` |
| FUSE 语义不完整 | mmap/ioctl/xattr/rename 语义差异 | 第一波明确支持集；不支持返回 `EOPNOTSUPP`；CLI 控制 socket 兜底 |
| xattr/SGEG 冲突 | 真实值存储与标签设计不一致 | xattr 先做 POSIX 可逆值；SGEG 走可选适配器，不阻塞核心 |
| 误格式化真实磁盘 | 把 U 盘当 `/dev/sdX` 直接写 | 只接受 `/dev/disk/by-id/usb-*`；校验 `TRAN=usb`、序列号、容量；拒绝根盘/已挂载分区；先 `sync` |
| 范围膨胀 | 把 RAIDZ/加密/集群一起做 | 非目标冻结；核心件保留但不作为首批验收 |
| 旧调用方断档 | 删 `EG_SNAPSHOT_*` 太早 | E4 先迁调用方再删；保留一个提交周期的临时包装，且包装只调同一核心 |

---

## 六、提交纪律与台账

- 文档与代码分离提交；E0 文档单独提交。
- 每个工作包一个可独立回滚的提交；不把 E2/E3/E4 混成一个大提交。
- 删除私有号前必须满足：调用方圆零 + 新路径测试通过 + ABI 边界测试更新。
- 不新增私有号；新需求先进 `private-abi-retirement.md` 评审。
- 每包收口更新 `progress-active-tasks.md`；T7-c 与 EGFS 可移植性分线记录，不混计数。
