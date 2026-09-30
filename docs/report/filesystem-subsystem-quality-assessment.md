# 文件系统子系统质量评估报告

> 总体判断：在"代码实现质量"这根轴上，fs 子系统在同类自研内核中属第一梯队——框内核分层干净、NestFS 企业特性真组网、fail-closed 纪律严、services 0 unsafe。但在"能力交付/可验证"这根轴上存在一条结构性大缝：大量文件系统实现完整却未接进 VFS 数据通路，默认启动实际只挂载一颗 ramfs。定性为"实现广度 > 已交付广度"——写好了没插电，而非写得差。

本报告是文件系统子系统质量评估的一次性快照，评估范围约 30.9K 行 services/fs 源码（84 个文件）+ 388 行 framework/fs trait 边界，逐模块核查 VFS 分派、真实磁盘 FS（ext2/exFAT/NestFS）、内存 FS（ramfs/tmpfs）与伪 FS（procfs/devfs/sysfs/overlay/cgroupfs/configfs/devpts/systree/virtiofs/initramfs）。聚焦一个此前审计未覆盖的维度：可达性（实现是否接进运行时 VFS 挂载/分派通路）。作为后续制定 plan 与修复工程的输入依据。

## 一、做得好的（实现质量证据）

**1. 框内核分层贯彻到位** —— framework/fs 仅 388 行 POD trait 边界（`VfsOps` / `FsBackend` / `poll_trait`），全部实现 100% 下沉 services；services/fs 全域 0 真实 unsafe（各文件顶部 `#![deny(unsafe_code)]` + `@SAFE:` 注释）。机制/策略分离教科书级。

**2. 块设备接真实驱动，不是 RAM 玩具** —— FS 读写经 `framework::driver::block::hdd_read/write_sector` ← Chitin `register_block_device` ← NVMe/AHCI/virtio-blk 真实驱动，非内存后端。

**3. NestFS 旗舰特性真组网** —— `nestfs_data.rs` 核心写路径真实引用 DMU→ZAP→TXG→ARC→ZIL→SPA→vdev→block 各模块；checksum 读路径做 Fletcher2/4 校验并有损坏检测；sync() 调 `zil.sync(txg)` + `arc.flush_dirty()` 落盘；持久化"写盘→开机导入 uberblock→反序列化 dataset"链路已接进 `init()`（详见 `mm-vmm` 同批的 nestfs 持久化核查）。

**4. ramfs 缓存失效严谨** —— dcache 作为路径/元数据缓存正确接入 ramfs 路径解析，unlink/rename/create/mkdir 全部配 `dcache_invalidate_parent` / `icache_invalidate` 调用，失效无遗漏。

**5. 安全代理纪律严** —— services 侧挂载/注册走 fail-closed：NestFS/tmpfs/overlay 未注册（`services::fs::init` 之前）直接返回 `NotInitialized`，不静默降级；fd 元数据用"测下游消费者"手法防"未接线恒 0"。

## 二、质量问题与"可达性大缝"

核心问题是"实现存在"与"运行时可达"之间的落差。VFS 采用 trait-object 分派（`resolve_mount_fs` 返回 `&'static dyn FileSystem`），`vfs_path.rs` 全程 `fs_opt.map_or(-1, …)` 无 FsType 兜底——**拿不到 trait object 的文件系统，任何 open/read/write 都直接失败**。据此得到三层可达性：

| 层 | 文件系统 | 真相证据 |
|---|---|---|
| A. 分派可达 | ramfs / nestfs / devfs / tmpfs / overlay | `vfs_mount.rs` L78-89 为这些类型挂 `&'static dyn FileSystem` |
| A′. boot 加载器 | initramfs | `init.rs` L80 `initramfs::unpack()` 解压进 ramfs（非独立挂载 FS） |
| B. 实现完整但结构上不可达 | **ext2 / exFAT** | `from_name` 认得（`vfs_types.rs` L108），但 mount 分派落 `_ =>`（`vfs_mount.rs` L90）只登记 FsType、不给 trait object；`Ext2FileSystem`/`ExfatFileSystem` 全仓无实例化点 |
| C. 连挂载名都不认 | procfs / sysfs / cgroupfs / configfs / devpts / systree / virtiofs | `from_name` 无对应分支（→ `NotSupported`）；各自 `mount_*()` boot 零调用者；`/proc/<pid>` 无 add_process 接线 |

质量问题清单：

| # | 问题 | 证据 | 定性 |
|---|---|---|---|
| 1 | **ext2/exFAT 未接 trait object**：实现完整（write_block/inode/dir_entry/balloc 位图 RMW 真落盘），但 mount 分派 `Ext2`/`ExFat` 臂为空注释、次臂落 `_ =>` 无 trait object，VFS 路径不可达 | `vfs_mount.rs` L54-59 空臂 + L90 `_ => mount()`；`mod.rs` L299-300 项目自陈"fs 为 None → open 返回 NotSupported"；`resolve_fs`（`mod.rs` L128-132）仅收录 tmpfs/overlay | 接线缺口（低成本高回报） |
| 2 | **伪 FS 集群未接 VFS**：procfs(889)/sysfs(191)/cgroupfs(395)/configfs(365)/devpts(243)/systree(601)/virtiofs(385) 共约 3.3K 行实现存在，但未进 `from_name`、`mount_*()` boot 无调用 | `vfs_types.rs` L108 `from_name` 仅 7 类；全仓搜 `mount_sysfs()` 等无 boot 调用者 | 预研代码未接线 |
| 3 | **默认 boot 仅挂 ramfs 根**：连 devfs 的 `/dev` 都未挂载（`devfs.mount("/dev")` 仅出现在自测），用户态拿不到设备节点 | `init.rs` L65 只 `vfs_mount_safe("/", "ramfs")`；`devfs.rs` L890 mount 仅在 `#[test]` | 交付面缺口 |
| 4 | **ext2/exFAT 无行为测试**：现有 host-test 不穿过 FS 代码——只裸读 `.img` 断言 magic/块大小，外加 `read_to_string("mount.rs")` grep 源码文本充当"结构门槛"，注释自陈"host 无 mock 无法行为验证" | `ext2_test.rs` L73/L113；`exfat_test.rs` L7-L47 全为镜像字节断言 | 验证缺口（实现分高、验证分低） |
| 5 | **NestFS raidz 孤儿**：403 行 RAID-Z 实现无任何调用者，未接入 vdev I/O 路径 | `nestfs/raidz.rs` 403 行；全 fs 搜 `raidz::`/`RaidZ` 外部引用计数 = 0 | 未接线模块 |
| 6 | **粗粒度锁 + 无数据页缓存**：块层同步 write-through（无 page cache），ext2 全 FS 单锁串行（`EXT2_FS.lock()`），限制 SMP 扩展性；VFS 通用 `resolve_path` 对 ext2/exfat/nestfs 仍是线性扫描（dcache 仅覆盖 ramfs） | `framework/driver/block.rs` 写透模型；`ext2/*.rs` 全 FS 锁 | 有意简化，代价是并发/性能 |

## 三、结论与后续方向

- **代码实现质量维度：第一梯队达标** —— 分层纪律、NestFS 特性纵深（DMU/ZIL/TXG/ARC/checksum/持久化）、fail-closed 安全代理、0 unsafe、fsx 正确性测试同时跑 tmpfs 与 ext2 路径，这些已达到甚至超过多数教学/研究内核，形态贴近 Asterinas/RedLeaf 一档。此结论不因本轮发现而动摇。

- **能力交付/可验证维度：本轮下修** —— 约 3.5K 行磁盘 FS（ext2+exFAT）与半数伪 FS 卡在 VFS trait-object 分派门口未接入，默认 boot 实际仅一颗 ramfs。这不是"写得差"，而是"写好了没插电"，与 mm/vmm 报告"能力上支持 ≠ 实测已证明"同属一类结构性欠账，应视为"实现广度 > 已交付广度"的研究系统特征如实记录，不以叙事掩盖度量。

后续据此制定 plan 时，建议以第二节编号 # 为条目索引，优先级候选：

- **#1 ext2/exFAT 接 trait object**（在 `vfs_mount.rs` L77 match 补 `FsType::Ext2 => &EXT2_FS` 分支复用现成实现）——把已写代码转成真实可挂载能力的最短路径，性价比最高；
- **#3 boot 挂载 `/dev`（devfs）**——补齐用户态设备节点可见性；
- **#4 接线后补穿 FS 代码的行为测试**（仿 nestfs 用 mock block 后端跑 `fs_mount→fs_read→fs_write`）；
- **#2 伪 FS 集群**：逐一定性为"接线进主叙事"或"标注预研、不计入 TCB/能力宣称"，避免以代码存在冒充功能可用；
- **#5 raidz / #6 锁与页缓存**：延续上轮已登记的欠账，属长期演进项。
