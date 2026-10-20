# `ci-rustdoc` 文档链接门禁收口

> **定位**：把 P-39 立案的实测事实收成一个可执行工程——本地一键 CI 的 rustdoc 腿在 HEAD 上即红（30 error + 19 warning，涉 19 文件），且该腿不在 `AGENTS.md` §2.3 六门槛内，所以历轮"六门槛全绿"从未覆盖它。本文档是该病灶的唯一推进场所。
>
> **来源**：`docs/plan/net-e2e-tcp-echo-flakiness.md` §6 P-39（本轮按用户裁定「单开」毕业至此，原条目改为指针）。
>
> **关系**：判据与工具链口径见 `AGENTS.md` §2.2/§2.3 与 §5 F8（公共 API 中文文档注释）；`docs/plan/progress-active-tasks.md` §对账方法。与 P-37（GitHub Actions 腿从不触发）正交：同一判据在 `ci-lint.yml` 的 `rustdoc-check` job 里也会红，但那条腿在 Gitee-only remote 上不可达，故本工程的验收只能走本地。
>
> **不做什么**：不改 `Makefile.ci` 的 `-D` 判据来"让门禁变绿"——那属门槛假绿的另一类（对照 `docs/plan/unresolved-issues-2026-08-09.md` 第 0A 类 AUDIT-TOOL-* 的根治立场：修病灶，不修判据）。

## 1. 立项依据

### 1.1 门禁是什么

`make -f Makefile.ci ci-rustdoc` = 双架构各跑一次 `cargo doc --no-deps`，`RUSTDOCFLAGS="-D rustdoc::broken-intra-doc-links"`。x86_64 腿直接失败（RC=101）。`Makefile.ci` 顶部有 `SHELL := /bin/bash -o pipefail`，故该 recipe 的 `| tee | tail -5` **不**掩盖退出码——报的是**真红而非假绿**（与本轮已根治的五处 workflow 假绿不同类）。

后果有二：一是 aarch64 腿从未被执行（同一 recipe 串联，x86 先红即中断）；二是 §2.3 不含 rustdoc，所以这条腿坏了多久无从追溯，历轮验收也不含它。

### 1.2 失败分布（实测，HEAD 上）

两类形态：

- **`error: unresolved link`**：被反引号包裹却无对应可解析项，rustdoc 当 intra-doc link 解析。样本：`` `idx` ``、`` `24` ``、`` `gs:0` ``、`pl011_phys_base`。
- **`warning: public documentation for X links to private item Y`**：pub API 的文档注释指向私有符号。

文件分布（30 error + 19 warning / 19 文件）：`privileged/mm/pmm.rs` 11 处、`functions/driver/usb/xhci.rs` 4 处、`privileged/mm/mod.rs` 3 处；`vmm_x86_64.rs` / `functions/proc/process_vm.rs` / `functions/driver/char/mod.rs` 各 2 处；`dtb.rs` / `arch/x86_64/gdt.rs` / `arch/x86_64/apic.rs` / `mm/cow.rs` / `timer/clockevent.rs` / `timer/calibration.rs` / `syscall/dispatch.rs` / `smp/mod.rs` / `proc/user_proc.rs` / `proc/process.rs` / `net/init/dns.rs` / `functions/proc/coredump.rs` / `functions/net/syscall.rs` 各 1 处。

### 1.3 为什么不是机械替换

每处需三选一定型：

| 修法 | 动作 | 适用 | 代价 |
| --- | --- | --- | --- |
| ① | 引用改成可解析真实路径（`crate::path::to::Item`） | 被引对象确为公开项、只是路径写错 | 需逐个核符号可见性 |
| ② | 让该词不成链接（去反引号 / 改写为文字描述） | 被引者是散文用词（变量代称、寄存器名、十进制字面量） | 零 API 影响，最保守 |
| ③ | 把被引符号升为 `pub` 并配中文文档注释 | 被引者本应属公共 API 却被私有化 | **改 API 可见性面**，触 F8 与 §7.2 review 要求 |

`links to private item` 一族尤其落在 ② / ③ 之间，那是 **API 可见性判定**，不能由工具或实施者代答（§9.1 决策灰色地带）。

## 2. 工程条目

- **R-1 深度裁定（甲 / 乙）**：本工程实修到哪一层，须用户先裁。
  - 描述：甲 = 只清 30 个 `unresolved link` error，使 `ci-rustdoc` 转绿，warning 按现状放行；乙 = error 与 19 个 `links to private item` warning 一并收口，并把 `RUSTDOCFLAGS` 的 `-D` 面加压到 `rustdoc::private_intra_doc_links`（此后 warning 即阻断）。乙含 ③ 类改动的逐处可见性判定，改动面与 review 成本显著高于甲。
  - 方案：按 §11.3 前置询问后择一开工；本条未裁定前 R-2 以下不得启动。
  - 状态：[ ]（待裁定）

- **R-2 ②类先行批次（散文用词去链接化）**：风险最低的一族先收。
  - 描述：把 `` `idx` `` / `` `24` `` / `` `gs:0` `` 这类被反引号包裹的散文用词改为不成链接的写法，零 API 影响、零行为影响，可先削掉 error 总数的相当份额。
  - 方案：逐处改写中文注释（§5 F7），跑 `make -f Makefile.ci ci-rustdoc` 复验；每批次以文件为单位提交，便于回退定位。
  - 状态：[ ]（依 R-1）

- **R-3 ①类批次（路径校正）**：被引者已是公开项、只是 intra-doc 路径写错。
  - 描述：为每处确定当前可见路径并改写为 `crate::…::Item` 形式；这批改动天然可验证（rustdoc 不再报 unresolved）。
  - 方案：先 `cargo doc` 输出按文件分组，逐文件收口并复验；若某处发现被引者其实私有，转 R-4 处理，不就地升 `pub`。
  - 状态：[ ]（依 R-1）

- **R-4 ③类清单（可见性判定台账）**：把需要"升 `pub`"的候选集中列出让用户裁。
  - 描述：`links to private item` 的每一处对应一个"该符号是否属公共 API"的问题。逐处登记：符号、所属模块、当前可见性、若升 `pub` 是否需配中文 doc（F8）、是否触 TCB 边界（I1-I6）。
  - 方案：先出清单不改动，交用户逐条或批量裁定后再实施。
  - 状态：[ ]（依 R-1）

- **R-5 aarch64 腿从未执行的验证**：修完后必须让第二架构真跑一次。
  - 描述：现状是 x86 先红即中断，aarch64 腿的 rustdoc 状态**完全未知**——x86 修绿后 aarch64 可能新报一批（`dtb.rs` / `pl011_phys_base` 等显属 aarch64 侧）。
  - 方案：R-2/R-3 收口后跑完整 `make -f Makefile.ci ci-rustdoc` 双架构，并把 aarch64 独有问题按 ①②③ 归类续修；复验记录写进本报告式段落（不留状态字段）。
  - 状态：[ ]（依 R-2/R-3）

- **R-6 门禁归位（可选，需另行授权）**：rustdoc 要不要进 §2.3。
  - 描述：rustdoc 现为 `Makefile.ci` 的 `ci` 聚合成员但不在 §2.3 六门槛，导致「`make -f Makefile.ci ci` 失败」与「本轮 §2.3 全绿」两个陈述可同时成立而不矛盾——这是口径漏洞而非代码缺陷。
  - 方案：两种收口：把 rustdoc 列为 §2.3 第 7 条（需改 `AGENTS.md`，属文档保护条款，须用户授权），或在 §2.3 明确注明 rustdoc 属本地可选腿。本工程默认后者（改动面小），前者由用户裁定。
  - 状态：[ ]（依 R-1 结论）

## 3. 验收判据

1. `make -f Makefile.ci ci-rustdoc` 双架构全绿（x86_64 **与** aarch64 都真跑过，不是串联中断）。
2. 若 R-1 选乙：`RUSTDOCFLAGS` 加压后仍全绿，且 R-4 台账每一处 `pub` 提升都有 F8 中文 doc 注释。
3. `AGENTS.md` §2.2/§2.3 的门槛复跑不受影响：F5 双架构 0w0e、F7 中文注释、F8 clippy `missing_docs` 全部仍绿。
4. 无 `#[allow(dead_code)]` / `#[allow(rustdoc::…)]` 类压制（§5 F9 零容忍；靠豁免注释让 rustdoc 变绿即拒收）。
