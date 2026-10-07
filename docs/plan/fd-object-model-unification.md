# fd 对象模型统一工程（per-process fd 表接管全局 FdPlan）

描述：终态目标——消除"全局 FD 编号 + 子系统基址分段"过渡设计。当前 8 个非 VFS 子系统（Smoltcp/UDS/EventFd/SignalFd/Inotify/TimerFd/PidFd/UserFaultFd）的 fd 由全局位图分配、fd 数值全局有效且与进程无关：任何进程可引用任何其他进程的 socket fd（I3/I4 类边界松动），dup2 到 socket、CLOEXEC、进程退出回收均缺失。终态采用 Linux `file_operations` / Asterinas `File` trait 同构设计：引入与文件系统解耦的统一 fd 对象 trait（read/write/poll/ioctl/release），全部对象注册进 per-process `FdTable` → `OpenFileTable`，`FdPlan`/`FdSubsystem`/`subsystem_of`/`idx_of` 整体退役，fd 分发按对象能力（`Arc<dyn File>` 动态分发）而非数值区间。
方案：分四阶段（各自独立 PR 序列，达成 §2.3 六门槛）：
1. [X] 设计立场先行：新增 `docs/design/fd-object-model-design.md`——trait 方法集、poll 事件模型（与 P4/D8b 衔接）、与 `Inode` trait 的关系（叠加 `File` 层 vs `OpenFile` 双持有）、fork/dup/exec 语义矩阵。
2. [ ] 抽象落地：`functions::fs` 引入 fd 对象 trait；`OpenFile` 与 Inode 解耦（socket/eventfd 等非文件对象进入 `OpenFileTable`）；VFS fd 迁移至统一分发点（read/write/close/poll 单入口）。
3. [ ] 子系统迁移：8 个非 VFS 子系统逐个把槽位注册改为 fd 对象挂表（每个子系统一个 PR + 回归测试）；全局位图分配器随最后一个子系统迁移后删除。
4. [ ] 收尾退役：删除 `FdPlan`/`FdSubsystem`/`subsystem_of`/`idx_of` 与 `sm_slot` 类适配层（2026-10 方案 C 引入的 sm_fi.rs `sm_slot` 是唯一残留消费面，grep 本符号即全部迁移点）；同步 `fd_allocator_unified_test` / `td15_fd_idx_of_test` 等契约测试退役或反转。
状态：[]（阶段 1 设计定稿已完成；阶段 2-4 未开始。前置依赖解除：P4/D8b poll 接线的签名/位值/契约已按设计文档第四节锚定（`sm_socket_poll` 为 `File::poll` 数值 fd 前影），P4 可先行施工；终态迁移时仅改写分发点。
详情：输入依据见 `docs/report/context-consistency-audit.md` §5（TCB 压力与双路径收敛约束）与 `other/HANDOFF-socket-layer-P3.md` §3.2（命名空间重叠根因）；方案 C（fd_alloc 基址重排 + `sm_slot` 单点换算，2026-10）为止血桥接，非本工程的替代。
