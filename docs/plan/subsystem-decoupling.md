# 子系统间解耦工程（接口中立化 + 注册式挂载 + 判定出口收敛）

> 一句话说清为什么有这个计划: 用户问「怎样让各子系统之间解耦」，并要知道 Linux / Windows / macOS
> 这类自研程度高的内核怎么做。源码核实的结论是: **三家共同形态是「ops 表 + 注册框架 + 中立载体」三件套，
> 不是编译期开关**; 而 Edgine 已具备前两件（`FileSystem`/`Inode`/`File` trait 分发、11+ 处策略 trait 注册族）
> 与三家都没有的依赖方向机器执法（4 条审计腿 + 双向 ratchet），**缺的恰恰是第三件「中立载体」与
> `register_filesystem`，以及判定出口未收敛**。三家里没人用开关解耦 —— 开关只决定「框架在不在」，
> 不决定「VFS 认不认识安全子系统」。故本工程不做开关，做结构性解耦。

---

## 一、背景：三家怎么做，Edgine 缺什么

### 三家内核的共同形态（源码/官方文档核实）

| | 接口层 | 注册框架 | 中立载体 |
|---|---|---|---|
| **Linux** | `file_operations` / `inode_operations` / `super_operations` / `address_space_operations` 分层 ops 表 | `register_filesystem`；安全侧 `security_add_hooks()` + `security_hook_list` 链表按 `CONFIG_LSM` 顺序调用 | `void *i_security` blob —— 框架只分配/复制/释放，**不解读内容** |
| **Windows** | I/O Manager 发 IRP 走设备栈 | FltMgr minifilter（`FltRegisterFilter` + `FLT_REGISTRATION` + altitude 排序、Pre/Post 回调）；Object Manager `ObRegisterCallbacks`；注册表 `CmRegisterCallbackEx` | 安全描述符由文件系统持久化，SRM `SeAccessCheck` 统一算法；**没人认为 SRM 是 NTFS 的一部分** |
| **macOS/XNU** | BSD 侧 vnode ops + `kauth` scope | TrustedBSD MACF（`mac_policy_conf` + `mac_policy_register` + 100+ `mpo_*`，未注册回调直接返 0） | `struct label` 挂 vnode/cred，由对象自身锁保护 |

三家共性两条，本工程据此立论:

1. **解耦靠「接口 + 注册 + 中立载体」，不靠编译期开关。** `CONFIG_SECURITY` / `lsm=` / kext 加载决定的是
   「框架在不在、谁先判」，与「VFS 是否认识安全子系统」无关。
2. **载体必须中立。** Linux 的 `void*` blob、FreeBSD 的 `struct label` 都不含安全子系统的语义;
   谁的语义泄漏进了 FS 接口，谁就成了 FS 的附庸。

### Edgine 现状（对照读数）

| 机制 | 现状 | 证据 |
|---|---|---|
| ops 表 + 统一分发 | **已有且成熟** | `FileSystem` trait 31 方法 [vfs_types.rs:315-558](../../src/kernel/functions/fs/vfs_types.rs#L315)；`Inode` / `File` 对象层；唯一 open 出口 `vfs_open_internal`（[handle.rs:59](../../src/kernel/functions/fs/handle.rs#L59)，E6-5 已移除 fallback），7 个 FS 全经 trait object 分发 |
| 框架持表、模块注册 | **已有但逐点** | 11+ 处策略 trait（`SyscallDispatch` / `SignalDecision` / `PageFaultPolicy` / `SwapPolicy` / `FREGDegradePolicy` / `FsBackend` / `VfsOps` / `VfsPollPolicy` / `ConfigValidateHook` / `IpcStrategy` / `CoredumpSink`）；4 处擦除 fn 钩子；DHCPv6 反转钩子 |
| 依赖方向机器执法 | **Edgine 领先** | `audit_coupling`（双向 ratchet 基线）/ `audit_reverse_deps`（零容忍）/ `audit_implicit_deps`（基线对账）/ `audit_feature_semantics`（计数登记）；functions 边界白名单 32 条 |
| `register_filesystem` | **缺失** | [vfs_mount.rs:66-81](../../src/kernel/functions/fs/vfs_mount.rs#L66) 仍 `match FsType` 硬编码; [mod.rs:137-145](../../src/kernel/functions/fs/mod.rs#L137) 的 `resolve_fs` 是硬编码两行 match（注释自述「新增文件系统仅需在此增加一行映射」），**不是注册表** |
| 中立载体 | **缺失** | `pwm: u64` 直接进 `FileSystem` trait 签名（[vfs_types.rs:341/351/364/374](../../src/kernel/functions/fs/vfs_types.rs#L341)），注释仍写「FS 实现负责权限检查」 |
| 判定唯一出口 | **缺失** | `open_by_handle_at` 绕过唯一 open 出口（[file_handle.rs:138-190](../../src/kernel/functions/fs/file_handle.rs#L138)）；判定散在 syscall 入口（[mount.rs:52](../../src/kernel/functions/fs/mount.rs#L52)、[misc.rs:124](../../src/kernel/functions/fs/misc.rs#L124)、[access.rs:75](../../src/kernel/functions/fs/access.rs#L75)、[path.rs:92](../../src/kernel/functions/fs/path.rs#L92)、[file_handle.rs:154](../../src/kernel/functions/fs/file_handle.rs#L154) 等）；ramfs 保有全仓唯一存活的 FS 内 `check_permission`（[ramfs_data.rs:383-426](../../src/kernel/functions/fs/ramfs_core/ramfs_data.rs#L383)，11 调用点） |

### EGFS 侧已不再有「判定耦合」

解耦批 D5-i 已摘净 EGFS 的 12 处 `check_permission`（`grep -rc check_permission src/kernel/functions/fs/egfs/` = 0）。
残留只有三类别的东西，**都不是判定**：

- 盘上数据模型字段 `owner_pwm` / `group_pwm` / `sensitivity` / `pwm_perm`（[dmu.rs:88](../../src/kernel/functions/fs/egfs/dmu.rs#L88)）与 uberblock `pwm_domain_id`（[spa.rs:72](../../src/kernel/functions/fs/egfs/spa.rs#L72)）—— 磁盘格式，SGEG v2 仍要用；
- 1 处 FREG 域注册（[egfs_data.rs:288](../../src/kernel/functions/fs/egfs/egfs_data.rs#L288)）—— 机制挂钩，FREG 已裁休眠；
- 1 处 SHA-256 算法复用（[checksum.rs:125](../../src/kernel/functions/fs/egfs/checksum.rs#L125)）—— 算法级，与安全判定无关。

另有 `EgfsData.current_pwm`（[egfs_data.rs:93](../../src/kernel/functions/fs/egfs/egfs_data.rs#L93) / `:137`）声明并初始化后**零读取**，是死状态残迹。

结论: **「EGFS 与 SGEG 耦合」的真正形态不是判定，而是 `pwm: u64` 这个 SGEG 专有语义泄漏进了共享的 FS 接口。**

## 二、用户裁定（2026-11-17）

- **方案 B 完整解耦工程，单独立项**（会话内 AskUserQuestion 选项 B）。理由: 出口收敛 + 两 trait +
  `register_filesystem` 是设计册 [sgeg-design.md:742-744](../design/sgeg-design.md) 已裁的终态，也是三家共同形态。
- **分两期，期二待授权**（后继裁定 A）: 除「出口收敛 + 两 trait」外的结构解耦与 SGEG 判定模型无关，立即入册施工;
  出口收敛在 SGEG 冻结期做等于先替 v2 定接口，须显式放行后才开工。
- **不做编译期开关**。依据: `freg` feature 现已存在但只翻 caps 位（[caps.rs:87](../../src/kernel/privileged/config/caps.rs#L87) 是唯一消费者，
  不门控任何代码），真正门控 FREG 的是 `fault_injection`（8 处）; SGEG 无任何 feature 与运行时开关。
  给已冻结/休眠的子系统加开关＝造将来要拆的脚手架，与 2026-11-17 的 SGEG 冻结裁定正面冲突。
- **不做 LSM 式注册表**。已有裁定见 [sgeg-design.md:190](../design/sgeg-design.md): Edgine 只有一个安全模块，
  注册表是纯粹的过度设计。

## 三、期一（授权施工）

### S-1: `register_filesystem` —— 注册式挂载解析

- **描述**: 退役 [vfs_mount.rs:66-81](../../src/kernel/functions/fs/vfs_mount.rs#L66) 的 `match FsType` 硬编码与
  [mod.rs:137-145](../../src/kernel/functions/fs/mod.rs#L137) 的两行 `match fs_name`，改为按名字查注册表。
  对照 Linux `register_filesystem` / `get_fs_type(name)`。
- **方案**: 在 functions 侧建 `(name, &'static dyn FileSystem)` 定长注册表（沿用 `audit_once_cell` 既有模式），
  暴露 `register_filesystem(name, fs) -> Result<(), _>`，由 `fs::init()` 注册 ramfs / egfs / devfs / ext2 / exfat / tmpfs / overlay
  全部实例; `vfs_mount_internal` 改为「名字 → 注册表查找 → fail-closed（未注册返 `NotInitialized`）」。
  `FsType` 降级为**挂载记账标签**（`vfs_manager` 的 `fs_type` 字段与测试断言继续用），不再参与实例化。
  **不改** `VFS_MANAGER.mount` 旧路径的行为，不改 privileged 侧。
- **状态**: []

### S-2: SHA-256 算法归位

- **描述**: EGFS 的校验和复用 `privileged::sgeg::sha256`（[checksum.rs:125](../../src/kernel/functions/fs/egfs/checksum.rs#L125)），
  使「FS 依赖安全子系统路径」在依赖图上成立，但实际只是要一个哈希原语。
- **方案**: 把 [privileged/sgeg/sha256.rs](../../src/kernel/privileged/sgeg/sha256.rs) 迁到中立位置
  `privileged/crypto/sha256.rs`（纯算法、零 unsafe，归 TCB 的通用密码原语而非安全模块），
  常量 `PWM_DIGEST_LEN` 改中立名; 更新 4 类消费者: `sgeg/secure_boot.rs`、`functions/sgeg/crypto.rs`、
  `functions/sgeg/sha256.rs`（现为 `pub use` 转发）、`kernel_tests/sys.rs`、`egfs/checksum.rs`。
  `functions/sgeg/sha256.rs` 保留为再导出以维持既有对外面，或按搜索结果改直连 —— **以不引入死代码为准（F9）**。
- **状态**: []

### S-3: 删 EGFS 死状态字段

- **描述**: `EgfsData.current_pwm` 声明 + 初始化后零读取（[egfs_data.rs:93](../../src/kernel/functions/fs/egfs/egfs_data.rs#L93) / `:137`）。
- **方案**: 删除字段与初始化语句。若构造/初始化签名因此有未使用参数，一并显式消费或删参（不得用 `allow(unused)` 压住，F9）。
- **状态**: []

### S-4: 设计立场册 `docs/design/subsystem-decoupling-design.md`

- **描述**: 把本册 §一 的三家对照、Edgine 已有/缺失、两条共性立论写成设计立场（长期主张），作为 v2 与后续工程的输入。
- **方案**: 按 AGENTS §6 design 册规范（自由描述风格，**禁**结构化字段与状态标记）; 明确写「开关不是解耦手段」
  与「不做 LSM 式注册表」两条立场及其理由; 与本册及 `sgeg-design.md` 互指。
- **状态**: []

### S-5: 期一收口

- **描述**: 七门槛 + 文档/台账同步 + 按路径提交推送。
- **方案**: §2.3 七门槛全绿（`bash ci/audit.sh full` + `make test-host` + `make test-kernel-host` +
  `make -f Makefile.ci ci-rustdoc` 分跑，判据取日志内 `*-RC=` 回显）; S-1 需新增 host 契约锁
  （注册表解析 + 未注册 fail-closed + 重复注册拒绝）; 同步 `docs/explain` 相关页与本册状态。
- **状态**: []

## 四、期二（待授权，不施工）

> 触发条件: 用户显式授权「先定接口、v2 只换实现」，或 SGEG v2 解冻。解冻前开工＝重演 2026-11-17 的 165 行返工。

### T-1: 判定出口收敛（`resolve → authorize → publish`）

- **描述**: 所有获得对象引用的路径（含 `open_by_handle_at`、exec、link、mount、rename）必须经过同一段 authorize。
- **方案**: 按 [sgeg-design.md:152](../design/sgeg-design.md) 的三段结构，把散在 syscall 入口的判定上提到唯一出口;
  **验收是可机械检查的**: 统计能获得对象引用的出口数量，要求为 1（或全部经同一 authorize 函数），
  而非逐个人工确认调用点（[sgeg-design.md:742-744](../design/sgeg-design.md)）。
- **状态**: []

### T-2: 主体 / 客体两 trait（薄抽象 A）

- **描述**: 落地 [sgeg-design.md:183-190](../design/sgeg-design.md) 已裁的「主体标签解析 + 客体标签查询」两 trait。
- **方案**: 定义两 trait，文件系统与进程各自实现; 检验标准同设计册 —— **删掉文件系统后 SGEG 的类型、判定、策略表、审计仍在**。
- **状态**: []

### T-3: ramfs 判定上提 + 载体中立化

- **描述**: 摘掉 [ramfs_data.rs:383-426](../../src/kernel/functions/fs/ramfs_core/ramfs_data.rs#L383) 的 FS 内 `check_permission`（11 调用点），
  与 D5-i 对 EGFS 的处理同构; 随后把 `FileSystem` trait 的 `pwm: u64` 换成 FS 层不透明 `SubjectId`
  （FS 只记录属主、不解读语义，对照 Linux `void* i_security`）。
- **方案**: 判定移到 T-1 的唯一出口; trait 签名改类型 + 7 个 impl + 调用方全改; host 权限回归锁逐条归因后改钉。
- **状态**: []

### T-4: 盘上字段语义中立化收尾

- **描述**: `owner_pwm` / `pwm_perm` / `pwm_domain_id` 等盘上字段名与注释中立化（**不改磁盘格式**）。
- **方案**: 仅改命名与文档，字节布局零变化; 与 SGEG v2 的客体标签模型对接（v2 仍需要属主与标签）。
- **状态**: []

## 五、调研发现：载体中立化为何不能留在期一

`SubjectId` 若在期一就进 trait 签名，ramfs 会成为**假中立**的受害者:

- [ramfs_data.rs:383-426](../../src/kernel/functions/fs/ramfs_core/ramfs_data.rs#L383) 的 `check_permission` 把 `pwm` 当身份用 ——
  `pwm_get_privilege_level(pwm)` 查 MLS 敏感级、`ace.pwm == pwm` 匹配 ACE、`node.owner_pwm != pwm` 比属主、
  `pwm_has_capability(pwm, 1, cap)` 查能力位。
- 不透明 ID 传进来，ramfs 只能调 `SubjectId::raw()` 转回 `u64` 再交给 SGEG ⇒ 签名换了、耦合没走，只是多了一层转换。
- 该函数正是 T-3 要摘的对象。**所以载体中立化必须排在 ramfs 判定上提之后**，即期二。

这一处是对「期中划分」的修正（会话内最初把 trait 中立化列入期一）; 按 §11.3 禁止事后自圆其说，
本册将其显式登记为调研发现而非静默扩大/缩小范围。

## 六、不做什么

- **不加 feature 开关**（理由见 §二）。
- **不做 LSM 式钩子注册表**（[sgeg-design.md:190](../design/sgeg-design.md) 已裁）。
- **不改 EGFS 磁盘格式**（T-4 仅命名与注释）。
- **不碰 SGEG 现版接口**（2026-11-17 冻结裁定）; 期一三件均与其判定模型无关。
- **不动 ramfs 判定**（属期二，未授权）。

## 七、验证门槛

期一收口按 AGENTS §2.3 七门槛全绿（判据取日志内 `*-RC=` 回显，`bash ci/audit.sh full` 档位必写）;
S-1 另需 host 契约锁与新增 public API 的中文文档（F8）; 新册须先 `git add` 再跑 `scan_obsolete_naming`（其扫描面是 git 索引）。
