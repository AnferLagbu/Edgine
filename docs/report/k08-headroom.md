# K-08 余量切片：klog 环减半

> 结论：默认 x86_64 镜像 `_kernel_end` 从 `0x400000` 降到 `0x3F0000`，K-08 余量从 0 恢复到 **64 KiB**。这次改的是 `.bss` 常驻项，不是文件体积；K-08 仍由启动期 fail-closed 兜底，决策 D 仍是根因级方案。

## 背景

D7 G1 收口时，x86_64 默认镜像 `_kernel_end_phys=0x400000`，恰好等于 `USER_CODE_BASE`，K-08 断言以 `<=` 恰好通过，余量 0。后续任何一个内核代码增长都会在启动期触发 fail-closed。

`kernel.map` 显示，`_kernel_end` 到 `0x400000` 时 `kernel.flat` 只有约 2.8 MiB：K-08 量的是含 `.bss` 的内存镜像，不是 ELF 文件大小。因此优先砍 BSS 常驻项，而不是 strip 符号。

## 剖分

x86_64 默认构建的 BSS 前五项：

| 符号 | 改前 | 改后 | 说明 |
|---|---:|---:|---|
| `klog::RING` | 130,840 B | 65,304 B | 全局日志环，默认容量从 128 KiB 降到 64 KiB |
| `...IDT_MANAGER` | 16,848 B | 16,848 B | 每 CPU IDT 管理面，未动 |
| `...GLOBAL_RAMFS` | 16,584 B | 16,584 B | 固定容量 ramfs，未动 |
| `...CGROUP_FS` | 13,712 B | 13,712 B | cgroup 文件系统状态，未动 |
| `...EGFS_DATA` | 12,312 B | 12,312 B | EGFS 全局数据，未动 |

`klog::RING` 是最大单项，减半即换回 65,536 B。`kernel.map` 的 `.bss` 收尾从 `0x400000` 退到 `0x3F0000`，正好是 64 KiB 余量；`_kernel_end_phys` 仍等于 `_kernel_text_lma + _kernel_size_lma`，没有改链接脚本或 `USER_CODE_BASE`。

## 变更

- [klog/mod.rs](../../src/kernel/privileged/klog/mod.rs#L535)：默认 `RING_SIZE` 从 `128 * 1024 - 256` 改为 `64 * 1024 - 256`，保留 256 B 尾部余量；模块文档和静态缓冲区注释同步改为 64 KiB。
- [ci/build.sh](../../ci/build.sh#L176)：新增默认 x86_64 镜像的 K-08 余量门禁：`_kernel_end_phys` 距 `USER_CODE_BASE=0x400000` 不足 32 KiB 即失败；`KERNEL_FEATURES` / `KERNEL_NO_DEFAULT` 自定义构建跳过，避免误杀合法诊断载体。
- [decision-d-kview-retirement.md](../plan/decision-d-kview-retirement.md)：决策 D 预研文档，拆分 D-1（只退役恒等窗口、不拆 kview）与 D-2（`copy_*_user` 也改直射区）；状态仍为 `[]`，未施工。

本轮没有引入新的 feature 维：K-08 门禁按默认产物执法，klog 容量保留为单常量回退点。若后续诊断确实需要 128 KiB 历史，再单独评估大环载体，不直接改常量绕过门禁。

## 验证

| 门槛 | 命令 | 结果 |
|---|---|---|
| 双架构构建 | `./ci/build.sh all` | `Passed: 7  Failed: 0`，RC=0；`K-08 headroom: _kernel_end_phys=0x3F0000, margin=0x10000 >= 32 KiB` |
| x86_64 QEMU | audit full 内 QEMU 腿 | 2/2 通过，FAIL_OK=0 |
| host tests | `make test-host` | 218 套 / 1676 passed / 0 failed，RC=0 |
| kernel host tests | `make test-kernel-host` | 3 套 / 1471 passed / 0 failed，RC=0 |
| 全量审计 | `bash ci/audit.sh full` | RC=0；未执行腿数 0；Lockbud 4/4；UEFI 可达面判据全符合；aarch64 COW 四判据 + EGFS 三相全符合；thread-stress 6/6 |

aarch64 侧同步构建通过。K-08 断言只对 x86_64 编译，aarch64 的 `_kernel_end` 不受 4 MiB 约束；klog 环减半同样降低了 aarch64 的 `.bss` 占用。

## 边界

- 64 KiB 是**止血余量**，不是把 K-08 变成可调预算。K-08 的硬边界仍是 `kernel_end <= USER_CODE_BASE`，真正的根因消除仍归决策 D。
- 余量门禁只检查默认 x86_64 出货构建；`KERNEL_FEATURES` / `KERNEL_NO_DEFAULT` 自定义构建合法地跳过。门禁里的 `USER_CODE_BASE` 与 [config/memory.rs](../../src/kernel/privileged/config/memory.rs#L59) 同值，改基址必须同步两处。
- `klog::RING` 当前只有写入路径，没有读取消费者；容量减半只影响历史保留上限，不影响现有日志输出。若未来接 `/proc/klog` 或崩溃转储，需重新评估 64 KiB 是否够用。
- 决策 D 预研建议先评审 **D-1**：kview 保留用户镜像，只退役 `[0,boundary)` 的 boot 恒等窗口，`user_va_lower_bound` 回到 `USER_CODE_BASE`。D-2 的 `copy_*_user` 直射区改造涉及 PA 生命周期竞态，不能和 D-1 混成一个批次。
