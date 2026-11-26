# D7 R1：`rseq` 注册/退出语义收口报告

日期：2026-11-26
分支：`main`
范围：`D7-DEFERRED-R1`（`rseq` 注册/注销、`rseq_cs` 信号/退出清理、pthread 并发腿、ABI 映射）

## 1. 结论

`rseq` 已从“未映射、恒 `-ENOSYS`”收口为 Linux legacy 32 字节 ABI 的可用实现：

- syscall canonical `SYS_RSEQ = 334`，aarch64 公开号 `293`，ABI 层双向映射；
- `Process` 持有 per-thread `rseq_ptr/rseq_len/rseq_sig` 原子状态；
- 注册/注销按 Linux 语义返回 `EBUSY`/`EINVAL`/`EPERM`/`EFAULT`；
- 返回用户态入口刷新 `cpu_id_start`/`cpu_id`，信号投递前清 `rseq_cs`；
- 线程退出清 `rseq_cs` 后摘注册，exec 直接摘注册；
- x86_64 glibc G2 与 pthread 腿串口验证通过，`qemu_boot_test.sh` 双架构 RC=0。

## 2. ABI 与状态

### 2.1 编号

| 侧 | 编号 | 位置 |
|---|---:|---|
| x86_64 canonical | `334` | `src/kernel/privileged/syscall/types.rs::SYS_RSEQ` |
| aarch64 公开号 | `293` | `src/rust/edgine-abi/src/linux_syscall.rs` |
| 反向 | `293 -> 334` | `aarch64_to_canonical_upper` |

`canonical 293 -> aarch64 59`（pipe2）与 `aarch64 59 -> canonical 293` 保持不变；
新增 `334 <-> 293` 不占用既有 pipe2 映射。`host-tests/tests/syscall_abi_boundary_test.rs`
的常用双向用例新增 `(334, 293)`，原“293 必须 UNKNOWN”断言改为 `293 -> 334`。

### 2.2 用户结构

`struct rseq` 使用 Linux 原始 32 字节布局：

| 偏移 | 字段 |
|---:|---|
| 0 | `cpu_id_start: u32` |
| 4 | `cpu_id: u32` |
| 8 | `rseq_cs: u64` |
| 16 | `flags: u32` |
| 20 | `node_id: u32` |
| 24 | `mm_cid: u32` |
| 28 | padding |

内核只写 `cpu_id_start`/`cpu_id`/`rseq_cs` 三个字段；`node_id`/`mm_cid` 当前保持
用户初值（legacy 模式不维护 NUMA/并发 ID）。glibc 2.44 在 Edgine 上
`__rseq_size = 20`（auxv feature 缺失的最小值），实际按 `max(20, 32) = 32`
注册；因此注册校验接受 `len >= 32`，而不是只接受精确 32。

### 2.3 状态机

`Process` 增加：

```text
rseq_ptr: AtomicU64   // 0 = 未注册
rseq_len: AtomicU32
rseq_sig: AtomicU32
```

新 `Process` 默认全 0；fork/clone 新 TCB 不继承父线程注册。注册状态只用原子量，
信号/退出路径不取新锁。

## 3. 注册/注销语义

### 3.1 注册

`rseq(rseq, len, 0, sig)`：

1. `rseq != 0`、`len >= 32`、`rseq % 32 == 0`；否则 `EINVAL`；
2. `validate_user_buf(rseq, len)` 失败返回 `EFAULT`；
3. 已注册时：
   - 同指针同长度同签名 → `EBUSY`；
   - 指针或长度不同 → `EINVAL`；
   - 签名不同 → `EPERM`；
4. 未注册时先写 `cpu_id_start = cpu_id = get_current_cpu()`；写失败 `EFAULT`；
5. 成功后才发布 `rseq_ptr`，长度/签名先于指针发布。

### 3.2 注销

`rseq(rseq, len, RSEQ_FLAG_UNREGISTER, sig)`：

1. `flags` 只允许 `RSEQ_FLAG_UNREGISTER`，否则 `EINVAL`；
2. 未注册、指针或长度不一致 → `EINVAL`；签名不一致 → `EPERM`；
3. 先写回 `cpu_id_start = 0`、`cpu_id = 0xFFFF_FFFF`，并写 `rseq_cs = 0`；
4. 写成功后才清 `rseq_ptr/rseq_len/rseq_sig`。

写失败保留注册状态，避免把用户区写成半注销。

## 4. 信号、退出与 exec

### 4.1 返回用户态入口

`do_signal_deliver` 是 x86_64 syscall/中断出口与 aarch64 `svc`/EL0 中断出口的共用入口。
在取到当前 `Process` 后：

- 无条件调用 `rseq::refresh_cpu(proc)`：写 `cpu_id_start = cpu_id = 当前逻辑 CPU`；
- 若 `signal_pick_next(proc).is_some()`，先调用 `rseq::clear_rseq_cs(proc)`，
  写 `rseq_ptr + 8 = 0`，再走 handler/默认动作。

这样线程被抢占后迁移到另一 CPU，下一次返回用户态时 CPU ID 会刷新；有信号时
handler 不会看到悬垂的 `rseq_cs`。

### 4.2 退出

`process_exit` 在切换内核页表前、`robust::exit_cleanup` 旁调用
`rseq::exit_cleanup(current_pid)`：

- 原子摘除 `rseq_ptr`，同时清长度/签名；
- 向旧指针 `+8` 写 0（用户地址空间仍有效）。

### 4.3 exec

`UserProcTable::replace_user_space` 在销毁旧页表前调用
`rseq::reset_registration(pid)`：只清内核注册，不向即将销毁的旧地址写值。

## 5. CPU ID 更新策略与边界

**没有**采用“调度器切换前直接写 next 用户页”的方案。原因：

- `next` 在 `context_switch` 前尚未成为当前 CR3，直接写其用户页需要
  `translate_in_pml4` + 直射区，涉及 PA 生命周期与 COW 竞态（D-2 预研已标记风险）；
- `update_user_context` 只改 GDT/CR3 槽，不实际加载 CR3；
- `context_switch_tail` 的返回点属于被切走任务，不能用于给 next 写值。

当前改为在**返回用户态入口**刷新，语义覆盖：

- syscall 返回：写当前 CPU；
- 中断/抢占返回：若迁移，写新 CPU；
- 信号投递前：刷新 CPU 后清 `rseq_cs`。

限制：如果用户线程在纯用户态长临界区内被迁移，且不经过 syscall/中断返回，
`cpu_id` 可能短暂保留旧值。当前 glibc 的 rseq 使用面（`sched_getcpu`/pthread
启动注册）不依赖该窗口；后续若出现强语义需求，再补调度器切换点写。

## 6. 探针与验收

### 6.1 G2：注册/注销/信号清理

`scripts/fixtures/dynamic-glibc-x86_64/glibc_g2.c` 从“只接受 ENOSYS”改为：

- 读取 glibc 自注册区 `__rseq_size`/`__rseq_offset`，按 `max(size, 32)` 计算注册长度；
- 重复注册同参 → `EBUSY`；
- 非法 `flags` → `EINVAL`（宿主 Linux 兼容接受 `EBUSY`）；
- 非法 `len = 31` → `EINVAL`；
- 注销错签名 → `EPERM`；
- 注销成功 → `cpu_id_start = 0`、`cpu_id = 0xFFFF_FFFF`；
- 重注册成功 → CPU ID 为当前逻辑 CPU；
- 设置合法 `rseq_cs`（`abort_ip` 前有 `RSEQ_SIG`）后触发 `SIGALRM`，handler 内
  读取 `rseq_cs == 0`。

Edgine x86_64 串口读数：

```text
[G2] glibc rseq feature=20 reg_len=32 offset=-160 flags=0 cpu=0/0
[G2] RSEQ PASS
```

宿主侧同一 fixture 也通过（`feature=33 reg_len=33`），说明探针不是只针对 Edgine 的特判。

### 6.2 pthread：退出清理与并发腿

`scripts/fixtures/dynamic-glibc-x86_64/glibc_pthread.c` 的 worker：

1. 取本线程 glibc 自注册区，按 `max(__rseq_size, 32)` 注销；
2. 注册线程自有 `worker_rseq`（32 字节对齐）；
3. 写 `rseq_cs` 指向合法 `struct rseq_cs`，随后返回退出；
4. 主线程 `pthread_join` 后检查 `worker_rseq.rseq_cs == 0`。

Edgine x86_64 串口：

```text
[PTHREAD] worker rseq unregister rc=0 errno=0
[PTHREAD] worker rseq register rc=0 errno=0 cpu=1/1
[PTHREAD] RSEQ EXIT PASS
[PTHREAD] PASS
```

该腿同时覆盖 CLONE_THREAD 新 TCB 的独立注册与退出清理；glibc pthread 并发腿
在 rseq 真正注册（而非 fallback）下继续通过。

### 6.3 ABI

`host-tests/tests/syscall_abi_boundary_test.rs`：

- `(334, 293)` 双向互逆；
- `aarch64_to_canonical(293) == 334`；
- `aarch64_to_canonical(59) == 293`（pipe2 反向不冲突）。

### 6.4 读数

- `./ci/build.sh all`：**Passed 7 / Failed 0**
- K-08 x86_64：`_kernel_end_phys = 0x3EF000`，margin `0x11000` = **68 KiB**（门禁 32 KiB）
- `make test-kernel-host`：**3 套 / 1472 passed / 0 failed**
- `make test-host`：**1676 passed / 0 failed**
- `qemu_boot_test.sh x86_64`：**1/1 通过**（G2/PTHREAD 门禁均绿）
- `qemu_boot_test.sh aarch64`：**1/1 通过**（glibc 腿按既有口径 SKIP）
- `bash ci/audit.sh full`：RC=0（QEMU 2/2、thread-stress 6/6、Lockbud 4/4、未执行腿 0）

## 7. 未覆盖/限制

- 只实现 legacy 32 字节字段；`node_id`/`mm_cid` 不写入，扩展 `rseq_len` 的
  feature 区只做长度校验和首 32 字节维护；
- 信号清理不做 `rseq_cs` 的 IP 范围判断与 `abort_ip` 重定向：当前一旦有待投递
  信号就把 `rseq_cs` 清零，属于比 Linux 更保守的清理；不实现临界区重启；
- CPU ID 按返回用户态入口刷新，不在调度器切换点直写 next 用户页（见 §5）；
- aarch64 只锁 ABI 映射，无 aarch64 glibc 运行时载体（`D7-DEFERRED-A1` 仍开放）。

## 8. 变更文件

- `src/kernel/privileged/proc/rseq.rs`（新增）
- `src/kernel/functions/proc/rseq.rs`（新增）
- `src/kernel/privileged/proc/process.rs`
- `src/kernel/privileged/proc/signal.rs`
- `src/kernel/privileged/proc/proc_ops.rs`
- `src/kernel/privileged/proc/user_proc.rs`
- `src/kernel/privileged/proc/mod.rs`
- `src/kernel/privileged/tests/mod.rs`
- `src/kernel/privileged/syscall/types.rs`
- `src/kernel/functions/proc/mod.rs`
- `src/kernel/functions/syscall/dispatch.rs`
- `src/rust/edgine-abi/src/linux_syscall.rs`
- `host-tests/tests/syscall_abi_boundary_test.rs`
- `scripts/fixtures/dynamic-glibc-x86_64/glibc_g2.c` / `glibc_g2`
- `scripts/fixtures/dynamic-glibc-x86_64/glibc_pthread.c` / `glibc_pthread`
- `scripts/fixtures/dynamic-glibc-x86_64/README.md` / `SHA256SUMS`
- `scripts/qemu_boot_test.sh`
- `docs/plan/dynamic-linker-project.md`
- `docs/plan/progress-active-tasks.md`
- `docs/explain/ref-lock-order.md`

## 9. 关联

- 计划：`docs/plan/dynamic-linker-project.md` 的 `D7-DEFERRED-R1`
- 前置：`docs/report/d7-g2-glibc-startup-syscalls.md`、`docs/report/d7-g4-glibc-pthread.md`
- Linux 参考：`kernel/rseq.c` v6.10、`Documentation/userspace-api/rseq.rst`
