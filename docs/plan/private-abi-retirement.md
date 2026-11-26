# EG 私有 ABI / syscall 退役台账（Linux 生态路线）

> 裁定：对外只承诺 **Linux ABI**；EG 私有号目标态为 **0**。若未来确有「Linux 无等价 + 真实产品用途」的需求，按本册三条件重新申请，不在本轮保留。
> 本册是退役台账，不是一次性删除清单。盘点基准：2026-11-24，
> `src/kernel/privileged/syscall/types.rs` + `src/rust/edgine-abi` 共 **76 个私有常量**；
> 另有 `PRIVATE_LEGACY_BASE` 下 31 个 x86-only 遗留映射，单独处理。
> 关联：[syscall-followup.md](./syscall-followup.md)、[wasm-userspace-migration.md](./wasm-userspace-migration.md)、
> [service-placement-design.md](../design/service-placement-design.md)、
> [egfs-portability-design.md](../design/egfs-portability-design.md)、
> [egfs-portability-project.md](./egfs-portability-project.md)。

---

## 一、判定规则

一个私有号只有同时满足下面三条才保留：

1. **Linux 无等价机制**：Linux 不能通过 syscall、`ioctl`、`sysfs`、`netlink`、`cgroupfs`、
   `debugfs`、`efivarfs` 或 libc 提供同等语义。
2. **有真实产品用途**：有实际出货的 EG 原生程序或内核服务在调用，不是只有 wrapper 或测试。
3. **有明确 owner 与测试**：有文档、有 host/集成测试、有退役或演进负责人。

不满足的，进入「迁移」或「直退」；**不新增私有号**，直到本轮退役收口。

---

## 二、最终保留集：0 个

依据 [sgeg-design.md](../design/sgeg-design.md) 的最终形态与 **OQ-6 选 A**：

- SGEG v2 是**内核安全模块（机制）**，不是安全服务；内核只保留主体标签、客体标签、转移规则、判定四个原语。
- **不新增任何 syscall**；现有 **32 条 `SYS_SGEG_*` 一次性全部删除**，不改造、不兼容保留。
- 策略加载走“写既有路径文件”，内省走 `/proc/<pid>/attr/...` / `getxattr`，审计走既有日志面。
- 账户、密码、登录整体搬到用户态（`/etc/passwd` + `/etc/shadow`，login/PAM 式服务）；内核不看密码，只看到 uid 与 label。
- `pwm` 不复用为安全域；主体标签是独立 `label` 字段。

因此，**此前把 16 条 SGEG 核心列为保留的方案作废**。`SYS_SGEG_BOOT_CHECK` 也不再作为 SGEG syscall：可信启动是 SGEG 的前提/独立能力，不是它的调用面。

### 2.1 目标态

| 项 | 目标 |
|---|---|
| 私有 syscall 数量 | **0** |
| `SYS_SGEG_*`（32 条，含 400–415、461） | 全部删除；同步清理用户库、dispatch、测试、文档 |
| `PRIVATE_BASE` / `PRIVATE_LEGACY_BASE` / `PRIVATE_END` | 31 个 legacy 迁完后整体删除；不再承载任何调用 |
| `is_private_nr` | 删除或恒 `false`；未知号继续 fail-closed |
| 外部 Linux 程序 | 零私有号依赖，只用 Linux 公开 ABI |
| EG 原生程序 | 只用 Linux ABI + 文件语义（策略文件、`/proc`、`getxattr`、`ioctl`） |

> `EG_SNAPSHOT_*`（895–898）同样不在保留集：EGFS 快照控制面走 `EGFS_IOC_SNAPSHOT_*`。
> `EG_WASM_*` 随 WASM 退用户态一起删除。

### 2.2 为什么“零 syscall”仍然有 SGEG

SGEG 的能力不靠 syscall 命名空间表达，而是靠内核内部机制：

- **判定点**：`vfs_open_internal` 等唯一出口的三段结构 `resolve → authorize → publish`；
- **策略**：用户态编译器生成定长表，内核只查表，不解析策略文本；
- **内省**：`/proc` 只读节点 + `getxattr`；
- **审计**：既有日志面留证；
- **身份**：用户态 login/PAM 式服务注入主体标签，内核只使用 label/uid。

这不是“把 SGEG 删掉”，而是把 SGEG 从“私有 syscall 服务”改成“内核安全模块”。

### 2.3 编号空间保留策略（建议）

结论：**保留定义域，不保留定义。**

| 区间 | 用途 | 是否分发 | 规则 |
|---|---|---|---|
| `0x4000_0000–0x4000_0FFF` | EG 私有未来扩展域 | 否 | 仅在台账/ABI registry 中预约；未来新私有号从这里向上分配 |
| `0x4000_1000–0x4000_1FFF` | 31 个 legacy 的墓碑区 | 否 | **永不复用**；旧号即使看起来空闲也不能重新分配，避免旧镜像/旧调用误命中 |
| 其他 `>= 0x4000_2000` | 不预约 | 否 | 不进入 EG 私有命名空间 |

执行口径：

- **不保留**具体 `SYS_*` 常量、dispatch 分支、用户库 wrapper、`ENOSYS` 占位桩。
- 代码里如需一个锚点，只保留 `PRIVATE_RESERVED_BASE/END` 之类**不参与分发**的常量，供测试断言“保留区内所有号都 fail-closed”。
- `canonical_to_aarch64` / `aarch64_to_canonical` 对保留区返回 `UNKNOWN`，不得原样透传。
- `is_private_nr` 删除或恒 `false`；`PRIVATE_BASE/PRIVATE_END` 不再参与 syscall 合法性判定。
- 未来真要用某个号：重新走本册三条件评审，登记具体常量、owner、测试与退役条件；**预约区间本身不等于 ABI**。

这样既保留“以后可能要用”的余地，又不会留下空壳 syscall 或隐式兼容承诺。

### 2.4 ABI 分层与承诺

**编号 ≠ ABI。** ABI 是契约：语义、参数/结构布局、错误码、生命周期、版本与兼容规则；一个号只是索引。预约 `0x4000_0000–0x4000_0FFF` 只是保留命名空间，**不构成 ABI**。

| 层 | 内容 | 稳定性 | 对外承诺 |
|---|---|---|---|
| L0 Linux syscall ABI | 公开 Linux 号、参数、errno、语义 | 稳定 | **是**；唯一对外 syscall 承诺 |
| L1 EGFS ioctl UAPI | `EGFS_IOC_SNAPSHOT_*` 等结构、版本、错误码 | 按 UAPI 版本稳定 | **是**；EGFS 工具与 Linux 适配器使用，不是 syscall 号 ABI |
| L2 EGFS 盘上格式 | v2 头、feature flags、快照表、ZIL 布局 | 按 feature flags 兼容 | **是**；跨 OS/跨版本的数据契约，不是运行时 ABI |
| L3 SGEG 内核内部接口 | 主体/客体标签 trait、判定入口、策略表查表 | 不稳定，可随内核重构 | **否**；不是用户态 ABI |
| L4 `edgine-abi` Rust crate | 内核/用户库共用的编号表与类型 | 内部 API，不作外部 ABI | **否**；不发布为第三方 ABI |
| L5 EG 私有 syscall | 具体 `SYS_*` 定义与 dispatch | 目标态为空 | **否**；预约号段也不承诺 |

执行口径：

- **对外只承诺 L0 + L1 + L2**；L3/L4 是内部实现，L5 目标为空。
- `edgine-abi` 里不保留 EG 私有 syscall 的公开常量；保留区如需测试锚点，放测试侧或 `#[doc(hidden)]` 的非分发常量，不进入用户库 API。
- 外部程序只拿 Linux 头/ABI；不发布 EG 私有 syscall 头文件。
- 未来若真要做 EG 私有 ABI，必须新建 **版本化契约**：`EG_ABI_vN` 文档 + 结构布局 + 错误码 + 生命周期 + 兼容/退役规则 + 测试；不能靠“号段已预约”自动获得 ABI 地位。
- 反向约束：**没有契约的数字不是 ABI，没有测试的契约不是稳定 ABI。**

---

## 三、迁移集（有 Linux 等价机制，迁完即删）

| 组 | 私有号 | Linux / 目标落点 |
|---|---|---|
| SGEG 全部 syscall | 32 条 `SYS_SGEG_*`（含 400–415、461） | 内核内部四原语 + VFS 唯一出口；策略写文件，内省 `/proc` / `getxattr`；身份搬用户态 |
| 身份补齐 | `SYS_SETEUID`、`SYS_SETEGID`（597–598） | Linux `setresuid` / `setresgid`；libc 的 `seteuid` 本身是用户态封装 |
| 系统管理 | `SYS_SGEG_REBOOT`、`GETHOSTNAME`、`SETHOSTNAME`、`PROC_LIST`、`PROC_SETPRI`、`PROC_CPUTIME`、`PROC_SLEEP`、`HOTPLUG_STATUS` | `reboot(169)`、`uname` / `sethostname(170)`、`/proc`、`setpriority(141)`、`/proc/[pid]/stat`、信号 / cgroup freezer、sysfs uevent |
| 存储 | `SYS_SGEG_DISK_LIST`、`DISK_INFO`、`DISK_FORMAT`、`DISK_PARTITION`、`DISK_INSTALL`、`FAT_FORMAT` | 块设备 `ioctl(BLK*)` + 用户态 `mkfs` / 分区 / 安装器；安装器不是内核 ABI |
| 显示 | `SYS_FB_OPEN`、`FB_MMAP`、`FB_RELEASE` | `/dev/fb0` + `FBIOGET_*` `ioctl` + `mmap`（fbdev） |
| 固件 | `EG_FW_LOAD`、`GET`、`GET_INFO`、`DETACH` | sysfs firmware class / `request_firmware` 用户态加载路径 |
| 网络 | `EG_ROUTE_ADD`、`ROUTE_DEL`、`ROUTE_QUERY`、`EG_NF_ADD_RULE`、`NF_DEL_RULE`、`EG_GETADDRINFO`、`EG_DHCPV6_SET_POLICY`、`GET_POLICY` | netlink `RTM_*` / nftables；`getaddrinfo` 是 libc 用户态；DHCPv6 是用户态客户端 |
| 资源与调试 | `EG_CGROUP_CREATE`、`DESTROY`、`ATTACH`、`SET_LIMIT`、`GET_STAT`、`EG_PM`、`EG_FTRACE_*`、`EG_KGDB_ENTER`、`EG_TICKLESS`、`EG_TIMESYNC`、`EG_CET`、`EG_UEFI`、`EG_SECURE_BOOT`、`EG_TPM`、`EG_IO_URING_SUBMIT`、`SYS_SGEG_SCHED_SNAPSHOT` | cgroupfs、`/sys/power/state`、debugfs/tracefs、kgdb、启动参数 / sysfs、`clock_adjtime` / `adjtimex`、`arch_prctl`、efivarfs、TPM 字符设备、原生 io_uring、tracefs |
| 栈保护 | `EG_GET_CANARY`（747） | `auxv` 的 `AT_RANDOM` / libc 自身 TLS canary；不应是 syscall |
| EGFS 快照 | `EG_SNAPSHOT_CREATE`、`DESTROY`、`ROLLBACK`、`CLONE`（895–898） | 转为 EGFS 自己的可移植控制面（`ioctl` UAPI 或用户态管理工具），不占 Edgine 私有 syscall |
| WASM | `EG_WASM_LOAD`、`RUN`、`GET_INFO`、`UNLOAD`（766–769） | 用户态 wasmi / WASI；**已于 2026-11-27（P4）随内核侧解释器一并真删**，见 [wasm-userspace-migration.md](./wasm-userspace-migration.md) P4 |

---

## 四、直退集（不需要 syscall 替代）

- `EG_WASM_*`：**已随 WASM 迁移 P4 真删（2026-11-27）**，不保留私有号。
- `EG_TICKLESS`：内核启动 / 调优参数，不是用户 ABI。
- `EG_GETADDRINFO`：由 libc / 用户态解析器接管，不新增 syscall。
- `EG_DHCPV6_*`：由用户态 DHCPv6 客户端接管，不新增 syscall。
- `EG_GET_CANARY`：由 auxv / libc TLS 接管，不新增 syscall。
- `EG_KGDB_ENTER`：调试器 / 调试口能力，不作为产品 syscall。
- `EG_SCHED_SNAPSHOT`：迁移到 tracefs / `sched_getattr` 后退役。
- `PRIVATE_LEGACY_BASE` 下 31 个 x86-only 遗留映射：仅服务 EG 原生用户库的旧调用；
  用户库改用 Linux aarch64 公开号后整体退役。外部 Linux aarch64 程序不需要这些号。

---

## 五、施工顺序

### R0：台账与冻结（本册）

- 建立 76 个私有号的保留 / 迁移 / 直退分类；SGEG 32 条按“全删”单独成包。
- 冻结新增私有号；新需求先进本册评审。
- 更新 `syscall_abi_boundary_test.rs`：目标态只允许 **0 个**私有号通过；SGEG 删除先于范围收缩。

### R0.5：SGEG syscall 全部退役

- 删除 32 条 `SYS_SGEG_*` 的常量、dispatch、用户库 wrapper、测试与文档引用。
- 保留内核内部四原语：主体标签、客体标签、转移规则、判定；判定点收敛到 `vfs_open_internal` 的 `resolve → authorize → publish`。
- 策略加载走既有路径文件；内省走 `/proc/<pid>/attr/...` / `getxattr`；审计走既有日志面。
- 身份/账户/密码/登录搬用户态；内核只使用 uid + label，不再有 `LOGIN/CREATE_IDENTITY/...`。
- `SYS_SGEG_BOOT_CHECK` 不作为 SGEG syscall；可信启动归独立 boot/measurement 面。
- 出口：`rg "SYS_SGEG_" src host-tests` 零命中；ABI 边界测试改为“未知私有号 fail-closed”；SGEG 功能测试改走文件语义/内核钩子。

### R1：低风险系统管理重复项

- 先迁 `SYS_SGEG_REBOOT`、`GETHOSTNAME` / `SETHOSTNAME`、`PROC_LIST`、`PROC_SETPRI`、`PROC_CPUTIME`。
- 每条腿：Linux 原生路径落地 → 用户调用方切换 → host/集成测试 → 删私有 dispatch 与常量。
- 这批不碰设备与网络，验证成本最低，适合作为退役样板。

### R2：存储、显示、固件

- 先补 Linux 原生面：块设备 `ioctl`、fbdev `/dev/fb0`、sysfs firmware class。
- 再把 `install` / `fbterm` / 固件加载调用方切过去，最后删私有号。
- 这批依赖设备模型与 VFS 布局，工作量大于 R1。

### R2.5：EGFS 控制面与可移植性

- 先定 EGFS 控制面 UAPI（建议文件/目录 `ioctl`）与快照语义。
- 实现 EGFS 核心的 OS 适配边界：块设备、VFS、时钟、随机、锁；清掉对 SGEG / FREG 的硬依赖。
- 补日志重放、fsck / scrub、盘上格式版本文档。
- 控制面落地后，删 `EG_SNAPSHOT_*` 四个私有号；EGFS 不保留任何 EG 私有 syscall。

### R3：网络、资源、调试、安全

- netlink、cgroupfs、io_uring、efivarfs、TPM、tracefs 逐组落地。
- `EG_CET`、`EG_PM`、`EG_TIMESYNC` 先做 syscall / sysfs 等价，再退役。
- 这批是长尾，按组独立验收，不合并成一个大提交。

### R4：WASM P4 — 已完成（2026-11-27）

- 依赖用户态 wasmi P1 + mm M2；`EG_WASM_*`（766-769 段）、`wasmstub`、`functions/wasm` 已一起真删。
- 过渡 fallback 已移除：`execve` 的 `\0asm` 派发唯一指向用户态 `/bin/wasmrun`；F9 零抑制复核通过。

### R5：legacy 31

- 用户库完成 Linux aarch64 公开号迁移后，删除 `PRIVATE_LEGACY_BASE` 回退映射。
- 外部 Linux 程序不得依赖任何 legacy 私有号。

---

## 六、验收与防线

- **兼容面**：外部 Linux 动态 / 静态程序只走 Linux 公开号；私有号零依赖。
- **编号面**：私有号只在 `0x4000_0000` 起的高位段；Linux 公开号段不出现私有号。
- **测试面**：目标态无私有号；每个退役号有“调用方已迁移”证据；未知号继续 fail-closed。
- **文档面**：`syscall-followup.md`、`progress-active-tasks.md`、`ref-naming.md` 与本册同步；
  文档改动与代码改动分离提交。
- **安全面**：不保留“假成功”兼容桩；不为了兼容而把 Linux 号映射到私有实现。

---

## 七、EGFS 可移植性专项

目标：**EGFS 可脱离 Edgine 单独存在，不依赖 EG 特有功能，同时保持好用可靠**。
因此 EGFS 的边界不是「Edgine 的一个文件系统后端」，而是「一个可移植文件系统实现 + 各 OS 适配器」。

### 7.1 分层

| 层 | 职责 | 约束 |
|---|---|---|
| EGFS 核心 | 盘上格式、inode / extent / 目录 / COW、快照、校验、WAL / 恢复 | 不调用任何 EG 私有 syscall；不依赖 SGEG / PWM；不依赖 FREG 才能保证正确性 |
| 块设备适配 | `read / write / flush / discard` 的抽象 | 接受块设备或文件作为后端；Linux 用块设备 / loop，Edgine 用 AHCI / NVMe / 内存盘 |
| VFS 适配 | inode 操作、目录项、权限、xattr、mount | Edgine 走 VFS trait；Linux 走内核模块或 FUSE 适配；语义以 POSIX 为准 |
| 控制面 | 快照、scrub、fsck、统计 | 可移植 UAPI：文件/目录 `ioctl`，或独立管理 CLI + 本地 socket；不使用 `EG_SNAPSHOT_*` |

### 7.2 控制面选型

优先选 **文件/目录 `ioctl` + 稳定 UAPI 头**：

- `ioctl` 在 Linux 与 Edgine 都能表达文件系统私有操作；
- 不占 syscall 编号，不绑定任一内核；
- 管理工具可以同时跑在两个 OS 上；
- 若未来做 FUSE，可用 `FUSE_IOCTL` 或管理 socket 转发，核心语义不变。

备选是**用户态管理工具 + 普通文件操作**（例如快照以隐藏子卷/目录表达），更简单，
但快照的原子性和可见性语义要另行定义。无论选哪种，`EG_SNAPSHOT_*` 都必须退役。

### 7.3 可靠性前置

在谈“好用可靠”之前，先补三条硬前提：

1. **崩溃一致性**：WAL / journal 必须在挂载时真正重放。当前 `zil_persist.rs` 在内核侧零调用者、
   挂载路径不重放日志，这是可靠性空洞，不是移植问题。
2. **可离线校验**：提供 `fsck` / scrub，能校验 uberblock、extent、目录项、校验和；
   盘上格式必须版本化、端序中立、字段布局自描述。
3. **不依赖 EG 恢复机制**：FREG 可以作为 Edgine 侧的可选增强，但不能是 EGFS 正确性的前提；
   EGFS 在 Linux 上必须能独立 mount、独立恢复。

### 7.4 验收

- 同一份 EGFS 盘镜像，在 Edgine 与 Linux 适配器上都能 mount、读写、快照、回滚；
- Linux 侧至少有一条 FUSE 或内核模块腿，证明“脱离 Edgine 单独存在”不是口头承诺；
- 掉电 / 中断注入后 mount 能重放日志并保持元数据一致；
- 外部 Linux 程序访问 EGFS 不需要任何 EG 私有 syscall。

---

## 八、待拍板

1. SGEG 判定出口收敛（`vfs_open_internal` 的 `resolve → authorize → publish`）是否先于 32 条 `SYS_SGEG_*` 删除？建议先收敛出口，否则删了 syscall 也没有稳定判定点。
2. EGFS 控制面选 `ioctl` UAPI 还是用户态管理工具？建议 `ioctl` 为主、CLI 为前端。
3. EGFS 的 Linux 适配器先做 FUSE 还是内核模块？建议 FUSE 先证明跨 OS 可移植性，
   内核模块留给性能阶段。
4. 是否按 §2.3 保留 `0x4000_0000–0x4000_0FFF` 作为**仅文档预约、不实现**的未来私有域？
   建议保留；`0x4000_1000–0x4000_1FFF` 作为 legacy 墓碑永不复用。
