# EGFS 可写持久化文件系统现状评估

> **核心结论**：EGFS 的磁盘链路是"能编译、能跑通启动、但跨重启必然丢数据"的骨架。最重的三条是可验证的代码事实：目录项序列化把对象号硬写成 0（重挂载后所有名字指向对象 0）；挂载时分配位图一律重建为全空闲（新写入会覆盖上一轮落在同一区域的数据）；块设备层到文件系统层都没有 flush/barrier，落盘顺序完全不受控。校验和只覆盖 uberblock，数据与元数据块从不计算也从不验证；ZIL 只有内存记录，559 行磁盘 WAL 实现在内核侧零调用者；持久化"测试"全部是内存 mock，CI 的 QEMU 腿根本不挂磁盘。结论：EGFS 当前不具备"持久化"语义，只具备"单次启动内的内存文件系统 + 一次性的写盘尝试"。

## 一、分级结论

| 级别 | 含义 | 条目数 | 代表项 |
|---|---|---:|---|
| S0 | 直接导致数据不可恢复丢失、跨重启必错，或可由用户态触发内核 panic | 4 | 目录项对象号清零（C-01）、挂载失败自动 format（C-06）、空间位图重置（S-01）、偏移丢弃导致部分写破坏与越界 panic（P-01） |
| S1 | 破坏持久化/一致性保证，或使某类操作完全不可用 | 23 | 无 flush（C-03）、uberblock 环形槽取旧（C-04）、块校验和缺失（C-05）、反序列化失败静默空数据集（C-07）、`sync` 假成功（C-08）、旧块不回收（S-02）、全局锁跨 I/O（L-01/L-02）、symlink/readdir/truncate/xattr/hardlink 语义破损（P-02..P-06）、磁盘路径零门禁（V-01..V-04） |
| S2/S3 | 性能、可维护性、平台与文档债 | 16 | 全量元数据重写（F-01）、压缩/去重未接线（F-03）、4 套 FS 重复实现（A-02）、virtiofs 桩（A-03）、硬编码分区 LBA（T-02）、能力探测封顶 32 MiB（T-03） |

三条最关键的结论：

1. **持久化在数据模型层面就是错的，不是"实现不全"**。序列化侧 `write_le64(buf, off, 0)` 把每个目录项的对象号清零（[egfs_data.rs:1663](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1663-L1663)），反序列化侧照单恢复。任何一次重启之后，所有文件名都映射到对象 0。既有的深层审计报告在模块改名之前已经记录过同一事实（[subsystem-gap-deep-audit.md:29](subsystem-gap-deep-audit.md#L29-L29)）。
2. **写入路径不具备 POSIX 语义**，且能触发越界 panic。`Inode::read` / `Inode::write` 丢弃 offset（[egfs_inode.rs:28-45](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L28-L45)），`FileSystem::fs_read` / `fs_write` 再次丢弃 offset（[egfs_inode.rs:198-236](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L198-L236)）。`EgfsData::write` 把本次写入的切片整体替换旧块（[egfs_data.rs:752-779](../../src/kernel/functions/fs/egfs/egfs_data.rs#L752-L779)），偏移只影响逻辑大小；`read` 的磁盘分支用 `start = offset` 直接切整块缓冲（[egfs_data.rs:703-719](../../src/kernel/functions/fs/egfs/egfs_data.rs#L703-L719)），当逻辑 size 大于物理块大小时 `end - start` 下溢，读操作即可 panic。
3. **"不存在崩溃一致性设计"是可证明的**：`BlockDevice` trait 只有读/写两个方法，没有 flush / barrier / discard（[egfd/mod.rs:90-120](../../src/kernel/privileged/egdf/mod.rs#L90-L120)）；`sync()` 的次序是"写元数据块 → 改内存 uberblock → 写 uberblock → 统计脏条目"，中间没有任何落盘屏障（[egfs_data.rs:1420-1446](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1420-L1446)）；分配器状态从不落盘（[spa.rs:334-367](../../src/kernel/functions/fs/egfs/spa.rs#L334-L367)）。

## 二、架构现状

### 2.1 层次与接线

```
VFS（functions/fs）
├─ inode.rs / vfs_types.rs / vfs_manager.rs / dcache.rs← 仅 ramfs 使用
├─ FileSystem trait（fs_read/fs_write/fs_readdir/fs_sync…）
│   ├─ EgfsData（egfs/，9,300 行）← 唯一被 boot 挂载的真实块设备 FS
│   ├─ Ext2Fs（ext2/，2,455 行）← 不可达（VFS 挂载分支为空）
│   ├─ ExfatFs（exfat/，1,009 行）← 不可达
│   ├─ RamFs（ramfs_core/）← 无盘时的真正根
│   ├─ TmpFs / OverlayFs ← resolve_fs 仅返回这两个
│   └─ VirtioFs（virtiofs.rs，351 行）← 桩，无协议实现
└─ block 层（functions/driver）
    └─ BlockDevice（privileged/egdf）: blk_read / blk_write
        ├─ virtio-blk（x86_64 + aarch64）
        └─ ATA/AHCI（x86_64）
```

EGFS 的实际挂载点在启动编排里，且**只在 x86_64**：`#[cfg(target_arch = "x86_64")]` 块内先 `egfs::init()`，再判断是否磁盘模式，最后在 `"/"` 上挂 EGFS（[lib.rs:938-955](../../src/kernel/lib.rs#L938-L955)）。无盘时 EGFS 不挂载，`functions/init.rs` 稍后在 `"/"` 挂 ramfs（[init.rs:130-140](../../src/kernel/functions/init.rs#L130-L140)）；同一路径重复挂载返回失败，被忽略。

关键后果：**磁盘模式与内存模式的根是互斥的两套语义**。有盘时 `"/"` 是 EGFS，initramfs 解包会写进 EGFS；无盘时 `"/"` 是 ramfs，EGFS 只在内存里初始化、从不参与挂载。

### 2.2 对象、DMU 与命名空间

EGFS 的对象模型是 ZFS 的简化版：`EgfsDmuObject` 有类型、size、`mtime`、`link_count` 与**一个** `bp` 字段（[dmu.rs:68-96](../../src/kernel/functions/fs/egfs/dmu.rs#L68-L96)）。注释称 `bp` 是"数据块树根"，但结构里没有任何子指针、间接块或树级字段——一个对象就是**一个连续 extent**，没有块指针树。

命名空间是**全局扁平 ZAP**：VFS 传进来的整条相对路径去掉前导 `/` 后，直接作为 key 插入一个全局 map（[egfs_data.rs:601-620](../../src/kernel/functions/fs/egfs/egfs_data.rs#L601-L620)）。没有每目录的子项索引，没有父指针，也不校验父目录是否存在；`mkdir("a")` 与 `create("a/b")` 之间没有父子约束。这解释了为什么"目录"在 VFS 侧不可枚举（见 P-02）。

对象号顺序分配，磁盘上只持久化 `{对象记录, 目录项记录}` 两段；目录项记录本应保存 `name → obj_id`，但实际写成 0（C-01）。

### 2.3 块指针、写入路径与 COW

- 块大小 4 KiB（`HV_POOL_BLOCK_SIZE`），metaslab 16 MiB（`HV_POOL_METASLAB_SIZE`），单 extent 的 `asize` 是 `u32`（[bp.rs:18-45](../../src/kernel/functions/fs/egfs/bp.rs#L18-L45)、[spa.rs:1-15](../../src/kernel/functions/fs/egfs/spa.rs#L1-L15)）。
- 写入是"整块 COW"：每次 `write` 都 `spa.allocate(...)` 申请一个新 extent，写入本次 `buf[..count]`，然后 `obj.cow_bp(new_bp, txg)` 顶替旧 bp（[egfs_data.rs:759-770](../../src/kernel/functions/fs/egfs/egfs_data.rs#L759-L770)）。`cow_bp` 只替换指针，**不释放旧块**（[dmu.rs:248-256](../../src/kernel/functions/fs/egfs/dmu.rs#L248-L256)）。
- 数据落盘用 `write_bp`：按 DVA 逐段 `vdev.write_sectors`，全程持 `vdevs` 自旋锁，写完不做任何 flush（[spa.rs:453-469](../../src/kernel/functions/fs/egfs/spa.rs#L453-L469)）。
- 因为旧块不回收 + 新块永远从 metaslab 起始位首次适配（[metaslab.rs:135-205](../../src/kernel/functions/fs/egfs/metaslab.rs#L135-L205)），一次会话内的连续写入会单调推进，最终耗尽 metaslab 后返回 `NoSpace`。

### 2.4 校验和

`EgfsChecksum` 实现了 Fletcher2 / Fletcher4 / SHA256（含 KAT 单测），但**调用面只有 uberblock**：`compute_checksum()` 仅在 `write_uberblock_to_disk` 里被调用（[spa.rs:471-489](../../src/kernel/functions/fs/egfs/spa.rs#L471-L489)）。数据/元数据块的 `bp.prop.cksum_type` 只是被记录成本字段（[spa.rs:403-409](../../src/kernel/functions/fs/egfs/spa.rs#L403-L409)），`bp.checksum` 从未被赋值；`read_bp` 只做搬运，不做校验（[spa.rs:434-450](../../src/kernel/functions/fs/egfs/spa.rs#L434-L450)）。因此**校验和覆盖范围 = 1 个 512 B uberblock**，其余全部裸盘。

### 2.5 压缩与去重

- 压缩：`compress()` 对 `Off` 直接返回 `None`；写路径把 `comp_type` 硬编码为 `Off`（[egfs_data.rs:756-758](../../src/kernel/functions/fs/egfs/egfs_data.rs#L756-L758)），symlink 同样 `Off`（[egfs_data.rs:1091-1098](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1091-L1098)）。`compress_lz4` 是自研的贪心匹配器，`compress_zstd_fallback` 实际是 RLE（[compress.rs:309-338](../../src/kernel/functions/fs/egfs/compress.rs#L309-L338)）。压缩能力存在但**从未启用**。
- 去重：`dedup.rs` 实现了一个内存 CAS 表与 `cas_aware_write`，全仓**零调用者**（既有零引用清单已登记同类事实，见 [syscall-followup.md:1243-1256](../plan/syscall-followup.md#L1243-L1256)），表也不持久化。当前既没有去重收益，也没有去重成本，属纯死重。

### 2.6 事务、日志与元数据落盘时序

- `txg.rs` 有事务组簿记（dirty bp 列表、`transition()`），但 `sync()` 从不消费 dirty bp，而是**全量重写整份元数据 blob**；`txg_group` 只被 `transition()` 推进一步。
- `zil.rs` 只有一个内存 `Vec`：`add_record` 追加，`sync(txg)` 清空/提交序号，`replay` 返回内存记录（[zil.rs:323-346](../../src/kernel/functions/fs/egfs/zil.rs#L323-L346)）。**没有磁盘 WAL，也没有挂载时重放**。
- `zil_persist.rs` 实现了 559 行磁盘 ZIL 编解码，内核侧零调用者（只出现在 host 测试与 bench）。
- `sync()` 的真实时序（[egfs_data.rs:1420-1446](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1420-L1446)）：

```
advance_txg → serialize_dataset_metadata(txg)      // 申请一个新 extent，写整份元数据
           → 内存 uberblock.root_bp = 新 bp
           → zil.sync(txg)                          // 仅内存
           → write_uberblock_to_disk()              // 单扇区，忽略返回值
           → txg_group.transition()
           → arc.flush_dirty()                      // 只统计，不回写
```

  其中没有一次 flush；`serialize` 失败时 `root_bp` 保持旧值但 uberblock 仍然发布，且函数仍返回 0（C-08）。

### 2.7 块设备抽象与 DMA 边界

`BlockDevice` trait 的完整能力是 `blk_read` / `blk_write`，没有 flush、没有 discard/TRIM、没有 barrier（[egfd/mod.rs:90-120](../../src/kernel/privileged/egdf/mod.rs#L90-L120)）。`functions/driver/block.rs` 只是对 `proto_block` 的薄包装。

设备驱动里**有** flush 原语，但 FS 用不到：virtio-blk 定义了 `BlkRequest::flush()`（[blk.rs:127-137](../../src/kernel/functions/driver/virtio/blk.rs#L127-L137)），ATA 有 `flush_cache`（[ata.rs:482-492](../../src/kernel/functions/driver/storage/ata.rs#L482-L492)），全仓除定义处无调用者。也就是说：**写序能力在驱动层"存在"，但被抽象层切断**。

DMA 边界：EGFS 通过 `hdd_read_sector` / `hdd_write_sector` 走 `proto_block`，I/O 缓冲是内核堆上的 `Vec`，不存在用户指针直通；I4/I6 边界形式上守住。但一次 I/O 全程持 `vdevs` 自旋锁（中断关闭），这在多核下把"磁盘延迟"转成"全核 IRQ 关闭时长"。

### 2.8 缓存层

- 页缓存：**没有**。EGFS 只有一层 ZFS 风格的 `EgfsArc`（MRU/MFU/GHOST 双向链表，`HV_ARC_MAX_SIZE = 4096` 条目上限，[arc.rs:12-12](../../src/kernel/functions/fs/egfs/arc.rs#L12-L12)、[arc.rs:248-267](../../src/kernel/functions/fs/egfs/arc.rs#L248-L267)）。它只是读缓存与内存模式的存储，**不是回写缓存**：`flush_dirty()` 的文档明确写"只统计，不执行回写"（[arc.rs:483-497](../../src/kernel/functions/fs/egfs/arc.rs#L483-L497)）。容量参数是**条目数**而非字节数，单个条目可以是整个文件长度的缓冲。
- 元数据缓存：没有独立元数据缓存。挂载时 `deserialize` 把整份元数据读进内存，之后内存副本就是权威状态；`serialize` 时全量回写。
- 目录缓存：`dcache`/`icache` 只被 ramfs 使用并失效（[ramfs_data.rs:1122-1122](../../src/kernel/functions/fs/ramfs_core/ramfs_data.rs#L1122-L1122) 等 5 处）；EGFS 因扁平 ZAP 不需要逐路径查找，也就没有目录项缓存，但因此**没有任何一层能发现"名字→对象"已经损坏**。

### 2.9 fsync / sync / flush 的映射

- `fsync` 走 `vfs_sync()`：遍历所有挂载点并对每个 `fs.fs_sync()` 调用，**没有按 fd 作用域、没有 data/metadata 分离**；遍历期间持有 `VFS_MANAGER.mounts` 全局锁（[vfs_mount.rs:142-165](../../src/kernel/functions/fs/vfs_mount.rs#L142-L165)），注释还写着"内部纯粹计算"——对磁盘模式而言这不成立。
- `EgfsInode::fs_sync()` 直接转 `EgfsData::sync()`（[egfs_inode.rs:412-416](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L412-L416)），而 `sync()` 的返回码恒为 0（C-08）。
- 底层的 flush 原语无法从 trait 调用（§2.7），所以"flush 语义"在整条链上不存在。用户执行 `fsync` 的实际语义 = "重写一份元数据 blob 并写一个 uberblock 扇区，成功与否都报 0"。

### 2.10 挂载发现与平台

- 发现流程：`scan_all_drives()` 逐个读 LBA 2046 找 `"ANTX"` 签名；找不到就用默认分区起点 **LBA 16384**（[egfs_data.rs:159-186](../../src/kernel/functions/fs/egfs/egfs_data.rs#L159-L186)）。全仓没有 GPT/MBR 解析。
- 容量探测：`probe_partition_size_for_drive` 以 `hi = 0xFFFF` 做二分（[egfs_data.rs:445-470](../../src/kernel/functions/fs/egfs/egfs_data.rs#L445-L470)），即**最多寻址 65,535 个扇区 ≈ 32 MiB**；扣除 16,384 扇区起点后，可用分区约 25 MiB。默认 asize 亦为 32 MiB。
- `mount_drive()`：先 `add_vdev`（重建全空闲 metaslab），再 `deserialize`；反序列化失败时**不报错**，回落为空数据集并返回成功（[egfs_data.rs:505-530](../../src/kernel/functions/fs/egfs/egfs_data.rs#L505-L530)）。
- `init()` 在 `mount_drive` 失败时直接 `format_drive()`（[egfs_data.rs:232-240](../../src/kernel/functions/fs/egfs/egfs_data.rs#L232-L240)），随后无条件把 `mounted` 置 true（[egfs_data.rs:276-276](../../src/kernel/functions/fs/egfs/egfs_data.rs#L276-L276)）。这意味着**uberblock 一旦损坏（没有 flush 时是常见事件），下一次启动会静默清空整盘**。
- 平台：EGFS 挂载被 `#[cfg(target_arch = "x86_64")]` 圈死（[lib.rs:938-955](../../src/kernel/lib.rs#L938-L955)）；aarch64 侧**注册了 virtio-blk**（[mod.rs:100-112](../../src/kernel/functions/driver/virtio/mod.rs#L100-L112)），但没有消费者。

### 2.11 其余实现状态

| 实现 | 行数 | 状态 | 证据 |
|---|---:|---|---|
| ext2 | 2,455 | 读写实现齐全，但 VFS 挂载分支为空，不进 `resolve_fs` | [vfs_mount.rs:31-44](../../src/kernel/functions/fs/vfs_mount.rs#L31-L44)、[mod.rs:96-118](../../src/kernel/functions/fs/mod.rs#L96-L118) |
| exfat | 1,009 | 同上 | 同上 |
| virtiofs | 351 | 桩：无 virtio-fs 协议、无请求队列 | [virtiofs.rs:1-40](../../src/kernel/functions/fs/virtiofs.rs#L1-L40) |
| overlayfs | 823 | 实现存在，文件目录 4 个，与真实 upper/lower 块设备无关 | [overlayfs.rs:1-35](../../src/kernel/functions/fs/overlayfs.rs#L1-L35) |
| tmpfs | 380 | ramfs 包装 | [tmpfs.rs:1-30](../../src/kernel/functions/fs/tmpfs.rs#L1-L30) |
| ramfs | ~2,340 | 无盘启动的实际根，dcache 失效完整 | [ramfs_data.rs:1122-1122](../../src/kernel/functions/fs/ramfs_core/ramfs_data.rs#L1122-L1122) |

`resolve_fs()` 只映射 `tmpfs` 与 `overlay`（[mod.rs:96-118](../../src/kernel/functions/fs/mod.rs#L96-L118)），因此只有 EGFS（经 `egfs_fs()` 单例）与这两类能真正被挂载。ext2/exfat/virtiofs 是"有代码、无入口"的第三类。

## 三、不足清单

### 3.1 崩溃一致性

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| C-01 | 目录项序列化把 `obj_id` 硬写成 0，重挂载后全部名字指向对象 0 | [egfs_data.rs:1539-1539](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1539-L1539)、[egfs_data.rs:1663-1663](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1663-L1663) | S0 |
| C-02 | 无 journal / intent log；`zil` 纯内存，`zil_persist` 零调用，挂载不重放 | [zil.rs:323-346](../../src/kernel/functions/fs/egfs/zil.rs#L323-L346)、[zil_persist.rs:1-40](../../src/kernel/functions/fs/egfs/zil_persist.rs#L1-L40) | S1 |
| C-03 | 写入链路无 flush/barrier；`BlockDevice` trait 无此能力，驱动层 flush 无调用者 | [egfd/mod.rs:90-120](../../src/kernel/privileged/egdf/mod.rs#L90-L120)、[spa.rs:453-469](../../src/kernel/functions/fs/egfs/spa.rs#L453-L469) | S1 |
| C-04 | uberblock 128 槽环形复用，挂载时从 127 向下取"第一个有效"而非最大 txg；同步超过 128 次后可能取到旧根 | [spa.rs:480-505](../../src/kernel/functions/fs/egfs/spa.rs#L480-L505) | S1 |
| C-05 | 数据/元数据块 `bp.checksum` 从不计算，`read_bp` 从不验证；torn write 静默接受 | [spa.rs:403-409](../../src/kernel/functions/fs/egfs/spa.rs#L403-L409)、[spa.rs:434-450](../../src/kernel/functions/fs/egfs/spa.rs#L434-L450) | S1 |
| C-06 | 挂载失败即自动 `format_drive()`，静默清空全盘；无确认、无只读降级 | [egfs_data.rs:232-240](../../src/kernel/functions/fs/egfs/egfs_data.rs#L232-L240) | S0 |
| C-07 | 元数据反序列化失败回落空数据集且 `mounted=true`；下一次 sync 覆盖旧根 | [egfs_data.rs:505-530](../../src/kernel/functions/fs/egfs/egfs_data.rs#L505-L530) | S1 |
| C-08 | `sync()` 在 `serialize` 失败时仍写 uberblock 并返回 0（fsync 假成功） | [egfs_data.rs:1424-1446](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1424-L1446) | S1 |
| C-09 | 元数据 block 无校验和，坏块只能靠"解析边界失败"偶然发现；无 fsck、无 scrub 实现 | [spa.rs:434-450](../../src/kernel/functions/fs/egfs/spa.rs#L434-L450)、[spa.rs:239-242](../../src/kernel/functions/fs/egfs/spa.rs#L239-L242) | S1 |

断电后的具体损坏形态（例如哪个结构先坏、是否可自愈）**属于推测**：代码能证明的是"没有写序、没有校验、没有恢复机制"，实际损坏面取决于设备缓存行为，仓库内没有任何注入实验数据。

### 3.2 空间管理

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| S-01 | 分配位图不持久：每次 `add_vdev` 无条件重建全空闲 metaslab，挂载后分配从起点重新开始，覆盖上一轮数据 | [spa.rs:334-367](../../src/kernel/functions/fs/egfs/spa.rs#L334-L367) | S0 |
| S-02 | COW 不释放旧块（只在写失败时 free），每次写永久泄漏一个 extent；unlink 才释放最新 bp | [egfs_data.rs:765-770](../../src/kernel/functions/fs/egfs/egfs_data.rs#L765-L770)、[egfs_data.rs:850-853](../../src/kernel/functions/fs/egfs/egfs_data.rs#L850-L853) | S1 |
| S-03 | 最后一个 metaslab 的 size 算成 `asize - ms_start + HV_VDEV_LABEL_SIZE`，多算 256 KiB，可越过 vdev 边界 | [spa.rs:352-356](../../src/kernel/functions/fs/egfs/spa.rs#L352-L356) | S1 |
| S-04 | 无 TRIM/discard：`BlockDevice` 无接口，vdev 的 `supports_trim` 字段无消费者 | [egfd/mod.rs:90-120](../../src/kernel/privileged/egdf/mod.rs#L90-L120)、[vdev.rs:73-110](../../src/kernel/functions/fs/egfs/vdev.rs#L73-L110) | S2 |
| S-05 | 无配额、无预留、无按数据集限流；单池单数据集 | [dataset.rs:60-130](../../src/kernel/functions/fs/egfs/dataset.rs#L60-L130) | S2 |
| S-06 | 碎片治理只有"选空闲最多的 metaslab + 首次适配"，无分配游标、无后台整理 | [spa.rs:382-396](../../src/kernel/functions/fs/egfs/spa.rs#L382-L396)、[metaslab.rs:135-205](../../src/kernel/functions/fs/egfs/metaslab.rs#L135-L205) | S3 |

### 3.3 并发

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| L-01 | 块 I/O 全程持 `vdevs` 自旋锁（关中断），磁盘延迟直接变成全核 IRQ 关闭时长 | [spa.rs:434-450](../../src/kernel/functions/fs/egfs/spa.rs#L434-L450)、[spa.rs:453-469](../../src/kernel/functions/fs/egfs/spa.rs#L453-L469) | S1 |
| L-02 | `vfs_sync()` 持挂载表全局锁遍历并逐 FS 落盘 | [vfs_mount.rs:142-165](../../src/kernel/functions/fs/vfs_mount.rs#L142-L165) | S1 |
| L-03 | 对象更新是 `get → 改副本 → update_obj`，无 per-object 锁；两个核同时写同一文件会丢更新 | [egfs_data.rs:780-783](../../src/kernel/functions/fs/egfs/egfs_data.rs#L780-L783) | S2 |
| L-04 | `fds` 是进程无关的全局 256 槽表，全系统共享上限；与 VFS 每进程 fd 表分离 | [egfs_data.rs:43-43](../../src/kernel/functions/fs/egfs/egfs_data.rs#L43-L43)、[egfs_data.rs:87-87](../../src/kernel/functions/fs/egfs/egfs_data.rs#L87-L87) | S2 |
| L-05 | `sync()` 持 `datasets` 锁做全量快照与序列化，写路径无法并行 | [egfs_data.rs:1474-1500](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1474-L1500) | S3 |
| L-06 | 锁序文档把"文件系统特定锁"作为一个桶，EGFS 内部 5 把锁没有声明次序 | [ref-lock-order.md:44-58](../explain/ref-lock-order.md#L44-L58) | S3 |

### 3.4 语义完整性（POSIX）

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| P-01 | offset 被 `Inode::read/write` 与 `FileSystem::fs_read/fs_write` 两层丢弃；写入整块替换；部分写会丢掉旧前缀；读越界可下溢 panic | [egfs_inode.rs:28-45](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L28-L45)、[egfs_inode.rs:198-236](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L198-L236)、[egfs_data.rs:703-719](../../src/kernel/functions/fs/egfs/egfs_data.rs#L703-L719)、[egfs_data.rs:752-779](../../src/kernel/functions/fs/egfs/egfs_data.rs#L752-L779) | S0 |
| P-02 | 目录不可枚举：`fs_readdir` 返回 NotSupported，`is_dir()` 恒 false | [egfs_inode.rs:95-96](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L95-L96)、[egfs_inode.rs:347-355](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L347-L355) | S0 |
| P-03 | `truncate` 未实现 | [egfs_inode.rs:81-86](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L81-L86) | S1 |
| P-04 | symlink 只 `allocate` 不 `write_bp`，目标字符串从不落盘；`readlink` 只查一个不存在的 ARC key，恒 EIO | [egfs_data.rs:1095-1102](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1095-L1102)、[egfs_data.rs:1211-1223](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1211-L1223) | S1 |
| P-05 | 硬链接 `unlink` 不看 `link_count`，直接释放共享数据块；另一名字后续读脏 | [egfs_data.rs:850-853](../../src/kernel/functions/fs/egfs/egfs_data.rs#L850-L853)、[egfs_data.rs:1158-1162](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1158-L1162) | S1 |
| P-06 | xattr 只存值的哈希到 4 个槽，名字也哈希；`getxattr` 返回哈希字节，`listxattr` 无法还原名字 | [egfs_data.rs:1256-1270](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1256-L1270)、[egfs_data.rs:1300-1311](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1300-L1311) | S1 |
| P-07 | 时间戳是 TSC / 虚拟计数器原值（`timestamp()`），不是墙钟秒或纳秒 | [egfs_data.rs:772-772](../../src/kernel/functions/fs/egfs/egfs_data.rs#L772-L772)、[arch.rs:28-30](../../src/kernel/privileged/cpu/arch.rs#L28-L30) | S2 |
| P-08 | 无 POSIX uid/gid；权限是 PWM 令牌（`check_permission`），无 ACL、无 setuid/sticky | [egfs_data.rs:841-843](../../src/kernel/functions/fs/egfs/egfs_data.rs#L841-L843)、[dmu.rs:68-96](../../src/kernel/functions/fs/egfs/dmu.rs#L68-L96) | S2 |
| P-09 | `open` 只识别自定义 CREAT/APPEND；无 O_TRUNC、O_EXCL；APPEND 只把偏移设为 size，写入仍整块替换 | [egfs_data.rs:592-620](../../src/kernel/functions/fs/egfs/egfs_data.rs#L592-L620) | S2 |
| P-10 | `rename` 在内存中是一步原子，但没有持久化序：崩溃后只能回到"上次 sync 的整份元数据" | [egfs_data.rs:1008-1032](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1008-L1032) | S2 |
| P-11 | 单 extent：无间接块/无块树，文件物理组织上限是一个连续分配区 | [dmu.rs:80-80](../../src/kernel/functions/fs/egfs/dmu.rs#L80-L80)、[egfs_data.rs:759-770](../../src/kernel/functions/fs/egfs/egfs_data.rs#L759-L770) | S1 |
| P-12 | 无稀疏文件：`allocate` 按 `size` 向上取整并真实占用连续空间 | [spa.rs:377-377](../../src/kernel/functions/fs/egfs/spa.rs#L377-L377) | S3 |

此外 `fs_resolve_inode` 返回的 inode 带空 `rel_path`（[egfs_inode.rs:418-426](../../src/kernel/functions/fs/egfs/egfs_inode.rs#L418-L426)），所以经 inode 句柄发起的 chmod/chown/utimens 全部落到 `FileNotFound`。

### 3.5 性能

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| F-01 | 每次 sync 全量重写元数据：对象记录 222 B × 最多 65,536 条，最坏约 14.5 MiB/次；单次 sync 需一次连续分配，碎片化时直接失败 | [egfs_data.rs:1448-1470](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1448-L1470) | S1 |
| F-02 | 每次写都新分配一个 4 KiB 对齐 extent 且不回收；写 100 B 实际占用 4 KiB（40 倍写放大） | [spa.rs:377-412](../../src/kernel/functions/fs/egfs/spa.rs#L377-L412)、[egfs_data.rs:759-770](../../src/kernel/functions/fs/egfs/egfs_data.rs#L759-L770) | S1 |
| F-03 | 压缩恒 Off；去重零调用；"Zstd" 实为 RLE | [egfs_data.rs:756-758](../../src/kernel/functions/fs/egfs/egfs_data.rs#L756-L758)、[compress.rs:309-338](../../src/kernel/functions/fs/egfs/compress.rs#L309-L338) | S2 |
| F-04 | 读一个字节也把整块（乃至整文件 extent）读进内存；ARC 上限是 4096 条目而非字节 | [egfs_data.rs:703-713](../../src/kernel/functions/fs/egfs/egfs_data.rs#L703-L713)、[arc.rs:12-12](../../src/kernel/functions/fs/egfs/arc.rs#L12-L12) | S2 |
| F-05 | 无预读、无请求合并、无 writeback；顺序读每次一个新分配或无缓存命中 | [arc.rs:483-497](../../src/kernel/functions/fs/egfs/arc.rs#L483-L497) | S2 |
| F-06 | 分配是 O(metaslab 数 × 16,384 位) 首次适配，无游标 | [metaslab.rs:135-205](../../src/kernel/functions/fs/egfs/metaslab.rs#L135-L205) | S3 |

### 3.6 可靠性验证

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| V-01 | QEMU 启动腿全部不挂磁盘（脚本内无 `disk`/`drive`/`virtio-blk` 参数），EGA 磁盘模式在 CI 中零执行 | [qemu_boot_test.sh:235-235](../../scripts/qemu_boot_test.sh#L235-L235)（全脚本无磁盘参数） | S1 |
| V-02 | `egfs_persist_test.rs` 名为持久化，实际只做内存模式往返，无 mock 盘、无 remount | [egfs_persist_test.rs:1-98](../../host-tests/tests/egfs_persist_test.rs#L1-L98) | S1 |
| V-03 | `e2e_crash_zil_replay` 的"崩溃"是内存 ZIL 状态机，无磁盘、无断电、无重放 | [egfs_e2e_test.rs:49-170](../../host-tests/tests/egfs_e2e_test.rs#L49-L170) | S1 |
| V-04 | `fsx` 的四个"文件系统"用例实为宿主 `std::fs` 目录，不测任何内核 FS | [fsx.rs:1-22](../../host-tests/src/fsx.rs#L1-L22)、[fsx_integration_test.rs:55-124](../../host-tests/tests/fsx_integration_test.rs#L55-L124) | S1 |
| V-05 | 无掉电注入、无块损坏注入、无 metadata parser fuzz、无长稳 | 全树 grep `power|crash|corrupt|fsck|scrub|fault` 无命中 | S2 |
| V-06 | 无 fsck / scrub 实现；`scrub_block` 无调用者，`scrub_in_progress` 只是字段 | [raidz.rs:423-434](../../src/kernel/functions/fs/egfs/raidz.rs#L423-L434)、[spa.rs:239-242](../../src/kernel/functions/fs/egfs/spa.rs#L239-L242) | S2 |
| V-07 | 现有测试集中在纯函数（81 个内联 + 19 个 host 用例），写路径/挂载/落盘时序不在覆盖内 | 内联 `#[test]` 计数 81；host 用例 7+4+7+1 | S3 |

六门槛现状：EGFS 的"能过门槛"只覆盖编译、clippy/fmt、审计、host 单测、QEMU 无盘启动、内核单测；**磁盘挂载→写入→重启→读回**这条主线不在任何门槛内。

### 3.7 架构一致性与冗余

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| A-01 | ext2/exfat 在 VFS 挂载分支是空实现，`resolve_fs` 也不返回它们 ⇒ 有实现、不可达 | [vfs_mount.rs:31-44](../../src/kernel/functions/fs/vfs_mount.rs#L31-L44)、[mod.rs:96-118](../../src/kernel/functions/fs/mod.rs#L96-L118) | S1 |
| A-02 | 四套 FS 各自实现命名空间、分配、读写在 32,709 行里重复；VFS 只提供 trait 形状，没有可复用的块/缓存内核 | [fs/ 目录统计](../../src/kernel/functions/fs) | S1 |
| A-03 | virtiofs 是 351 行桩，无协议/队列/挂载入口 | [virtiofs.rs:1-40](../../src/kernel/functions/fs/virtiofs.rs#L1-L40) | S2 |
| A-04 | dedup / raidz / scrub / 快照持久化 / zil_persist 均未接线；快照 syscall 可达但只存内存 | [egfs_data.rs:1933-1941](../../src/kernel/functions/fs/egfs/egfs_data.rs#L1933-L1941)、[dedup.rs:1-40](../../src/kernel/functions/fs/egfs/dedup.rs#L1-L40) | S2 |
| A-05 | `Inode` trait 用默认 `NotSupported` 掩盖能力缺失，既不声明 FS 能力位，也不让 VFS 降级 | [inode.rs:40-95](../../src/kernel/functions/fs/inode.rs#L40-L95) | S3 |
| A-06 | 无 `docs/explain/` FS 子系统文档、无 `docs/design/` FS 设计立场文档；唯一现状说明是旧报告 | docs 树无对应文件 | S3 |

### 3.8 平台

| ID | 现象 | 证据 | 级别 |
|---|---|---|---|
| T-01 | EGFS 挂载 `#[cfg(target_arch = "x86_64")]`；aarch64 有 virtio-blk 但无任何 FS 消费 | [lib.rs:938-955](../../src/kernel/lib.rs#L938-L955)、[virtio/mod.rs:100-112](../../src/kernel/functions/driver/virtio/mod.rs#L100-L112) | S1 |
| T-02 | 分区起点硬编码 LBA 16384，全仓无 GPT/MBR 解析；签名 `"ANTX"` 由外部写入，`format_drive` 不写它 | [egfs_data.rs:159-186](../../src/kernel/functions/fs/egfs/egfs_data.rs#L159-L186)、[egfs_data.rs:345-367](../../src/kernel/functions/fs/egfs/egfs_data.rs#L345-L367) | S2 |
| T-03 | 容量探测上限 0xFFFF 扇区（≈32 MiB），再扣起点只剩 ≈25 MiB；超过部分永远不可达 | [egfs_data.rs:445-470](../../src/kernel/functions/fs/egfs/egfs_data.rs#L445-L470) | S2 |

## 四、与 ext4 / XFS / btrfs / ZFS 的对照

| 机制 | ext4 | XFS | btrfs | ZFS | EGFS 现状 | 判断 |
|---|---|---|---|---|---|---|
| 崩溃一致性 | JBD2 journal（metadata / ordered / writeback） | 元数据日志 + 顺序写数据 | COW + 双 superblock 根 | COW + uberblock 链 + ZIL | 无 journal；ZIL 内存；uberblock 环 | **未完成**（骨架在，语义不在） |
| 写序 | journal 提交带 flush/FUA | log 带 flush | COW 事务提交带 barrier | txg 提交带 flush | trait 无 flush | **方向错**：抽象层切断了设备能力 |
| 校验和 | 元数据 csum（可选） | 元数据 CRC32C | 每块 CRC32C | 每块 Fletcher2/4/SHA256，嵌入块内 | 仅 uberblock | **未完成**（算法有，覆盖=0） |
| COW | 否 | 否 | 是 | 是 | 是，但不回收旧块 | **方向对，实现半截** |
| 空间管理 | extent + 位图 | AG + B+tree 空闲空间 | free-space tree（持久） | metaslab + spacemap（持久） | metaslab + 内存位图（不持久） | **未完成**（最关键缺口） |
| 压缩 | 无 | 无 | zlib/lz4/zstd | lz4/zstd（成熟实现） | 自研 LZ4 + RLE 冒充 Zstd，未启用 | **方向存疑**：私格式不可维护 |
| 去重 | 无 | 无（reflink 邻近） | 无原生 | DDT（内存，代价高，通常默认关） | 内存 CAS，未接线 | **保留为实验特性即可** |
| 快照 | 无 | reflink | 原生 | 原生 | 内存快照，不持久 | **未完成** |
| fsck/scrub | e2fsck | xfs_repair | btrfs check（弱） | zpool scrub（在线） | 无 | **未完成** |
| 硬链接/符号链接/readdir | 完整 | 完整 | 完整 | 完整 | 硬链接破坏、符号链接不可用、readdir 不可用 | **未完成** |

**合理取舍**（保留）：

- COW + 根指针 + uberblock 环的总体形态与 ZFS 同源，方向正确；问题在执行细节（不回收旧块、槽位选择、无 flush）。
- Fletcher4 作默认块校验、SHA256 作可选强校验，与 ZFS 的算法谱系一致，性能/强度可分级。
- metaslab + 权重选盘（挑空闲最多者）避免了最坏碎片落点，是合理的最小策略。
- 单 extent 起步、暂不做块树，对"先跑通"是可接受的阶段简化——但它必须与"文件大小上限"一起对外声明，目前没有。

**尚未做完**（补课）：

- 持久化 spacemap（ZFS 用 spacemap 链 + uberblock 指针；EGFS 完全没有）。
- 事务提交序（ZFS 的 txg 四阶段：open→quiesce→sync→commit）与 flush 点。
- ZIL 的磁盘形态与挂载重放（`zil_persist.rs` 有素材，缺接线）。
- 校验和覆盖到每个数据/元数据块，读时验证并在 scrub 时修复。
- 目录索引（每目录子项 ZAP）与 readdir。

**方向存疑**（应重新考虑）：

- **私研 LZ4/"Zstd"**：算法不带兼容性，且 `compress_zstd_fallback` 实际是 RLE，标签会误导后续维护者。压缩要么接标准实现（哪怕先放用户态），要么明确标成实验。
- **内存去重表**：ZFS 的 DDT 已被业界证明是"内存换空间"的高代价特性；EGFS 连持久化都未做，保留代码只会增加审计面。建议移出内核路径，作为实验模块保留。
- **全量元数据重写**：每 sync 写 14.5 MiB 的模型不适合"日常可用"，更不适合断电窗口最小化。应改为增量 dirty 集合 + 有序写。
- **自动 format 作为恢复策略**：任何生产文件系统都不会把"元数据读不出"处理成"清空重来"。这是方向性错误，必须改。

## 五、长期最优规划

### 5.1 路线原则

1. **先让"跨重启正确"成立，再谈性能与特性**。当前最缺的不是压缩/去重/快照，而是"数据在重启后还在"。
2. **每次只引入一个持久化不变量**（校验和→写序→spacemap→WAL），每个不变量都配一条可注入故障的门禁。
3. **能力建设与缺陷修复分开验收**：缺陷修复必须补齐回归测试；能力建设必须有明确的格式版本与回滚路径。
4. 所有新格式字段都进"on-disk v1"清单，变更需版本号与兼容读路径。

### 5.2 阶段一：缺陷修复（目标：跨重启不丢已 sync 的数据）

| 项 | 决策 | 验证 |
|---|---|---|
| F1-1 目录项对象号 | `write_dir_entry_record` 传入真实 `obj_id`；反序列化侧加 `obj_id < obj_count` 边界检查 | host 单测：create→sync→模拟重启→lookup 命中同一对象 |
| F1-2 spacemap 持久化 | 二选一：(a) 位图随元数据 blob 一起序列化；(b) 独立 spacemap 区 + uberblock 指针。优先 (a)，改动面小 | 重启后写入不覆盖旧数据：写 A、重启、写 B，读 A 一致 |
| F1-3 校验和覆盖 | 先覆盖元数据 blob 与目录记录，再覆盖数据块；写时算、读时验、失败返 EIO | 坏块注入：翻转 1 字节 → 读返回 EIO 而非错数据 |
| F1-4 写序 | `BlockDevice` 增加 `blk_flush`；virtio/ATA 接到现有 flush 原语；`sync()` 固定为 data→flush→meta→flush→uber→flush；`write_sector` 错误必须上抛 | 日志断言：flush 调用次数与顺序；断电模拟丢未 flush 写 |
| F1-5 uberblock 选择 | 扫描全部 128 槽取 `max(txg)`；写入时同时写主/备两份并各自带 CRC | 单测：写 200 个 txg 后 mount 必须取到 200 |
| F1-6 恢复策略 | 移除"挂载失败自动 format"；改为只读挂载 + 显式 `format` 要求；反序列化失败返回错误而非空数据集 | 坏 uberblock/坏 blob → mount 返回错误且不写盘 |
| F1-7 写路径语义 | `write` 改为"读旧 extent → 合并 → 写新 extent → 释放旧 extent"；`read` 越界返回 0 或错误，不得 panic | POSIX 单测：offset 追加/覆盖/跨块；读越界不崩 |
| F1-8 错误传播 | `sync()` 任一阶段失败返回负 errno；`fsync` 不再假成功 | 注入分配失败 → fsync 返回 ENOSPC |

**验证接入六门槛**：

1. `./ci/build.sh all`：新增 `blk_flush` 后双架构仍 0 warning。
2. clippy/fmt：不变。
3. `bash ci/audit.sh full`：新 unsafe（若有）配 SAFETY；不改 functions 边界。
4. `make test-host`：新增 `MockBlockDevice`（扇区数组 + 故障注入），覆盖"格式化→写→模拟掉电→重挂载→读"矩阵；现有 4 个 egfs 测试文件改为真实 mock 盘。
5. `qemu_boot_test.sh all`：新增一条**带 virtio-blk 磁盘镜像**的腿（镜像内预置 `"ANTX"` 签名），断言 `mount egfs` 日志、创建文件、重启后读回。
6. `make test-kernel-host`：无变化。

**估算（估算）**：3–5 人月。依据：EGFS 当前 9,300 行；本阶段改动集中在 `spa.rs` / `metaslab.rs` / `egfs_data.rs` / `egfd` trait / virtio+ATA 驱动，约 1.5–2k 行改 + 新增 mock 与用例 1–1.5k 行；按每 40–60 行有效实现配同量测试约需 60–90 人日，加两次架构评审与 QEMU 调试，折算 3–5 人月。

### 5.3 阶段二：达到"日常可用"

| 方向 | 关键决策 | 验证 |
|---|---|---|
| POSIX 语义 | 完整实现 readdir（每目录子项 ZAP）、truncate、symlink 数据落盘、hardlink 引用计数、xattr 存值、uid/gid/mode、O_TRUNC/O_EXCL；时间戳接 RTC | 以 fsx/POSIX 兼容用例驱动真实内核 FS（不是宿主 `std::fs`） |
| 持久化事务 | 二选一：(a) 引入 intent log（redo）+ 提交屏障；(b) 继续全量 COW 快照但做增量 dirty 集合与有序写。推荐 (b) 作为主路径、(a) 仅用于 fsync 加速 | 掉电注入：在每个 txg 边界丢写，重挂载后要么旧态要么新态，无中间态 |
| 并发 | per-object 锁 + 分配器锁分离；块 I/O 不持关中断锁；fd 表按进程 + 全局池 | 多核压力：两进程同写一文件、并发 sync；`-smp 4` 腿 |
| 空间 | 释放旧 extent（引用计数或 COW 引用集）+ 持久 spacemap + 分配游标 | 写满/删空循环后空间回到初值 |
| fsck | host 侧离线 fsck（读 spacemap 与对象树做交叉校验）+ 挂载时轻量校验 | 人为制造三类损坏，fsck 能定位并给出可修复/不可修复判定 |
| 平台 | aarch64 磁盘根接入（复用 virtio-blk + 同一 EGFS） | aarch64 QEMU 挂盘腿 |
| 快照 | 要么持久化（快照根进 uberblock），要么从 syscall 面移除 | 重启后快照仍可回滚（若保留） |

**估算（估算）**：8–12 人月。依据：POSIX 面与块树/间接块约 2.5–3k 行；事务与 intent log 1–1.5k 行；并发改造 0.8–1.2k 行；fsck 0.8–1k 行；测试矩阵（掉电/损坏/fuzz/多核）约 2–3k 行。按同量测试与三轮门禁调试折算。

### 5.4 阶段三：达到"工业级可信"

- 校验和全覆盖 + 在线 scrub + 副本或 raidz 自愈（把 `raidz.rs` 与 `scrub_block` 真正接入写读路径）。
- DDT 持久化 + 按数据集可选去重 + 内存上限/水位；压缩换标准实现并给出格式标识。
- 多设备池、快照/send-recv、在线扩容、格式升级与向后兼容读。
- 长稳：多日循环挂载/掉电/坏块；性能基线纳入 `host-tests/benches` 并每次 PR 更新。

**估算（估算）**：18–30 人月。依据：仅"校验和全覆盖 + scrub + 自愈"就涉及格式、写路径、读路径、后台任务与测试四线；多设备与升级兼容再把状态机翻倍。

### 5.5 排序理由与"缺陷修复 / 能力建设"分界

- 阶段一全部是**缺陷修复**：修的是"已声明能力不成立"（持久化、校验、写序、错误传播）。
- 阶段二是**缺陷修复 + 能力建设混合**：POSIX 语义与并发属于"日常可用"的必要修复；intent log/fsck 属于能力建设。
- 阶段三是**能力建设**：副本自愈、在线 scrub、去重、多设备。
- 顺序不可交换：没有 F1-2（持久 spacemap），阶段二的"释放旧 extent"会立刻把新分配踩到旧数据；没有 F1-4（flush），阶段二的掉电注入无法定义"什么叫提交点"。

## 六、需要拍板的决策点

| ID | 决策 | 选项 A（利 / 弊） | 选项 B（利 / 弊） | 建议 |
|---|---|---|---|---|
| D-01 | 自研 EGFS 是否继续投入，还是收敛到 ext4 兼容/移植 | A 继续：格式自主、可做校验和/快照等差异化；弊：当前 9.3k 行的持久化骨架需重做，维护面长期由本团队承担 | B 收敛：可复用 ext2 现有 2.4k 行并补 ext4 journal，工具链/互操作成熟；弊：ext4 的 unsafe/复杂 journal 进 TCB，差异化特性（校验和、快照）需另起 | 先完成阶段一（3–5 人月）再决策；若阶段一后仍无"跨重启正确"，则转 B |
| D-02 | 去重是否保留 | A 保留：潜在空间收益；弊：DDT 内存代价业界公认高，当前零调用、零测试，纯审计负担 | B 移出内核路径、保留实验模块；弊：放弃一个"卖点" | 选 B。等阶段三再评估持久 DDT |
| D-03 | 压缩实现 | A 自研 LZ4/RLE；弊：私格式、无兼容、标签误导 | B 接标准 LZ4/Zstd（先用户态或引入受审计 crate）；弊：依赖与 TCB 评估成本 | 选 B；在此之前把 `Off` 作为唯一合法值并在格式层显式声明 |
| D-04 | 校验和算法与覆盖 | A Fletcher4 全覆盖（快、弱）；B Fletcher4 数据 + SHA256 元数据（强、慢） | 两者都可行 | 选 B：元数据量小，SHA256 成本可接受；数据块用 Fletcher4 |
| D-05 | 崩溃一致性模型 | A intent log（redo）+ 屏障（类 ext4/XFS）；弊：双写、实现复杂 | B 纯 COW 快照 + 增量 dirty + uberblock 双写（类 ZFS）；弊：fsync 延迟高，需 txg 四阶段 | 选 B 为主 + 对 fsync 热点补小型 intent log（混合） |
| D-06 | 快照语义 | A 持久化快照；B 从 syscall 面移除直到有持久化 | — | 选 B（当前"内存快照 + 对外 syscall"是最差的组合） |
| D-07 | 平台优先级 | A 先 x86 打磨；B 同时补 aarch64 磁盘根 | — | 阶段一后补 aarch64（复用同一 FS，成本主要是驱动与 QEMU 腿） |
| D-08 | on-disk 格式冻结 | A 现在冻结 v1 并写兼容读；B 继续演进不冻结 | — | 选 A：阶段一落地即冻结，否则每次改动都在扩大不可恢复窗口 |

## 七、明确标注的不确定项

**推测（有代码依据但未用实验证实）**

- 断电后的具体结构损坏形态与优先级（章节 3.1 末）：代码证明"无写序/无校验/无恢复"，但何种结构先坏取决于设备缓存与调度。
- 磁盘模式下 initramfs 与 EGFS 根的实际运行行为（解包写入 EGFS、`/init` 读取是否成功）：无磁盘 QEMU 腿，未实测。
- 空间位图重置后的覆盖顺序（先覆盖元数据还是先覆盖文件数据）：取决于分配序列，属推理。
- 各缺陷的实际触发概率（例如"某个 AP 起不来"与"uberblock torn"的现场概率）：无故障注入统计。

**未验证**

- ext2/exfat 是否在历史版本中被挂载过：仓库内无记录。
- `zil_persist.rs` 是否曾经接线后又被摘除：未查 git 历史。
- ARC 的实际命中率与内存占用：无基准数据。
- aarch64 磁盘挂载的可行性与性能：无挂载路径、无实测。
- `format_drive` 写入的分区起点是否与外部 `"ANTX"` 约定一致：无外部工具源码可查。

**估算**

- 阶段一 3–5 人月、阶段二 8–12 人月、阶段三 18–30 人月：均为量级估算，依据是当前代码规模（EGFS 9,300 行 / 全 fs 32,709 行）、改动面行数与"实现:测试 ≈ 1:1"的项目惯例，不含需求变更与外部依赖等待。

## 八、既有文档一致性核对

| 文档 | 陈述 | 与现状是否一致 |
|---|---|---|
| [filesystem-subsystem-quality-assessment.md:13](filesystem-subsystem-quality-assessment.md#L13-L13) | "checksum 读路径做 Fletcher2/4 校验并有损坏检测" | **不一致**：`read_bp` 不校验，数据块 `bp.checksum` 从不赋值（C-05） |
| 同上 | "sync() 调 `zil.sync(txg)` + `arc.flush_dirty()` 落盘" | **不一致**：`zil.sync` 只清内存，`flush_dirty` 只统计不回写（C-02、§2.8） |
| 同上 | "持久化链路已接进 `init()`" | **结构上一致**，但链路在 C-01/S-01/C-08 处断开，不能推出"可持久化" |
| 同上 | "默认启动实际只挂载一颗 ramfs" | **不一致**：有盘时 `"/"` 是 EGFS，无盘才是 ramfs（§2.1） |
| [subsystem-gap-deep-audit.md:29](subsystem-gap-deep-audit.md#L29-L29) | UNKFS 目录项对象号清零、ZIL 内存、`zil_persist` 零调用、持久化测试全内存 | **结论一致**；路径已改名（`unkfs_data.rs` → `egfs_data.rs`，当前行号为 1663） |
| [subsystem-gap-deep-audit.md:33](subsystem-gap-deep-audit.md#L33-L33) | raidz 仅被 `pub mod` 引用、dedup 生产写路径未接入 | **一致** |
| [syscall-followup.md:1243-1256](../plan/syscall-followup.md#L1243-L1256) | 零引用 pub fn 清单含 FS 侧条目（报告级，不阻断） | **一致**：`cas_aware_write` 等仍零调用者 |
| [ref-lock-order.md:44-58](../explain/ref-lock-order.md#L44-L58) | 文件系统特定锁作为一个桶，未声明内部锁序 | **一致但不足**：EGFS 内部 5 把锁无次序（L-06） |
| `docs/explain/`、`docs/design/` | 无 FS 子系统文档 | **缺失**（A-06） |

本报告只读取代码与文档，未修改任何源码，未修改 `AGENTS.md`。

## 修订记录

- **日期**：2026-11-09
- **授权来源**：用户授权
- **涉及位置**：§5.4 阶段三：达到"工业级可信"（原第 318 行附近），原句「性能基线纳入 `host-tests/benches` 并每次 PR 更新」
- **修订说明**：其中"每次 PR 更新"已不适用——本文正文只读，后续读数应以新报告形式发布，不在本文更新。
- **范围声明**：本文其余结论与数据未变；本节不修改任何既有正文。除上述一处外，未发现其它同类"随 PR/时间更新读数"的承诺句。
