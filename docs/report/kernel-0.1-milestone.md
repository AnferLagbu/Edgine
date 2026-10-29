# 内核 0.1 版本目标（深度审计同批交付）

> 核心结论：0.1 定为**外部可验证里程碑**——判据不是内部读数全绿，而是「内核加载并运行一个本项目之外的静态链接用户态程序，且其行为可观测正确」。G1（发布必达）压在单线程静态程序上，多线程列 G2 冲刺且是否纳入取决于 §七 D1；本文档与 [内核与子系统欠缺全景审计报告](./subsystem-gap-deep-audit.md) 同批产出，发布即冻结——后续调整应另开新报告或在 `docs/plan/` 立施工条目，不回头改本文的事实陈述。

这份文档是本次审计对「0.1 该长什么样」的结论陈述：判据如何选定、由哪些前置缺陷决定、哪些范围明确不兑现、四处仍需用户裁定的闸门在哪里。条目的可执行分解与状态追踪属 `docs/plan/` 的范畴，由本文派生，本文不承担该职责；条目编号 `V01-<域>-<序>` 保留作为后续 plan 条目与提交信息的溯源锚。

判据的选择本身即是审计的产物：项目至今的成熟度全部由自研用户态程序与自评门禁背书，从未被第三方二进制检验过。它同时服务于 [项目愿景与长期立场](../explain/vision-hope.md) 的「以 Linux ABI 兼容度承载 OpenHarmony standard 用户态」战略——0.1 是该战略的第一个可证伪节点，不追求 ABI 覆盖度提升，只追求「已宣称的 ABI 面不再假成功」。

## 一、判据定义

**G1（发布必达）**：一个非本项目编写的单线程静态链接用户态程序（`musl` 静态或 busybox 单二进制），在 QEMU x86_64 上由本内核 `execve` 加载，完成串口 `write`、`brk`/`mmap`、`read`/`write`/`close`、`pipe`、`fork`/`wait4`、`exit_group`，以及一次真实 `mount` 后的 `open`/`read` 往返，全程无内核 panic、无假成功。取舍依据是把 0.1 压在已接线子系统的正确性上，而不是压在 0/11 的多线程实装上；它硬依赖 §四 全部条目，并以 §三 的 panic 面与 I6 收口作为「无 panic」这一条件的兑现前提。

**G2（冲刺）**：多线程程序（pthread 建/ join + futex 争用）稳定运行 100 轮无丢唤醒、无 TLS 互踩。前置是 §四 的 V01-THR-1 线程组实装闭环，纳入与否见 §七 D1。

**对外口径降级**：凡未达「已接线」的能力，在文档中不得计入能力宣称。以 V01-DLV-1/2/3 的收口为准重写 `docs/explain/` 相关子系统段落与 `README.md` 的能力面表述；aarch64 的口径见 §七 D3。

## 二、M0 验收地基

门禁的可信度决定前四节读数的解释方式，因此这七条虽最不像功能缺陷，却必须与功能条目同期收口——否则 0.1 的验收本身不可信。七条相互独立，可并行推进。

- **V01-M0-1 `audit_missing_docs_debt` 的读数归属歧义**：P-71 拆 crate 后 `src/kernel/privileged/**` 的同一物理文件同属 `kernel` 与 `kernel-privileged` 两个 workspace 成员，而债册对账模型假定「文件→成员」单射，于是任一 privileged 文件翻成 `deny(missing_docs)` 且存在缺项时该腿必然 fail-closed（详见审计报告 §六），读数还可能双计。收口方向是在脚本内以 `cargo metadata` 的 target 源清单建立权威「源文件→归属成员」映射，显式排除 glue crate 对 `privileged/**` 的 `#[path]` 复用，并把映射本身做成一项自测，映射缺失即红。
- **V01-M0-2 `skip_leg()` 不改退出码**：`ci/audit.sh:141-146` 只累加计数器，未执行的腿与通过的腿在验收读数上不可区分；Lockbud 腿在本机恒走 skip（`:752-755`），即 F4 的并发面至今零次机械验证。收口方向是引入硬腿名单——凡 AGENTS.md §2.2 标注「强制」的腿未执行即非零退出，名单与 §2.2 表格做反向对账（沿用 P-69 的「到位断言反向对账」机制以消除自参照），工具缺失时由 `scripts/requirements.sh` 产出装机校验而非静默降级。
- **V01-M0-3 clippy 与 F5 的 manifest/配方漂移**：clippy 腿只 lint `src/kernel/Cargo.toml --workspace`（`ci/audit.sh:553`、`:581`），host-tests、src/rust、src/user 从不被 clippy，与 AGENTS.md §2.3 第 2 条「四 crate」的表述漂移；F5 宣称 `./ci/build.sh all` 是 release，而 `ci/audit.sh` 步骤 1 实为 dev 配方，release 的 0 warning 没有本地必跑点。两条漂移方向相反（文档欠描述、工具欠覆盖），建议同时收口：要么把 clippy 扩到四路 manifest（与 `:608`/`:614` 的 fmt 腿同构），要么把 §2.3 措辞改为与实装一致，并把 release 配方补进门槛 1，不允许 dev/release 双口径。
- **V01-M0-4 文本断言型测试清点与改造**：host-tests 的 144 个 `.rs` 文件中 91 个含至少一处 `read_to_string`/`include_str!` 读源码再匹配的断言（共 146 处），对源文本形状负责、对行为不负责，是「全绿」读数失真的主要来源。逐处分类：真正的源文本不变式（如「此文件不得出现 `static mut`」）保留并显式命名为结构锁，不计入行为验证覆盖率；伪装成行为测试的（net 的可观测性契约、mm 的所有权契约锁）改为行为断言或删除。新增一条腿统计「行为测试数 / 结构锁数」，作为 0.1 的验收读数之一。
- **V01-M0-5 QEMU e2e 判据的双向锁**：e2e 判据是 grep 日志串，里程碑串仅存在于 `src/user/init/src/main.rs`，两侧无结构锁耦合（历史上已发生一次因日志串改名导致的 7/7 误报）；默认 `E2E_REPEATS=1`（`scripts/qemu_boot_test.sh:43`）使约四十分之一的抖动日常不可见。收口方向是把脚本判据与内核/用户态字符串常量单源化，并把 0.1 验收用例的重复轮数提到能覆盖已知抖动量级，写入 AGENTS.md §2.3 的判据描述。
- **V01-M0-6 在干净提交上重取验收基线**：本轮审计的基线跑在并发在制改动之上，三个非零退出码同源于 `privileged/driver/storage/nvme.rs` 的 F8 补写中间态，不构成 HEAD 属性。待并行补写批次收口后，在无脏文件的提交上重跑四阶段并归档读数，作为 0.1 的起点基线；临时日志现落在 gitignore 的 `other/gate-run-01/`，正式基线须以仓库内文档承载。
- **V01-M0-7 审计腿的可命中性自检**：`scripts/audit_invariants.py:111-121` 的 `check_i6()` 扫的是 `dma_map`/`dma_unmap`/`dma_sync` 三个全仓零命中的符号名，真实 DMA 出口 `alloc_coherent`/`free_coherent`/`DmaStream` 全在 privileged 且被扫描范围排除，故该腿恒报「零违规」。现有门禁没有任何机制区分「检查通过」与「检查从未可能触发」，同类失效可复制到任何一条以文本模式判定的腿。收口方向是为每条模式判定型腿配一份最小阳性样本（植入一处应被抓住的违规），腿在正常树与阳性树上各跑一次，两次读数相同即判失效并阻断；沿用仓内既有自测腿形态（`AUDIT_REQUIRED_LEGS_FULL` 里的 `audit_selftest`、`implicit_deps_selftest`）扩展，不新建框架。

## 三、安全边界兑现

用户指定的必含项。这一节的性质是「卖点真伪」——framekernel 的两个核心宣称（无 UB、外设不可写内核内存）与 panic 可触达性各自是否名副其实。

**V01-SEC-1 panic 可触达性的执法面**。现状是 24 个审计脚本无一条覆盖 panic 面（逐一 grep `panic` 关键字零命中），而实测生产代码有 51 处 `.expect(` 与 66 处 `.unwrap()`，vendored smoltcp 另有 13 与 469 处。framekernel 保证「无 UB」不保证「无 panic」，一条可触达的 panic 即等价拒绝服务。方向是新增 `scripts/audit_panic_surface.py`，以 AST 而非文本正则判定，统计生产代码（截断测试模块）与 syscall 可达闭包内的 panic 源（`unwrap`/`expect`/切片下标/`panic!`/`unreachable!`/算术溢出），输出「可达 panic 点」名单并棘轮化；同时为 release profile 显式声明 `overflow-checks` 取舍——当前未设即静默 wrapping，`src/kernel/functions/syscall/dispatch.rs:841` 的时间换算属之。

**V01-SEC-2 入向报文 panic 点**。内核在两侧 `Cargo.toml` 均启用 `proto-ipv4-fragmentation`，于是 `functions/net/smoltcp/src/iface/fragmentation.rs:171` 的 `self.total_size.unwrap()` 是编译进生产且位于入向重组路径的 panic 点；`iface/interface/mod.rs:821` 的 `neighbor_addr.expect("non-IP response packet")` 同样在入向 ARP/NDP 处理路径上。处置路径待裁定（§七 D2）：(a) 内核侧前置校验，在 RX 入口对分片完整性与 ARP/NDP 响应做判据过滤使 panic 分支不可达，不改 vendored、与 `audit_smoltcp_purity.py` 无冲突，但依赖「我们能穷举出触达条件」这一前提；(b) 最小本地化补丁改 vendored，须用户授权并同步 purity 腿口径。审计的建议是先 (a) 并以 fuzz/构造包验证覆盖，(b) 留待后续版本。

**V01-SEC-3 句柄到 socket 的非 panic 取值路径**。特权侧直接以会 panic 的 smoltcp API 消费用户 fd 派生的句柄：`privileged/net/init/raw.rs:709-720` 调 `get_mut::<T>(handle)` 与 `remove(handle)`，而 `iface/socket_set.rs` 对错误类型、失效句柄与 SocketSet 满的处理是 `expect`/`panic`（`:83`、`:101`、`:103`、`:115`、`:116`、`:128`）。方向是在内核封装层强制非 panic 取值路径（`get`/`try` 语义 + 类型判别分派，句柄与类型的一致性由内核侧 slot 表单一来源裁决），`add` 前先做配额检查，超出返回 `EMFILE`/`ENOBUFS`；并为句柄生命周期竞态补一条行为级回归（close 与 send 并发）。

**V01-SEC-4 不变式 I6 从 0 到 1**。`get_dmar_drhd_list()`（`privileged/arch/x86_64/acpi.rs:887`）全仓零消费者，DMAR/DRHD 表被读入但寄存器从未编程，实际防线是恒等直映射加地址约定；配套的门禁腿本身空转（见 V01-M0-7）。分三步，深度见 §七 D4：消费 DMAR/DRHD 为每个执行 DMA 的设备建立禁止写内核内存的域，至少守住「外设不可写内核高半区」这条底线；`DmaStream`/`free_coherent` 补 Drop 屏障与设备侧停用同步，消除释放后 DMA 写窗口；把 I6 判据改为「DMA 分配出口必须经有界句柄类型」的结构判定并把扫描面扩到 privileged。另加启动期自检——IOMMU 未能启用时 fail-closed 或明确降级宣告，二者取一，不许沉默。

**V01-SEC-5 不变式 I3 收口与 F2 黑名单派生化**。`privileged/proc/process.rs:172` 以 `pub context: Mutex<ProcessContext>` 暴露用户态寄存器，`functions/proc/clone.rs:118`/`247`/`254` 在 safe 层直接改写，`:181` 另经 `privileged/proc/proc_ops.rs:49` 的 `process_ref_mut` 取 `&mut Process`；F2 黑名单（`scripts/audit_functions_boundary.py:45-89`）封了九个 `::raw` 却无任何 `privileged::proc::*` 条目，故这条穿透不会被任何腿报出。方向是把用户寄存器改写收拢到 `privileged::userctx` 的显式 API（functions 只传意图不传指针），`context` 字段去 `pub` 或改私有并提供只读快照，黑名单补 `privileged::proc::raw` 与 `&mut Process` 出口，并让黑名单从手抄枚举改为以 `privileged/mod.rs` 的 pub 面派生 + 白名单例外，消除新增内部模块默认可见的问题。

**V01-SEC-6 中断行注册的启动期类型强制**。中断行注册的安全性压在「启动期单线程」这一未被强制的前提上（`privileged/irqline.rs:85-91` 的 safe `on_interrupt` → `:149-155` 写 `RacyCell`），且 `InterruptHandler` 为裸函数指针无法捕获状态、无注销 API。方向是以类型级强制启动期窗口（Once/Phase 标记，运行期注册返回错误）、补注销路径，并把「handler 需捕获状态只能靠全局槽」的 workaround（`driver/input/keyboard.rs:779-973`）改为 `Fn` + 捕获或显式上下文参数；同批处置 EGDF 的 `driver_as_mut` 可变别名问题（`privileged/egdf/mod.rs:222` 可并发多开 `&mut`）。

**V01-SEC-7 I5 执法面扩到 privileged**。`scripts/audit_invariants.py:98-108` 的 `check_i5()` 只扫 functions 层的 `inb/outb/inl/outw` 文本，privileged 内部的绕过全部逃逸门禁，实测残余集中在 `privileged/driver/display/mod.rs:449-468` 裸 PIO 与 `:200-204` VBE 裸 `read_volatile`、`privileged/pci/mod.rs:246-381` 的 ECAM 裸解引用与 CFAM 直写、`pci/msi.rs:369-393` 的 MSI-X Table 裸 `*mut`、`privileged/pci/infra.rs:38-136` 导出的裸 `outb/inb`。方向是把这四类残余收拢到 `IoPort`/`IoMem` 安全代理之后，将 I5 判据扫描面扩到 privileged 的白名单外全量；无法收拢的（早期 PCI 配置探测）进定点清单，并按 P-50 口径声明为「定点回归锁，非树级防线」。

## 四、可交付性断层

G1 的硬前置：这几件事不修，外部用户态的失败会以「看似成功实则错误」的形式扩散。

**V01-DLV-1 幻影挂载**。`functions/fs/vfs_mount.rs:50-55` 的 `Ext2`/`ExFat` 臂体仅注释，控制流落到 `functions/fs/vfs_manager.rs:262-282` 的 `mount()`，该函数只置 path/fs_type/used 即返回 `Ok(())`，不调 `fs_mount`、不绑 trait object（同文件 `:284` 起另有一条带 trait object 的真实入口，Ext2/ExFat 没用它）；devfs 的挂载调用只存在于测试。收口是让 `mount(2)` 的返回语义与后端存在性绑定，未注册后端即 fail-closed 返回错误，并补一条行为级回归——`mount` 成功后必须能完成一次真实 `open`/`read`，当前测试口径发现不了这个缺陷。

**V01-DLV-2 UNKFS 目录项持久化与 readdir/truncate**。`functions/fs/unkfs/unkfs_data.rs:1623-1636` 的序列化循环写作 `for (name, _value)` 并把对象号硬写为 0，remount 后全部目录项指向对象 0；它不是死代码而是「活着且错」，磁盘往返零覆盖是它至今未暴露的唯一原因。顺序是先补磁盘往返测试（RAM-backed block 设备 mock 或 QEMU 磁盘 + `mount_drive` 恢复）用测试锁定缺陷，再修序列化/反序列化对称性；同批补 `readdir`（现返回 `NotSupported`，`unkfs_inode.rs:330-337`）与 `truncate`——`readdir` 不补则挂载后连 `ls` 都不成立，G1 的「mount 后 read 往返」用例无法通过。

**V01-DLV-3 syscall 假成功臂清零**。`functions/syscall/dispatch.rs` 实测 242 条分发臂（241 个唯一 `SYS_` 名），其中 `:241` 把 `SYS_select` 按 poll 语义接线（fd_set 指针被当 pollfd 数组、`writefds` 被当 nfds）、`:191` 把 `SYS_newfstatat` 的路径指针截成 i32 当 fd、`:675` 的 `SYS_getcpu` 不写 `*cpu`/`*node` 出参而用返回值打包，`privileged/mm/numa.rs:399-415` 的 `sys_get_mempolicy` 读出策略后 `let _ = (mode, mask);` 显式丢弃并返回 0、`:439-441` 的 `sys_migrate_pages` 空操作恒返 0。假成功使第三方程序带错误结果继续运行，故障以远离根因的形式呈现，比 `ENOSYS` 危险得多。方向是对全部分发臂做一次真实现/部分实现/恒错 stub/假成功四分类读数（本次审计给出约 150/50/5/5，需以脚本固化而非人工），假成功臂逐条处置——能实装的实装，不能的改 `ENOSYS`，并把四分类读数纳入验收作为 ABI 可信度的单源指标。

**V01-DLV-4 边界与并发缺陷**。`privileged/mm/vma.rs:866` 的 brk 收缩分支不可达并在 `:868` 造出 `start > end` 的倒置 VMA（页表不拆、堆泄漏）、`functions/mm/mmap.rs:210` 的 munmap 缺 `start + length` 溢出检查、`MAP_FIXED` 未实装、`privileged/syscall/futex.rs:310-317` 的丢唤醒窗口（入桶释锁后才 `process_block`，期间的 wake 被吞，等待者永久阻塞）与 `:274` 处 `_timeout` 被直接丢弃、`privileged/net/route.rs:95-109` 在未持 `NET_STATE` 下调 `stack_mut()`（SMP 可变借用竞态）。逐条修复并各配一条行为级回归：brk 收缩用「扩后缩到区间内再查 VMA 一致性」，futex 用「入桶与阻塞之间注入 wakeup」的时序测试，route 用 SMP 下与 poll 并发的压测。route 的修法优先「取 `NET_STATE` 后再改表」，其次才考虑把路由同步移出 syscall 上下文。

**V01-DLV-5 syscall 编号侵入 Linux 新编号带**。`SYS_SGEG_LOGIN=400`、`SYS_SGEG_DISK_INSTALL=453`、`SYS_SGEG_HOTPLUG_STATUS=463` 落在 Linux 6.x 已分配区间，与 G1 的外部程序共存时构成编号碰撞风险；另有 `renameat2=316` 有常量无臂。方向是把自定义编号迁入未分配带或以 `EG_` 前缀约定单源隔离，补一条「与 Linux x86_64 官方表零碰撞」的校验腿（现有校验只覆盖到当时已知表，需随基线更新），`renameat2` 补臂或撤常量。

**V01-THR-1 线程组实装闭环（G2 的前置）**。现状为 0/11：`gettid==getpid`（`functions/proc/info.rs:48-50`）、`CLONE_SIGHAND` 只校验不共享、`CLONE_VM` 子进程拿到全新 fd_table/sigaction_table、TLS 切换未实装（`switch.asm` 无 FS_BASE 恢复，同核互踩）、`exit_group` 等同 `exit` 不杀兄弟线程、无 `set_tid_address`。若 §七 D1 裁定纳入，最小闭环顺序为 tid/tls 与 per-thread 寄存器集 → 共享 fd/sighand 的语义拆分 → `exit_group` 全组终止 → futex 正确性（复用 V01-DLV-4）→ `set_tid_address`/`CLONE_SETTID`，每项配 glibc 兼容行为测试；若不纳入，0.1 的能力表述必须限定为「单线程用户态」。既有施工拆解见 [多线程模型工程](../plan/multithreading-project.md) 的 K-01~K-07，本条与之为同一议题，不重复立案。

## 五、外部用户态试点

**V01-PLO-1 执行载体选型**。G1 需要一个静态链接的非本项目用户态程序与最小行为集：`musl` 静态 + busybox（单二进制多功能，可只挑 `ls`/`uname`/`sh` 内建），或 busybox `nc` 走一次 TCP echo（复用既有网络 e2e 判据）。产物入 `other/build/<arch>/user/` 按架构分目录，遵循 S-6 三条防线，让 `scripts/verify_image_arch.py` 的 ELF 头 + 逐字节包含判据自然覆盖它。

**V01-PLO-2 失败归因机制**。外部程序首次跑通的过程中每一条缺口都要可追踪：以「缺口 → 本文条目编号 → 是否 0.1 必收」三列表逐轮登记；缺口若属 §六 明确不做范围，则记录为对外口径的裁剪项，而不是悄悄跳过。

## 六、0.1 明确不做

范围红线的价值不在于「少做」，而在于把不做的项目从能力宣称里摘出去——不启动实装的前提是完成 §一 的对外口径降级，写清「已实现未接线」。

**V01-OUT-1 未接线子系统不启动实装**：FREG 恢复框架复活（维持 DECISION-L 冻结，护栏已 fail-closed，恢复语义不兑现）；wasm/WASI 接线（约 5,600 行运行时、15 个 `.rs`，`dispatch.rs` 中对 `wasm`/`wasi` 零命中）；USB 传输路径（`functions/driver/usb/xhci.rs:1189-1191` 只 `ring_doorbell` 不写 Transfer Ring TRB，EHCI/UHCI/OHCI 不存在）；UNKFS 的 raidz/dedup 写路径、netfilter 判决与 NAT、DHCPv6、MSI-X 多队列与 aarch64 GIC ITS；EGDF 可加载模块的符号版本与引用计数、用户态驱动接线。

**V01-OUT-2 性能维整体不做**：PCID/`invpcid` 单点失效、buddy 分片、VMM 全局锁、驱动持 `NET_STATE` 忙等、RX 背压与流控。`NET_STATE` 忙等仅作为 V01-DLV-4 的 route 修法附带风险登记，不单独优化；性能基线文件按现状更新，不新增门槛。

**V01-OUT-3 aarch64 功能面对等不做**：aarch64 缺失的是**缺页处理器本身**——`privileged/arch/aarch64/exception.rs` 约 `:857-880` 把 EL0 同步异常一律处理为杀进程，故 demand paging/COW/swap/uffd 全不可兑现，信号投递未实现，`vmm_aarch64.rs:526` 的 `unmap_page` 是空函数。这属新增子系统级实装而非补洞。0.1 判据限定 x86_64，aarch64 对外口径按 §七 D3 裁定后同步到 `docs/explain/` 与 `README.md`。

## 七、需用户裁定的决策闸门

四处均属 AGENTS.md §9.1 的决策灰色地带，未裁定不开工。审计侧的倾向已写明，但裁定权在用户。

**D1 G2（多线程程序跑通）是否纳入 0.1**：纳入则关键路径增加线程组实装（tid/TLS/共享表拆分/`exit_group`），周期显著拉长且与 futex 正确性强耦合；不纳入则 G1 只用单线程静态程序验收，且必须在对外表述中明确「尚不支持 pthread」。倾向不纳入——G1 已足以暴露 ABI 与可达性断层。

**D2 入向 panic 的处置路径**：内核侧前置校验（不改 vendored，与 `audit_smoltcp_purity.py` 无冲突、可回退，但要求能穷举触达条件）还是最小本地化补丁（改锁定的第三方源码，更彻底，须用户授权并重议 purity 腿口径）。倾向先者，后者留待后续版本。

**D3 aarch64 的 0.1 对外口径**：(甲) 明示「仅启动与固定映射用户态」，最诚实；(乙) 补最小缺页处理器以支持静态非 PIE 程序，成本约一个子系统级工程；(丙) 从 0.1 宣称中暂时移除 aarch64 用户态。建议甲。

**D4 I6 的实装深度**：(甲) 保守策略——读 DMAR/DRHD、为 DMA 设备配置「禁止写内核高半区」的最小域，不追求完整 remapping 语义，成本可控且能把 I6 从「无防线」变「有底线防线」；(乙) 完整 IOMMU remapping + 每设备隔离域 + 中断重映射，对齐 Asterinas `dma_remapping` 形态，成本数倍。建议甲，并把乙登记为后续里程碑。

## 八、执行序列

M0（§二，地基，先行且独立可收口）→ 安全边界（§三 的 panic 执法面与 I3/I5/I6 三条）→ 可交付性（§四 的幻影挂载、UNKFS 持久化、假成功臂、边界并发缺陷）→ 外部试点（§五，边跑边归因，其失败清单反向决定是否回头补 §四 的遗漏项）。

理由是门禁不修则后续所有「全绿」无解释力，I6 与 panic 面是卖点真伪问题，可交付性是 G1 硬前置。试点本身应排在最后，但必须尽早开第一次尝试以校准缺口——建议在 M0 收口后即做一次「失败取数」而不追求通过。
