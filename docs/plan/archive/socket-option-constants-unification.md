# Socket 选项常量统一（userlib ↔ 内核值域治理）

> 在 P4 socket 层语义完善（[socket-layer-completion.md](./socket-layer-completion.md) D8/G9）中发现 **userlib 与内核 privileged 两侧的 socket 选项常量值域不一致**。本计划独立立项治理该跨层常量漂移，属预存问题（非 P4 引入），登记为待授权工程。

---

## 背景

Edgine 的 `setsockopt`/`getsockopt` 系统调用要求**用户态传入的 optname 数值**与**内核 `sm_fi.rs` 匹配的常量数值**逐一吻合（内核按 `level`+`optname` 整数分派，无符号名映射层）。当前两侧常量各写各的，已出现值域漂移：

| 常量 | userlib `src/user/lib/src/sys.rs` | 内核 `privileged/net/init/sm_fi.rs` | Linux x86_64 asm-generic |
|---|---|---|---|
| `SO_REUSEADDR` | 2 | 2 | 2 ✅ |
| `SO_KEEPALIVE` | **8** | 9 | 9 |
| `SO_BROADCAST` | **32** | （未实装） | 6 |
| `SO_TYPE` | （未定义） | 3 | 3 |
| `SO_ERROR` | （未定义） | 4 | 4 |
| `SO_REUSEPORT` | （未定义） | 15 | 15 |
| `TCP_NODELAY` | 1 | 1 | 1 ✅ |

- **`SO_KEEPALIVE` 漂移**：userlib 传 `8`，内核只认 `9` → 经 userlib `setsockopt(SO_KEEPALIVE)` 落入内核未匹配分支得 `-ENOPROTOOPT`，用户永远打不开保活。
- **`SO_BROADCAST` 漂移**：userlib `32` 既非内核值，也非 x86_64 asm-generic 的 `6`；内核当前未实装 `SO_BROADCAST`（组播报文可发性由 D11/P6 组播工程连带评估）。
- userlib 的 `8`/`32` 疑似取自非 x86 架构（alpha/mips 系 `SO_KEEPALIVE=8`）或历史笔误，与本项目主目标架构（x86_64）的 asm-generic 值域不符。

P4 的 e2e 只覆盖了 `TCP_NODELAY`（两侧都 `=1`，恰好一致），未触达上述漂移，故该预存问题被 `// SIMPLIFIED` 注释就地标注而未修（见 [sys.rs](../../../src/user/lib/src/sys.rs) D8/D8b 常量段注释）。

---

## 目标

- userlib 与内核两侧 socket 选项常量对齐到**单一权威值域**（x86_64 `asm-generic/socket.h` + `netinet/tcp.h`）。
- 消除"两处各写各的、改一处忘另一处"的结构性漂移根源。
- 用契约测试锁定"同一常量名两侧数值相等"，纳入 CI 门禁。

---

## 待决议题（已裁定 2026-10-19）

- **统一机制** → **裁定方案 A（共享常量源）**：新建 `edgine-abi` 纯常量 crate（`src/rust/edgine-abi/`，no_std、零依赖），内核 `sm_fi.rs` 与 userlib `sys.rs` 各自引用同一来源。选定理由：用户裁定采长远视角（§12.1.1）——结构上消灭双写，并为后续 ABI 常量族（errno/poll/ioctl）扩张预留落点；已调研确认 socket 选项常量定义全仓仅 sm_fi + userlib 两处，接入面可控（F2/F3 无冲突：纯常量 crate 不属 privileged 内部模块，依赖方向 kernel/user → abi 单向无环）。
- **`SO_BROADCAST` 内核侧** → **裁定“接受但忽略”**：userlib 值 32→6 对齐 asm-generic；内核 `sm_setsockopt` 将 `SO_BROADCAST` 并入 SO_REUSEADDR/REUSEPORT 既有“接受但忽略”先例分支（前向兼容用户程序）；广播闸门真语义待广播消费者出现时单开工程（牵连 D11 组播，当前零消费者）。getsockopt 同样不新增分支（与 REUSEADDR 先例一致）。

---

## 任务条目

- **S1: 常量值域对齐（方案 A 共享 crate）**
  - 描述：新建 `edgine-abi` crate（`pub mod socket`，asm-generic x86_64 值域）；内核 `sm_fi.rs` D8/D8c/D11 常量块与 userlib `sys.rs`  socket 选项段均改为引用单一源；userlib `SO_KEEPALIVE` 8→9、`SO_BROADCAST` 32→6 随迁移自然修正；移除 P4 遗留 `// SIMPLIFIED` 偏差标注。
  - 方案：level（SOL_SOCKET/IPPROTO_IP/TCP/UDP/IPV6）+ 选项（SO_REUSEADDR/TYPE/ERROR/BROADCAST/KEEPALIVE/REUSEPORT/PASSCRED/RCVTIMEO/SNDTIMEO + TCP_NODELAY + IP/IPV6 ADD/DROP_MEMBERSHIP）入 abi；内核内部件（SOCK_STREAM/DGRAM 回值、MSG_DONTWAIT、IP_MREQ_LEN、E_* errno）与 userlib 非选项族（AF_*/POLL*）不入本轮范围；`sm_setsockopt` 接受忽略分支补 SO_BROADCAST。
  - 状态：[X]
  - 详情：依赖裁定已闭合（方案 A）；edgine-tests 中另有一套镜像定义（SOCK_STREAM/SOL_SOCKET 等），属 host 测试封装层既有形态，本轮不动（外科手术原则），登记为后续可收敛项。实装实录（2026-10-19）：新建 `src/rust/edgine-abi`（独立 package，no_std + deny(missing_docs)，`pub mod socket` 19 常量）；`sm_fi.rs` D8/D8c/D11 选项常量块删除改 `use edgine_abi::socket`（SOCK_STREAM/DGRAM、MSG_DONTWAIT、IPV6_MREQ_LEN、E_* 等内核内部件保留就地）；userlib `sys.rs` socket 选项族改 `pub use edgine_abi::socket`（补齐 SO_TYPE/ERROR/REUSEPORT/PASSCRED/RCVTIMEO/SNDTIMEO 缺项，IPPROTO_RAW/AF_*/SOCK_*/POLL* 留本处）；`sm_setsockopt` 接受忽略分支增 SO_BROADCAST（getsockopt 不新增，与 REUSEADDR 先例一致）；三处 Cargo.lock 随之更新。验证：双架构 0w0e / ci-audit（含 fmt 修正后 import 重排）/ kernel-host 986/0 / host-tests 138 套件 0 failed / QEMU x86_64 e2e 全里程碑过。

- **S2: 防漂移契约测试**
  - 描述：方案 A 下单源已天然消除“两侧不等”风险，契约测试改为钉住“abi crate 常量值 == x86_64 asm-generic 预期值表”（锁住值域本身正确，而非两侧自洽）。
  - 方案：host-tests 新增 `abi_socket_constants_test.rs`（path 依赖 edgine-abi，内嵌预期值表逐项 assert）；纳入 `make test-host`。
  - 状态：[X]
  - 详情：与 S1 同批落地；测试口径“测用户可见行为”（AGENTS.md §8）——即经 setsockopt 传该常量应被内核识别（非 `ENOPROTOOPT`）。实装实录：新增 `host-tests/tests/abi_socket_constants_test.rs`（编译期直引 edgine-abi，asm-generic 预期表逐常量 assert_eq，含 IPV6_ADD_MEMBERSHIP=SO_RCVTIMEO=20 靠 level 区分的留档断言）；既有文本断言型测试 `socket_multicast_membership_test.rs` 两项同步改造为“引用单源 + 旧就地定义不得复活”断言，避免双套锁位维护负担。

---

## 验证门槛

实施完成后须满足 AGENTS.md §2.3 全部 6 条（双架构 0w0e / clippy+fmt / 核心审计 / host-tests / QEMU / kernel-host 单测），并新增：

- S2 契约测试在 host-tests 与 `make test-kernel-host` 全绿。
- 经 userlib `setsockopt(SO_KEEPALIVE)` / `getsockopt(SO_TYPE)` 的 QEMU e2e 里程碑（若择机扩展探针）。

---

## 关联文档

- [socket-layer-completion.md](./socket-layer-completion.md) — P4 D8 sockopt 精简集（本漂移的发现现场，`SO_KEEPALIVE` 内核侧已用 `9`）
- [src/user/lib/src/sys.rs](../../../src/user/lib/src/sys.rs) — 用户态 socket 常量（`SO_KEEPALIVE=8` / `SO_BROADCAST=32` 漂移点）
- [src/kernel/privileged/net/init/sm_fi.rs](../../../src/kernel/privileged/net/init/sm_fi.rs) — 内核 `setsockopt`/`getsockopt` 分派常量（asm-generic 值域）
