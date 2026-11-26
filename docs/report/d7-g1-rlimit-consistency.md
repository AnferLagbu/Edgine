# D7 G1：`getrlimit` 与 `prlimit64` 口径统一

> 日期：2026-11-26
> 状态：x86_64 已收口
> 范围：`getrlimit` 读取 per-process rlimit 表、与 `prlimit64` 同源、glibc G2 集成腿

## 1. 结论

`getrlimit` 已从“恒返回 `RLIM_INFINITY`”改为读取当前进程的 `rlimit_table`，
与 `prlimit64(pid=0, ...)` 使用同一张表和同一个访问 helper。真实 glibc G2 探针
逐项比较 16 个资源的两个入口读数，QEMU 串口：

```text
[G2] getrlimit/prlimit64 resources=16 consistent nofile=32/18446744073709551615
[G2] GETRLIMIT PASS
```

`bash scripts/qemu_boot_test.sh x86_64` 返回 `QEMU_RC=0`。

## 2. 根因

`getrlimit_syscall` 原先忽略 `resource`，固定写回：

```rust
let r = Rlimit { rlim_cur: u64::MAX, rlim_max: u64::MAX };
```

而 `prlimit64_syscall` 已经读取 `Process::rlimit_table`。两者因此分叉：

- `RLIMIT_NOFILE` 表默认 32 / infinity，`getrlimit` 却报 infinity / infinity；
- `RLIMIT_STACK` 表默认 8 MiB / infinity，`getrlimit` 同样报 infinity；
- 依赖 `getrlimit` 的程序会误判可打开文件数、栈上限等资源；
- `prlimit64` 设置后的值，`getrlimit` 读不回来，形成两个 syscall 的口径矛盾。

## 3. 实现

### 3.1 单源表访问 helper

`src/kernel/functions/proc/sysinfo.rs` 新增：

```rust
fn rlimit_for_process(pid: u32, resource: usize) -> Result<Rlimit, Errno>
```

- 资源号越界 → `EINVAL`；
- `pid == 0` 或目标不在进程表 → `ESRCH`；
- 从 `p.rlimit_table.lock().get(resource)` 读取唯一权威条目。

`getrlimit` 与 `prlimit64` 都经该 helper 取 `old`，不再各自实现表读取。

### 3.2 `getrlimit` 读当前进程表

- 保留既有入口错误口径：`rlim_ptr == 0` → `EINVAL`；
- 先 `validate_user_buf(rlim_ptr, 16)`，非法范围直接 `EFAULT`，不触碰进程表；
- 资源号转换为 `usize` 后由 helper 统一校验；
- 当前 PID 由 `process_get_current_pid()` 取，写入 `[cur, max]` 两个 u64。

### 3.3 `prlimit64` 复用 helper

`prlimit64` 原先内联的 `process_with(...).flatten()` 读取删除，改调
`rlimit_for_process(target, index)`；权限判定、`new_ptr`/`old_ptr` 校验与设置
逻辑保持不变。

## 4. 集成腿

`scripts/fixtures/dynamic-glibc-x86_64/glibc_g2.c` 在原有 `prlimit64` 设置后新增：

1. 对 `res = 0..15` 逐个调用 `SYS_prlimit64(0, res, NULL, &via_pr)`；
2. 用 `SYS_getrlimit(res, &via_get)` 读同一资源；
3. 比较 `cur/max`，任意一项不一致即打印 `[G2] rlimit mismatch` 并失败；
4. 记录 `RLIMIT_NOFILE` 读数，打印 `[G2] GETRLIMIT PASS`。

这直接走内核 `SYS_getrlimit`（不是 glibc `getrlimit` 包装），避免被 glibc 内部
转调 `prlimit64` 掩盖。QEMU 门禁新增 `[G2] GETRLIMIT PASS` 正断言；既有
`[G2].*FAIL` 反断言继续覆盖失败分支。

## 5. 验证

- QEMU x86_64：`QEMU_RC=0`；串口
  `[G2] getrlimit/prlimit64 resources=16 consistent nofile=32/18446744073709551615`
  与 `[G2] GETRLIMIT PASS`；
- `make test-kernel-host`：`0 failed`；kernel-functions `819 passed`（新增
  `test_rlimit_for_process_reads_table`）、kernel `640 passed`、
  kernel-privileged `12 passed`；
- `./ci/build.sh all`：`Passed: 6 / Failed: 0`；
- `bash ci/audit.sh full`：`AUDIT_RC=0`，QEMU 双架构 `2/2`，未执行腿数 `0`；
- 内核 `_kernel_end_phys=0x400000`，K-08 断言为 `<= USER_CODE_BASE=0x400000`，
  本次恰好等于上限，余量 0；后续任何内核增长都会触发启动期 fail-closed，
  下一轮需先做体积收敛。

证据路径：

- `/tmp/qemu_g1_x86.log`、`other/build/log/qemu_boot_x86_64.log`
- `/tmp/test_host_g1.log`
- `/tmp/build_all_g1.log`
- `/tmp/audit_g1.log`

## 6. 边界

- `getrlimit` 的 `rlim_ptr == 0` 仍返回 `EINVAL`，沿用本内核既有测试口径；
  未改为 Linux `EFAULT`，这是独立错误码收敛项，不在 G1 范围。
- `getrlimit` 的非法指针先用 `validate_user_buf` 做范围校验；真正写入仍由
  `write_struct_to_user` 的映射/可写/SMAP 判据兜底，两个失败面都可能返回
  `EFAULT`。
- `prlimit64` 的跨进程权限仍按既有口径：非 init 改其他 pid 返回 `EPERM`；
  G1 不改权限模型。
- 资源号 0..15 的默认值来自 `RlimitTable::new()`；G1 不新增资源类型，不改
  `RLIMIT_NLIMITS=16`。
- 集成腿只在 x86_64 真实 glibc 上跑；aarch64 无 glibc 载体，仅由 QEMU 回归腿
  证明内核构建未回归，不冒充 `GETRLIMIT` 双架构读数。
- `glibc_g2` 的二进制 hash 已随源码更新，README/SHA256SUMS 同步；大小仍为
  11,200 B。
