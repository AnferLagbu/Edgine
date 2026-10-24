# FREG proc 恢复域 rollback UAF 护栏与生命周期实装

> **定位**：把 P-30 收尾发现的一条 **latent inert use-after-free 暗桩**先以 fail-closed 护栏钉死（杜绝误激活），再把其真正的生命周期保护（pin / refcount）作为后续工程立项。当前该路径在生产中**不可达、零影响**；本工程处置的是"一旦误激活即咬人"的隐患，而非现存故障。
>
> **来源**：`docs/report/p30-boot-freeze-rootcause-diagnosis.md` §五「遗留」项 A（FREG latent inert UAF）。
>
> **关系**：与 FREG 冻结立场（阶段性版本不再开发）一致——护栏是**防御性收口**（不改恢复语义），pin/refcount 实装则**属复活 FREG**，须待 FREG 重新立项时再做；本工程默认只做前者。
>
> **关联**：`docs/explain/` FREG 相关说明；`src/kernel/privileged/freg/`（恢复域机制）；`docs/plan/progress-active-tasks.md`。

## 1. 立项依据

### 1.1 根因一句话

`proc_freg_capture`（`src/kernel/privileged/proc/process.rs:917`）把进程表槽位里的 `NonNull<Process>` **裸指针**整体快照进 `PROC_SNAPSHOT`，既不 pin 也不加 refcount；`proc_freg_rollback`（`process.rs:927`）随后把这批裸指针原样写回进程表。**若**某进程在 capture 之后、rollback 之前被释放，rollback 会把悬垂指针重新挂进进程表 → **rollback use-after-free**。

### 1.2 为什么"当前安全"是一层窗户纸

| # | 事实 | 位置 | 含义 |
| --- | --- | --- | --- |
| F1 | 恢复域池所有槽位 `RecoveryDomain::new(0)`，`id` 恒为 0 | `freg/api.rs:40-47` | 池里没有任何 `id != 0` 的域 |
| F2 | `recovery_domain_register(domain_id)` 取模选中槽位后**从不写回 `domain.id = domain_id`**（注释 `api.rs:74` 声称"在此设置实际 id"但代码缺失） | `freg/api.rs:69-80` | 注册是"声明性归属"，实际 id 仍是 0 |
| F3 | `proc_register_freg_domain` 走 `find(4)`，因 `direct_map[4]` 为空恒返回 `None`，回调从未挂载 | `proc/process.rs:946`（护栏前） | capture/rollback 回调未接入 FREG → `PROC_SNAPSHOT` 恒 `None` → `rollback` 的 `if let Some` 空转 |

⇒ 今天不炸，纯粹是 **F2 的实现缺口意外挡住了激活路径**。一旦有人"顺手补全 `domain.id` 归属"（把 F2 当 bug 修），F3 立即翻转为 `Some` → 裸指针快照回调挂载 → 引爆 §1.1 的 UAF。护栏要钉死的就是这个翻转点。

### 1.3 现状：id 归属已补全，改由护栏拦截

`recovery_domain_register` 已实装注册时写回 `domain.id`（`id % 32` 槽位被不同 id 占用时拒绝注册），F2 缺口消除。激活路径不再由"id 恒 0"挡住，而由 §3 的 `PROC_FREG_ROLLBACK_SAFE = false` 拦截：`find(4)` 返回 `Some` 后走 `Some(_) =>` 分支拒绝挂载并留 warn trace（启动日志固定出现一条，非故障）。pmm 侧（域 3）挂载的是物理帧记账快照，无裸指针，不构成 §1.1 的 UAF；D-2/D-3 结论不变。

## 2. 源码调研结论

- 挂载漏斗唯一：`proc_register_freg_domain`（`process.rs:946`），由 `src/kernel/lib.rs:711` 在开中断前调用一次。
- `find(id)`（`freg/manager.rs:167`）对 `id < DIRECT_MAP_SIZE` 直取 `direct_map[id]`；因注册只填了 `direct_map[0]`，`find(4)` 运行时恒 `None`。
- **同构旁证**：`pmm_register_freg_domain`（`mm/pmm.rs:2419`，`find(3)`）落在完全相同的 inert 机制上；但其 capture 快照的是物理帧记账而非 `NonNull<Process>`，UAF 语义不同，本工程不改动，仅在 §4 D-3 登记待查。
- 护栏加在**挂载点**（而非注册点）：注册点的 id 归属补全恰是我们防御的"误激活触发器"，故必须在下游消费处 fail-closed。

## 3. 设计立场

- **护栏 = fail-closed 编译期常量门**：`const PROC_FREG_ROLLBACK_SAFE: bool = false;` 只有显式翻为 `true`（伴随 pin/refcount 落地）才挂载回调；当前 `find(4)` 恒 `None` → 走 `None => {}` 静默分支，**零新增 boot 噪声**。
- **误激活即报警**：若将来 id 归属被补全使 `find(4)` 返回 `Some`，但 `PROC_FREG_ROLLBACK_SAFE` 仍为 `false`，则命中 `Some(_) =>` 分支打出 `klog_ffi_warn`——把"暗桩被踩响"变成**显式 trace**，而不是静默引入 UAF。
- 不复活 FREG：护栏不改任何恢复语义，只把已不可达的路径钉成"必须显式授权 + 生命周期安全才可达"。

## 4. 施工条目

### D-1 挂载点加 fail-closed 护栏

描述：在 `proc_register_freg_domain` 引入 `const PROC_FREG_ROLLBACK_SAFE: bool = false;`，把 capture/rollback 回调挂载收进 `Some(dom) if PROC_FREG_ROLLBACK_SAFE` 分支，并为误激活补一条 `klog_ffi_warn` trace。
方案：`find(4)` 结果改为 `match` 三分支——护栏放行才挂载；`Some` 但护栏关则拒绝并留 trace；`None`（当前常态）静默。保持既有 `RECOVERY_MANAGER`→`capture_cb` 嵌套持锁顺序不变（不新增锁序，符合 `audit_deadlock_matrix`）。
状态：[X]
详情：见 `src/kernel/privileged/proc/process.rs:945` 起的护栏实现与文档注释；双架构 `./ci/build.sh all` 0w0e、dev/`--release` clippy、`./ci/audit.sh quick`、`make test-kernel-host` 均通过（随 P-30 遗留处置 PR 一并验证）。

### D-2 实装 pin / refcount 生命周期后放开护栏

描述：为 `proc_freg_capture` 快照的每个 `NonNull<Process>` 建立引用保护——capture 时对在册进程加 refcount（或 pin 其生命周期），rollback 时据此校验槽位仍有效再写回，释放旧快照引用；确认安全后方把 `PROC_FREG_ROLLBACK_SAFE` 翻为 `true`。
方案：属**复活 FREG** 范畴，须与 FREG 重新立项合并推进；设计要点（进程释放路径递减 refcount、快照持有引用直至下一次 capture/显式清理、rollback 前后成对校验）留待该立项细化。**在此之前禁止翻护栏**。
状态：[]

### D-3 pmm 侧同构 inert 复核

描述：`pmm_register_freg_domain`（`find(3)`）落在相同的 id-归属缺口上；物理帧快照的 rollback 是否也需要生命周期保护，语义与 proc 不同，需单列复核。
方案：待 D-2 的 FREG 复活立项时一并评估 pmm capture 的帧引用计数是否需要与 proc 侧对称加护栏；本轮不改。
状态：[]

## 5. 验收

- D-1（本轮）：护栏在位；`find(4)` 恒 `None` → 无 boot 噪声；强制令 `find(4)` 返回 `Some` 的负向用例（或临时置 `PROC_FREG_ROLLBACK_SAFE=true` 的本地验证）能观测到"仅护栏放行才挂载"；§2.3 全门槛绿。（id 归属补全后，第一条现表现为「护栏固定打一条 warn trace」——见 §1.3。）
- D-2/D-3：待 FREG 复活立项时按各自判据验收。
