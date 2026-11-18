# 硬编码容量上限评估

> 结论：全仓存在**系统性的小常数上限**，且缺少统一治理。按危害分为三类：**A 类静默截断**（文件名被 `.min()` 截短后照常成功，调用方无法察觉）、**B 类容量过紧**（fd 64、挂载点 8、`/dev` 16、单接口 socket 32 等，多数失败闭合）、**C 类同名多值**（同一常量名在不同层取不同值，共 4 组）。其中 A 类与 C 类性质比 B 类严重：前者破坏正确性，后者使“上限是多少”无法从单一位置回答。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读：静态检索与源码阅读，未构建、未运行负载。

## 一、扫描方法与读数

范围：`src/kernel/**/*.rs`，排除 vendored `functions/net/smoltcp/`。

- 容量类常量（名含 `MAX` / `CAPACITY` / `LIMIT` / `LEN` / `DEPTH` / `SLOTS` / `BUCKETS` / `COUNT` / `SIZE`，值为纯数字）：**231 条**；
- 定长数组 / 静态表（`[T; N]` 形态）：**124 处**；
- 其中 ≤ 512 的紧容量常量：**275 条**（含跨文件重复名）。

判断分三问：**超限时是失败闭合还是静默出错**、**该上限与真实工作负载的比值**、**同一语义是否只有一个定义**。

## 二、A 类：静默截断

这一类不返回错误，而是把输入截短后继续执行。

### 2.1 文件名截断（确证）

`.min(...)` 形态广泛存在：

| 文件系统 | 截断代码 | 上限 |
|---|---|---|
| RamFS | `functions/fs/ramfs_core/mod.rs:285` | `VFS_MAX_NAME` = 64 |
| RamFS 节点 | `ramfs_core/ramfs_node.rs:111` | `VFS_MAX_NAME - 1` = 63 |
| TmpFS | `functions/fs/tmpfs.rs:354` | 64 |
| OverlayFS | `functions/fs/overlayfs.rs:791` | 64 |
| cgroupfs | `functions/fs/cgroupfs.rs:118`、`:179` | `MAX_NAME_LEN` = 32 |
| configfs | `functions/fs/configfs.rs:75`、`:167` | 32 |
| virtiofs | `functions/fs/virtiofs.rs:135` | 32 |
| systree | `functions/fs/systree.rs:96`、`:162` | 32 |
| DevFS | `functions/fs/devfs.rs:196` | `DEVFS_MAX_NAME - 1` = 31 |

后果：创建一个 65 字节的文件名（`VFS_MAX_NAME=64`）会被截成前 64 字节并**返回成功**。若截断后的名字已存在，实际写入的是另一个文件；若不存在，则创建了一个与用户请求不同的名字。这类错误在任何返回值与日志里都不出现，符合「假成功」的同一模式。

对照：Linux `NAME_MAX = 255`。这里最紧的 32 字节只有其 1/8。

### 2.2 路径长度：实现失败闭合，但文档说截断

- 上限：`VFS_MAX_PATH = 128`（`functions/fs/vfs_types.rs:19`）、`FS_MAX_PATH = 128`（`privileged/proc/fs_context.rs:32`，注释要求两者必须一致）。
- 实现：`resolve_user_path` 在「根前缀 + 视图路径」拼接后 `if root_len + 1 + tail.len() >= VFS_MAX_PATH { return None; }`（`functions/fs/vfs_manager.rs:585-587`）——**失败闭合**，不截断。
- 文档：`functions/fs/api.rs:45` 写「超过 `VFS_MAX_PATH` 长度 → **截断**到该上限」。

两者不一致。以代码为准，路径超限是失败；但文档描述会让读者以为存在截断语义。无论采用哪种，当前都没有向用户态返回 Linux 的 `ENAMETOOLONG` 契约——要么静默失败为 `None`，要么（按文档）截断到错误目标。

`VFS_MAX_PATH = 128` 相对 Linux `PATH_MAX = 4096` 只有 1/32；深目录（构建产物、包管理器缓存）在外部程序试点中很容易越过。

## 三、B 类：容量过紧

| 能力 | 上限 | 满时行为 | 位置 |
|---|---|---|---|
| 每进程 fd 表 | 64 | `None` → `EMFILE` | `privileged/proc/fd_table.rs:27` |
| `RLIMIT_NOFILE` 默认 | 32 | 软上限，与 fd 表不一致 | `privileged/config/capacity.rs:45`、`proc/rlimit.rs:120-121` |
| 进程总数 | 256 | 插入失败 → 析构后 `None` | `config/capacity.rs:32`、`proc/scheduler.rs:585-592` |
| 线程总数 / 每进程线程 | 128 / 16 | fail-closed | `config/capacity.rs:35-38` |
| 挂载点 | 8 | `KernelError::NoSpace` | `functions/fs/vfs_types.rs:23`、`vfs_manager.rs:310-330` |
| DevFS 设备节点 | 16 | `KernelError::NoSpace`（`devfs.rs:228`） | `functions/fs/devfs.rs:36` |
| RamFS 节点 | 256 | fail-closed | `functions/fs/ramfs.rs:36` |
| 单接口 socket | 32 | SocketSet 满（panic 语义） | `functions/net/smoltcp_impl.rs:61` |
| socket 总量 | 256（请求 1024 被 clamp） | fail-closed | `privileged/net/init/sockets.rs:20`、`:60`、`:68` |
| POSIX timer | 32 | fail-closed | `privileged/proc/posix_timer.rs:64` |
| timerfd 槽 | 16 | fail-closed | `privileged/syscall/timerfd.rs:51` |
| IPC：pipe / semaphore / shm / msg queue | 64 / 64 / 16 / 32 | fail-closed | `privileged/ipc/types.rs:32-70` |
| 路由表 | 16 | `Errno::Enomem` | `privileged/net/route.rs:30`、`:105` |
| UDS 绑定 / 每进程 UDS fd | 32 / 16 | fail-closed | `functions/net/unix.rs:39`、`:30` |
| futex 槽 | 64 桶 × 16 = 1024 | fail-closed | `privileged/syscall/futex.rs:98-101` |
| uffd 实例 / 事件 / 区间 | 16 / 16 / 4 | `EINVAL` / `ENOSPC` | `privileged/mm/uffd.rs:175-179`、`:408-411` |
| ELF 程序头 | 128 | 拒绝 | `privileged/proc/elf/mod.rs:101` |

这一类的共同优点是**失败闭合**：返回明确错误，不越界、不 panic（进程表满时还正确走了 `drop_boxed_process`，避免 `Arc<ThreadGroup>` 泄漏）。问题只在「上限是否够用」与「用户能否感知」。

需要单独指出的是 **fd 表 64 与 `RLIMIT_NOFILE` 32 的错位**：fd 表实际能容纳 64 个，但默认 rlimit 只允许 32 个；程序若按 `getrlimit` 的读数做容量规划，会低估内核真实上限，反之 `setrlimit` 抬高到 64 后又会撞上表容量。

## 四、C 类：同名常量多值

| 名字 | 取值 1 | 取值 2 | 后果 |
|---|---|---|---|
| `MAX_OPEN_FILES` | 32（`privileged/config/capacity.rs:45`，供 rlimit） | 256（`functions/fs/open_file_table.rs:20`） | 两套“打开文件上限”并存 |
| `MAX_SOCKETS` | 256（`privileged/net/init/sockets.rs:20`） | 32（`functions/net/smoltcp_impl.rs:61`） | 总配额 256，单接口实际 32 |
| CPU 上限 | `MAX_CPUS = 1024`（`config/capacity.rs:20`） | `CPU_MPIDR_MAX = 8`（`privileged/dtb.rs:65`） | aarch64 超过 8 核不记录 |
| `MAX_NAME_LEN` | 32（cgroupfs / configfs / systree / virtiofs） | 64（`VFS_MAX_NAME`） | 跨 FS 行为不一致 |

这类不是容量大小问题，而是**语义漂移**：修改其中一处不会传播到另一处，且没有门禁对账。`DEFAULT_MAX_SOCKETS = 1024` 再被 `clamp(1, 256)`（`sockets.rs:68`）压制，是同一漂移的第三种表现。

## 五、与已知问题的同源性

前一份《简化实现与稳定性评估》记录的三项（page cache 64×16、swap 4096 槽 + LRU 256、无 DMA zone）与本文的 B/C 类共享同一模式：**用小常数换取实现简单，但没有容量治理层**。区别在于：

- page cache / swap 的耗尽影响性能与可用性；
- A 类截断影响**正确性**（写到错误的文件名）；
- C 类多值影响**可推理性与可维护性**（无法从单一位置回答“上限是多少”）。

## 六、对 0.1 外部程序试点的触达分析

按「外部静态程序跑通」路径的触达概率排序：

1. **路径 128 / 文件名 64**：深路径与长名字在任何真实构建产物中都会出现；且 A 类是静默截断，故障归因成本最高；
2. **挂载点 8 / DevFS 16**：根 + `/proc` + `/sys` + `/dev` + `/tmp` 已占 5 个挂载点；`/dev` 的 null / zero / full / random / urandom / tty / console / ptmx 已占 8 个；
3. **fd 64 + rlimit 32**：单个静态程序通常够用，涉及管道与临时文件时开始紧张；
4. **单接口 socket 32**：仅当试点含网络用例时触达。

因此 A 类与「挂载点 / DevFS」应视为 0.1 的**隐式前置条件**，而不是“以后再说”的优化项——它们不会在编译期报错，只会在试点运行时以无法归因的方式失败。

## 七、为什么没有防线

- 无任何审计腿统计容量常量、检查同名多值、或检查截断语义；
- `check_doc_paths.py` 只校验路径是否存在，不校验 `api.rs:45` 这类“文档说截断、实现是失败”的语义一致性；
- F9 只覆盖死代码抑制，不覆盖“常数是否合理”；
- `SIMPLIFIED` 标记本可承载“此处以 N 为上限、何时需扩展”，但前一份清单已证明 142 条中 0 条写了扩展条件。

结论：容量上限目前是**纯人工知识**，既不进 CI 读数，也不进文档契约。

## 八、对计划的意义

三类问题应分开处置，不能合并成「调大常数」：

1. **A 类先改语义再改数值**：截断必须先变成显式错误（`ENAMETOOLONG` / `ENOSPC`），再讨论上限从 64/128 提到多少。先调大而不改语义，只会把故障推迟到更长的名字上。
2. **B 类按触达排序扩容**：挂载点与 DevFS 是结构性瓶颈（需要动态分配而非定长表），fd/socket 是数值瓶颈（可先调大）。两者实现代价不同。
3. **C 类先建单源**：所有容量上限应收敛到一个可被审计的单一来源（或至少建立同名常量对账腿），否则每次扩容都会漏掉一处。
4. **文档与行为对齐**：`api.rs:45` 的“截断”描述属于需要修正的契约陈述；无论最终选失败还是截断，文档必须与实现一致，否则下一个维护者会按错误前提修改代码。

在这些完成前，任何「内核支持 N 个进程 / N 个文件 / N 个挂载」的宣称都应以本文清单为准，而不是以常量的名字为准。
