# SGEG 能力位常量治理计划（复用先例回溯）

> 独立计划文档：登记能力位常量的"新增 vs 复用"决策准则与既有复用先例的回溯治理方案。
> **本计划仅登记决策与规划，不修改任何代码**（2026-08-31 用户明确指示）。
> 准则来源：分册 7 DECISION-078（用户 2026-08-31 确立）。

## 背景

分册 7 委托前，用户为 B07-05（pwm_set_syscall 任意提权）确立了常量决策准则：

> **重要或特殊用途常量采用新增，其他的采用复用既有。**

该准则确立后，需回溯检查此前所有"复用能力位"的先例，评估是否也存在"语义不精确 / 魔法数 / 缺命名常量"问题。本计划对此前 10 处 `pwm_has_capability` 复用点逐一定性，并登记治理决策。

## 既有复用先例盘点（10 处）

### A 类：语义精确、与 Linux 对齐 → 保持复用（3 项）

| # | 先例 | 位置 | 使用位 | Linux 对应 | 处置 |
|---|---|---|---|---|---|
| A1 | mount / umount2 | `functions/fs/mount.rs:52,86` | SYSTEM 域 `0x01` | CAP_SYS_ADMIN（同义） | 保持复用 ✅ |
| A2 | setns | `functions/proc/namespace.rs:812` | SYSTEM 域 `0x01` | CAP_SYS_ADMIN（同义） | 保持复用 ✅ |
| A3 | ramfs/unkfs 文件权限 | `functions/fs/ramfs_core/ramfs_data.rs:345`、`functions/fs/unkfs/unkfs_data.rs:530` | `FS_CAP_*`/`PROC_CAP_*` 命名常量 | 能力制 | 保持复用 ✅（命名规范） |

### B 类：语义不精确 / 魔法数 / 缺命名 → 需治理（5 项）

| # | 先例 | 位置 | 现状问题 | 治理决策 | 状态 |
|---|---|---|---|---|---|
| B1 | reboot（重启系统） | `functions/proc/sysinfo.rs:155` | 复用 SYSTEM `0x01`，但重启是独立系统操作，Linux 语义应 CAP_SYS_BOOT | **新增专用能力位**（CAP_SYS_BOOT 语义） | [] 待实施 |
| B2 | open_by_handle_at | `functions/fs/file_handle.rs:150` | 复用 SYSTEM `0x01`（B06-03 曾裁决），但绕过路径直接开 inode 句柄属高敏操作，Linux 语义应 CAP_DAC_READ_SEARCH | **新增专用能力位**（CAP_DAC_READ_SEARCH 语义，回溯 B06-03 裁决） | [] 待实施 |
| B3 | sethostname | `functions/proc/sysinfo.rs:136` | **0x09 未命名魔法数**（=bit0\|bit3，非 0x01 也非命名常量），疑似 bug | **修复为命名常量**（新增 SET_HOSTNAME 位，或归入命名后的系统管理位） | [] 待实施 |
| B4 | mmap | `functions/mm/mmap.rs:304` | MEM 域 `0x01` **无命名常量**（capability.rs 无 MEM_CAP_* 定义） | **新增 MEM_CAP_* 命名位**（补 capability.rs MEM 域常量） | [] 待实施 |
| B5 | sys_boot_install | `privileged/syscall/dispatch.rs:1188` | `pwm_has_capability(pwm, 4, 0)` **required=0 可疑**（无实际位要求） | **留待澄清**——先确认意图（缺位 bug 还是有意为之）再定 | [] 待澄清 |

### C 类：域号冲突 / 语义错位（2026-08-31 追加核实，比 B 类更严重）

| # | 先例 | 位置 | 现状问题 | 治理决策 | 状态 |
|---|---|---|---|---|---|
| C1 | **sgeg disk storage 域冲突** | `functions/sgeg/storage/disk.rs:23-24` | `PWM_DOMAIN_STORAGE = 4` 与 `CAP_DOMAIN_DEVICE = 4`（capability.rs:17）**编号冲突**——storage"域"实际就是 DEVICE 域；且用 `required=1`（= DEVICE_CAP_MMIO bit0）保护磁盘格式化，**语义错位**（格式化是存储操作，非 MMIO 访问） | **澄清 + 命名**：要么归属 DEVICE 域并新增 DEVICE_CAP_STORAGE 位，要么独立域；消除本地重复常量，改用公共命名 | [] 待澄清+实施 |
| C2 | **裸数字域号**（非语义 bug，规范问题） | `mount.rs:52/86`（0）、`namespace.rs:812`（0）、`file_handle.rs:150`（0）、`mmap.rs:304`（7）、`ramfs_data.rs:345`（1）、`unkfs_data.rs:530`（3）、`dispatch.rs:1188`（4）、`sysinfo.rs:136/155`（0） | 域号本身正确（0=SYSTEM,7=MEM,1=FS,3=PROC,4=DEVICE），但用**裸数字**而非 `CAP_DOMAIN_*` 命名常量，可读性差、易写错 | **统一改用 `CAP_DOMAIN_*` 命名常量**（纯规范，低风险） | [] 待实施 |

> 注：C1 与 B5 同处 dispatch.rs:1188 的 `4` 域，合并排查。A/B/C 类合计，除已治理的 A 类 3 项外，待办共 B 类 5 项 + C 类 2 项。

## 实施规划（供后续委托人执行，本计划不落地代码）

### 新增能力位布局建议（待实施时按 capability.rs 实际布局设计）

- **B1 reboot**：SYSTEM 域新增 `CAP_SYS_BOOT` 位，替换 sysinfo.rs:155 的 `0x01` 复用。
- **B2 open_by_handle_at**：新增 `CAP_DAC_READ_SEARCH` 语义位（归属域待定，可 SYSTEM 或 FS 域），替换 file_handle.rs:150 的 `0x01`。
- **B3 sethostname**：新增命名常量（如 SYSTEM 域 `CAP_SET_HOSTNAME`），替换 sysinfo.rs:136 的魔法数 `0x09`。
- **B4 mmap**：capability.rs 新增 `MEM_CAP_*` 命名常量族（如 MEM_CAP_ALLOC 等），替换 mmap.rs:304 的 `0x01`。
- **B5 sys_boot_install**：实施前先由委托人澄清 `required=0` 的真实意图（若为缺位 bug，补齐所需位）。

### 验证门槛（实施时）

1. `capability.rs` 新增常量后，`privileged/sgeg/mod.rs` re-export 同步（若属公共 API）。
2. 各调用点替换后，`audit_functions_boundary.py` 0 违规（functions 层引用合规）。
3. 双架构 `cargo check` + clippy（-D pedantic）0 error。
4. host-tests 全量通过（cred 相关回归）。
5. 涉及 B2 时，补充 open_by_handle_at 权限拒绝 host-tests（B06-03 已有先例）。

## 架构裁决：sensitivity（对象敏感级别）与 privilege_level（身份等级）归属（2026-08-31 用户裁决）

### 调研结论（两模型现状）

- **privilege_level**：已属 sgeg 权威——定义于 [sgeg/types.rs:240](file:///home/anfer/Code/Edgine/src/kernel/functions/sgeg/types.rs#L240)（PwmEntry 身份字段），API 由 sgeg 提供（[engine.rs:54](file:///home/anfer/Code/Edgine/src/kernel/privileged/sgeg/engine.rs#L54)），ramfs/unkfs/signal 均为消费者（经 `pwm_get_privilege_level` 读取），**无副本**。现状已统一，无需处置。
- **sensitivity**：真正独立模型——定义分散于各 FS 节点结构（[ramfs_node.rs:11](file:///home/anfer/Code/Edgine/src/kernel/functions/fs/ramfs_core/ramfs_node.rs#L11)、unkfs dmu.rs:59、dataset.rs:30），**不在 sgeg 类型**；仅 ramfs/unkfs 实际使用（check_permission 的 clearance 比较），其余 FS（tmpfs/devfs/overlayfs/ext2/exfat）字段恒 0；cred 无敏感性概念。属类 Bell-LaPadula/MLS 多级安全模型，当前为半成品。

### 方案评估（长期视角）

| 方案 | 内容 | 长期评估 |
|---|---|---|
| 1 统一到 cred | sensitivity 并入 sgeg | ❌ 架构债——cred 变 monolith（身份+对象+策略混杂），违反单一职责；TCB 上升；Linux 用 LSM 分离的教训 |
| 2 独立补全 | 独立"对象访问控制层"，与 cred 平级 | ✅ **长期最优**——与 Linux capabilities↔LSM 分离同构；新 FS/新策略（Biba 等）只需接独立层；审计边界清晰 |
| 3 折中 | cred 定义语义 + FS 存储 | ⚠️ 过渡形态——权威与状态分离，产生新的分散 |

### 用户裁决（2026-08-31）

- **长期方向**：**方案 2（独立对象访问控制层）**——sensitivity 作为对象级安全属性，定义统一 API、接入全部 FS、可审计；cred 继续专注身份能力。方案 3 可作为过渡，方案 1 否决。
- **signal.rs 策略**：用 `get_privilege_level` 判断信号权限（signal.rs:50）**单独评估**——倾向改为能力位判定或明确等级策略归属，不混入对象安全层。
- **实施范围**：本裁决仅登记架构方向，落地（新建对象安全子系统/API、FS 接入）作为独立工程，不在当前常量治理计划内实施。

### 关联

- 与 [DECISION-078](./archive/audit-fix-07-functions-net-ipc-credo.md) 常量准则（重要/特殊用途新增）互补：本裁决是**系统级归属**决策，DECISION-078 是**常量级命名**决策。

## 变更历史

- **2026-08-31**：创建本计划。用户确认决策倾向 1① 2① 3① 4① 5②（B1/B2 新增专用位、B3 修魔法数、B4 补命名、B5 待澄清）；A 类 3 项保持复用。**仅登记计划，不修改代码**。
- **2026-08-31**：追加 C 类 2 项（域号冲突/语义错位）——C1 sgeg disk storage 域 4 与 DEVICE 域冲突 + required=1 语义错位（严重）；C2 裸数字域号规范问题（8 处）。合并 B5 与 C1 排查。待办共 B 类 5 项 + C 类 2 项。
- **2026-08-31**：登记架构裁决——sensitivity/privilege_level 归属：privilege_level 已 cred 权威无需处置；sensitivity 长期采用**方案 2（独立对象访问控制层）**，方案 3 可作过渡，方案 1 否决；signal.rs 信号权限策略单独评估。仅登记方向，落地另立工程。
