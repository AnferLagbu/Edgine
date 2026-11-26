# D7 P1：`prlimit64` 跨进程权限收口

> 日期：2026-11-26
> 状态：x86_64 已收口
> 范围：`prlimit64` 跨 pid 权限、`setrlimit` hard limit 提权口径、SYSTEM 能力位、QEMU 回归

## 1. 结论

`prlimit64` 不再用 `current == 1` 判断特权，改为检查调用方是否持有
SYSTEM 域 `SYSTEM_CAP_SET_RLIMIT`。无能力的非 bootstrap 进程修改其他 pid
的 rlimit 返回 `EPERM`；bootstrap PWM 0（`engine::check` 恒真）与持有该
能力位的身份可以跨 pid 查询/设置。`setrlimit` 与 `prlimit64` 提高 hard limit
的 `RlimitTable::set(is_privileged)` 也收敛到同一能力判定，避免两个入口
继续各说各话。

内核单测 `test_prlimit64_cross_process_requires_rlimit_capability` 覆盖：

- 非 bootstrap 身份（新身份仅有 `VIABLE_FLOOR`，无 bit10）改他 pid → `EPERM`；
- 同一 pid / `pid=0` 查询自身 → 不要求跨进程能力；
- 登出回到 bootstrap PWM 0 → 他 pid 查询通过；
- 测试结束删除临时身份并恢复调度器当前进程。

`./ci/build.sh all` **7 / 0**，K-08 余量仍为 **64 KiB**；`make test-kernel-host`
**3 套 / 1472 passed / 0 failed**；`make test-host` **218 套 / 1676 passed /
0 failed**；`bash ci/audit.sh full` **RC=0**，QEMU 双架构 **2/2**、
thread-stress **6/6**、Lockbud **4/4**、未执行腿 **0**。

## 2. 根因

`src/kernel/functions/proc/sysinfo.rs` 原先在两处硬编码 init：

```rust
// prlimit64 跨 pid 门
if target != current && current != 1 { return EPERM; }

// hard limit 提权门
let is_privileged = current == 1;
```

这带来两个问题：

1. 特权判定只看 PID，不看 SGEG 能力面；非 PID 1 的身份即使被授予了
   rlimit 管理能力也无法跨 pid 操作。
2. `setrlimit` 与 `prlimit64` 各自维护同一语义，后续任何一处调整都可能
   让 self 路径与跨 pid 路径再次分叉。

同时 `capability.rs` 只有 `SYSTEM_CAP_SET_PWM`、`SYSTEM_CAP_SET_DOMAIN_FLAGS`、
`SYSTEM_CAP_UTS_SETNAME` 等 SYSTEM 位，没有 rlimit 专用位，不能复用
`SET_PWM`（语义不同，且会把身份设置权限混入资源限制路径）。

## 3. 实现

### 3.1 新增 SYSTEM 域能力位

`src/kernel/privileged/sgeg/capability.rs`：

```rust
pub const SYSTEM_CAP_SET_RLIMIT: u64 = 1 << 10;
```

bit10 此前空闲。能力位通过 `sgeg/mod.rs` 顶层 re-export，供 functions 层
使用；PWM 0 由 `engine::check` 恒真，保持 init/bootstrap 的既有语义。

### 3.2 单源能力判定

`src/kernel/functions/proc/sysinfo.rs` 新增私有 helper：

```rust
fn current_has_rlimit_capability() -> bool {
    pwm_has_capability(
        pwm_get_current(),
        CAP_DOMAIN_SYSTEM,
        SYSTEM_CAP_SET_RLIMIT,
    )
}
```

`setrlimit` 的 `is_privileged` 与 `prlimit64` 的跨 pid 门、hard limit 门
全部改用该 helper，不再出现 `pid == 1`。

### 3.3 `prlimit64` 目标解析

`prlimit64_syscall` 先算一次 `privileged`：

- `pid == 0` → 当前进程；
- `pid > 0` 且 `target != current && !privileged` → `EPERM`；
- `pid < 0` → `EINVAL`；
- 目标不存在 → 后续 `rlimit_for_process` 返回 `ESRCH`。

`new_ptr == 0` 只查询；`old_ptr == 0` 只设置。写入路径仍调用
`process_with_mut(target, |p| p.rlimit_table.lock().set(..., is_privileged))`，
所以“有能力的非 init 身份可提高 hard limit”与“无能力仍可降低限制”
由 `RlimitTable::set` 的既有第三道闸保证。

## 4. 验证

### 4.1 内核单测

```text
test proc::sysinfo::tests::test_prlimit64_cross_process_requires_rlimit_capability ... ok
```

测试用 `identity::create` 创建一个仅含 `VIABLE_FLOOR` 的非 bootstrap 身份，
`session::login` 后断言无能力改他 pid 为 `EPERM`；`session::logout` 回 PWM 0
后断言跨 pid 查询通过。`FS_GLOBAL_TEST_LOCK` 串行化全局调度器/进程表现场，
测试结束恢复 `Scheduler::current` 并删除临时身份。

### 4.2 门禁读数

| 门禁 | 读数 |
|---|---|
| `./ci/build.sh all` | Passed 7 / Failed 0 |
| K-08 | `_kernel_end_phys=0x3F0000`，margin `0x10000` = 64 KiB，门禁 32 KiB |
| `make test-kernel-host` | 3 套 / 1472 passed / 0 failed |
| `make test-host` | 218 套 / 1676 passed / 0 failed |
| `bash ci/audit.sh full` | RC=0；QEMU 2/2；thread-stress 6/6；Lockbud 4/4；未执行腿 0 |

`prlimit64` 的真实用户指针写路径仍由 QEMU glibc `prlimit64(RLIMIT_STACK)`
回归覆盖；跨 pid 的无能力拒绝与 bootstrap 放行由上述内核单测覆盖。
带 `new_limit` 的跨 pid 写没有在 host 侧构造用户映射，避免在 host 测试里
伪造用户页表；实际写入由 `RlimitTable::set` 既有
`set_raising_hard_limit_requires_privilege`（privileged=true 可提高 hard
limit）与 syscall 同一 `is_privileged` 变量保证。

### 4.3 静态锁序基线

新增单测的 `FS_GLOBAL_TEST_LOCK` 取锁点按 A-3b 覆盖率棘轮登记：
`scripts/lock_order_static.baseline.json` 增加
`src/kernel/functions/proc/sysinfo.rs::FS_GLOBAL_TEST_LOCK: 1`，并同步扫描器
现读数（未分类 870 / 已分类 456 / 取锁点 1326 / 组合边 17）。这是测试串行化
锁，不是新增生产锁；基线更新后 `audit_full` 的 `deadlock_matrix` 腿通过。

## 5. 变更文件

- `src/kernel/privileged/sgeg/capability.rs`：新增 `SYSTEM_CAP_SET_RLIMIT`；
- `src/kernel/privileged/sgeg/mod.rs`：顶层 re-export；
- `src/kernel/functions/proc/sysinfo.rs`：能力 helper、`prlimit64`/`setrlimit`
  权限收敛、单测与 `CurrentGuard`；
- `docs/plan/dynamic-linker-project.md`：`D7-DEFERRED-P1` 转 `[X]`，剩余项
  5 → 4；
- `docs/plan/progress-active-tasks.md`：D7 G1 条目追加 P1 记录；
- `scripts/lock_order_static.baseline.json`：A-3b 测试锁基线登记。

## 6. 未覆盖边界

- 非 bootstrap 身份持有 bit10 的“允许写”路径没有单独造身份授权测试；
  代码路径与 bootstrap 共用 `pwm_has_capability`，身份授权由 `identity::grant`
  负责，不在本切片引入新的授权面。
- 目标进程的 PWM 级别不参与判定，当前语义与 Linux `CAP_SYS_RESOURCE` 的
  “调用方能力”口径一致；若后续要叠加 `pwm_check_privilege` 的目标层级限制，
  需单独立项，避免把两套权限模型混在同一门。
