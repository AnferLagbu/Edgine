# mm M4：wasmi 双架构回归与用户 VA 断言收口

> 任务：`mm-lifecycle-project.md` M4 — 把「生产 ELF 装载必须安装 mm、mmap 不得回退裸页」钉成不可静默复发的判据。
> 结论：**M4 已收口**。双架构 wasmi 真实构建 + 真实启动通过，`[WASMI] exit=0`，mmap 返回值同时满足用户 VA 下界与 mmap 区下界；host 契约锁与 QEMU 负控制均实测转红后复原。

## 1. 判据结构

M4 不是新增功能，而是把 M2/M3 的性质做成三层判据：

| 层 | 判据 | 位置 | 负控制 |
|---|---|---|---|
| host 源码形态 | 装载路径必须 `Box::new(MmStruct::new())`、`store_mm`、`start_brk`，PT_LOAD 必须 `insert_vma` + `NX` | `host-tests/tests/user_proc_frame_release_test.rs` 的 `elf_load_installs_mm_and_vma_per_pt_load` | 摘掉 `store_mm` 后该例 RC=101 |
| host 失败语义 | `mmap` 入口无 mm 一律 `ENOMEM`，不得出现裸页回退 | `host-tests/tests/mmap_no_mm_fail_closed_test.rs` 的 `mmap_entry_has_no_raw_page_fallback` | 源码形状锁 |
| QEMU 真跑 | wasmi 用户态 stdio + `exit=0` + mmap VA 断言 | `scripts/wasmi_boot_test.sh`，已接 `ci/audit.sh` full | 摘掉 `store_mm` 后双架构腿转红 |

host 两例随 M2/M3 的 `eacd51bb` 已在树内；本轮补的是第三层真跑判据与 audit 接线。

## 2. QEMU 判据

`wasmi_boot_test.sh` 在 `KERNEL_FEATURES=wasm_userspace` 下双架构构建并启动，要求：

- `hello from wasmi`；
- `[WASMI] exit=0`；
- `[MM] user_va_lower_bound=0x...` 与 `[WASMI] mmap=0x...` 同时在日志中；
- mmap 返回值 `>= user_va_lower_bound`，且 `>= user_mmap_lower_bound`。

最后一条是本轮复核后补的。只判 `>= user_va_lower_bound` 不够：旧裸页回退返回的物理地址（如 `0x1F800000`）本身就大于 `0x400000`，下界判据会假绿。`user_mmap_lower_bound = USER_MMAP_BASE - (1 << ASLR_MMAP_BITS) * PAGE_SIZE = 0x7FFF_F6F0_0000`，把物理页段排除在外，才真正能把「又返物理地址」在日志层判红。

实测读数：

| 架构 | mmap 返回值 | `user_va_lower_bound` | `user_mmap_lower_bound` | 结果 |
|---|---:|---:|---:|---|
| x86_64 | `0x7FFFF6FE6000` | `0x400000` | `0x7FFFF6F00000` | PASS |
| aarch64 | `0x7FFFF6FBC000` | `0x400000` | `0x7FFFF6F00000` | PASS |

双架构同时出现 `hello from wasmi` 与 `[WASMI] exit=0`。wasmi 专属腿默认超时由 40s 提到 90s：x86 SMP=2 在 40s 下曾偶发未出 hello，SMP=1 与 120s 均可过；现象是慢而非永久挂死，根因未完全定因，先以 90s 作为工程口径，并在报告边界中保留。

另有一条独立的 QEMU 门禁超时：`qemu_boot_test.sh` 的 `TIMEOUT_QEMU` 由 40s 提到 60s。40s 下 x86_64 主启动的串口会停在 `[dhcpv6-policy] PASS` 之前，`qemu_boot_test.sh` 因此按 fail-closed 判 `RESULT=1`，但汇总仍打印 `2/2 通过`（另一架构与 P-62 feature 腿都过），容易误读成 e2e 抖动。实测 60s 下 `[dhcpv6-policy] PASS` 出现，`qemu_boot_test.sh x86_64` RC=0。该超时与 wasmi 的 90s 是两个独立口径，不能合并。

## 3. 负控制

### 3.1 host 契约

临时删除 `src/kernel/privileged/proc/user_proc.rs` 中装载路径的：

`proc_ref.store_mm(alloc::boxed::Box::into_raw(mm));`

运行 `cargo test --test user_proc_frame_release_test elf_load_installs_mm_and_vma_per_pt_load`，RC=101：

`M2: 装载路径必须创建 MmStruct、初始化 brk、并挂到新进程`

复原后该例恢复通过。

### 3.2 QEMU 真跑

同一删除状态下执行 `WASMI_BOOT_TIMEOUT=90 ./scripts/wasmi_boot_test.sh x86_64`，RC=1；串口只有：

`[MM] user_va_lower_bound=0x400000`
`[WASMI] exit=256`

没有 `hello from wasmi`。脚本按「未出现 wasmi stdio 输出」判红。复原源码后重新构建并复跑双架构，恢复 `hello from wasmi` + `exit=0` + VA 断言全绿。

两条负控制证明判据不是恒真：host 形状锁能拦住「摘安装点」，QEMU 腿能拦住「无 mm 的用户态运行」。

## 4. audit 接线

`ci/audit.sh` full 档在 initramfs 子腿之后、lockdep feature 子腿之前新增 wasmi 子腿：

- 证据：`other/build/log/audit_wasmi_boot.log`；
- 判据：`hello from wasmi` + `[WASMI] exit=0` + mmap 双下界断言；
- 脚本自行切换 feature 并在判定后复原默认构建，避免与后续 lockdep feature 腿产物互相污染。

接线后 `bash ci/audit.sh full` RC=0：审计腿 17/17、P-49 存量腿 11/11、QEMU 2/2、thread-stress 3 轮 × 2 架构（6/6 轮次）、Lockbud 4/4、静态锁序取锁点 1327 / 函数 10672 / 违规 0、未执行腿 0；`✓ wasmi 用户态 stdio + mmap 用户 VA 断言: 双架构通过`。

首轮 full 的 x86 主启动在 40s 处被 `TIMEOUT_QEMU` 截断，缺 `[dhcpv6-policy] PASS`，整轮 RC=1（QEMU 汇总仍 2/2、feature 4/4）。独立复跑 `qemu_boot_test.sh x86_64` 后定位到该缺行，把 `TIMEOUT_QEMU` 由 40s 提到 60s 并复跑 RC=0；随后 full 复跑 RC=0。此前把它记作 P-80 e2e 抖动是误归因：e2e 本身在 40s 轮已 3/3 回显通过，红的是 DHCPv6 策略硬门禁。

## 5. 实现过程与已排除路线

最初尝试在 `wasmrun` 内、`load_module` 之前插一次 `mmap` 探针，x86 在用户堆 allocator free-list 处报：

`vec=14 err=0x4 rip=0x400ACD cr2=0xFFFF800000100000`

KPTI 拒绝高半区访问；aarch64 可过。把探针移到 `load_module` 之后，x86 仍偶发同一 allocator 崩溃/停住。说明额外 `mmap` 会扰动 wasmrun 首次堆增长，不适合作为探针。最终改为内核侧日志：

- `src/kernel/lib.rs` 在 `wasm_userspace` feature 下打印 `[MM] user_va_lower_bound` 与 `[MM] user_mmap_lower_bound`；
- `src/kernel/functions/mm/mmap.rs` 在 feature 下对每次成功 mmap 打印 `[WASMI] mmap`；
- `wasmrun` 恢复原样，不再插入探针。

## 6. 七门槛读数

| 门槛 | 结果 |
|---|---|
| `./ci/build.sh all` | `Passed: 7 Failed: 0`；K-08 `_kernel_end_phys=0x3F0000`, margin `0x10000`（64 KiB） |
| `make test-host` | 1676 passed / 0 failed |
| `make test-kernel-host` | 1481 passed / 0 failed |
| `make -f Makefile.ci ci-rustdoc` | x86_64 / aarch64 均 exit 0，0 link 错误 |
| `bash ci/audit.sh full` | RC=0；17/17 + 11/11、QEMU 2/2、feature 4/4、thread 6/6、Lockbud 4/4、wasmi 双腿、未执行 0 |
| `wasmi_boot_test.sh all` | 双架构 `hello from wasmi` + `[WASMI] exit=0` + VA 双下界断言 |
| 负控制 | host RC=101；QEMU RC=1 且 `[WASMI] exit=256`；复原后全绿 |

## 7. 边界与遗留

- 本轮的 mmap VA 断言只覆盖 wasmi 腿实际走到的 mmap 路径，不是所有 mmap 调用的全域断言；默认镜像不打印这两行。
- `MmStruct` 目前仍无统一释放路径（`Process::mm` 字段注释已标明），fork 子进程按指针继承、exec 转移所有权、临时进程清空槽位；退出释放属于 M5 收口项，不在 M4 判据内。
- x86 SMP=2 的 40s 慢启动未定根因；90s 为当前默认，若后续复现永久挂死需另立问题。
- `TIMEOUT_QEMU` 40→60 只解决 x86 主启动被截断；若后续串口继续变长，仍应继续按实测余量调，不要用 `FAIL_OK=1` 放宽硬门禁。
