# TLB shootdown 代计数（epoch）与机会式排空工程

> **定位**：在已落地的 IPI 骨架（`0xFD`/`0xFE` 的 stub、IDT 门、`handle_irq` 前置分支、定向投递、aarch64 SGI）之上，**替换**其中的"手写 ack 同步等待协议"为 **全局代计数 + 每核已追平代**（epoch），并把帧释放从"等 ack 计数归零"改为"等到全部在线核的已追平代 ≥ 该帧的释放代"。
>
> **目标**：2/3/4 核配置下跨核 TLB 失效语义正确；**帧在全部在线核失效对应映射之前绝不归还 PMM**；任何等待发生在锁外且中断开启，不产生结构性死锁。
>
> **来源**：`smp-ipi-protocol.md`（A 档）的 S-5/S-6/S-7 走"临界区外等 ack"路线；用户裁定改走本文件的代计数路线（B 档）。本文件对 A 档为**取代**关系，非增量。
>
> **关联**：D-9-3 / D-9-5 / D-9-7；`docs/explain/explain-framekernel.md`（I2/I4/I5 不变式）。

## 1. 决策记录

| 决策点 | 选择 | 说明 |
|---|---|---|
| D1 同步协议 | **全局代计数 + 每核已追平代** | 替代 `SHOOTDOWN_IN_FLIGHT`/`TLB_ACK_PENDING` 计数协议；代比较天然幂等，重复/迟到 IPI 无害 |
| D2 读代时机 | **flush 之前读代，flush 之后声明该值** | 反序会"假追平"（推导见 §2.3 P1），是本次规划修正的核心正确性规则 |
| D3 发送侧本核 | **定向 IPI 集合含本核** | 使发送侧与接收侧完全同构，避免"发送侧自行声明本批代"所需的额外全量 flush 论证（§2.3 P2） |
| D4 未追平帧存放 | **全局 pending 表（代随帧记录）** | 消除 A 档"缓冲溢出即立即释放"的正确性漏洞（§2.2 L1） |
| D5 排空点 | **`release_lock` 出口唯一** | 额外排空点（tick / 返回用户态前）本轮不引入，登记为扩展项（§6） |
| D6 编译门 | **`#[cfg(feature = "smp")]` → 运行时门控** | 全仓无任何构建启用 `smp`（§2.2 L2），不改为运行时门控则本工程**无运行验证载体** |
| D7 aarch64 | **接收侧同语义；发送侧不建平行实现** | ~~aarch64 无 AP 上线路径（`SMP_ENABLED` 恒 false），发送侧改动不可运行验证~~ **（前提已订正）** DECISION-082 使 aarch64 次核上线；DECISION-083 抽公共层 `mm/deferred_free.rs` 并接入发送侧，已在 QEMU `-smp 2` 运行验证（不建平行实现的约束仍有效） |
| D8 `destroy_page_table` | **保持现状不动；缺口移交 D 立项** | 该路径的**框架**（帧数上界十万级 vs 定长缓冲）与"mm 生命周期契约"两项都已由 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) 承接（§3 S-6）。原"方案 A（保持现状即安全）"的前提（destroy 时机正确）经核实不成立 ⇒ 本工程不对该路径作任何处置 |
| D9 与 D 档的次序 | **B 档（本文件）先行**，随后 D2+、再 D3 | ① 两者**正交**（见 §3 S-4 详情：`ref_count == 0` 与"TLB 已追平"须 AND），B 档不因 D2+ 返工；② HEAD 不含 `defer_free`（L1/L2/L3 由 A 档未提交改动引入）⇒ 先行即就地收口活跃缺陷；③ 约束不变：同在 framework 子树，**不可并行**。依据见 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) §7 |
| D10 deferred 帧容器 | **侵入式帧链表（无容量上限）** | 取代原"栈上定长缓冲 + 溢出即释放"：定长容器在"锁内不能等"的约束下**不存在任何正确的溢出动作**（批次代只能在 `release_lock` 处发布，§2.3 P1；锁内等待会与 IF=0 自旋等锁的对端互等死锁，§2.4）⇒ 溢出帧除"记住"别无出路，**容器容量本身就是正确性问题**。帧自身即链表节点（经 `physmap` 写 `(gen, next)`），静态开销 O(1)，一次消除 §2.2 L1。依据：§7.4 A7-3①（外部对照亦要求"容器无上限"）+ §3 S-4 修订 + §6 路线次序 |

## 2. 调研结论（源码依据，行号均已核实）

### 2.1 当前工作区状态（A 档已落地但**未提交、未验收**）

- 工作区改动 7 文件 +309/−18：`Makefile`、`arch/aarch64/exception.rs`、`boot/isr.asm`、`idt/idt.rs`、`idt/mod.rs`、`mm/vmm_x86_64.rs`、`smp/mod.rs`；`docs/plan/smp-ipi-protocol.md` 未跟踪。
- 已落地骨架（B 档沿用）：`isr.asm` 的 `irq_stub 253/254`；`idt.rs:382-401` 的 `init_ipi_idt`；`idt.rs:745-764` 的 `handle_irq` 前置 `0xFD`/`0xFE` 分支；`smp/mod.rs` 的定向投递；`exception.rs:629-690` 的 SGI 13/14 分支；`Makefile` 的 `test-smp`（变量 `SMP_CORES ?= 2`）与 `test-smp-multicore`（2/3/4）。
- 将**删除**的协议部分：`smp/mod.rs:17-22`（`SHOOTDOWN_IN_FLIGHT`、`TLB_ACK_PENDING`）、`smp/mod.rs:105-178`（`tlb_shootdown_sync`、`tlb_shootdown_ack`）。
- 前次 4 个后台门槛任务均为 `exit -1` 且日志为空（wrapper 层失败），**无有效门槛数据**，本轮由主 agent 串行复跑。
- 本节与 §2.2/§2.4 引用的 `vmm_x86_64.rs` 行号为**波 2 施工前**采集；施工后该文件因新增帧链节点与排空函数整体下移约 180 行（其余文件行号未变），符号名与语义不受影响。

### 2.2 已核实的正确性漏洞（三项）

- **L1「溢出即释放」是常态而非罕见**：`defer_free` 溢出路径在 [vmm_x86_64.rs:2112-2113](../../src/kernel/framework/mm/vmm_x86_64.rs#L2112-L2113) 直接 `free_page`，上限 `DEFERRED_FREE_CAP = 64`（[L67](../../src/kernel/framework/mm/vmm_x86_64.rs#L67)）。而 [destroy_page_table](../../src/kernel/framework/mm/vmm_x86_64.rs#L1169-L1259) 在**单临界区内**对整棵页表树与全部独占用户页逐个 `defer_free`（[L1231](../../src/kernel/framework/mm/vmm_x86_64.rs#L1231)、[L1236](../../src/kernel/framework/mm/vmm_x86_64.rs#L1236)、[L1240](../../src/kernel/framework/mm/vmm_x86_64.rs#L1240)、[L1244](../../src/kernel/framework/mm/vmm_x86_64.rs#L1244)、[L1248](../../src/kernel/framework/mm/vmm_x86_64.rs#L1248)），帧数上界为十万级 ⇒ 溢出路径必然触发，该退路**违反"追平先于释放"**。
- **L2 协议代码从未被编译**：`smp = []` 已声明（[Cargo.toml:70](../../src/kernel/Cargo.toml#L70)），但 `default = []`（[L61](../../src/kernel/Cargo.toml#L61)）且全仓无构建传 `--features smp`。于是 `flush_tlb` 的置位（[L2079-2088](../../src/kernel/framework/mm/vmm_x86_64.rs#L2079-L2088)）与 `release_lock` 的 shootdown 调用（[L2016-2019](../../src/kernel/framework/mm/vmm_x86_64.rs#L2016-L2019)）**均不参与编译**；2 核 QEMU 下不发 IPI，远端 TLB 永不失效，而 `defer_free`（无 cfg 门）仍在延迟释放 ⇒ 比 HEAD 更危险。
- **L3（已证伪，原论断作废）**：原文称 `destroy_page_table` 区内不置位标志——**失实**。该函数在临界区末尾显式置位（[vmm_x86_64.rs:1266-1272](../../src/kernel/framework/mm/vmm_x86_64.rs#L1266-L1272)；原文引用的行区间 L1169-1259 恰好截在置位之前，属**引用截断导致的失实**）⇒ 启用 `smp` 时 destroy 路径是"延迟**且**有 IPI"的。该路径的真实缺口是**容器容量**（L1）与 **mm 生命周期契约**（L4），二者均已在 §1 D8 / §3 S-6 移交 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md)。
- **L4 mm 生命周期契约不对等**：[vmspace.rs:190-199](../../src/kernel/framework/vmspace.rs#L190-L199) 的 `destroy` 明确以 `unsafe fn` 契约要求"调用前确保无 CPU 运行在此地址空间上"；而 [process.rs:617-635](../../src/kernel/framework/proc/process.rs#L617-L635) 的 `Drop for Process` 调 `vmm_destroy_page_table(cr3)`（D 立项后已加 `frame_dec` 计数归零闸门，但**仍无"无核驻留"该契约**——计数只保证无其他**持有者**，不保证无核**正在运行**于此地址空间），**无该契约**。⇒ `destroy_page_table` 帧释放的充分条件是"该 mm 已不驻留任何核"，而非"TLB 已失效"，A 档以 TLB 语义处理它属语义错配。

### 2.3 两条关键正确性推导

**P1（读代必须在 flush 之前）**：设接收核 X 于 `t0` 读代得 `g0`、于 `t1 ≥ t0` 完成 flush、随后声明 `CPU_TLB_GEN[X] = g0`。要证"所有代 ≤ `g0` 的页表修改在 `t1` 前已发生"：代 `g` 的发布经 `fetch_add`（AcqRel），若 `g ≤ g0` 且 `g0` 取自 `t0`，则 `g` 的 `fetch_add` 不晚于 `t0`；该批修改又先于其 `fetch_add`（同临界区、早于 `release_lock`）⇒ 修改早于 `t0 ≤ t1` ⇒ 被全量 flush 覆盖。**反序（flush 后读代）不成立**：若读到 `g0' > g0`，只能推出 `fetch_add(g0')` 早于**读**时刻，推不出早于**flush**时刻，中间发布的批次的修改可能未被本次 flush 覆盖却被声明为已追平。⇒ 接收侧与发送侧本核一律"先读代、再 flush、再声明"。

**P2（发送侧本核不能自行声明"本批代"）**：发送核 A 在临界区内只对**本批**地址做了逐页 `invlpg`。若 A 在临界区前有挂起未处理的 `0xFD`（对应更早代 `G'`），则 `G'` 的地址未被 A 的逐页失效覆盖，而 A 声明"本批代 `> G'`"即为假追平。⇒ 出路有二：(a) A 额外做一次**全量** flush 后再声明；(b) **把 IPI 也发给自己**，令 A 走与接收核完全相同的"读代 → 全量 flush → 声明"路径。选 (b)（D3）：路径统一，无需为发送侧单列论证，且 `send_ipi` 已支持任意 APIC ID（[apic.rs:153-160](../../src/kernel/framework/arch/x86_64/apic.rs#L153-L160)）。

### 2.4 复核补充

- `acquire_lock` 先 `disable_interrupts` 再自旋（[vmm_x86_64.rs:1957-1974](../../src/kernel/framework/mm/vmm_x86_64.rs#L1957-L1974)）；`release_lock` 现结构为"持锁搬出 → 释放锁 → 恢复中断 → 等 ack → free"（[L1989-2033](../../src/kernel/framework/mm/vmm_x86_64.rs#L1989-L2033)）。B 档保留"锁外做远程/回收"这一结构，仅把"等 ack"换成"发布代 + 机会式排空"。
- `defer_free` 调用点共 8 处：unmap 路径 3 处（[L1139](../../src/kernel/framework/mm/vmm_x86_64.rs#L1139)、[L1144](../../src/kernel/framework/mm/vmm_x86_64.rs#L1144)、[L1149](../../src/kernel/framework/mm/vmm_x86_64.rs#L1149)，所在临界区有 `flush_tlb`），destroy 路径 5 处（见 L1）。
- `MAX_CPUS = 1024`（[capacity.rs:20](../../src/kernel/framework/config/capacity.rs#L20)）⇒ `[AtomicU64; 1024]` 为 8 KiB，可接受。
- aarch64 `release_lock`（[vmm_aarch64.rs:267-272](../../src/kernel/framework/mm/vmm_aarch64.rs#L267-L272)）**无**任何 shootdown/deferred 逻辑，与 x86_64 现状不对称（D7 说明其本轮不改）。
- A 档 `smp-ipi-protocol.md` §5.1 称"`smp`/`preempt`/`kaslr`/`freg` feature 未在 `Cargo.toml` 声明"**失实**：`smp`(L70)/`preempt`(L91)/`freg`(L93) 均已声明。须回写（§3 S-7）。

## 3. 施工条目

- **S-1. 代计数基础设施（架构无关）**
  - 描述：`smp/mod.rs` 现以两个原子计数承载 ack 协议，与代计数不可共存。
  - 方案：删除 `SHOOTDOWN_IN_FLIGHT`、`TLB_ACK_PENDING`（[smp/mod.rs:17-22](../../src/kernel/framework/smp/mod.rs#L17-L22)）与 `tlb_shootdown_sync`、`tlb_shootdown_ack`（[L105-178](../../src/kernel/framework/smp/mod.rs#L105-L178)）。新增 `TLB_GEN: AtomicU64`、`CPU_TLB_GEN: [AtomicU64; MAX_CPUS]` 及访问函数：`tlb_gen_now()`、`tlb_gen_publish()`、`tlb_gen_set_self(g)`、`tlb_gen_min_online()`；`tlb_gen_min_online` 只遍历 `CPU_ONLINE[i] == true` 的槽位（离线槽位不得参与 `min`，否则最小值永久卡在旧值）。
  - 状态：[X]
  - 详情：`register_cpu` 成功后须把新核 `CPU_TLB_GEN` 置为"当时的 `TLB_GEN`"——新核上线前不驻留任何用户地址空间，置为当前代安全；不置则默认 0 会把 `min` 拉回 0。已落地形态：`smp/mod.rs` 删除 `SHOOTDOWN_IN_FLIGHT`/`TLB_ACK_PENDING`/`tlb_shootdown_sync`/`tlb_shootdown_ack`，新增 `TLB_GEN`、`CPU_TLB_GEN` 与 `tlb_gen_now`/`tlb_gen_publish`/`tlb_gen_set_self`/`tlb_gen_min_online`/`tlb_gen_publish_and_shoot`；`register_cpu` 在置 ONLINE **之前**预置新核代。

- **S-2. 接收侧改为「读代 → flush → 声明」**
  - 描述：`0xFD` 处理现为 `tlb_flush_all()` + `tlb_shootdown_ack()`（[idt.rs:751-756](../../src/kernel/framework/idt/idt.rs#L751-L756)），aarch64 SGI 13 同形（[exception.rs:676-683](../../src/kernel/framework/arch/aarch64/exception.rs#L676-L683)）。
  - 方案：两处改为 `let g = smp::tlb_gen_now();` → `mm::arch::tlb_flush_all();` → `smp::tlb_gen_set_self(g);` 三步；`send_eoi` 次序与其余逻辑不动。
  - 状态：[X]
  - 详情：**顺序不可颠倒**（§2.3 P1）。此处是全工程唯一两处"声明已追平"的接收点。已落地：[idt.rs:745-764](../../src/kernel/framework/idt/idt.rs#L745-L764) 的 `0xFD` 前置分支与 [exception.rs:629-690](../../src/kernel/framework/arch/aarch64/exception.rs#L629-L690) 的 SGI 13 分支均为「读代 → `tlb_flush_all` → 声明」三步。

- **S-3. 发送侧：代发布 + 定向 IPI（含本核、不等待）**
  - 描述：需要一个"发布新代并通知全部在线核"的发送原语，且不阻塞。
  - 方案：`smp/mod.rs` 新增 `tlb_gen_publish_and_shoot() -> u64`：`let g = TLB_GEN.fetch_add(1, AcqRel) + 1;` → 遍历 `0..get_cpu_count()`，对所有 `CPU_ONLINE[i]` 的 `get_apic_id(i)` 发 `0xFD`，**含本核**（D3）；返回 `g`。发布必须先于发 IPI（对端收到即会读代，反序会读到旧代）。
  - 状态：[X]
  - 详情：不再需要"在途串行化"（原 `SHOOTDOWN_IN_FLIGHT` 的职责）——代计数下并发发布只是得到不同的代，各自的目标核都会 flush 并声明，无需互斥。

- **S-4. deferred 帧携带释放代 + pending 表**
  - 描述：帧的"可释放"判据从"ack 计数归零"变为"`tlb_gen_min_online() >= 帧的释放代`"，需要一个跨临界区、可容纳未追平帧的记录区。
  - 方案（容器形态已裁定，见 §1 D10）：容器改为**侵入式帧链表，无容量上限**。帧自身即链表节点，经 `PhysAddr(p).to_virt()`（常量偏移直映射，`mm/mod.rs:262`）写入 `next`（偏移 0，u64 物理地址，0 表尾）与 `gen`（偏移 8）。静态量三处：`BATCH_HEAD`（本临界区批次头，**仅持 `VMM_LOCK` 期间**读写）、`PENDING_HEAD`（已发布代但尚未追平的帧，由 `IrqSpinLock<u64>` 守护——持锁即关中断，仅锁外排空路径使用）、`PENDING_NONEMPTY`（与 `PENDING_HEAD` **同锁更新**的廉价门控，免去常态空链表也取一次锁）；删除 `DEFERRED_FREE_FRAMES/LEN/CAP/OVERFLOW_WARNED`。`defer_free` 只做"头插 `BATCH_HEAD`、`gen` 写占位 0（出锁前不会被读）"。`release_lock` 持锁时 `BATCH_HEAD.swap(0)` 摘走整条批次 → 释放锁 / 恢复中断 → 锁外 `let g = if shootdown_needed && smp::is_enabled() && smp::get_cpu_count() > 1 { smp::tlb_gen_publish_and_shoot() } else { smp::tlb_gen_now() };` → 取 `min = smp::tlb_gen_min_online()`：批次内 `min >= g` 者 `free_page`，其余回填 `gen = g` 后挂入 `PENDING_HEAD`（持锁搬运）；`PENDING_HEAD` 上 `min >= gen` 者**先摘链、后释放**（禁止持锁时调 `free_page`，避免与 PMM 锁嵌套）。
  - 状态：[X]
  - 详情：① 取 `PENDING_HEAD` 锁为**阻塞获取，不得 try_lock 后放弃**——批次链已由 `release_lock` 从 `BATCH_HEAD` 摘入本调用栈上的局部量，放弃插入即无人再引用该链，等于丢帧（泄漏）。该等待在锁外且中断开启，持锁者只做链表搬运、不会反向等本核，无死锁。② 单核 / 未启用 SMP 时 `tlb_gen_now()` 恒 0、`min` 恒 0 ⇒ 立即释放，与 HEAD 行为等价（该情形下 `flush_tlb` 本就不置位）。③ pending 链**不保证按代有序**（多核各自头插），排空只按 `min` 逐节点判定，不依赖次序。④ 容器无上限的必然性见 §1 D10。⑤ **本表不因 D 档（帧持有计数）落地而消失**：`ref_count == 0`（无持有者）与"TLB 已追平"（无核仍缓存旧映射）是**两个独立条件，必须 AND**——未被共享的页（`ref_count` 1→0）在远端 TLB 仍缓存其映射时归还 PMM 即产生 UAF。故 D2+ 落地后本条目**代码形态不变**，仅"归还"的最终语义由 `free_page` 内部收紧（依据见 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) §7.1）。⑥ 路线次序见 §6。⑦ **`gen` 写入时左移 1 位**：被覆盖的 16 字节在页表帧里就是 PTE 0 / PTE 1，`next` 是页对齐物理地址（bit0..11 恒 0）、`gen << 1` 使 bit0 恒 0，二者对并发硬件页表遍历均呈"不存在" → 正常缺页，不会被误当作"存在"项翻译到其它物理页；残留窗口登记见 §6。⑧ 排空**单趟**（摘链 → 锁外分割 → 回挂），不循环重试；出口次序为**先排空历史 pending、再结算本批**（`min` 共用一次采样）——反序会让刚挂入的整条批次链被立即摘下又挂回（两次 O(n) 无效往返）。

- **S-5. `release_lock` 出口改为「发布 → 排空」**
  - 描述：[release_lock:1989-2033](../../src/kernel/framework/mm/vmm_x86_64.rs#L1989-L2033) 现为"等 ack → 无条件 free 全部搬出帧"。
  - 方案：保留"持锁读清标志 + 搬出记录 + 释放锁 + 恢复中断"骨架；把 `#[cfg(feature = "smp")]` 的 `tlb_shootdown_sync()` 替换为运行时门控下的 `smp::tlb_gen_publish_and_shoot()`；随后按 S-4 的代判据逐帧释放或放回。**等待分支**（若保留）必须在锁外且中断开启。
  - 状态：[X]
  - 详情：`TLB_SHOOTDOWN_NEEDED` 的两个置位点（`flush_tlb`、`destroy_page_table` 末尾）保留，其 `#[cfg(feature = "smp")]` 编译门已改为 `smp::is_enabled() && smp::get_cpu_count() > 1` 的运行时判定（D6，共两处）；`release_lock` 中的 `tlb_shootdown_sync()` 调用与 `#[cfg(not(feature = "smp"))] let _ = shootdown_needed;` 兜底已随协议删除一并清除，本路径不再是"不传 `--features smp` 就被编译掉"的死代码。施工后行号：`release_lock` = [L2146-2183](../../src/kernel/framework/mm/vmm_x86_64.rs#L2146-L2183)，`flush_tlb` = [L2222-2235](../../src/kernel/framework/mm/vmm_x86_64.rs#L2222-L2235)。

- **S-6. `destroy_page_table` 路径处置：保持现状不动**
  - 描述：该路径单临界区 defer 十万级帧（§2.2 L1），且其正确性依赖"mm 不驻留任何核"（§2.2 L4）。原设想的"撤销 defer、恢复立即 `free_page`"方案（§1 D8 旧文）**前提被证伪**——`Process::drop` 无条件整表销毁且无 mm-off 契约，[process.rs:617-635](../../src/kernel/framework/proc/process.rs#L617-L635)，destroy 时机本身不成立。（该"无条件销毁"前提**已由 D 立项改为计数闸门**，见本条目末条详情；本条描述保留为当时的判断依据。）
  - 方案：本工程**不改动** `destroy_page_table`（既不撤销 `defer_free`，也不放大 pending 容量）。该路径的帧容器容量与 mm 生命周期契约两项缺口，连同"cr3 无使用者计数"的根因，一并移交 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) 作为**独立立项**处理。
  - 状态：[X]
  - 详情：本工程的 deferred 集合因此仍含 destroy 路径的 5 处（§2.4）——S-4 的**无上限**容器正为此设计（未追平则放回，绝不提前释放），不得依赖"溢出不可达"。destroy 路径在 B 档内**不引入 flush_tlb**（L3 保持），其帧释放正确性由 D 立项的 mm 生命周期契约承担；该路径的"整表已拆链"表述不成立（父表项未清），残留窗口登记见 §6。
  - 详情：**移交结果（D 立项已落地）**：`docs/plan/cr3-lifetime-ownership.md` 的 D-1~D-5 已实施——帧持有计数面（PMM `frame_inc`/`frame_dec`）建立，"创建者持有 1 / 共享方 `frame_inc` / 销毁点 `frame_dec` 归零才销毁"全链闭环，`Process::drop` 的整表销毁改为**计数归零闸门**（该文件 D-3 X1）。§2.2 L4 的"mm 生命周期契约不对等"因此从"无条件销毁"变为"最后一个持有者销毁"，契约形式改为计数表达。**本工程的 deferred 语义、S-4 容器形态、D5 排空点均不受影响**（该文件 §7.1 已核实二者正交）。

- **S-7. 文档回写（A 档前提失实项）**
  - 描述：`smp-ipi-protocol.md` 有失实/被取代的表述，且台账 D-9-3/5/7 需指向本文件。
  - 方案：① §5.1 的"feature 未声明"失实项更正（§2.4）；② 标注其 S-3/S-6/S-7 中**依赖 ack 计数**的部分被本文件取代（S-5 的"定向投递"能力不废止，由本路线的 `tlb_gen_publish_and_shoot` 承接）；③ §2.3/§2.4 的"临界区内等 ack 必死锁"结论在本路线下不再适用（改为"代计数下不存在 ack 等待"），并补记 P1 的读代次序规则；④ 台账 D-9-3/5/7 指向本文件。
  - 状态：[X]
  - 详情：仅回写事实，不改动 A 档已落地骨架的记录。已落地形态：① `smp-ipi-protocol.md` §5.1 的 feature 失实项已更正（四 feature 实测**均已声明**于 `src/kernel/Cargo.toml` 的 `[features]` 段，原句「未声明」为误，现已在原处标注更正）；② 该文件 header 新增「文件状态」段，声明 S-3/S-6/S-7 中依赖 ack 计数的部分被本文件取代、S-5 定向投递能力不废止；③ 该文件 §2.3 与 §2.4 各追加「代计数路线下的更新」段（无 ack 等待 + P1 读代次序规则；判据改为代追平 + 容器改为无上限侵入式链表）；④ 台账 D-9-3 / D-9-5 / D-9-7 均已追加「归属转移」详情指向本文件（状态保持 `[]`，待 §4 门槛复跑后回填）。

- **S-8. 验收门槛（见 §4）与 fail-closed 复验（见 §5）**
  - 描述：本工程为同步原语/TCB 边界变更，门槛须由**非实施者**独立复跑并取脚本自身退出码。
  - 方案：主 agent 串行执行 §4 全部条目；§5 注入式反向验证必须使指定门槛 FAIL。
  - 状态：[X]
  - 详情：① **§4 门槛 1~7 复跑结果（S-11/S-12 修复后，主 agent 串行执行并取脚本自身退出码）**：`./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`，host tests 11 passed）；`./ci/audit.sh` RC=0（双架构 check + x86_64 clippy pedantic lib + kernel_test/host-test 特征维 + QEMU 双架构启动）；aarch64 clippy 显式复跑 RC=0；`make test-smp`（2 核）RC=0；`make test-smp-multicore`（2/3/4 核）RC=0；`./scripts/qemu_boot_test.sh x86_64` RC=0。② **§5 注入 1（前半）在 S-11 修复后复跑**：临时把 `handle_irq` 的 `0xFD` 前置分支条件改为不匹配值，`make test-smp` **RC=2 FAIL**（报"未找到 `[SMP] first user syscall from pid=N`"）⇒ 判别力成立；还原后 RC=0，`git diff src/kernel/framework/idt/idt.rs` 仅余本工程既有改动、无注入残留。③ **原"未闭合项"已全部收口**：注入 1 后半（令 `tlb_gen_set_self` 空转）由 S-10 的 `pending=false` 运行期断言覆盖（两轮实测判据字段为 `pending`）；注入 2（接收侧三步乱序）由 S-14 的静态 fail-closed 审计 `audit_tlb_receive_order.py` 覆盖（已进 `ci/audit.sh quick`）；注入 3（`flush_tlb_remote` 不置位）与注入 4（`tlb_flush_all` 变空）由 S-13 的运行期跨核探针覆盖（`Makefile` 第六项断言，均实测可检出）。§5 四类注入现均有确定性判别力。

- **S-9. map 路径过度 shootdown（门槛 7 实测发现，须修复）**
  - 描述：埋点实测（2/3/4 核，各 62 次）显示这些 shootdown **全部由 map 路径触发**——日志中 `map_page:` / `huge split:` 之后紧跟 shootdown 行；而 `defer_free` 的 8 个调用点（unmap 3 处 / `destroy_page_table` 5 处）在 boot 期**一次未走**，`BATCH_HEAD` 恒 0。即"新建映射"也在做全量远程失效，属过度失效。
  - 方案（已落地，用户裁定「B 相对完整全量分类」）：把失效语义**显式化到入口**，逐点选择，不用隐式判据。① 删除语义歧义的 `flush_tlb`，新增两个入口——`flush_tlb_local`（仅本核 `invlpg`，**不**置位 `TLB_SHOOTDOWN_NEEDED`）与 `flush_tlb_remote`（本核 `invlpg` + 置位，语义＝"替换/覆盖既有翻译或权限变更"）。② `get_or_create_table_entry` 改签名返回 `(*mut PageTableEntry, bool)`，`bool` 为"本次是否拆分巨页"，**13 个调用点**逐一解构；三处叶子写入点（`map_page_internal` / `map_page_in_table` / `map_kernel_page_in_table`）据 `split_pdpt || split_pd || split_pt || leaf_was_present` 选择入口（拆分与覆盖既有叶子均属替换）。③ 其余 17 处按分类显式改 `flush_tlb_remote` 并各带分类注释：拆除（`unmap_page` 3 / `unmap_page_in_table` 3）、重写 PTE（`set_pte_value` 1）、巨页拆分（`split_2mb_page` 1）、权限变更（`protect_page` 3 / `protect_page_in_table` 3 / `ensure_pml4_user` 1 / `ensure_path_user` 2）。④ `map_2mb_page` / `map_1gb_page` 经守卫推导（`present && huge → return Ok`、`present && !huge → Err`）恒为纯新建且不可能拆分 ⇒ 直接 `flush_tlb_local`，不留"恒 false 的运行时条件"。
  - 状态：[X]
  - 详情：① **调用点计数更正**：原文"19 处"实为 **22 处** `flush_tlb` 调用点（全树仅 `vmm_x86_64.rs` 一个文件持有该函数）。B 方案把其中 3 处（三个叶子写入点）改为二选一条件分支后，入口调用点变为 **25 处 = 20 `flush_tlb_remote` + 5 `flush_tlb_local`**；另有 `destroy_page_table` 内 1 处**直接置位**（不经 `flush_tlb` 入口，原本即如此），不在 22/25 计数内。② **判定准则（沿用原表述并落到判据）**：目标 VA 此前**不存在**翻译 ⇒ 远程核不可能缓存该 VA 的陈旧条目（x86 不缓存"不存在"的翻译）⇒ 仅本核失效；**替换/覆盖**（拆除、重映射、巨页拆分、swap 改写 PTE）与**权限变更**（mprotect / 设 USER 位）⇒ 远程核可能持有旧条目/旧权限位 ⇒ 必须远程失效。③ **不得无条件降级的反例（实测）**：`map_page_internal` 的调用点看似"新建"，但同一次调用可能在 `get_or_create_table_entry` 内**拆分内核 2MB 恒等页**（日志 `[VMM] map_page: virt=0x400000` 紧跟 `[VMM] huge split: entry=0xFFFF800000105010 …`），拆分后 512 个子项全部 present ⇒ 必须远程失效；故判据取"拆分标志 || 叶子原已存在"，而非"叶子存在位"单一隐式判据。④ **KPTI 共享结构的独立分类**：`map_kernel_page_in_table`（RSP0 等内核结构映射进进程页表）的中间级与内核页表**共享**（KPTI 只复制 PML4 顶层），故本次若拆分巨页即改动共享结构，同样归入"必须远程失效"。
  - 详情：**实测（门槛 6 的 2/3/4 核，主 agent 串行执行取脚本自身退出码）**：shootdown **45 → 22**（各核数一致；`gen` 仍连续 1→22 无跳变、`targets` 恒等于核数），降幅约 51%；`make test-smp`（2 核）与 `make test-smp-multicore`（2/3/4 核）RC=0，`deferred-free … pending=false` 断言仍成立。残余 22 次的构成（4 核日志行序核实）：**16 次**来自用户栈 16 页的 `ensure_path_user`（设 USER 位属权限变更；该函数不持锁，置位由**下一次** `release_lock` 消费，故 16 次合并为 15 行 + 1 行并入 `#17`；其"无锁写页表"本身是 §6 已登记项，本项不处置）、**2 次**为真实巨页拆分（`0x400000` 与 RSP0）、**1 次**为 KPTI 用户页表创建阶段、**3 次**为退出/拆除路径（语义必需）。**残余项的归因（本轮不修，属 §6 预存项）**：16 次中的绝大多数由「`vmm_map_user_page` 污染内核页表」这条预存缺陷放大——该函数每映射一个用户页都额外对**内核页表**低半区调 `vmm_ensure_path_user`，使每页各触发一次权限变更失效；按 §12.5「工程外预存问题只报不动」登记于 §6，待污染缺陷单独立项后该余量自然回落。⑤ **lint 配套**：三处叶子写入点因 `split_pd`/`split_pt` 触发 `clippy::similar_names`（pedantic + `-D warnings`），依本文件既有约定加 `#[expect(clippy::similar_names, reason = …)]`（非 `allow`，F9 不涉）。

- **S-10. 帧释放路径的运行覆盖缺口（门槛 7 实测发现，须修复）**
  - 描述：新增埋点 `DEFERRED_FREE_RELEASED`（`vmm_x86_64.rs`，计数粒度为帧）在 2/3/4 核日志中**均恒为 0**（3 份日志各 0 行）⇒ 现有测试面对「代追平后帧确实释放」这一 B 档核心语义**零覆盖**。这也是注入 1 后半（令 `tlb_gen_set_self` 空转）检不出的原因：帧永久滞留 pending 链，不崩、不报错。
  - 方案：甲-2a（已裁定）——改 [init/src/main.rs](../../src/user/init/src/main.rs)，令子进程 `proc_exit` 触发 `destroy_page_table`（其内 5 处 `defer_free`），并**补第二次 VMM 操作**以产生第二次 `release_lock` 排空 pending；门槛 6 的断言升级为「`released_total` 非零」。
  - 状态：[X]
  - 详情：单次 destroy **不够**——5 处 `defer_free` 同处一个临界区，出口 `release_lock` 结算时 IPI 刚发出、对端未及 flush，`min < g` 必然成立，该批帧必挂入 `PENDING_HEAD`；须等下一次任一核的 `release_lock` 出口 `drain_pending` 才真正归还 PMM（此即 D5「排空点唯一」的实测后果）。改动须复核门槛 4/5 不受影响。
  - 详情：**实测更新（S-11/S-12 修复后复跑 2/3/4 核）**：载体前提**已具备**——用户态已真正执行（`[SMP] first user syscall from pid=5`／`pid=7`），但 `tests/reports/smp_test_*.log` 中 `[VMM] deferred-free released_total=` **仍为 0 行** ⇒ 释放路径依旧零覆盖，本项**未闭合**，门槛 6 的断言**尚不可**升级为"`released_total` 非零"（升级即 FAIL）。下一步须先以埋点区分「fork 子进程未走到 `destroy_page_table`」与「已走到但代未追平、帧滞留 `PENDING_HEAD`」两种情形，再定断言与载体形态。
  - 详情：**埋点判别已完成（D1/D2 修复后复跑 2 核，`tests/reports/smp_test_20260920_142638.log`，完整日志 261 行非终端截断）**：结论为**情形 (a)「销毁路径从未被走到」**，且根因不在 fork、不在代计数，而在**退出路径的调用顺序**。① **载体已成立**——`X`、`Y` 均实际输出到串口（D1 修复前它们不可见，见 S-11 详情 ⑥ 的更正），`[PROC] exit: pid=6 code=0`（正常退出码，非修复前的 `code=1`）⇒ 两次 `fork` + 子进程 `proc_exit` 确实执行。② **销毁路径仍零调用**——`[VMM] destroy_page_table:` 与 `[VMM] deferred-free admitted_total=` **均 0 行** ⇒ `vmm_destroy_page_table` 从未被进入，与"帧已入链但滞留 pending"无关（`DEFERRED_FREE_ADMITTED` 为 0 即证明连入链都未发生）。③ **根因链（源码核实，即 §6 的 D4）**：`proc_ops.rs::process_exit` 先调 `USER_PROC_MANAGER.destroy_by_pid_no_kstack(pid)`、**后**调 `SCHEDULER.exit(code)`；而前者内部的守卫 `user_proc.rs::destroy` 要求 `proc_ref.is_exited()`，`is_exited()` 判据为 `Process::state ∈ {Zombie, Terminated}`（`UserProcRef::load_state` 委托到同一 `Process::state` 字段）；置 Zombie 却发生在**后面**的 `scheduler.rs::exit()` 内 ⇒ destroy 被调用时 state 仍为 Running ⇒ 守卫恒 false ⇒ 立即 return ⇒ 页表与帧永不释放；且无 `wait4`/reap 二次回收点。④ **对本项断言升级的直接后果**：门槛 6 的「`released_total` 非零」升级**必须等 D4 修复后**方可进行，否则必然 FAIL（本次未升级，维持原断言）。
  - 详情：**新增埋点归属（本轮，保留）**：`DEFERRED_FREE_ADMITTED`（`vmm_x86_64.rs`，`defer_free` 入链计数）与 `[VMM] destroy_page_table: cr3=… deferred_in_call=…` 出口日志——二者与既有 `DEFERRED_FREE_RELEASED` 构成三段分位（入链 / 调用 / 释放），是本项判别得以定位到 D4 的依据；`[IDT] user exception`（`idt.rs`，仅用户态异常现场）保留为异常归因入口。`[SYSCALL] trace` 探针在判别完成后已撤除。
  - 详情：**埋点保留/撤除复核（D 立项收尾时清点，判据＝"有实名消费者"）**：撤除项经全树 `grep` 复核**确已不在代码中**（`switch: cpu=` / `resume: cpu=` / `wait: spin` / `[DBG]` / `[SYSCALL] trace` / `TEMP-INJECT` 于 `src` 返回空；仅余既存特性 `FAULT_INJECTION_RATE`，非本工程探针）——它们是**按上下文切换计的**高频探针（`switch` / `resume` / `wait: spin`），单次运行成百上千行，判别结束即属残留。保留项均为**按进程生命周期/异常计**的低频事件日志（每进程退出 ≤1 行、每个 CPL3 异常 1 行），且各有实名消费者：`[SMP] first user syscall from pid=N`、`[SMP] TLB shootdown #N gen=N targets=M`、`[VMM] deferred-free admitted/released/pending=false` 三者被 `Makefile` 的 `test-smp` 正则断言（fail-closed）；`[VMM] destroy_page_table: cr3=…` 与 **[PROC] exit: pid=N code=N**（`scheduler.rs`，本项分位序列的"退出段"）共同构成本项闭合证据的"退出 → 销毁"时序，是本项已闭合结论唯一可复核的入口——删之则该证据不可复现；`[IDT] user exception` 承载 §6 **未闭合**缺陷 D3（用户栈写 `#PF`）的唯一归因入口。故六项**全部保留**，仅高频临时探针撤除。
  - 详情：**埋点判别第二轮（本轮，2 核，`tests/reports/smp_test_20260920_160508.log`，258 行完整日志）——上条提出的两种待选情形（fork 子进程未走到 / 代未追平）均被证伪，真因是第三类「内核栈被覆盖」**。分位序列（新增 `switch` / `resume` / `wait: spin` 三组埋点，判别完成后已撤除）：`wait: self=5 child=6 state=Ready` → `wait: spin=1/2` → `switch: cpu=0 5 -> 6` → `exit: cpu=0 pid=6 code=0`（**子进程正常执行并正常退出 ⇒ 非 fork 链缺陷**）→ `exit: cpu=0 wake parent pid=5` → `switch: cpu=0 6 -> 5` → `resume: cpu=0 was=0 next=1597180`（**恢复点局部量已损坏，与 `was=5 next=6` 的预期值不符**），此后**整机静默**，`wait: spin=3` 永不出现。
  - 详情：**该缺陷的根因链（源码 + 机制核实）**：syscall 入口 [isr.asm](../../src/kernel/framework/boot/isr.asm) 无条件 `mov rsp, [gs:KERNEL_RSP_OFF]`，而该字段指向**每 CPU 一份**的 64 KiB `syscall_stack`（[gdt.rs:347](../../src/kernel/framework/arch/x86_64/gdt.rs#L347)），`kernel_rsp` 仅在 [gdt.rs:542](../../src/kernel/framework/arch/x86_64/gdt.rs#L542)（BSP）与 [gdt.rs:676](../../src/kernel/framework/arch/x86_64/gdt.rs#L676)（AP）各写一次、**运行期从不按任务更新**。于是：pid 5 在 `wait_reap` 忙等中 `scheduler_yield()` 让出（此刻其内核栈帧位于该 per-CPU 栈顶部）→ pid 6 被调度到**同一核**并执行 `proc_exit` syscall → 入口把 RSP 重置到栈顶 ⇒ **覆盖 pid 5 的栈帧** ⇒ 切回 pid 5 时其 `schedule()` 栈帧已损坏。故 S-10 载体失败的准确表述是"**父进程确被恢复，但恢复点的内核栈帧已被覆盖**"，而非"父进程不再被调度"。
  - 详情：**对本项的直接后果与次序裁定**：`wait_reap` 永不抵达 `reap_zombie` ⇒ `remove_and_free` 不执行 ⇒ `Process::drop` 不运行 ⇒ `destroy_page_table` 0 调用（与 `admitted_total=0` 一致）⇒ **"销毁路径零覆盖"的最上游原因不在 destroy / 代计数 / fork，而在上下文切换原语本身**（§6 D5）。次序裁定（用户，本轮）：**D5 →（G1/G2/G3 与 P1/P2 同批）→ D2+/D3**。理由：① D5 与 TLB/epoch 正交，且是全部退出/收割/销毁路径的载具，不修则任何相关载体都造不出来；② G 与 P1/P2 必须同批（[cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) §2.5：单修 P1 会使 G3「在用地址空间被销毁」的潜在 UAF 变为现实）；③ D2+/D3 依赖前两步才具备可验证载体。门槛 6 的断言升级须等 D5 修复后（届时 `[VMM] destroy_page_table:` 与 `deferred-free admitted_total=` 才可能出现非零行）。
  - 详情：**本项已闭合（D5 修复后，2/3/4 核复跑，主 agent 串行执行取脚本自身退出码）**。① **三段分位全部出现非零行**：`[PROC] exit: pid=N code=0`（退出）→ `[VMM] destroy_page_table: cr3=… deferred_in_call=27`（销毁调用，入链 27 帧）→ `[VMM] deferred-free admitted_total=27 released_total=27 pending=false gen_g=44 min=43`（归还）；且销毁行时序**晚于**退出行（发生在父进程收割路径，非退出路径，与 D4 已落地形态一致）。② **`X`/`Y` 均在串口可见**，第二次 `fork` 后的 `wait_pid` 正常返回 ⇒ 收割链（`wait_reap` → `reap_zombie` → `remove_and_free` → `Process::drop`）确已走通。③ **门槛 6 断言已升级并固化进 `Makefile` 的 `test-smp`**：新增 `grep -qE "\[VMM\] deferred-free admitted_total=[1-9][0-9]* released_total=[1-9][0-9]* pending=false"`（fail-closed）；`make test-smp`（2 核）与 `make test-smp-multicore`（2/3/4 核）RC=0。④ **断言形态的两轮校准（先验后被实测推翻）**：首版断言只要求 `released_total ≥ 1`，实测**对注入 1 后半无判别力**——保留注入时日志仍出现 `admitted_total=37 released_total=7 pending=true gen_g=45 min=0`（滞留批次的少数漏还帧满足正则），`make test-smp SMP_CORES=2` 仍 PASS；**真正的判别字段是 `pending`**。收紧为同时要求 `pending=false` 后复验：注入态 FAIL（EXIT=2）、还原态 PASS（EXIT=0，2/3/4 核）。详见 §5 注入 1。
  - 详情：**闭合的使能项（D 立项已落地）**：本项"销毁路径零调用"的最后一环不在本工程内，而在 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) **D-5**（`init_kernel_process_fields` 显式初始化 `ref_count = 1`）——该路径经 `alloc_zeroed` 分配、不走 `Process::new`，不写该字段则 `ref_count` 恒 0、`dec_ref()` 在 0 上回绕到 `u32::MAX` ⇒ `Box::from_raw` 不执行、`Process::drop` 永不运行 ⇒ `destroy_page_table` 0 调用、帧路径零覆盖。D-5 落地后该链首次贯通：`Process::drop` → `frame_dec` 归零 → `vmm_destroy_page_table` → 27 帧入链 → 代追平后归还。

- **S-11. 用户态执行面不通：内核 IST 栈恒等映射与用户 ELF 装载区 VA 争用（已定位并修复）**
  - 描述：按甲-2a 改 `init`（两次 `fork` + 子进程 `proc_exit`）后实测无效——`released_total` 仍 0、shootdown 仍 62，串口日志止于 `[USER] SELF-CHECK: calling enter_user_asm...`。定性结论：**不是"测试载体缺失"，而是真实的用户态映射缺陷**（用户态根本无法执行），且缺陷由内核静态布局漂移触发，非本工程的逻辑错误。
  - 方案（已裁定：A 根治）：① [gdt.rs](../../src/kernel/framework/arch/x86_64/gdt.rs) 的 `gdt_init` / `gdt_init_ap` 把 `TSS.ist[0..4]` 改为**高半区 VA**（`KERNEL_BASE + 恒等地址`），与 RSP0 的约定统一——用户页表继承内核 PML4[256..511]，该别名天然可达，故无需低半区恒等映射；② [vmm_x86_64.rs](../../src/kernel/framework/mm/vmm_x86_64.rs) 的 `create_user_page_table` **删除** IST 栈页低半区恒等映射（撤销原 B05-55 修复的那段循环）。
  - 状态：[X]
  - 详情：① **根因链（`build/kernel.map` 实测 + `#[repr(C)]` 布局算术，非推测）**：`PER_CPU_GDT[0]` base `0x3EAC48`、条目 stride `0x200d000`、`PerCpuGdt::ist1` 偏移 82122 ⇒ `ist1` 覆盖物理区间 `0x3FED12–0x402D12`，**含 `0x400000`**；IST 映射循环取 `ist_top=0x402D12` ⇒ `start=0x3FE000`、`end=0x402000`，把 `0x400000` 以 `PRESENT|WRITABLE`（**无 USER**）写进用户页表 ⇒ VA `0x400000` 的 `PTE=0x400003 P=1 W=1 U=0`；ELF 装载器复用判据 `existing_phys = virt_to_phys(cr3, vaddr) != 0` 命中该"已有映射" ⇒ **跳过代码段映射**（日志无 `map_page: virt=0x400000`）⇒ Ring 3 取指 `#PF`。② **HEAD 为何正常（证明是布局相关缺陷，非稳定错误）**：HEAD 的 `PER_CPU_GDT[0]` base = `0x2BD738` ⇒ `ist1` 覆盖 `0x2D1802–0x2D5802`，不含 `0x400000`；本工程的 +787 行把静态布局后推约 `0x1F0000`，刚好推过该地址 ⇒ 先前提出的「二分定位触发文件」对该缺陷**无判别力**。③ **修复判别证据（排除"布局巧合"）**：GDT/IDT/TSS 阶段的 TLB shootdown 计数 **24 → 4**，正好减少 20 次 = 4 个 IST × 5 页恒等映射调用消失，证明是"映射删除"生效；修复后 `PER_CPU_GDT` base = `0x3A31D0`（`ist1` = `0x3B729A–0x3BB29A`，远离 `0x400000`）。④ **修复后门槛（主 agent 串行复跑，取脚本自身退出码）**：`make test-smp`（2 核）RC=0——`user_code PTE=0x630A005 P=1 W=0 U=1 NX=0`、`[VMM] map_page: virt=0x400000`、`[SMP] first user syscall from pid=5`、`targets=2`；`make test-smp-multicore`（2/3/4 核）RC=0（同时覆盖 AP 路径 `gdt_init_ap` 的 IST 改动）；`./scripts/qemu_boot_test.sh x86_64` RC=0；QEMU stderr 为 `terminating on signal 15 from pid ... (timeout)`、`CPU Reset` 仅 6 次 ⇒ 内核未自杀、无三重故障循环。⑤ **门槛 5/6 判据已收紧**：`Entering Ring 3` 打印在 `iretq` **之前**，不构成"用户态已执行"的证据；`Makefile` 的 `test-smp` 现已断言 `[SMP] first user syscall from pid=N`（本修复前该断言 FAIL，修复后 PASS）。⑥ **`X`/`Y` 输出不可见——原归因已被推翻（见 §6 D1）**：曾判为"预存行为（用户态 `print_char` 输出不走 COM1）"，但 D1（`stac` 无条件发射 ⇒ 首次 `copy_from_user` 即 `#UD`）修复后 `X`/`Y` **立即出现在串口**（`smp_test_20260920_142638.log`）⇒ 真实原因是 init 在第一次 `print_char` 内部即被终止；原判据（"HEAD 基线同样只有 `first user syscall` 而无 `X`/`Y`"）不成立——HEAD 基线同样是 D1 的受害者。⑦ **配套修复**：用户程序构建目标缺前置依赖导致 `init` 改动长期不生效（`make user` 永不重编），已在 `Makefile` 补 `USER_SRCS` 依赖；`vmm_aarch64.rs` 失效的 `#[expect(clippy::similar_names)]` 已删除。

- **S-12. 移除「无任务可运行即退出整机」的 QEMU debug-exit 路径（用户指令，随本轮落地）**
  - 描述：S-11 症状的**直接触发点**是 [scheduler.rs](../../src/kernel/framework/proc/scheduler.rs) `exit()` 末尾——`self.schedule()` 返回 `None`（无可运行任务）时内核**主动写 `0xf4`**（x86_64 ISA-debug-exit）令 QEMU 退出，退出码 `(exit_code << 1) | 1`（手动实测 7 ⇒ `exit_code=3` = init 的 pid）。按真实内核设计，**该代码不应存在**：内核不得因"没有任务"而结束运行。
  - 方案：① `exit()` 末尾改为 `let _ = self.schedule();` 并注明"生产路径不因无任务而结束运行"；② `pick_next_task` 增加第 5 步**每 CPU idle 任务兜底**——本地候选（DL/RT/CFS + 负载均衡）全空时运行本 CPU 的 idle 任务，使 `schedule()` 在运行期永不为 `None`；③ 整机退出（QEMU exit）职责归测试框架（`framework/tests` 的 `qemu_exit`），不属调度器；④ 配套改 [switch.asm](../../src/kernel/framework/proc/switch.asm) 的**内核线程分支**（idle 是 `cs=0x08` 的内核线程，同特权级 `iretq` 无法换栈，故走 `mov rsp, [rsi+64]; jmp [rsi+56]`）。
  - 状态：[X]
  - 详情：① 判据依据——`outb(0xf4, …)` 是 QEMU ISA-debug-exit **调试设施**，真实硬件 / aarch64 / 容器环境均无此机制，不得作为生产路径的"无任务"出口（与 `docs/plan/archive/audit-2026-08-14/subsystem-proc.md` 的既有判断一致）。② idle 兜底置于 `None` 返回**之前**：`schedule()` 的 `None` 返回仅保留给"连 idle 任务都未登记"的初始化早期窗口（fail-safe，不掩盖配置错误）。③ 修后实测：门槛 6 的 QEMU 不再自杀（stderr 为 timeout 杀死、`CPU Reset` 仅 6 次），`test-smp` 可用窗口恢复为完整 60s。④ 随行项：`pick_cfs_task` 单次 pick 饥饿已修；TSS `RSP0` 与内核栈 VA 约定的复核见 §6（本轮不修）。

- **S-13. 运行期跨核 TLB 失效探针（消除 §5 注入 3 的判别力缺口）**
  - 描述：§5 注入 3 实测「无判别力」——令 `flush_tlb_remote` 不置位后 `make test-smp` 仍 PASS，根因是既有断言只查**日志计数**（shootdown 次数 / `deferred-free pending`），无任何"跨核陈旧翻译是否真的发生"的运行期检出能力。需要一个确定性判别「不置位 / IPI 未送达 / 全量失效退化为空」三类故障的运行期载体。
  - 方案（用户裁定：A+C 组合，A 的页表处置取「A1 完整回收」）：**不比对 shootdown 计数**，而是让每个**远程核**在 `0xFD` 接收路径内读一个专用探测页并按 `(代, 观测字节)` 报告。① `framework/smp/mod.rs` 新增探针状态与 API：`TLB_PROBE_SEQ`、每核 `TLB_PROBE_OBS_SEQ`/`TLB_PROBE_OBS_VAL`，以及 `tlb_probe_arm/ disarm/ report/ wait_remotes`；驱动侧 `wait_remotes` 有界自旋（`TIMEOUT_SPINS=50_000_000`）fail-closed 返回"未达成远程核数（非 0 即失败）"，**不含本核**（本核不构成跨核传播证据）。② `framework/mm/mod.rs` 新增架构无关常量 `TLB_PROBE_VA = 0x0000_7F80_0000_0000`（用户半区 `PML4[255]` 空闲槽）。③ `framework/mm/vmm_x86_64.rs` 新增驱动器 `tlb_probe_selftest()`：建映射到帧 A(`0xAA`) → `arm(1)` + 显式 `tlb_gen_publish_and_shoot()` + `wait_remotes(1, 0xAA)` → `arm(2)` + 重映射到帧 B(`0xBB`，叶子已存在 ⇒ 必经 `flush_tlb_remote`) + `wait_remotes(2, 0xBB)` → `disarm()` → 完整回收。④ `kernel/lib.rs` 引导路径在 `interrupt_late_init`（AP 上线）之后、进入用户态之前调用一次。⑤ `Makefile` 的 `test-smp` 新增两条 fail-closed 断言：日志须含 `[SMP] TLB probe PASS`，且不得含 `[SMP] TLB probe FAIL`。
  - 状态：[X]
  - 详情：① **判别力来源与三类注入的映射**：seq=2 的检出点是「远程核是否报告」——`flush_tlb_remote` 不置位（无远程失效登记 ⇒ 无 IPI）或 IPI 未送达 ⇒ 远程核永不报告 ⇒ 超时 FAIL；`tlb_flush_all` 退化为空操作 ⇒ 远程核报告**陈旧字节 0xAA**（帧 A）⇒ 字节比对失败 FAIL。② **探测页选址**：取用户半区 `PML4[255]`——用户半区不参与 KPTI 共享（KPTI 仅复制高半区 `PML4[256..512]`），且引导期只有 `PML4[0]`（低 4 GiB 恒等映射）与 `PML4[256]`（高半区恒等映射）被 boot 建立，故 `PML4[255]` 可整槽新建 / 整槽拆除；`TLB_PROBE_VA` 落于 `pml4_idx=255 / pdpt_idx=0 / pd_idx=0 / pt_idx=0`，全部为目录 0 号项。③ **不得设 GLOBAL 位**：`tlb_flush_all` 的实现是重载 CR3，重载 CR3 **不失效 GLOBAL 页** ⇒ 探测页带 GLOBAL 会令远程核跨 flush 保留陈旧翻译，探针随即失去判别力（字节比对恒为旧值）。④ **A1 完整回收（零残留，复用既有入口而非自写 unsafe 拆除）**：`unmap_page_in_table(get_kernel_pml4(), TLB_PROBE_VA)` 已具备整链拆卸能力——清叶子 + `flush_tlb_remote` + 递归 `release_frame_locked` 释放变空的 PT/PD/PDPT 三个表页 + 清零 `KERNEL_PML4[255]`；因探测器叶子**非 USER**（`PageFlags::PRESENT | WRITABLE`），其回收路径**不做 `frame_dec`**，故两个数据帧由驱动器自行 `release_frame` 归还。⑤ **接收侧收敛**：`idx`/aarch64 两处 `0xFD`/SGI 13 接收分支的「读代 → 全量失效 → 声明」三步**收敛进 `smp::tlb_catch_up_local()`**（消除逐字平行实现），并紧随其后调 `tlb_probe_report()`。
  - 详情：**门槛实测（主 agent 串行执行取脚本自身退出码）**：`./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`）；`./ci/audit.sh quick` RC=0；`make test-smp`（2 核）RC=0，日志 `[SMP] TLB shootdown #1 gen=1 targets=2` → `#2 gen=2 targets=2` → `[SMP] TLB probe PASS (remotes observed seq1=0xAA, seq2=0xBB; cpu_count=2)`；`make test-smp-multicore`（2/3/4 核）RC=0；`make test-unit` RC=0（QEMU exit 33）；`scripts/qemu_boot_test.sh x86_64` / `aarch64` RC=0（单核路径打印 `[SMP] TLB probe SKIP (single core / SMP disabled)`，不构成失败）。
  - 详情：**§5 注入 3 / 注入 4 复验（判别力成立）**：注入 3（令 `flush_tlb_remote` 不置位）实测 `make test-smp`（2 核）**FAIL**，日志 `[SMP] TLB probe FAIL (miss1=0 miss2=1 remap_ok=true cpu_count=2)` ⇒ 与设计一致（seq=1 由显式发布触发仍过、seq=2 因无 IPI 而超时）；注入 4（令 `mm::arch::tlb_flush_all` 退化为空操作）实测同样 **FAIL**，日志 `miss2=1`（远程核读到陈旧字节）。两轮注入均以 `grep -rn "TEMP-INJECT" src/` 复核**无残留**，还原后 `make test-smp` RC=0。⑥ **已知边界（如实标注）**：seq=2 的**字节比对**存在理论上限——若远程核恰在 seq=1 与 seq=2 之间发生 CR3 重载（如定时器中断触发的切换），其探测页非全局条目被清掉，重映射后即使不认真失效也会读到新字节而"假 PASS"；但**注入 3 的主判据是"远程核是否报告"（超时即 FAIL），不依赖字节比对**，故该注入仍被确定性检出。⑦ **判据取舍**：aarch64 侧接收分支同步收敛 `tlb_catch_up_local()` + `tlb_probe_report()`，但因 aarch64 无 AP 上线路径（`SMP_ENABLED` 恒 false），探针在 aarch64 恒走 SKIP 分支，**仅编译验证**（见 §6）。

- **S-14. 接收侧三段次序静态 fail-closed 审计（消除 §5 注入 2 的判别力缺口）**
  - 描述：§5 注入 2（接收侧三步乱序 ⇒ 假追平）本质是**结构性次序问题**——"假追平"的后果需"远端核访问已 unmap 页、验证不再命中陈旧映射"的跨核交错探针才能稳定检出，属运行期不可观测（构造特定交错代价高、不稳定）。用户裁定：**承认其结构性、非运行期可观测**，以**静态 fail-closed 审计**确定性覆盖，不构造运行期交错探针。
  - 方案：新增 `scripts/audit_tlb_receive_order.py`：结构定位 `framework/smp/mod.rs` 的 `tlb_catch_up_local` 函数体，断言 `tlb_gen_now`（读代）先于 `tlb_flush_all`（失效）先于 `tlb_gen_set_self`（声明）三条语句的**出现次序**；**fail-closed**——函数未找到 / 出现多次 / 括号不闭合 / 任一语句缺失 / 次序倒置，一律判违规 (`sys.exit(1)`)。注册进 `ci/audit.sh` 的 quick 模式（`step "0.5h/6 …"`）。
  - 状态：[X]
  - 详情：① **单点收敛是该审计成立的前提**：三步次序现只存在于 `tlb_catch_up_local` 一处（x86_64 与 aarch64 接收分支均只调用它），故静态断言该点即覆盖两条架构路径，无"某架构漏改"的盲区。② **§5 注入 2 复验（判别力成立）**：临时把函数体改为「flush → 读代 → 声明」并打 `TEMP-INJECT-2` 标记，`python3 scripts/audit_tlb_receive_order.py` 输出 `✗ 三段次序颠倒`（列出各语句 `pos=`，`tlb_gen_now` 的 `pos=83` 晚于 `tlb_flush_all` 的 `pos=54`）并 `exit=1`；还原后 `exit=0`，`grep -rn "TEMP-INJECT" src/` 无残留。③ **与注入 1 后半的关系**：注入 1 后半（令 `tlb_gen_set_self` 空转）由 `Makefile` 的 `pending=false` 运行期断言承担（S-10 详情），本审计不重复覆盖——二者分别覆盖"声明未发生"（运行期可观测）与"声明次序错"（静态可判定）。

- **S-15. 用户态异常埋点门控与栈 guard 自检判据订正（D3 误判的两处成因）**
  - 描述：§6 D3 原按「`[IDT] user exception: vec=14 err=0x7` + `[USER] SELF-CHECK: user_stack_rsp_page … (unexpected)`」判定为"用户栈被映射为只读"的权限缺陷。经实测该前提**被证伪**（该 `#PF` 是 fork 后 COW 写共享页的正常缺页，见 §6 D3 判据 4 项），但两处**诊断噪音**使正常路径在日志中呈现为故障：① `idt.rs` 对**所有** CPL3 异常在**处理之前**无条件 `klog_err!` 打印现场，含被 `Recovered` 的正常缺页；② `user_proc.rs` 的自检以"含 `rsp` 的页"作 guard 判据，而 `initial_rsp` = 栈顶边界 - 8（同文件 `create` 内已写明）⇒ rsp 恒落在栈顶**已映射**页内 ⇒ 该判据**恒报 unexpected**。
  - 方案：① 现场埋点从 `handle()` **之前**移到**之后**，并加门控 `!matches!(action, RecoveryAction::Recovered)`——只对真正未被恢复（终止 / Panic / 域恢复）的 CPL3 异常按 ERR 打印，保留其原始用途（把 `exit code` 归因到异常向量与 RIP；内核态异常仍走 Panic 路径不重复）。② 自检改为按 `stack_bottom - USER_STACK_GUARD` 判定 **guard 页**是否未映射（`stack_bottom` 由 `create` 写入、`try_expand_user_stack` 随扩展下移，guard 页恒紧随其下）；"含 rsp 的页"判据删除（栈顶页已映射属正常布局）。
  - 状态：[X]
  - 详情：① **门槛实测（主 agent 串行执行取脚本自身退出码）**：`./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`）；`./ci/audit.sh quick` RC=0；`make test-host` RC=0；`make test-unit` RC=0（QEMU exit 33）；`make test-smp`（2 核）RC=0；`make test-smp-multicore`（2/3/4 核）RC=0；`scripts/qemu_boot_test.sh x86_64` / `aarch64` 各 RC=0。② **日志复验（修复只消除误报，不改行为）**：2/3/4 核日志中 `[IDT] user exception` 由 1 次降为 **0 次**、旧 `user_stack_rsp_page` 判据消失、`user_stack_guard_page … NOT MAPPED (expected: guard page) ✓` 各出现 1 次；同一次缺页位置仍按序出现 `TLB shootdown #24` → `deferred-free admitted_total=15 released_total=15 pending=false` → `Y` → 两个 `[PROC] exit: pid=N code=0`，即 COW 恢复路径与帧计数与改前**一致**（`admitted == released`，无新增分配）。
  - 详情：③ **负向验证（fail-closed 复核门控未把真故障一起吞掉）**：临时改 `handlers.rs` 的 `PfResult::Fixed => return RecoveryAction::Recovered` 为 `PfResult::Fixed => {}`（标 `TEMP-INJECT-5`，令该 #PF 恢复失败并落到 `TerminateProcess`），`make test-smp`（2 核）日志重新出现 `[ERR] [KERN] [IDT] user exception: vec=14 err=0x7 rip=0x400048 rsp=… cr2=<rsp+6>`（同一位置）⇒ 门控仅在 `Recovered` 时静默，未恢复异常仍照常打印现场；还原后 `grep -rn "TEMP-INJECT" src/` 为空、`git diff --stat src/kernel/framework/idt/handlers.rs` 为空无残留。④ **改动面**：仅 `idt/idt.rs`（埋点位置 + 门控）与 `proc/user_proc.rs`（guard 判据）两文件，无新增 API、无 unsafe 变化。

## 4. 验证门槛（§2.3 五条底线 + 本工程附加项）

1. `./ci/build.sh all`：双架构 0 error / 0 warning。
2. `./ci/audit.sh quick`：核心审计全部通过（F1/F2 边界、F4 SAFETY 覆盖、F7 中文注释、F9 死代码零豁免）。
3. `make test-host`：退出码按脚本内容判读（该目标末尾带 `; true` 属既有 fail-open，见 §6，本工程不改）。
4. `make test-unit`。
5. `scripts/qemu_boot_test.sh x86_64`。
6. **本工程附加**：`make test-smp` 与 `make test-smp-multicore`（2/3/4 核）全部通过。
7. **本工程附加**：门槛 6 的日志中须能观察到 TLB 失效 IPI 的**实际触发**（非零计数），否则 D6 的"运行验证载体"未成立。
   - **实测结论（已成立）**：2/3/4 核各 62 次 `[SMP] TLB shootdown #N gen=N targets=M`，`M` 恒等于核数（含本核，直接验证 P2 决策），`gen` 连续 1→62 无跳变（无并发发布丢失）。
   - **实测更新**：S-11 修复后 **42** 次；S-9 落地后 **22** 次（`gen` 连续 1→22、`targets` 仍恒等于核数），即 map 路径过度失效已按分类收敛，较 42 降约 51%；判据与残余构成见 S-9 详情。
   - 断言已固化进 `Makefile` 的 `test-smp`：① 日志含 `[SMP] TLB shootdown #`；② 存在 `targets=$(SMP_CORES)` 的行（非数字边界匹配——串口日志行尾为 CRLF，行尾锚点 `$` 不适用）。
   - **覆盖边界（实测，D5 修复后已扩展）**：上述计数项只证明 IPI 路径真实执行；「代追平后帧确实释放」由**第五项断言**承担——`make test-smp` 现断言日志含 `[VMM] deferred-free admitted_total=N released_total=M pending=false`（`N,M ≥ 1` 且 `pending` 必为 `false`，fail-closed），依据见 S-10 详情（本项已闭合）。`pending` 是判别字段：只断言 `released_total ≥ 1` 时，注入 1 后半（滞留 pending）仍可通过（§5 注入 1 两轮实测）。**原"未覆盖"的两项缺口现已闭合**：① 「跨核陈旧翻译是否真的发生」由**第六项断言**承担——`make test-smp` 现断言日志含 `[SMP] TLB probe PASS` 且不含 `[SMP] TLB probe FAIL`（fail-closed），依据见 S-13（运行期跨核探针，注入 3 / 注入 4 实测均可检出）；② "假追平"（接收侧三步乱序）由 `./ci/audit.sh quick` 的 `0.5h/6` 静态审计 `audit_tlb_receive_order.py` 承担（fail-closed），依据见 S-14（注入 2 实测可检出）。

> 门槛 1~5 与 6 全部由主 agent 串行复跑；子智能体不得执行 `cargo`/`make`/`ci`/`qemu`。

## 5. fail-closed 注入复验（必做）

- **注入 1**：临时移除 `handle_irq` 的 `0xFD` 前置分支（或使 `tlb_gen_set_self` 空转），确认 `make test-smp` **必须 FAIL**；验后完全还原。
  - **判别力（两轮实测校准，结论取代此前推断）**：前半（移除 `0xFD` 前置分支）**有判别力**——`irq` 越界直索 `irq_descriptors[128]` 会 panic，现有断言足以捕获。后半（令 `tlb_gen_set_self` 空转）**首轮实测判为无判别力**：断言若只要求 `released_total ≥ 1`，注入态日志仍出现 `admitted_total=37 released_total=7 pending=true gen_g=45 min=0`（滞留批次里漏还的少数几帧满足正则），`make test-smp SMP_CORES=2` 仍 PASS（EXIT=0）。**判别字段是 `pending`，不是 `released_total`**。故门槛 6 第五项断言收紧为同时要求 `pending=false`：
    - **注入态实测（保留注入，EXIT=2）**：`make test-smp SMP_CORES=2` FAIL，日志末批 `admitted_total=37 released_total=7 pending=true gen_g=45 min=0`，报错行 `未在 … 中找到 '[VMM] deferred-free admitted_total=N released_total=M pending=false'` ⇒ 后半**现已具备判别力**。
    - **还原态实测（删除注入，EXIT=0）**：`make test-smp-multicore`（2/3/4 核）全部 `SMP TEST PASSED`，各核日志序列一致：`admitted_total=26 released_total=0 pending=true`（首批判定代未追平）→ `admitted_total=26 released_total=26 pending=false`（排空成功）→ `admitted_total=37 released_total=26 pending=true`（下一批入链待排空）。
    - `git diff` 复核：`grep -rn "TEMP-INJECT\|TEMP-DBG" src/` 返回空 ⇒ 无残留。
- **注入 2**：临时把接收侧三步顺序改为"flush → 读代 → 声明"，确认门槛 7 的观测项**必须 FAIL**（假追平被检出）；验后完全还原。
  - **判别力（S-14 修复后，已具备；口径＝静态 fail-closed，非运行期）**：用户裁定**承认其本质为结构性、非运行期可观测**（"假追平"后果需跨核交错探针才能稳定检出，代价高且不稳定），改以静态审计确定性覆盖。实测：把 `tlb_catch_up_local` 改为「flush → 读代 → 声明」（`TEMP-INJECT-2`），`python3 scripts/audit_tlb_receive_order.py` 判 `✗ 三段次序颠倒` 并 `exit=1`；还原后 `exit=0`。该审计已进 `./ci/audit.sh quick`（`0.5h/6`），故"次序错"在 CI 即被阻断。详见 S-14。
- **注入 3（S-9 分类的 fail-closed 复验）**：临时令 `flush_tlb_remote` **不置位** `TLB_SHOOTDOWN_NEEDED`（等价于把全部"替换/权限变更"点错降级为本核失效），确认门槛 6/7 **应当 FAIL**；验后完全还原。
  - **判别力（S-13 修复后，已具备；结论取代此前的"无判别力"）**：此前实测**无判别力**（注入态 `make test-smp` 仍 PASS）——根因是既有断言只查日志计数，而 `destroy_page_table` 内的**直接置位**仍照常推进代，`pending=false`/`targets=N` 均被满足。**S-13 运行期跨核探针落地后复验**：注入态 `make test-smp`（2 核）**FAIL**（EXIT=2），日志 `[SMP] TLB probe FAIL (miss1=0 miss2=1 remap_ok=true cpu_count=2)` ⇒ seq=2 因无跨核 IPI 而超时，与设计一致；断言由 `Makefile` 的 `[SMP] TLB probe PASS` 强制（fail-closed）。
  - **还原态实测**：`git diff` 复核 `grep -rn "TEMP-INJECT" src/` 返回空（无残留）；`make test-smp`（2 核）RC=0，日志 `[SMP] TLB probe PASS`，`make test-smp-multicore`（2/3/4 核）RC=0。
- **注入 4（`tlb_flush_all` 退化为空的 fail-closed 复验，S-13 新增）**：临时令 `framework/mm/arch.rs` 的 `tlb_flush_all` 变空体（requested 的失效不再执行），确认门槛 6 **应当 FAIL**；验后完全还原。
  - **判别力（实测，已具备）**：注入态 `make test-smp`（2 核）**FAIL**（EXIT=2），日志 `[SMP] TLB probe FAIL (miss1=0 miss2=1 remap_ok=true cpu_count=2)`——远程核未失效其 TLB，seq=2 仍读到陈旧字节（帧 A），字节比对失败；还原后 `[SMP] TLB probe PASS (remotes observed seq1=0xAA, seq2=0xBB)`，`grep -rn "TEMP-INJECT" src/` 无残留。
- 三项注入的还原状态须以 `git diff` 复核，禁止残留。

## 6. 登记项与超范围项（§12.5，只报不动）

- `Process::drop` 调用 `vmm_destroy_page_table` 缺"mm 不驻留任何核"契约（[process.rs:617-635](../../src/kernel/framework/proc/process.rs#L617-L635)）；与 `vmspace.rs:190-199` 不对等。**已移交** [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md)（该缺陷的根因是 cr3 无使用者计数，非本工程的 TLB 语义面）。**移交结果（D 立项已落地）**：该调用改为 `frame_dec` 计数归零闸门（同处），"无条件整表销毁"的形态消除；与 `vmspace.rs` 契约的**形式**差异（`unsafe fn` 显式契约 vs 计数表达）保留，登记归 D 立项后续段。
- **S-4 的残留窗口（本设计固有，非误用）**：帧在进入延迟释放时即被写入前 16 字节（`next`/`gen`），相较 HEAD 的"帧内容在 shootdown 前不被触碰"，语义由"延迟"变为"立即"。unmap 路径在写之前已清零父表项、且软件遍历者被 `VMM_LOCK` 排除；仍存在的是**已读过父表项的并发硬件页表遍历**读到被覆盖的 PTE 0 / PTE 1——`gen << 1` 与 `next` 的页对齐共同保证该槽位对硬件呈"不存在"（正常缺页），故最坏后果是伪缺页而非误翻译。destroy 路径的父表项未清零，其正确性仍归 L4/mm 生命周期契约。数据页帧被覆盖属"该帧已无映射、内容本要丢弃"的既有语义。
- `Makefile` `test-host` 末尾 `; true`（fail-open，D-9-4）。
- `acpi.rs` MADT 占位串（D-9-6）；`0x82`（freg）与 MSI 段 `0x80-0x9F` 门重叠。
- `ensure_path_user` 无锁写页表、`ensure_pml4_user` 零调用点。
- **D1: `stac`/`clac` 无条件发射，在无 SMAP 能力的 CPU 上触发 `#UD`（本轮实测发现，已修）**：`copy_user.rs` 的 `smap_begin`/`smap_end` 与 `userptr.rs` 的两对**内联** `stac`/`clac` 共 6 处 asm **无任何门控**，而 CR4.SMAP 仅在 CPUID 报告 SMAP 时置位（`cpu::init_msr`）；`x86_64` 的 `QEMU_FLAGS` 未指定 `-cpu` ⇒ 默认 `qemu64` 不枚举 SMAP ⇒ 首次 `copy_from_user`（`write_syscall` 取用户缓冲）即在 `stac` 处 `#UD`（反汇编定位：`stac` 编码 `0f 01 cb` 位于 `1d0ee0`）。**修复（已裁定：读 CR4 门控）**：`smap_begin`/`smap_end` 先读 CR4 判 bit21，为 0 时退化为 no-op（CR4 是 per-CPU 寄存器 ⇒ 逐核正确、无初始化顺序耦合）；`userptr.rs` 的两对平行内联改为复用同一封装。**该缺陷是 S-10 载体长期不成立的直接原因**——init 在第一次 `print_char` 内部即被终止。
- **D2: `is_user_mode()` 的 RIP 启发式把内核态异常误判为用户态（本轮实测发现，已修）**：`idt/types.rs::is_user_mode` 的 `rip_check = rip < KERNEL_TEXT_BASE && rip > USER_ADDR_MIN` 假定内核运行在高半区，但内核 `.text` 实际链接于低半区（`. = 0x100000`，`KERNEL_BASE` 仅作别名）⇒ 该条件对内核态异常**恒真** ⇒ D1 的 `#UD`（内核态）被当作用户态异常，走 `handlers.rs` 的 `TerminateProcess(1)` 而非 Panic，内核现场被静默吞掉（实测 exit code=1）。**修复（已裁定：仅按 CS.RPL）**：`is_user_mode` 只判 `(cs & 3) == 3`，删除 RIP 启发式；`test_user_mode_detection` 中固化错误行为的断言同步反转。
- **D3: 用户态栈写入触发 `#PF`（前提于本轮证伪：非缺陷；两处诊断噪音已修，见 S-15）**：D1/D2 修复后用户态首次真正执行，实测 `[IDT] user exception: vec=14 err=0x7 rip=0x40003A rsp=0x7FFFFFFE8FF0 cr2=0x7FFFFFFE8FF6`（`err=0x7` = P=1|W=1|U=1 ⇒ 页存在但写保护；`cr2` 落在用户栈页 `0x7FFFFFFE8000` 内）。同一日志的 `[USER] SELF-CHECK: user_stack_rsp_page ... (unexpected: should be guard/unmapped)` 指向同一处。**本轮收口（前提证伪 + 诊断订正）**：该 `#PF` 并非"用户栈被映射为只读"的权限缺陷，而是 **fork 后 COW 写共享页**的正常缺页——处理器返回 `Recovered` 后原指令重试即成功；使它在日志中"看起来像故障"的两处诊断噪音已修（见 S-15）。原"独立缺陷，须单独定位 PTE 来源"的归属**撤销**。
  - 详情：**D5 修复后复跑（2/3/4 核，突然复现且位置稳定）**：三份日志各出现**一次** `[IDT] user exception: vec=14 err=0x7 rip=0x400048 cr2=<rsp+6>`（2 核 `rsp=0x7FFFFFF9EFF0`/`cr2=…EFF6`；3 核 `rsp=0x7FFFFFF01FF0`；4 核 `rsp=0x7FFFFFF6FFF0`），出现时机均为**第二次 `fork` 的前一个 `wait_pid` 返回之后**（紧跟首个 `destroy_page_table` 与 `[SMP] TLB shootdown`），此后进程继续执行并输出 `Y` ⇒ **该异常非终止性**（恢复机制未核实：可能是页表项被后续路径修正，也可能是硬件重试时权限已变，须单独定位）。与 `[USER] SELF-CHECK: user_stack_rsp_page ... (unexpected: should be guard/unmapped)` 指向同一处（用户栈顶页被映射为只读）。**归属订正（本轮证伪）**：非缺陷——恢复机制即 COW 唯一引用 fast path（判据见下条）；但**确实不属 D5 范围**（D5 修复前该处直接挂起在内核态，无从观测到本异常）。
  - 详情：**根因与判据（实测 4 项）**：① **机制**——`clone_user_page_table_cow` / `mark_cow_readonly`（[cow.rs:41-70](../../src/kernel/framework/mm/cow.rs#L41-L70)）在 fork 时对父子共享页清 WRITABLE 位，被写时经 `cow_handle_fault`（[cow.rs:442-458](../../src/kernel/framework/mm/cow.rs#L442-L458)）**唯一引用即直接恢复 WRITABLE 位、不分配新页** ⇒ `err=0x7`（页存在 + 写 + CPL3）正是 COW 缺页的应有编码。② **帧总数不变**——改前日志（`tests/reports/smp_test_20260922_162310.log`，2 核）缺页后为 `deferred-free admitted_total=16 released_total=16 pending=false`；真复制路径必使 `admitted` 增长 ⇒ 命中唯一引用 fast path。③ **时序自洽**——缺页行紧接 `[PROC] exit: pid=6 code=0` + `destroy_page_table`（子进程先退出，`frame_dec` 把共享帧计数降回 1）与 `TLB shootdown #24`（权限变更点）；缺页后紧接输出 `Y` 与 `[PROC] exit: pid=7 code=0`，全程 `SEGV`/`SIGKILL` 0 次、`user exception` 仅 1 次 ⇒ 异常被**恢复**而非终止。④ **地址自洽**——`cr2 = rsp+6` 与用户侧 `print_char` 约定一致（同次日志 `SELF-CHECK: user_code first 16 bytes: 50 48 8D 74 24 04 C6 06 58 …` = `push rax; lea rsi,[rsp+4]; mov byte [rsi],'X'`）：写入的是**自身栈帧槽**，页本应可写，仅因 fork 刚把它标只读而缺页。
  - 详情：**误判成因（两处诊断噪音，已修，见 S-15）**：① `idt.rs` 对所有 CPL3 异常在**处理之前**无条件 `klog_err!` 打印现场，被 `Recovered` 的正常缺页因此呈 ERR 级"故障"；② `user_proc.rs` 的自检以"含 `rsp` 的页"作 guard 判据，而 `initial_rsp` = 栈顶边界 - 8（[user_proc.rs:1060-1065](../../src/kernel/framework/proc/user_proc.rs#L1060-L1065)）⇒ rsp 恒落在栈顶**已映射**页内，该判据**恒报 unexpected**。二者共同把一次正常缺页读作"栈权限缺陷"。
- **D4: 退出路径中「销毁地址空间」调用早于「状态置位」（前序实测发现，已修，本轮 D5 修复后实测确认可达）**：原缺陷为 `proc_ops.rs::process_exit` 先调 `destroy_by_pid_no_kstack(pid)`、后调 `SCHEDULER.exit(code)`，而前者的守卫 `destroy()` 要求 `is_exited()`（`Process::state ∈ {Zombie, Terminated}`）却在**后者内部**置位 ⇒ 守卫恒 false ⇒ 页表与帧永不释放（彼时 261 行完整日志中 `destroy_page_table` 0 行）。
  - 详情：**已落地形态（前序修复，源码核实）**：`process_exit` 已**不再**调用销毁（[proc_ops.rs:395-397](../../src/kernel/framework/proc/proc_ops.rs#L395-L397) 处明确注记"此处不可调用…会使页表永不销毁"）；Zombie 置位收敛到 [scheduler.rs:988](../../src/kernel/framework/proc/scheduler.rs#L988)（`exit` 内），销毁点**单一收敛**到 `Process::drop` → `vmm_destroy_page_table`，而 `Process` 只在**收割 (reap)** 时释放（`wait4::reap_zombie` → `remove_and_free`，[wait4.rs:102](../../src/kernel/framework/syscall/wait4.rs#L102)；或 `tick_accounting` 的周期僵尸回收）——[scheduler.rs:1029-1035](../../src/kernel/framework/proc/scheduler.rs#L1029-L1035) 已就此写明。全仓生产路径已无 `destroy_by_pid_no_kstack` 调用点（仅 `tests/test_smp.rs` 使用）。
  - 详情：**本轮实测确认（D5 修复后）**：2/3/4 核均出现 `[VMM] destroy_page_table: cr3=… deferred_in_call=27` 与 `[VMM] deferred-free admitted_total=27 released_total=27 pending=false` 非零行，且时序**晚于** `[PROC] exit: pid=6 code=0`（即发生在父进程收割路径，非退出路径）⇒ 销毁路径可达、帧确有归还，S-10 的"零覆盖"前提消除。
  - 详情：**仍属已登记设计差异（不随本条闭合）**："退出即释放"与上游（Asterinas `set_vmar(None)` / Linux `exit_mm`）不同，地址空间晚于退出被释放；该差异归 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) G4。
- **D5: syscall 内核栈为「每 CPU 共享」，非任务私有（本轮实测定位，已修）**：syscall 入口 [isr.asm](../../src/kernel/framework/boot/isr.asm) 无条件把 RSP 重置为**每 CPU 一份**的 `syscall_stack` 栈顶，`kernel_rsp` 仅在 [gdt.rs:542](../../src/kernel/framework/arch/x86_64/gdt.rs#L542)（BSP）与 [gdt.rs:682](../../src/kernel/framework/arch/x86_64/gdt.rs#L682)（AP）各写一次、**运行期从不按任务更新** ⇒ **任何在 syscall 上下文中让出 CPU 的任务，其内核栈帧会被随后在同一核上执行的 syscall 覆盖**，恢复该任务时其 `schedule()` 栈帧已损坏。实测（`tests/reports/smp_test_20260920_160508.log`）：`switch: cpu=0 6 -> 5` 之后 `resume: cpu=0 was=0 next=1597180`（预期 `was=5 next=6`），随后整机静默。影响面：`wait_reap` 忙等（[wait4.rs](../../src/kernel/framework/syscall/wait4.rs)）及一切"syscall 内阻塞/让出"路径。**它是 S-10 全部载体失败的共同最上游原因**。
  - 方案（用户裁定：乙 = 任务私有内核栈 + 内核栈契约统一）：① **契约统一**——`TSS.RSP0` 与 `SyscallPerCpu.kernel_rsp` 恒为「当前任务内核栈顶的**高半区 VA**」（`phys + KERNEL_BASE`），且只经 [cpu/arch.rs](../../src/kernel/framework/cpu/arch.rs) 的 `set_kernel_stack` **单一写入点**同时更新（x86_64 分支先 `tss_set_kernel_stack` 再 [gdt_set_kernel_rsp](../../src/kernel/framework/arch/x86_64/gdt.rs#L805)）；BSP / AP 初值（[gdt.rs:547](../../src/kernel/framework/arch/x86_64/gdt.rs#L547) / [gdt.rs:684](../../src/kernel/framework/arch/x86_64/gdt.rs#L684)）同步改为高半区 VA。② syscall 返回侧不再需要 `add rsp, KERNEL_BASE` 别名修正（[isr.asm:283-293](../../src/kernel/framework/boot/isr.asm#L283-L293) 该段已删）；[switch.asm](../../src/kernel/framework/proc/switch.asm) 的同类修正改为**按 RSP 实际所处半区条件转换**（仅 boot 低半区栈转别名，避免对已是高半区的任务内核栈重复偏移）；内核线程（`cs=0x08`）分支改用 `mov rsp, [rsi+64]; jmp qword [rsi+56]` 换栈（同特权级 `iretq` 不加载 RSP/SS）。
  - 状态：[X]
  - 详情：① **三处调用点已全部收敛到 `set_kernel_stack`**（[user_proc.rs:1138](../../src/kernel/framework/proc/user_proc.rs#L1138)、[scheduler.rs:709](../../src/kernel/framework/proc/scheduler.rs#L709)、[scheduler_ex.rs:698](../../src/kernel/framework/proc/scheduler_ex.rs#L698)）⇒ §6 前序登记的「TSS `RSP0` 与内核栈 VA 约定未逐点核实」随之闭合（见下条）。② **修复中同步定位并修复的第二个缺陷（同属切换原语，不单列条目）**：[switch.asm](../../src/kernel/framework/proc/switch.asm) 恢复侧原先对**所有**路径执行 `mov gs, ax`。按本仓库已核实的 x86 语义（[arch/x86_64/mod.rs](../../src/kernel/framework/arch/x86_64/mod.rs) `enter_user_asm` 处注释），`mov gs, sel` 以描述符基址（数据段基址恒 0）写入 `IA32_GS_BASE`：用户路径（`cs=0x23`，其前已 `swapgs`）正需 base=0，但**内核态恢复路径**（任务阻塞在 syscall 内被换回，`cs=0x08`）会把 per-CPU 基址清零 ⇒ 随后 syscall 出口 `mov rax, [gs:USER_PML4_OFF]`（偏移 16，[isr.asm:161](../../src/kernel/framework/boot/isr.asm#L161)）读到物理低地址 `0x10` 处的实模式 IVT 垃圾 ⇒ `mov cr3, 垃圾` ⇒ 挂起。**修复**：`mov gs, ax` 移入用户态分支（`swapgs` 之后），内核态 / 内核线程路径不再触碰 GS 段寄存器。③ **实测证据（临时埋点，判别后已全部删除）**：修复前 `[DBG] EXIT pid=5 nr=61 ret=6 gs=0x0`、修复后 `gs=0x3ef060`；2/3/4 核日志均出现第二次 `fork` 之后的 `Y` 与 `[PROC] exit: pid=N code=0`。修复前 2 核的"判据恰好满足但进程实际已挂起"不再可能——门槛 6 已同时断言 `Y` 所在路径的产物（见 §4）。④ **门槛复跑（主 agent 串行执行，取脚本自身退出码）**：`./ci/build.sh all` RC=0（`Passed: 5 Failed: 0`，host tests 全通过）；`./ci/audit.sh quick` RC=0；aarch64 clippy 显式复跑 RC=0；`make test-host` RC=0；`make test-smp`（2 核）RC=0；`make test-smp-multicore`（2/3/4 核）RC=0。⑤ **未修隐患（登记，独立缺陷）**：[gdt_init_ap](../../src/kernel/framework/arch/x86_64/gdt.rs#L641) 只写 `IA32_KERNEL_GS_BASE = &ap.syscall`（[gdt.rs:702-704](../../src/kernel/framework/arch/x86_64/gdt.rs#L702-L704)），**从不写 `IA32_GS_BASE`** ⇒ AP 在内核态 `GS.BASE = 0`（实测 `[DBG] IPI-FD cpu=1 gs=0x0 fcs=0x8`）。本轮 AP 不进入调度器（其 `per_cpu.current` 恒 0），且 `0xFD`/`0xFE` 分支不读 `[gs:…]`，故当前无可见后果；但 AP 侧任何 KPTI 入口/出口路径（读 `USER_PML4_OFF`）会落到与 ② 同一「物理低地址 → 垃圾」模式。BSP 在 [gdt.rs:575-585](../../src/kernel/framework/arch/x86_64/gdt.rs#L575-L585) 有写该 MSR，AP 缺此步——属独立缺陷，须单独定位。
- **GDT / IDT / TSS 低半区恒等映射的同源风险（S-11 只处理了 IST 一路）**：`create_user_page_table` 仍把 GDT / IDT / TSS 页**低半区恒等映射**进用户页表（硬件按 GDTR / IDTR / TSS 基址以低 VA 访问）；若用户 ELF 段落落在这些页上，会被 ELF 装载器的"已有映射即复用"吞掉——与 S-11 **同机制**。本轮**不修**：需要把这些页的 VA 与用户装载区（`0x400000`）纳入同一布局约束，属独立设计。
- **ELF 装载器复用判据不校验 U 位（S-11 的放大器）**：`load_elf_from_memory` 以 `existing_phys = virt_to_phys(cr3, vaddr) != 0` 判定"复用现有页"并**跳过映射**，不校验目标 PTE 的 USER 位；一旦命中无 USER 的内核映射（S-11 情形），用户代码段即**静默**无映射。本轮**不修**：判据收紧（要求 U=1 且权限兼容）与 fork / CoW 复用路径耦合，须单独设计。
- **`vmm_map_user_page` 污染内核页表（预存）**：该函数同时调 `vmm_map_page_in_table(cr3, …)` 与全局 `vmm_map_page(vaddr, …)`，把**用户 VA 映射写进内核页表**（boot 日志中可见针对内核高半区地址的 `huge split entry=0xFFFF80000…`，其归因未逐一核实）。本轮**不修**（与本次缺陷无因果关系）。
- **TSS `RSP0` 与内核栈 VA 约定的复核（前序裁定登记，随 D5 已闭合）**：用户态返回路径 [switch.asm](../../src/kernel/framework/proc/switch.asm) 原对当前栈做 `add rsp, KERNEL_BASE` 别名修正（B05-55），而 `TSS.RSP0` 由 [user_proc.rs](../../src/kernel/framework/proc/user_proc.rs) / [scheduler.rs](../../src/kernel/framework/proc/scheduler.rs) / [scheduler_ex.rs](../../src/kernel/framework/proc/scheduler_ex.rs) 三处 `set_kernel_stack` 分别写入——三处是否恒为同一 VA 约定（高半区别名 vs 低 LMA）**未逐点核实**；若不一致，会在同一物理栈上产生双重别名位移。**D5 处置结果**：三处调用点已全部收敛到 [cpu/arch.rs](../../src/kernel/framework/cpu/arch.rs) 的 `set_kernel_stack`（单点同时写 `TSS.RSP0` 与 `SyscallPerCpu.kernel_rsp`，值恒为高半区 VA），别名修正改为**按 RSP 实际半区条件转换** ⇒ 双重位移前提消除，本项闭合（依据见 D5 方案 ① 与详情 ①）。
- ~~aarch64 无 AP 上线路径 ⇒ `SMP_ENABLED` 恒 false，本工程 aarch64 侧**仅编译验证**，不得标注为已验证。~~ **（前提已订正）** DECISION-082（aarch64 SMP bring-up）使 aarch64 次核上线，`SMP_ENABLED` 不再恒 false；DECISION-083（[aarch64-tlb-shootdown-send.md](./aarch64-tlb-shootdown-send.md)）将发送侧接入架构无关公共层 `mm/deferred_free.rs`，并在 QEMU `-smp 2` 下**运行验证**（`[SMP] TLB shootdown #N gen=… targets=…` 与 `IRQ: intid=13`）。**注意**：「可收 SGI 13」并非 DECISION-082 自动成立 —— 施工期实证发现 SGI 13/14 从未使能且 `send_ipi` 目标编码有误，由 DECISION-083 的 ST-09 修复（详见该文件前提订正）。
- `vmm_x86_64.rs` 在 HEAD 即存在的 fmt 违规。
- 扩展项：页级 shootdown（替代 `tlb_flush_all`）、额外排空点（tick / 返回用户态前）、`destroy_page_table` 的 mm 生命周期屏障。
- 容器路线的**工程最优次序**（已裁定）：**甲（侵入式帧链表，即本工程 S-4）→ 乙（容器/所有权下沉 PMM，随 D2+ 一并做；届时 D5"排空点唯一在 `release_lock`"须一并重裁）→ 丁（`destroy_page_table` 分片重写：有界批量 + 批次间同步等待 + 可恢复四级遍历游标，即 Asterinas `Cursor` 形态，独立立项）**；**丙不采纳**（"定长表 + destroy 溢出即释放"不修复 L1，与 §7.4 A7-3①"容器无上限"的结论冲突）。

## 7. 外部实现对照：Asterinas 0.18.1（`other/asterinas-0.18.1`，仅 `ostd` 侧）

> 核实范围：`ostd/src/mm/tlb.rs`、`ostd/src/mm/vm_space.rs`、`ostd/src/smp.rs` 均已逐行读过；`kernel/` 侧整表销毁路径**未核实**。

### 7.1 结论先行：它用**帧引用计数**取代了 ack 计数/代计数

Asterinas 表达"哪些核尚未完成失效"不是靠计数器，而是靠**帧自身的引用计数**：`dispatch` 时把每个待释放帧 `clone` 进**每个目标核的队列**（[tlb.rs:331](../../other/asterinas-0.18.1/ostd/src/mm/tlb.rs#L331) `frame_keeper.extend(other.frame_keeper.iter().cloned())`），各核在完成 flush 后各自 `drop` 自己那份（[tlb.rs:364-370](../../other/asterinas-0.18.1/ostd/src/mm/tlb.rs#L364-L370)），物理页在**最后一个引用归零**时才归还。于是"失效先于释放"由生命周期自然成立，**无需 ack、无需代、无需全局 pending 表、无需排空点**。

### 7.2 完整链条

```text
kernel 层 → VmSpace::unmap(len)                    [vm_space.rs:466-532]
  ├ 逐段 take_next() 取页表片段
  ├ Mapped::TrackedFrame → issue_tlb_flush_with(for_single(va), RcuDrop<Frame>)   [L483-496]
  ├ StrayPageTable(空页表页) → issue_tlb_flush_with(for_range(va..), RcuFrame)     [L521-524]
  └ dispatch_tlb_flush()                                                          [L529]
      ├ irq::disable_local()
      ├ target_cpus = self.target_cpus.load(Release)      ← per-mm CPU 集合 (mm_cpumask 等价)
      ├ 若含本核 → 从集合移除, need_flush_on_self = true                             [L96-99]
      ├ 对每目标核: FLUSH_OPS[cpu].lock().push_from(&ops_stack)  ← 帧被 clone 到该核队列   [L102-105]
      ├ inter_processor_call(&target_cpus, do_remote_flush)                       [L107]
      │   ├ 逐核: CALL_QUEUES[cpu].push_back(fn) + HAS_PENDING_IPIS[cpu]=true      [smp.rs:124-130]
      │   ├ 逐核 send_ipi                                                          [smp.rs:132-139]
      │   └ 目标含本核 → 直接 call_fn() (不发 IPI 给自己)                            [smp.rs:140-143]
      └ 本核最后 flush_all() / clear_without_flush()   ← 本核那份帧在此交 RcuDrop      [L111-116]

远端核: IPI 中断 → drain CALL_QUEUES → do_remote_flush()   [tlb.rs:251-263]
      ├ swap FLUSH_OPS[current] ↔ new_op_queue (持锁极短) → 立即解锁
      └ new_op_queue.flush_all() → clear_without_flush() → 该份帧交 RcuDrop
         (OpsStack::drop 为兜底, 见 [L380-385])

仅 2 处调用 sync_tlb_flush: dma/util.rs:143, io_mem/mod.rs:138
      → PendingIpis::wait() 逐核自旋等 HAS_PENDING_IPIS[cpu]==false  [smp.rs:77-93]
         (断言 IRQ 必须开启, 否则 panic — 与 A 档 §2.3 结论同源)
```

### 7.3 与我们 B 档的差异（按维度）

| 维度 | Asterinas 0.18.1 | 本文件（B 档） |
|---|---|---|
| 完成判定 | **帧引用计数**（谁最后 drop 谁释放） | 全局代 + 每核已追平代 |
| 等待 | 常态**不等待**；仅 DMA/IoMem 2 处 `sync` | 从不等待 |
| "谁是相关核" | per-mm `AtomicCpuSet` | `CPU_ONLINE` 全体广播 |
| 帧容器 | 调用方栈上 `Vec`（无上限），**且 clone 到各核队列** | 全局定长表 + 拥塞退路 |
| 额外屏障 | RCU grace period（`RcuDrop`） | 无 |
| 粒度 | all / 单页 / 范围（8 字节编码，32 页阈值退化） | 仅 flush all |
| 本核 | 发完 IPI 后自行 flush（同步执行回调） | 计划发 IPI 含本核（D3） |
| 帧表示 | 引用计数的 `Frame`（`Arc` 风格） | 帧类型仍是裸 `u64`；持有计数面已由 PMM 侧表承载（[cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) §8.1） |

### 7.4 对本工程的直接影响（三条）

- **A7-1（可替代性；前提已部分达成）**：Asterinas 证明存在一条不需要"代"的路线，且它比 B 档更短——但**前提是帧本身是引用计数对象**。Edgine 的帧类型仍是无内嵌计数的裸 `PhysAddr`（`u64`）；原先"唯一现成的引用计数面是 `COW_REFS`（COW 专用）"的判断**已失效**——`COW_REFS` 与 `Frame.ref_count` 两条重复计数面已删除，改为由 PMM 维护**按 pfn 索引的帧持有计数面**（`frame_inc` / `frame_dec` / `frame_ref_count`，契约见 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md) §8.1）。剩余差距只在"计数面位置"（PMM 侧表 vs 帧内嵌），不再是"完全没有通用引用计数"。
- **A7-2（S-6 的定性已变更）**：帧持有计数面现已存在，其"最后持有者归零才释放"语义已在 `destroy_page_table` / `unmap_page_in_table` 落地（USER leaf 逐项 `frame_dec`，归零才 `defer_free`）。当前裁定仍为 **S-6 保持现状不动**（§1 D8 / §3 S-6）——容量与生命周期缺口的收敛路径整体移交独立立项（[cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md)），本工程不据此改动 `destroy_page_table` 的容量策略。
- **A7-3（两点可立即借鉴，与"代"无关）**：① 帧容器**无上限**（Asterinas 用 `Vec`，非定长数组 + 溢出即释放），这一点无论走哪条路线都应采纳，直接消除 §2.2 L1；② **目标核集合按 mm 维度**而非全在线核（Asterinas 与 Linux 一致），可消除对本工程"离线核 gen 参与 min"的整类边界情形——但是否引入 `mm_cpumask` 属范围扩张，需单独裁定。
- **A7-4（已核实；原为未核实项）→ 结论：本工程不需要 RCU 宽限期层**。Edgine 的 RCU API（`rcu_read_lock/unlock`、`rcu_dereference/assign_pointer`、`synchronize_rcu`、`call_rcu`，见 [rcu.rs:6-21](../../src/kernel/framework/sync/rcu.rs#L6-L21)）在全仓的调用点**只有三类**：`rcu.rs` 自身、[prelude.rs:17](../../src/kernel/framework/prelude.rs#L17) 与 [sync/mod.rs:95](../../src/kernel/framework/sync/mod.rs#L95) 两处 re-export、以及 `tests/`（`test_new_features.rs:153-164` 等）。**生产代码（mm / proc / fs / net）零使用** ⇒ 不存在"RCU 读者持有页表或帧"的场景 ⇒ "代追平即 free" 充分，本工程**不引入** RCU 屏障。对照：Asterinas 需要它，是因为它的页表页本身受 RCU 保护（§7.5）。

### 7.5 整表销毁链条（已核实）

- **无显式 `Drop for VmSpace`**：`VmSpace` 靠 `Arc` 引用计数归零触发隐式 Drop。[PageTableGuard::drop](../../other/asterinas-0.18.1/ostd/src/mm/page_table/node/mod.rs#L253-L257) 只做 `self.inner.meta().lock.store(0, Ordering::Release)`，**不释放帧**。
- **页表页走 RCU 回收**：[PageTablePageMeta](../../other/asterinas-0.18.1/ostd/src/mm/page_table/node/mod.rs#L262-L277) 的 `stray` 字段注释原文——“A page table can be detached from its parent while still being accessed, **since we use a RCU scheme to recycle page tables**”。即"页表页可能在被访问的同时被父节点回收"，故须 RCU 宽限期。**这正是 Edgine 不需要该层的对照依据**（§7.4：Edgine 无 RCU 读者）。
- **"无核驻留"由 per-mm 激活集合显式保证**：[VmSpace::activate](../../other/asterinas-0.18.1/ostd/src/mm/vm_space.rs#L126-L150) 在切换时 `self.cpus.add(cpu, ...)`（L138），并把**上一个 VmSpace** 从本核移除（L147 `last.cpus.remove(cpu, ...)`）；`TlbFlusher` 的目标核集合即取自该集合（[L120](../../other/asterinas-0.18.1/ostd/src/mm/vm_space.rs#L120)）。⇒ 一个 VmSpace 被销毁时其 `cpus` 必为空，**整表销毁不需要 shootdown**。
- **对 S-6 的直接意义**：外部对照支持"整表销毁不靠 TLB shootdown，而靠 mm 生命周期"。但**该方向在 Edgine 当前不可直接采用**——Asterinas 靠 `Arc` 引用计数 + per-mm 激活集合两条前提，Edgine 一条仍缺：**没有** per-mm 激活集合（只有全局 `CPU_ONLINE`）。"地址空间使用者计数"一条**已补齐**——`Process::drop` 不再无条件整表销毁，而是对 cr3（PML4 帧）先 `frame_dec`，**归零才** `vmm_destroy_page_table`（[process.rs:617-635](../../src/kernel/framework/proc/process.rs#L617-L635)），故 `CLONE_VM` 兄弟进程共用同一 cr3 时不会被先退出方回收。故本工程 S-6 裁定仍为"保持现状不动"，剩余前提（per-mm 激活集合）与容量缺口的收敛移交 [cr3-lifetime-ownership.md](./cr3-lifetime-ownership.md)。