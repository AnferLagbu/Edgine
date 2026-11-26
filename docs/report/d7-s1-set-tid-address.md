# D7 S1：`set_tid_address` 退出清理收口

> 日期：2026-11-26
> 状态：x86_64 + aarch64 已收口
> 范围：`set_tid_address` 登记、线程退出清零与 futex 唤醒、只读坏指针退出清理

## 1. 结论

S1 已在双架构真实启动中跑通。QEMU 串口新增：

```text
[TIDCLEAR] PASS: set_tid_address clear+wake, readonly target silent
```

`bash scripts/qemu_boot_test.sh x86_64` 与 `bash scripts/qemu_boot_test.sh aarch64`
均返回 `QEMU_RC=0`；`[TIDCLEAR]` 判据在两条架构分支都从“缺失即红”的门禁读取。

## 2. 根因

`set_tid_address` 入口本身按 Linux 语义只保存地址、不校验可写性；问题在退出清理：

```rust
fn write_u32_user(uaddr: u64, val: u32) {
    if !check_user_ptr(uaddr) { return; }
    unsafe { core::ptr::write_volatile(uaddr as *mut u32, val); }
}
```

`check_user_ptr` 只验证地址落在 `[1, USER_ADDR_MAX)`，不验证目标页已映射、可写。
把只读 `.text` 地址登记为 `clear_child_tid` 后，退出路径的裸 `write_volatile` 会在
内核态写只读页，触发写保护 `#PF`；旧实现还在写失败后无条件执行 `futex_wake`，
会误唤醒等待者。字段复位也在写/唤醒之后，退出清理重入时存在重复写窗口。

## 3. 实现

### 3.1 退出清理改为完整用户写判据

`src/kernel/privileged/proc/robust.rs`：

- 先 `clear_child_tid.store(0)` 摘登记，再写用户字，重入不会重复写；
- `write_u32_user` 返回 `bool`，内部走
  `crate::privileged::userptr::write_struct_to_user`：区间、映射、逐页可写、SMAP
  一次校验，坏指针/未映射/只读页直接返回 `false`，不产生用户内存访问；
- 只有写成功才 `futex_wake(clear_tid, 1)`；失败跳过唤醒，对齐 Linux `put_user`
  失败语义；
- 仍使用 `write_unaligned`，保留未对齐 `clear_child_tid` 的写能力。

### 3.2 用户库入口

`src/user/lib/src/sys.rs` 新增：

```rust
pub fn set_tid_address(tidptr: *mut u32) -> i64
```

编号复用 `edgine_abi::thread::SYS_SET_TID_ADDRESS`（x86_64 canonical 96 /
aarch64 218 经边界表翻译）；`tidptr == 0` 表示取消登记，入口不解引用。

### 3.3 init 集成探针

`src/user/init/src/main.rs` 在 `thread_probe()` 之后新增 `tidclear_probe()`：

- **好指针腿**：`clone` 一个共享地址空间的线程，不带 `CLONE_CHILD_CLEARTID`；
  线程调用 `set_tid_address(&TIDCLEAR_GOOD)`，把 `gettid()` 写入共享
  `AtomicU32` 并报到；父线程放行后，线程 `proc_exit(0)`。父线程用
  `futex_wait_until` 等 `TIDCLEAR_GOOD == 0`，同时验证“写 0 + 唤醒”而非轮询。
- **坏指针腿**：`fork` 子进程，把 `tidclear_probe` 的只读 `.text` 地址登记为
  `clear_child_tid`；`set_tid_address` 必须返回 TID，否则子进程退出码 3。
  父进程 `wait4` 必须收到正常退出码 0，证明退出清理走完且没有在内核态写只读页。

### 3.4 门禁

- `scripts/qemu_boot_test.sh`：x86_64 / aarch64 各加 `[TIDCLEAR] PASS:` 正断言；
- `scripts/initramfs_boot_test.sh`：载体腿加入同一标记（判据由七条改为八条）；
- `scripts/uefi_boot_test.sh`：UEFI 可达面加入 `need "[TIDCLEAR] PASS:"`；
- `scripts/thread_stress_test.sh`：每轮同时要求 `[THREAD] PASS` 与
  `[TIDCLEAR] PASS`；
- `host-tests/tests/ci_uefi_boot_leg_test.rs`：标记同源表加入 `[TIDCLEAR]`，
  正向判据计数 21 → 22；
- `host-tests/tests/thread_group_k06_test.rs`：源码契约改为“先摘登记再写、
  写成功才唤醒、写路径走 userptr 完整判据”。

## 4. 验证

- QEMU x86_64：`QEMU_RC=0`，串口 `[TIDCLEAR] PASS`；G0–G4 与 L1 保持绿；
- QEMU aarch64：`QEMU_RC=0`，串口 `[TIDCLEAR] PASS`；
- `make test-kernel-host`：`640 passed / 0 failed`；
- `./ci/build.sh all`：`Passed: 6 / Failed: 0`；
- `bash ci/audit.sh full`：`AUDIT_RC=0`，QEMU 双架构 `2/2`，未执行腿数 `0`；
- 内核 `_kernel_end_phys=0x3ff000`，K-08 上限 `0x400000`，余量 4 KiB。

证据路径：

- `/tmp/qemu_s1_x86_final.log`、`other/build/log/qemu_boot_x86_64.log`
- `/tmp/qemu_s1_a64_final.log`、`other/build/log/qemu_boot_aarch64.log`
- `/tmp/audit_s1.log`、`other/build/log/audit_uefi_boot.log`
- `/tmp/test_host_s1.log`
- `/tmp/build_all_s1.log`
- `/tmp/audit_s1.log`

## 5. 边界

- `set_tid_address` 入口仍不校验 `tidptr` 可写性，这是 Linux 语义；坏地址只在
  退出清理时静默跳过，不在 syscall 入口返回 `EFAULT`。
- 坏指针腿使用独立 `fork` 子进程（最后一条线程）；好指针腿使用线程组内存活的
  线程。当前 `exit_cleanup` 只要字段非 0 就尝试写，不实现 Linux `mm_release`
  的 `signal->live > 1` 跳过条件；最后一条线程退出时写自己的地址空间无害，但
  与 Linux 精确语义仍有差异。
- 未覆盖“线程组内登记只读坏指针且兄弟线程仍存活”的组合；该组合会走同一条
  `exit_cleanup` 写判据，风险面已由独立子进程腿与源码契约共同锁住。
- 未测量 futex 唤醒次数；“不重复写/不误唤醒”由“先摘登记 + 写成功才唤醒”
  源码契约和清零后字段为 0 的运行时读数保证，不是唤醒计数器。
- 未覆盖未对齐 `clear_child_tid`、`tidptr == 0` 取消登记、`CLONE_CHILD_CLEARTID`
  与 `set_tid_address` 交替覆盖同一字段的组合；写路径使用 `write_unaligned`，
  未对齐能力保留但无专项探针。
- `SYS_SET_TID_ADDRESS` 通过 canonical 编号表在 aarch64 翻译；本报告不新增
  aarch64 专属实现。
