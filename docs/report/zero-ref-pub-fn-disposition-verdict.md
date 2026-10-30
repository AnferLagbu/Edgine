# 零引用 pub fn 存量处置裁定（H-2 丙）

> 442 项零引用 `pub fn` 经核查是「已实现、未接线」的真实 API 面，不是可批量删除或降可见性的垃圾代码；丙 路线依赖的两个机制前提经实测均不成立，裁定整体维持，INFO 作为稳定的接线积压读数。

## 读数基线

`python3 scripts/audit_unwired_pub_fn.py --strict` 当前读数：CRITICAL=0 / HIGH=0 / WARN=0 / **INFO=442**。脚本扫描 603 个文件、5,861 个 `pub fn` 声明（其中 362 个落入脚本承认的豁免面：`#[no_mangle]` / 顶层 re-export / trait impl），589 个 `pub mod`、152 个核心 `pub struct/enum`、32 个 syscall 编号（全部已分发）。

台账 [B-6](../plan/syscall-followup.md) 的机器可读区块为 **442 行**，与脚本逐项匹配、stale=0——上一轮的 H-2 甲 对账（465 → 442，清掉 23 条失效条目）已把「脚本读数」与「台账」拉齐，本报告以该基线为准。

目录分布（前 10）：

| 目录 | 项 | 目录 | 项 |
|---|---:|---|---:|
| `functions/fs` | 83 | `privileged/mm` | 20 |
| `functions/driver` | 75 | `privileged/freg` | 14 |
| `privileged/arch` | 51 | `privileged/sync` | 13 |
| `privileged/proc` | 30 | `functions/ipc` | 12 |
| `privileged/driver` | 23 | `privileged/idt` / `privileged/pci` | 11 / 11 |

## 丙 路线的两个机制前提均不成立

### 降可见性不会降低 INFO

R1 的声明收集正则见 [audit_unwired_pub_fn.py:221](../../scripts/audit_unwired_pub_fn.py)：

```python
r"^\s*pub(?:\([^)]+\))?\s+(?:unsafe\s+)?..."
```

`pub(?:\([^)]+\))?` 同时匹配裸 `pub` 与 `pub(crate)` / `pub(super)`，因此把函数降为 `pub(crate)` 后它仍会进入 R1 计数，INFO 不下降。台账键格式（[audit_unwired_pub_fn.py:83](../../scripts/audit_unwired_pub_fn.py) 的 `CLASSIFIED_KEY_RE`）只有 `<path>::<name>`，也不存在「可见性变化即退出分类集」的通路。

### 零引用 `pub(crate)` 无法通过 0-warning 门禁

用独立 scratch crate（`#![no_std]`、edition 2024）做最小探针：

```rust
pub mod api {
    pub(crate) fn internal_unused() {}
    pub fn public_unused() {}
}
```

`cargo check --release` 只对 `internal_unused` 报 `warning: function 'internal_unused' is never used`，裸 `pub fn` 无告警。原因是 rustc 的 `dead_code` lint 不判可导出项，而 `pub(crate)` 对编译器是 crate 内私有项。

推论：442 项全部零引用，一旦降为 `pub(crate)` 就必然触发 `dead_code`，与 AGENTS §2.3 的 0 warning 门禁冲突；F9 又禁止用 `#[allow(dead_code)]` 压制。降可见性因此不是「更保守的清理」，而是**构建不可通过**。这也是 442 项能长期存活的机制：裸 `pub fn` 对 `dead_code` 不可见，只有 R1 这类按引用计数的审计面能看见它们。

附带约束：`pub use` 一个 `pub(crate)` 项是 E0365，丙 ② 类「纯内部实现但被 `pub use` 暴露 ⇒ 降 `pub(crate)`」在代码结构上同样不成立。

## 存量性质核查

- 442 项全部是裸 `pub fn` 声明（逐行核对，`pub(crate)` 声明 0 处），函数名互不重复，因此不存在「同名遮蔽」造成的成组误判。
- 用「空实现 / `todo!` / `unimplemented!`」启发式筛出 13 项候选，逐项读源后 **12 项是真实实现**（内联汇编寄存器原语、klog 诊断、注册表遍历等），唯一真空体为 [functions/fs/sysfs.rs:185](../../src/kernel/functions/fs/sysfs.rs) 的 `umount_sysfs`。
- 抽样形态集中在平台能力面：xhci / apic / ioapic / acpi / uart / timer 的寄存器读写与诊断，`functions/fs` 各文件系统的 mount / umount 与元数据操作，freg / sync 的机制钩子。它们与 T5 台账里「未来功能 / 完整性保留」同性质——实现完整、只差接线。

## 裁定

**整体维持 442 项**：本轮不删除、不改可见性、不改台账区块（仍 442 行）。INFO 定位为**稳定的接线积压**，而不是必须清零的债务指标。

- 不删除的理由：① 没有门禁因此转红，442 是分类后的 INFO；② 删除会命中「将来要用」的 API（交接文档已点名 T1/T2 的 readv / writev / statx 等 syscall 面），删掉后要在接线时写回；③ 逐项删除的判据涉及 ABI 面、反射式调用与计划引用，启发式不可靠，人工逐项裁定的成本与收益不匹配。
- 不登记逐项去向的理由：B-6 区块是机器可读格式且 fail-closed（行格式非法即全量回落 HIGH），加注会破坏解析；给 442 项逐条写「保留理由」等于把整体裁定拆成 442 次人工裁定，收益不足。若后续确需「每项都有去向」，应单独立项，并同时扩台账/脚本格式（属门禁改动，需单独授权）。

## 覆盖面边界

- 只判**仓内**引用：`extern "C"`、`#[no_mangle]`、反射式函数指针、宏生成调用全部不可见；本轮的豁免面为 362 项。442 里不能排除仍混有 ABI 面，删除前必须逐条人工确认。
- 只数 `pub fn`，不数 `pub struct/enum/const`（后者归 R4，本轮 0 项）。
- 不做反向对账：被接线或删除的条目不会自动退出台账，需要人工维护（H-2 甲 的 23 条失效即此成因）。
- 本报告只主张「当前证据不足以支持批量删除或降可见性」，不主张 442 项将来一定都会被使用。

## 复现

```bash
python3 scripts/audit_unwired_pub_fn.py --strict
```

`dead_code` 探针：新建 `no_std` crate，写入上文两个函数，执行 `cargo check --release`，观察只有 `pub(crate)` 版本产生 `never used` 告警。
