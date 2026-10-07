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

P4 的 e2e 只覆盖了 `TCP_NODELAY`（两侧都 `=1`，恰好一致），未触达上述漂移，故该预存问题被 `// SIMPLIFIED` 注释就地标注而未修（见 [sys.rs](../../src/user/lib/src/sys.rs) D8/D8b 常量段注释）。

---

## 目标

- userlib 与内核两侧 socket 选项常量对齐到**单一权威值域**（x86_64 `asm-generic/socket.h` + `netinet/tcp.h`）。
- 消除"两处各写各的、改一处忘另一处"的结构性漂移根源。
- 用契约测试锁定"同一常量名两侧数值相等"，纳入 CI 门禁。

---

## 待决议题（实施前需用户裁定）

- **统一机制二选一**（决策灰色地带，依 AGENTS.md §9.1/§12.1 不擅自选定）：
  - **方案 A（共享常量源）**：新增一个 `no_std` 常量模块/crate，userlib 与内核 `sm_fi.rs` 各自 `pub use` 同一来源。彻底消除重复定义，但跨 privileged/functions/userlib 三层引入共享依赖，需核对 F3 循环依赖与 F2 边界（userlib 属用户态、不依赖内核内部模块）。
  - **方案 B（对齐 + 契约测试）**：两侧仍各写常量，但值统一到 asm-generic，并加 host-tests 契约测试断言"两侧同名常量数值一致"（借 `#[cfg(test)]` 或编译期常量比对）。改动面小、无新依赖，但不消除重复定义本身。
- **`SO_BROADCAST` 是否本轮实装内核侧**：仅统一常量值，还是连带在内核 `sm_setsockopt` 增 `SO_BROADCAST` 分支（UDP 广播开关语义，可能牵连 D11 组播）。

---

## 任务条目

- **S1: 常量值域对齐**
  - 描述：userlib `SO_KEEPALIVE` 由 `8` 改 `9`、`SO_BROADCAST` 由 `32` 改 `6`（x86_64 asm-generic）；核对补齐 `SO_TYPE`/`SO_ERROR`/`SO_REUSEPORT` 等内核已用而 userlib 缺项（若 e2e/用户态程序需要）。
  - 方案：以权威值域为准逐一修正；移除 P4 遗留的 `// SIMPLIFIED` 偏差标注。
  - 状态：[]
  - 详情：依赖"统一机制"待决议题（方案 A vs B）的用户裁定。

- **S2: 防漂移契约测试**
  - 描述：锁定"同一 socket 选项常量名，userlib 与内核数值相等"，防止再次漂移。
  - 方案：host-tests 增加契约测试（或编译期 `const _: () = assert!(...)`）比对两侧常量；纳入 `ci/audit.sh` 或 `make test-host`。
  - 状态：[]
  - 详情：与 S1 同批落地；测试口径"测用户可见行为"（AGENTS.md §8）——即经 setsockopt 传该常量应被内核识别（非 `ENOPROTOOPT`）。

---

## 验证门槛

实施完成后须满足 AGENTS.md §2.3 全部 6 条（双架构 0w0e / clippy+fmt / 核心审计 / host-tests / QEMU / kernel-host 单测），并新增：

- S2 契约测试在 host-tests 与 `make test-kernel-host` 全绿。
- 经 userlib `setsockopt(SO_KEEPALIVE)` / `getsockopt(SO_TYPE)` 的 QEMU e2e 里程碑（若择机扩展探针）。

---

## 关联文档

- [socket-layer-completion.md](./socket-layer-completion.md) — P4 D8 sockopt 精简集（本漂移的发现现场，`SO_KEEPALIVE` 内核侧已用 `9`）
- [src/user/lib/src/sys.rs](../../src/user/lib/src/sys.rs) — 用户态 socket 常量（`SO_KEEPALIVE=8` / `SO_BROADCAST=32` 漂移点）
- [src/kernel/privileged/net/init/sm_fi.rs](../../src/kernel/privileged/net/init/sm_fi.rs) — 内核 `setsockopt`/`getsockopt` 分派常量（asm-generic 值域）
