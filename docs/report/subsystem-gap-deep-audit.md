# 内核与子系统欠缺全景审计报告

> 总体判断：分层纪律、注释卫生与死代码执法在同类自研内核中属第一梯队，且经实测可复现；真正的短板不在代码质量，而在**可交付性**——大量子系统处于「实现完整但未接线」的中间态，而承担验收职责的门禁体系中有多条腿并不真正执法。结论性口径：**「已实现」面显著大于「已接线」面，「全部门禁绿」弱于「行为已验证」**，任何能力宣称都需按可达性折价。

本报告是一次跨子系统深度取证的可复现快照，评估基线为 HEAD `ca484ac9` 加当时工作树的在制改动；交付前对全部 S1/S2 条目的行号按工作树重验一次，此时 HEAD 已推进到 `f4249d7c`，`privileged/mm/vma.rs` 等文件仍在并发改动中。取证方式为八个并行审计线逐子系统读码，再对每条 S1/S2 结论由主线亲自复验。作为制定 0.1 版本目标（见 [内核 0.1 版本目标](./kernel-0.1-milestone.md)）与后续修复工程的输入依据。

## 一、证据口径与三项前提

**三档可达性判据**（全文统一使用）。*已实现*：代码存在且能编译。*已接线*：存在一条从 syscall 分发或硬件中断路径出发的真实调用者。*已验证*：存在真正执行被检行为的测试——**用 `read_to_string`/`include_str!` 读源码再做文本匹配的断言不计入已验证**，因为这类断言只证明源文本形状，不证明行为。

**并发施工前提**。本机同时有多个 agent 在同一工作树施工，因此全文行号只对交付时刻的工作树有效，后续读者必然遇到漂移；引用时以「函数名 + 行号」双重定位为准，判据本身（分支可达性、调用者有无）以函数体为准而非行号。

**一处数字更正**。本报告立项阶段的中间读数曾把非 vendored 的 `.expect(` 记为 1,731 处、functions 层 488 处，该读数不成立，根因是检索式未按字面匹配（`.` 被当通配符），把 1,661 处 `#[expect(clippy::…)]` lint 抑制属性一并计入。按字面匹配的实测口径是：

| 计数面 | `.expect(` | `.unwrap()` |
| --- | --- | --- |
| 全层（不含 vendored smoltcp，含测试代码） | 156 | 245 |
| 生产代码（再截断到文件内首个测试模块之前） | 51 | 66 |
| vendored smoltcp 单独计数 | 13 | 469 |

修正后的结论不是「safe 层 panic 面很大」，而是更尖锐的一条：**自有生产代码的 panic 面很小（117 处，且抽查八条 syscall 链未见参数直接稳定触达），真正的 panic 面集中在被 `#![deny(unsafe_code)]` 光环覆盖的 vendored smoltcp 与其调用方式上**（见 §三.2）。

## 二、可交付性断层：已实现但未接线

这是本次审计最大的一类欠缺，也是「内部看起来完备、对外无法兑现」的直接来源。

**幻影挂载**属 S1。`functions/fs/vfs_mount.rs` 的 `FsType::Ext2` 与 `FsType::ExFat` 臂体内只有注释（约 :50-55），控制流落到 `VFS_MANAGER.mount()`；而该函数（`functions/fs/vfs_manager.rs:262-282`）只做 `set_path` + `set_fs_type` + `used = true` 便返回 `Ok(())`，既不调 `fs_mount` 也不绑 trait object。同文件约 :284 另有一条带 trait object 的挂载入口，才是真实路径——Ext2/ExFat 没用它。用户态可见后果是 `mount()` 返回成功、挂载点无后端、后续所有 I/O 失败。同型问题：devfs 的挂载调用只存在于测试里，默认启动仅挂 ramfs。

**UNKFS 目录项持久化丢映射**属 S1，也是单点最重的一条。`functions/fs/unkfs/unkfs_data.rs:1623-1636` 的序列化循环写作 `for (name, _value)` 并把值域硬写成 `write_le64(&mut buf, off, 0)`，即目录项的名字进了盘、对象号被清零；反序列化侧恢复出 name→obj 0，**remount 后全部目录项指向对象 0**。它不是死代码而是「活着且错」，之所以从未暴露，是因为持久化测试只做内存结构往返（`host-tests` 的 unkfs 六份文件全走内存 mock），磁盘往返与 `mount_drive` 恢复路径零覆盖。同一子系统的崩溃一致性卖点也未闭环：内核内的 `zil.sync` 只清内存记录（`zil.rs:323-333`），559 行的磁盘 WAL 实现 `zil_persist.rs` 在内核侧零调用者、挂载路径从不重放日志。

**syscall 假成功臂**属 S1，对外部用户态是直接阻断。`functions/syscall/dispatch.rs` 实测 242 条分发臂（241 个唯一 `SYS_` 名），存在若干「返回成功但未做事」的臂：`:241` 把 `SYS_select` 接到 `poll_syscall`，而两者参数语义完全不同（fd_set 指针被当 pollfd 数组、`writefds` 被当 nfds）；`:191` 把 `SYS_newfstatat` 接到 `fstat_syscall(a1 as i32, a2)`，即路径指针被截成 32 位当 fd，低位撞上有效 fd 即产生假成功；`:675` 的 `SYS_getcpu` 不写 `*cpu`/`*node` 出参而用返回值打包（Linux 语义应返回 0）；`privileged/mm/numa.rs` 的 `sys_get_mempolicy`（:399-415）从进程表读出策略后用 `let _ = (mode, mask);` 显式丢弃、从不写回用户指针即返回 0，`sys_migrate_pages`（:439-441）空操作恒返 0。假成功比 `ENOSYS` 危险得多：第三方程序会带着错误结果继续跑，故障以远离根因的形式呈现。

**其余未接线清单**（S2 为主，逐条已在代码中核实调用者缺失）：wasm 运行时约 5,600 行（15 个 `.rs`），`functions/syscall/dispatch.rs` 中对 `wasm`/`wasi` 零命中，即整个 WASI 面无入口；UNKFS 的 `raidz.rs` 仅被 `unkfs/mod.rs:17` 的 `pub mod` 引用一次，`dedup` 的生产写路径未接入；`netfilter` 有规则增删 syscall 分派而判决函数 `nf_hook()` 全仓零调用，防火墙规则静默无效，NAT 无任何实现；`timer` 的 tickless 状态机完整但 `enter/exit_tickless` 生产零调用者，仅自定义 `EG_TICKLESS` syscall 可达，idle 仍是周期 tick；`egdf/user_driver.rs` 的 bind/map/forward_irq 无任何 syscall 调用者，用户态驱动整条路未接线；USB 侧 `functions/driver/usb/xhci.rs:1189-1191` 只 `ring_doorbell` 而不写 Transfer Ring TRB（SIMPLIFIED 自认），EHCI/UHCI/OHCI 不存在；`functions/net/smoltcp` 的 DNS 只有内核内 API，无 `getaddrinfo` 用户面；`msysnd` 类 System V IPC 亦未接线而注释声称已分发。

**aarch64 的能力塌方**属 S2 且影响版本判据。`privileged/arch/aarch64/exception.rs` 约 :857-880 把 EL0 同步异常一律处理为杀进程——**aarch64 没有缺页处理器**，于是 demand paging、COW、swap、uffd 在 aarch64 全部不可兑现，`vmm_aarch64.rs:526` 的 `unmap_page` 是空函数；`arch/aarch64/exception.rs` 约 :603 处信号投递未实现；GIC 无 ITS/LPI，MSI-X 在 aarch64 写入 `addr=0`（`pci/msi.rs:385-388`）即该侧 MSI-X 全废。双架构「能编译、能启动」是真的，但**功能面对等是假的**。

## 三、正确性与并发暗桩

**brk 收缩分支不可达**属 S1。`privileged/mm/vma.rs:866` 以 `if page_aligned > start_brk` 为首要判据，命中即走扩展分支并在 `:868` 构造 `Vma::new(current_brk, page_aligned, …)`；当收缩后的 `page_aligned` 落在 `start_brk` 与 `current_brk` 之间时，构造出 `start > end` 的倒置 VMA，而 `:872` 的 `else if page_aligned < current_brk` 收缩分支永不可达，页表不拆、堆内存泄漏。syscall 侧可达（`functions/syscall/brk.rs`）。同族边界缺陷：`functions/mm/mmap.rs:210` 的 munmap 未做 `start + length` 溢出检查，mmap hint 既不校验重叠也不要求对齐，`MAP_FIXED` 常量已定义但未实装。

**路由 syscall 缺锁**属 S1。`privileged/net/route.rs` 的 `route_add`（:95-109）/`route_del`（:122）只持 `KERNEL_ROUTE_TABLE`，却在临界区内调 `raw::stack_mut()` 写 smoltcp 路由表（:290、:364），而 `init/raw.rs:16-27` 的契约明写「调用方必须持 `NET_STATE`」。SMP 下与持锁的网络 poll 并发取得可变借用，构成数据竞态乃至 UB——这是本审计里少数几条**不依赖输入即可成立**的正确性缺陷。

**持全局锁忙等**属 S1 且是 SMP 吞吐硬伤。驱动 TX 在 `NET_STATE` 临界区内自旋等待设备完成：virtio 轮 used 位至多 10 万次（`functions/driver/virtio/net.rs:501-514`）、e1000 轮 DD 位（`functions/driver/net/e1000.rs:212-219`）。等待期间全核的其他 socket 操作与 tick 触发的 poll 都被串行冻结；超时仅丢帧，而协议栈侧仍认为已发送。

**futex 丢唤醒与超时失效**属 S1。等待队列实装在 `privileged/syscall/futex.rs`（`functions/sync/futex.rs` 只做参数校验与转发）。入桶与阻塞不在同一临界区：`futex_wait` 于 :302-310 持桶锁挂入等待者后立即释放，直到 `:317` 才调 `process_block`，而 `FutexBucket::wake`（:155-175）在锁内清掉等待槽并调 `process_unblock`——落在 :310 与 :317 之间的一次 wake 先 `SCHEDULER.unblock(pid)`、随后本线程无条件 `SCHEDULER.block(FutexWait)`（`proc_ops.rs:396-407`），唤醒被吞掉，等待者永久阻塞。`_timeout` 参数在签名处即被丢弃（:274），`FUTEX_WAIT` 语义上没有超时。等待槽固定 64 桶（:108）且等待者只在自身醒来后自摘（:324），退出路径 `privileged/proc/robust.rs` 只做 robust list 与 CLEARTID 唤醒、不作通用摘槽，故进程异常退出会留下脏槽，pid 复用后产生虚假 unblock。futex 属「已接线未验证」，直接决定任何 pthread 程序能否稳定运行。

**smoltcp panic 面**属 S1，且是修正口径后真正的 panic 风险主体。内核在两侧 `Cargo.toml` 均启用 `proto-ipv4-fragmentation`，于是 `functions/net/smoltcp/src/iface/fragmentation.rs:171` 的 `self.total_size.unwrap()` 是**编译进生产且位于入向重组路径**的 panic 点；`iface/interface/mod.rs:821` 的 `neighbor_addr.expect("non-IP response packet")` 同样在入向 ARP/NDP 处理路径上。特权侧又直接以会 panic 的 API 消费由用户 fd 派生的句柄：`privileged/net/init/raw.rs:709-720` 一路调用 `sockets.get_mut::<tcp::Socket>(handle)` 与 `sockets.remove(handle)`，而 `iface/socket_set.rs` 对错误类型与失效句柄的处理是 `expect("handle refers to a socket of a wrong type")`（:101/:115）与 `panic!("handle does not refer to a valid socket")`（:103/:116/:128），SocketSet 满时 `:83` 亦直接 panic。framekernel 保证的是「无 UB」，不保证「无 panic」——**一条入向畸形分片即远程拒绝服务，一个句柄生命周期竞态即本地拒绝服务**。现有 24 个审计脚本无一条覆盖 panic 可触达性（逐一 grep `panic` 关键字零命中），kani harness 存在但未接入门槛。

**不变式 I3 未真实守住**属 S1 级设计偏差，且门禁结构性看不见。`privileged/proc/process.rs:172` 以 `pub context: Mutex<ProcessContext>` 暴露用户态寄存器实例，`functions/proc/clone.rs:118`/`:247`/`:254` 在 safe 层直接加锁改写寄存器，`:181` 另经 `proc_ops.rs:49` 的 `process_ref_mut` 取得 `&mut Process`。AGENTS.md §4.2 要求用户态 CPU 状态只能经 `privileged::usermode`/`userctx` 修改，而 functions 层对 `userctx` 的引用仅 4 处。F2 黑名单（`scripts/audit_functions_boundary.py:45-89`）封了 `sync::raw`/`frame::raw`/`iomem::raw` 等九个 `::raw`，**却没有任何 `privileged::proc::*` 条目**，故这条穿透不会被任何腿报出。

**中断行注册与 DMA 释放窗口**属 S1 前提型风险。`privileged/irqline.rs:85-91` 的 safe 方法 `on_interrupt` 内部调用以「启动期单线程」为 SAFETY 前提的 `register_isr`（:149-155 写 `RacyCell`），该前提无任何编译期或运行期强制，运行期注册即数据竞争；`InterruptHandler` 是裸函数指针无法捕获状态，驱动只能靠全局槽位绕（`driver/input/keyboard.rs:779-973` 即此 workaround），且缺少注销 API。DMA 侧 `free_coherent` 可被独立调用而设备侧无停用同步，释放后仍有 DMA 写入窗口；`DmaStream` 全仓仅一处生产调用且无 Drop 屏障。

## 四、安全不变式：宣称与防线的差距

**I6 没有防线**，这是与文档差距最大的一条。`privileged/arch/x86_64/acpi.rs:887` 的 `get_dmar_drhd_list()` 全仓零消费者；`iommu` 关键字仅出现在两个 ACPI 解析文件中。即 DMAR/DRHD 表被读进内存，寄存器从未编程，实际防线是恒等直映射加纯地址约定（`dma/engine.rs:100-146` 的 `alloc_coherent` 返回裸 `(Virt, Phys)` 对）。更糟的是**唯一的 I6 门禁腿是空转的**：`audit_invariants.py:111-121` 的 `check_i6()` 只扫 functions 目录里的 `dma_map`/`dma_unmap`/`dma_sync` 三个符号名，而实测全仓这三个名字**零命中**——真实 DMA 出口叫 `alloc_coherent`/`free_coherent`/`DmaStream` 且全部位于 privileged（`dma/api.rs`、`dma/engine.rs`、`driver/net/e1000.rs`、`driver/storage/mod.rs`），故该腿无论外设 DMA 怎么写内核内存都恒返回零违规。AGENTS.md 把 I6 列为「违反即重新设计」级别，但现状下恶意或故障外设可写任意物理页。**这不是加固题，是从 0 到 1 的题**。

**I5 的执法面窄于宣称**。`scripts/audit_invariants.py:98-108` 的 `check_i5()` 只扫 functions 层的 `inb/outb/inl/outl/inw/outw` 调用文本，privileged 内部的绕过全部逃逸门禁。实测残余清单：`privileged/driver/display/mod.rs:449-468` 裸 `asm!(out/in)` PIO、`:200-204` VBE DISPI 裸 `read_volatile`；`privileged/pci/mod.rs:246-381` ECAM 裸解引用与 CFAM 0xCF8 直写；`pci/msi.rs:369-393` MSI-X Table 裸 `*mut`；`privileged/pci/infra.rs:38-136` 导出裸 `outb/inb` 并被 `driver/mod.rs:98` re-export。安全代理本身在核心驱动上是真贯彻的（e1000、serial、pl011、framebuffer 均经 IoMem/IoPort），未完成的集中在 display 与 pci 两块。

**TCB 门面比宣称的大**。`privileged/mod.rs:138-226` 实际约 45 个 `pub mod`，而非「8 类安全 API 门面」；`privileged/mm/api.rs:611-616` 的 `vma_get_current_mm() -> Option<&'static MmStruct>` 连同 Vma/VmaType 的 re-export，把内核 VMA 引用直接交给 functions（`dispatch.rs:644` → `mremap.rs:31`），与 I2 的类型级封装立场相悖。

**`// SAFETY:` 注释质量参半**。F4 只保证注释存在，不保证论证成立。真论证的例子如 `mm/slab.rs:776`、`iomem.rs:191`；最可疑的三类是：`sched/scheduler.rs:1436` 以「单核 + cli」为前提而 smp 侧确有 online CPU 模型；`net/sm_fi.rs`/`net/dns.rs` 反复 `&mut *socket_set()`（其安全性全压在 NET_LOCK 纪律上，无类型强制）；`userptr.rs:57-91` 的 `new()` 契约声称「全可读」却无运行时校验，`as_slice` 直接构造引用。`audit_static_mut.py` 只扫 privileged 且以 60 个豁免名单覆盖全部约 52 处命中——新增一个只需往名单加一行，「冻结清单」在实践上是可自我扩权的；`functions/driver/usb/mod.rs:155` 的 `Box::leak` 取得 `&'static mut` 完全落在扫描范围之外。

**SGEG 是 enforcement 而非摆设，但 secure boot 不是**。`seccomp_check` 确实位于分发路径硬执行（`privileged/syscall/dispatch.rs:230` + `privileged/proc/seccomp.rs:296-325`，已接线）；capability 检查是散点式的（`proc/namespace.rs:64`、`proc/sysinfo.rs:237`、`fs/access.rs` 等），漏写某处不会被任何腿发现。启动路径的 `boot_check_syscall`（`sysinfo.rs:225-230`）只查身份存在性，**镜像验签失败不硬阻断**，与「安全启动」的表述不符。

## 五、门禁体系可信度实测

这一节决定前四节读数的解释方式，也直接对应台账 P-60~P-69。

**真正在执法的防线**：编译期 `#![deny(unsafe_code)]`（functions 层实测 0 个真实 unsafe 块）；F9 死代码抑制硬零（`allow(dead_code)` 实测 0 处，`audit_dead_code_comment.py` 有属性锚定与跨行配平并自带 12 例自测）；F8 存在性层的 `missing_docs_debt`（真编译取 rustc 读数，三成员独立 target 防 Fresh 假绿，基线棘轮机器派生且实测收缩与补写行数精确对应——该腿确实在执法，但执法方式在 P-71 后已退化为无条件 fail-closed，见 §六）；rustfmt 四路 manifest 全覆盖；F3 耦合双向棘轮；QEMU 腿 `FAIL_OK=0` 真阻断且 `verify_image_arch.py` 的 ELF 头 + 逐字节嵌入判据可靠。

**宣称存在但实际空转或可绕的防线**，逐条附影响：

`skip_leg()` 不改退出码（`ci/audit.sh:141-146` 只累加计数器），故未执行的腿不阻断「全绿」。Lockbud 腿在本机恒走 skip（`:752-755` 两条判据：工具不在 PATH，或内建 rustc 与仓库工具链不匹配）——**结果是 F4 的并发面至今零次机械验证**，而 `audit_deadlock_matrix.py:14-17` 自述 AB-BA 锁序检测未实现，锁序全靠注释与人工。

clippy 腿的 manifest 面是 `src/kernel/Cargo.toml --workspace`（`ci/audit.sh:553`、`:581`），即只 lint kernel / kernel-privileged / kernel-functions；host-tests、src/rust、src/user **从不被 clippy**，而 AGENTS.md §2.3 第 2 条写的是「四 crate」。同理 F5 宣称 `./ci/build.sh all`，但 `audit.sh` 步骤 1 用的是 dev 配方，release 配方的 0 warning 没有本地必跑点。

`audit_functions_boundary.py` 是纯文本行正则（:164-174），把 `unsafe` 折行或把 `use` 跨行书写即可绕；黑名单为手抄枚举，新出现的内部模块默认不可见（§三 的 `privileged::proc::raw` 即活例）。`audit_invariants.py` 的六条判据全部只扫 functions 目录（:23 单目录常量），且 I6 腿的三个模式名在全仓零命中（见 §四），即该腿是结构上空转。`audit_safety_coverage.py` 的模块集从 `privileged/mod.rs` 的 `pub mod` 正则发现（:38），私有 `mod` 与 cfg 门控模块成盲区。`audit_coupling.py`/`audit_invariants.py` 取单行 `use` 成边（P-65 自认扩面即撞红墙）。`audit_unwired_pub_fn.py` 的 R1「已分类」降噪数据源是一份 plan 文档——往文档加一行即把违规降级为 INFO，属自参照基线换壳。

测试侧最集中的失真是文本断言：host-tests 的 144 个 `.rs` 文件中 91 个含至少一处 `read_to_string`/`include_str!` 读源码再匹配的断言（共 146 处），包括四份 mm 帧所有权契约锁与多数 net host-tests。这类测试对「源码里出现了某个字符串」负责，对行为不负责——改名即假红，改行为不改注释即假绿。QEMU e2e 的判据是 grep 日志串，而里程碑串只存在于 `src/user/init/src/main.rs`，两侧无结构锁耦合，历史上已真实发生一次因日志串改名导致的 7/7 误报；默认 `E2E_REPEATS=1`（`scripts/qemu_boot_test.sh:43`）使约四十分之一的抖动在日常不可见。

一处读数文案缺陷：`audit_repr_c` 以 `✗ repr(C) 缺失: 0` 打印零计数（本次基线日志同现），紧随其后才是 `✓ audit_repr_c 通过`——人工读日志时易把零计数的 `✗` 前缀误判为失败。

## 六、本轮验收门槛实测基线

在基线 HEAD（含并发在制改动）上实跑四阶段，结果如下（日志 `other/gate-run-01/gates.log`，该目录 gitignore，非仓库产物）：

| 阶段 | 命令 | 退出码 | 事实 |
| --- | --- | --- | --- |
| 1 | `./ci/build.sh all` | 0 | 双架构编译与链接通过，6 份用户态产物生成 |
| 2 | `bash ci/audit.sh full` | 1 | 失败于 `audit_missing_docs_debt` |
| 3 | `make test-host` | 2 | 编译期失败，未进入测试 |
| 4 | `make test-kernel-host` | 2 | 同上 |

三个非零码同源：`privileged/driver/storage/nvme.rs:35` 已写 `#![deny(missing_docs)]`，而 `:85-89`、`:267-271` 等结构体字段文档尚未补完——这是并行 F8 补写的在制中间态，**不构成 HEAD 属性，需在干净提交上复跑确认**。

但腿 2 的失败方式暴露了一个结构性缺陷，值得单独登记：该腿以 fail-closed 报「读数归属歧义：同一 `(源文件, lint 种类)` 同时来自多个 workspace 成员」。根因是 P-71 把 privileged/functions 拆成独立 crate 后，`src/kernel/privileged/**` 的同一物理文件同时属于 `kernel` 与 `kernel-privileged` 两个 workspace 成员，而债册的对账模型假定「文件→成员」单射。这意味着**只要 privileged 下任一文件翻成 `deny(missing_docs)` 且存在缺文档项，F8 存在性腿必然红**，且同类歧义也可能让读数在两个成员间重复计账。这是 F8 棘轮的地基问题，不是在制改动的副作用。

## 七、确实扎实的部分（平衡读数）

避免把本报告读成一边倒，以下均经实测复核：functions 层 0 unsafe 是真的（16,742 行的 `functions/driver/` 全线 `#![deny(unsafe_code)]` 且无真实 unsafe 块）；用户内存访问单点代理含 SMAP 与异常表修复，抽查 sockaddr/stat/iovec/ioctl/poll 五类结构参数全部经代理，无 I4 违反，且新实装臂普遍带 `IOV_MAX`、对齐与溢出防护；跨核 TLB 失效的 epoch 协议带运行期判别力探针，属同类自研内核中少见的自证设计；`exit`/`reap` 的事件序（自引用锚 + `reap_off_cpu` 防最后切换 UAF + 孤儿投归 pid1）论证与实现都在；三级回退调度与 rq 锁序纪律扎实，历史 SMP livelock 根因已除；framebuffer 绘图策略经 trait 注入下沉是一个质量很高的下沉范式样本（未注册即 fail-quiet，不 panic）；`SIMPLIFIED` 标记 123 处均登记简化点、影响面与扩展条件，且 TODO/FIXME/HACK/`unimplemented!`/`todo!` 实测为 0；自定义 syscall 编号与 Linux 编号经校验零碰撞。

## 八、作为 0.1 输入的判读

把上述欠缺按「是否阻断对外兑现」排序，得到四条必然落在关键路径上的判断。可交付性断层是首要瓶颈：幻影挂载、UNKFS 目录项持久化、五类假成功臂三件事不修，外部用户态的失败会以「看似成功实则错误」的形式扩散。aarch64 缺页处理器缺失决定「双架构可用」这句话能不能说出口。smoltcp panic 面与 I6 无防线共同决定 framekernel 的两个核心卖点（无 UB、外设不可写内核内存）各自是否名副其实。门禁的 skip 不改退出码与文本断言测试，决定后续所有「全绿」读数的解释力——因此它虽然最不像功能缺陷，却必须与前三者同期收口，否则 0.1 的验收本身不可信。

对应的版本判据、里程碑拆解与明确不做的范围，见 [内核 0.1 版本目标](./kernel-0.1-milestone.md)。
