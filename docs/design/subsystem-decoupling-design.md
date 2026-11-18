# 子系统解耦设计立场

这份文档回答一个反复被问到的问题: **怎样让各子系统之间解耦**. 它不是任务清单
(可执行分解在 [子系统解耦工程](../plan/subsystem-decoupling.md)), 而是长期主张:
解耦靠什么、不靠什么、用什么检验、以及在 SGEG 冻结期间哪些能先做.

## 一、判据: 什么算"解耦"

一句话判据:**删掉任一子系统, 其余子系统仍然成立**. 换成可观测的形式, 就是三条:

- 依赖方向上, 上层不认识下层的**内部名** (模块路径、枚举变体、私有常量);
- 交互面上, 交叉点收敛为**少数几个签名**, 而不是散落在各调用方;
- 承载语义的载体是**中立的** —— 载体里不含某个子系统的专有语义.

第三条最容易被漏掉. "文件系统依赖安全模块"通常不是某行代码写错了, 而是安全模块
的**身份类型**被塞进了文件系统的接口签名. 那时两种子系统在类型层面已经焊死,
任何一处改签名都要跨两个子系统改, 而没人说得清这是谁的接口.

## 二、三家内核的共同形态

自研程度高的内核在这件事上收敛到了同一个形状: **接口表 + 注册框架 + 中立载体**.

| | 接口层 | 注册框架 | 中立载体 |
|---|---|---|---|
| **Linux** | `file_operations` / `inode_operations` / `super_operations` / `address_space_operations` 分层 ops 表 | `register_filesystem`; 安全侧 `security_add_hooks()` 把 `security_hook_list` 挂进链表, 按 `CONFIG_LSM` 顺序依次调用 | `void *i_security` 安全 blob —— 框架只分配/复制/释放, **不解读内容** |
| **Windows** | I/O Manager 把请求打成 IRP 走设备栈 | FltMgr 统一接管 minifilter (`FltRegisterFilter` + `FLT_REGISTRATION` + altitude 决定顺序, Pre/Post 回调); Object Manager 有同构的 `ObRegisterCallbacks`; 注册表有 `CmRegisterCallbackEx` | 文件安全描述符由文件系统持久化, 判定算法统一在 SRM (`SeAccessCheck`)。**没人认为 SRM 是 NTFS 的一部分** |
| **macOS/XNU** | BSD 层 vnode ops + `kauth` scope | TrustedBSD MACF (`mac_policy_conf` + `mac_policy_register` + 上百个 `mpo_*` 回调, 未注册的回调直接返回 0) | `struct label` 挂在 vnode/cred 上, 由对象自身的锁保护 |

三家里有两处细节值得单拎出来:

**第一, Windows 的判定与存储分家.** 访问判定统一在 SRM, 但"谁能读这个文件"的
安全描述符存在 NTFS 里. 它之所以不叫"NTFS 附庸 SRM", 正是因为判定入口唯一、
存储按对象类型分散. 这与 Linux 的 `security.selinux` xattr、FreeBSD 的 `v_label`
是同一形态.

**第二, 中立载体是硬要求.** Linux 的 `void *i_security` 在 VFS 眼里就是一个不透明
指针; 安全模块换了、加载了几个, VFS 的 `inode` 结构体一行不用改. 反过来说, 一旦
VFS 的 `struct inode` 里出现 `selinux_sid` 这种字段, VFS 就再也删不掉 SELinux 了.

## 三、解耦不靠开关

一个常见的直觉是"加个 feature 开关, 关掉就不耦合了". 三家都不这么做, 原因是开关
解决的是**另一个问题**:

- `CONFIG_SECURITY` / `lsm=` / kext 加载, 决定的是"框架在不在、谁先判";
- 它们**不决定**"VFS 认不认识 SELinux".

开关只能把耦合藏进 `#[cfg]` 分支: 关掉的代码仍需被编译到 (否则违反死代码禁止),
而两套分支还要各自维护、各自测试. 开关能带来的唯一收益是"少编译一点", 代价却是
接口分叉与盘上格式分叉 (EGFS 若在关闭态把属主字段写 0, 两种盘就互不兼容).
所以本项目不引入任何"启用/关闭 SGEG 或 FREG 特性"的开关 —— 见 §六.

## 四、Edgine 的现状

Edgine 并非从零开始: 解耦三件套里前两件已经相当成熟.

**接口层.** `FileSystem` trait (`functions::fs::vfs_types`) 31 个方法, 七个文件系统
全经 trait object 分发; 其上还有 `Inode` 与 `File` 两层对象模型. open 有唯一出口
`vfs_open_internal` (E6-5 已移除 fallback). 这一层等价于 Linux 的 ops 表加 Windows
的 IRP 分发.

**注册框架.** 全仓已有十余处"框架持表、子系统注册"的策略 trait: 系统调用分发、
信号决策、缺页策略、交换策略、FREG 降级策略、文件系统后端、VFS 操作契约、poll
策略、配置校验钩子、IPC 策略、coredump 落点. 另有若干擦除函数指针钩子
(`pwake` / `tick_query` / `memlock_limit` / `process_cleanup`) 与 DHCPv6 的
"反转钩子". 它们等价于 LSM 的 `security_add_hooks`、FltMgr 的 `FltRegisterFilter`,
只是**逐点铺开**而非全树铺开 —— 这是刻意的 (见 §五).

**依赖方向执法.** 这一项三家都没有: Edgine 用四条审计腿把"谁依赖谁"变成机器判据 ——
跨模块循环依赖 (双向棘轮基线)、privileged→functions 反向引用 (零容忍)、隐式全局
依赖 (基线对账)、构建期门控合法性 (计数登记), 加上 functions 边界白名单. Linux /
Windows / macOS 在这些方向上是靠评审与约定, 没有对等物.

**当时的缺口也是三件, 正好对应三件套里最容易被忽略的部分; 三件均已在 2026-11-17
的工程里闭合**[^1]:

- **没有 `register_filesystem`** → 现为 `FsRegistry` 注册表, 新增 FS 只需在注册点加一行.
- **没有中立载体** → 现为 `SubjectId` (`FileSystem` trait 不再出现 `pwm: u64`).
- **判定出口不唯一** → 现为唯一 publish 出口 (`publish_opened_inode`) 加单点判定算法
  (`authorize::decide`); `open_by_handle_at` 不再是绕过路径.

[^1]: 改动面与门槛读数见 [子系统解耦工程](../plan/subsystem-decoupling.md). 同一工程还
收敛了 fs 层对安全模块的**直接引用**: 子树内只允许 `authorize.rs` 与 `subject.rs` 两个
出口引用 `privileged::sgeg`, 其余文件一律经它们访问 (由源码契约锁把守).

## 五、不做 LSM 式注册表

[SGEG 设计](sgeg-design.md) 已经裁定: **不做 LSM 式注册表**. 理由原样成立 —— 注册表
(value: 钩子链表、模块优先级、全树铺开的 `security_*`) 是为"十几个可插拔策略模块"
设计的; Edgine 只有一个安全模块, 铺那套框架是纯粹的过度设计. 本项目沿用该裁定:

- 需要按名解析的, 用**定长表 + 名称查找** (`register_filesystem` 形态), 不做优先级调度;
- 需要注册回调的, 用既有的策略 trait 或擦除函数指针, 不做钩子链表;
- 需要标签的, 用"一个查询接口 + 若干按对象类型的实现", 不铺 `mpo_*` 全树.

## 六、与 SGEG 冻结、FREG 休眠的关系

SGEG 现版已冻结待重设计, FREG 处于休眠. 这条约束把解耦工程切成两半:

**冻结期可以做的**: 算法归位 (SHA-256 从 `privileged/sgeg/` 迁到
`privileged/crypto/` —— 算法归算法, 安全归安全)、注册式挂载、删除死状态.
这些都不预设 SGEG 的判定模型, 换成任何 v2 都成立.

**冻结期不能做的**: 判定出口收敛与主体/客体两 trait. 它们等于"先替 v2 定接口";
若 v2 改判定模型 (例如从能力制改为标签格), 先定的接口要返工. 这正是 2026-11-17
中断当前实现的那条约束的延续. 所以本项目把出口收敛列为**待授权**.

更进一步, "载体中立化"也不能拆开做: ramfs 的 `check_permission` 把 `pwm` 当身份用
(查特权级、匹配 ACE、比属主、查能力位), 不透明 ID 传进去只能再转回整数 —— 那是
假中立. 它必须等 ramfs 判定上提与出口收敛一起做.

## 七、检验方式

长期检验不用读代码, 用两条可机械执行的判据:

1. **删子系统声明**: 注释掉子系统的模块声明, 其余子系统仍能编译. 编译不过, 说明被
   反向了. (Linux 的 `void *i_security` 就是让这条成立的形状.)
2. **数出口**: 统计能获得对象引用的路径数, 要求为 1 或全部经过同一个 authorize 函数.
   [SGEG 设计](sgeg-design.md) 已把这个数写成验收标准, 本项目沿用.

两条都不是"看起来解耦了", 而是能跑的命令. 若某次改动让第二条从 1 变成 2, 那不是
风格问题, 是耦合面扩大.
