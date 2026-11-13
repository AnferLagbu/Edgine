# syscall 假成功面评估

> 结论：分发臂中存在 5 条可确证的**假成功**——返回成功值但未兑现宣称语义，或把参数按完全不同的 ABI 解释。假成功比返回 `-ENOSYS` 危险得多：第三方程序会带着错误结果继续运行，故障以远离根因的形式出现在后续路径上。当前没有任何门禁腿能区分「真实现」与「返回常量的臂」。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读。

## 一、判定口径

假成功需同时满足两条：

1. 该臂对合法参数返回非负值（或返回值被解释为成功）；
2. 被调实现未执行该 syscall 名称所承诺的语义——包括「什么都不做」「只写一部分出参」「把参数按另一套 ABI 解释」。

与两类正确行为区分：**恒错 stub** 返回 `-ENOSYS` / `-EINVAL`，调用方能明确感知不支持；**部分实现**完成可观测子集并在文档中声明边界。二者都不是假成功。

分发主战场：`src/kernel/functions/syscall/dispatch.rs`（237 条臂），回退路径 `src/kernel/privileged/syscall/dispatch.rs`。

## 二、可确证的假成功条目

### 2.1 `SYS_SELECT` 按 poll 语义接线

`functions/syscall/dispatch.rs:399-403`：

```rust
SYS_POLL | SYS_SELECT => crate::functions::fs::file_ops::poll_syscall(
    a0,
    crate::functions::intcast::low_u32_from_u64(a1),
    crate::functions::intcast::low_i32_from_u64(a2),
),
```

`poll_syscall` 的签名语义是 `(fds 数组指针, nfds, timeout)`（实现见 `functions/fs/file_ops.rs:79-130`，按 `PollFd { fd, events, revents }` 读取）。而 `select` 的参数是 `(nfds, readfds, writefds, exceptfds, timeout)`。

后果：`nfds` 被当成 pollfd **指针**解引用（可能直接越界或读到无意义内存）；`readfds` 指针被截成 u32 当作 **nfds**，即一个巨大且无意义的计数；`writefds` 指针被截成 i32 当作**超时毫秒**。只要三个指针低位不恰好为 0，返回值就是随机成功，调用方据此认为某些 fd 就绪。

### 2.2 `SYS_NEWFSTATAT` 把路径指针截成 fd

`functions/syscall/dispatch.rs:291-293`：

```rust
SYS_NEWFSTATAT => as_ret(crate::functions::fs::stat::fstat_syscall(
    crate::functions::intcast::low_i32_from_u64(a1),
    a2,
```

Linux 语义为 `newfstatat(dirfd, path, statbuf, flags)`。此处 `a0`（dirfd）被丢弃，`a1`（路径**指针**）经 `low_i32_from_u64` 截成 32 位当作 **fd**，`a2` 恰好仍是 statbuf 指针。

后果：低 32 位若撞上当前进程一个有效 fd（例如路径字符串地址低位是 3/4/5），会返回该 fd 的 stat 结果而不是路径对应文件的 stat；否则返回 `-EBADF`。两条路都不会告诉调用方「这个 syscall 没被正确实现」，而 `stat` 类调用是任何程序启动早期就会执行的。

### 2.3 `SYS_GETCPU` 不写用户出参

`privileged/mm/numa.rs:470-478`：

```rust
pub fn sys_getcpu() -> i64 {
    let cpu = crate::privileged::smp::current_cpu_index();
    let node = ...;
    i64::from(cpu) | (i64::from(node) << 32)
}
```

Linux `getcpu(unsigned *cpu, unsigned *node, void *tcache)` 通过**两个用户指针**写出，返回值应为 0。此处完全忽略用户指针，把 cpu/node 打包进返回值。

后果：调用方的 `*cpu` / `*node` 保持未初始化（内存安全上不是 UB，但语义上是垃圾值）；返回值非 0 会被当作错误码，`cpu=0` 时返回 0 又会被当作「成功但未写」。glibc 的 `sched_getcpu` 走 vDSO 或此 syscall，一旦退回此路径即得到错误拓扑。

### 2.4 `SYS_GET_MEMPOLICY` 显式丢弃结果并返回 0

`privileged/mm/numa.rs:423-435`：

```rust
pub fn sys_get_mempolicy(_mode_ptr: u64, _nodemask_ptr: u64) -> i64 {
    ...
    match result {
        Some((mode, mask)) => {
            let _ = (mode, mask);
            0
```

参数名以 `_` 前缀声明本身即表明不写用户内存；`let _ = (mode, mask);` 把刚读出的策略显式丢弃。调用方得到「成功」，但 `mode` 与 `nodemask` 缓冲区内容未定义。

### 2.5 `SYS_MIGRATE_PAGES` 空操作恒返 0

`privileged/mm/numa.rs:464-466`：

```rust
pub fn sys_migrate_pages(_target_nodemask: u64) -> i64 {
    0
}
```

函数体只有 `0`。调用方认为页已迁移到目标节点，实际上没有任何页面移动；后续按新拓扑假设的性能与亲和性全部落空。

## 三、四分类读数

本次可确证的结构如下：

| 类别 | 本次读数 | 说明 |
|---|---|---|
| 恒错 stub（`-ENOSYS` / `-EINVAL`） | 未见误判 | 分发兜底返回错误，属正确行为 |
| 常量返回臂 | 5 处 | `dispatch.rs:1254` 与 `:1515/:1524/:1533/:1552`，均为 `Ok(()) => 0` 的正常结果映射，非假成功 |
| 部分实现 | 未逐条量化 | 需按名称逐一核对语义，本轮只做定向取证 |
| **假成功** | **5 条** | 见第二节 |

上一轮全景审计给出的「约 150 / 50 / 5 / 5」四分类是一个估算，至今**没有脚本固化**：没有任何腿会因新增一条 `=> 0` 的臂而变红。这意味着四分类读数无法随代码演进自动更新，只能靠人工重做——这本身是门禁缺口，而非一次性遗漏。

## 四、危害为什么高于 ENOSYS

`-ENOSYS` 是**可判别契约**：调用方可以降级、可以报错、可以在启动阶段就发现「此内核不支持 X」。假成功则把失败推迟并转移：

- `newfstatat` 假成功会让 `ls`、`cp`、动态链接器的路径探测拿到错误的 inode 元数据，故障表现为「文件存在但打不开」或「权限莫名其妙」；
- `select` 假成功会让事件循环空转或误唤醒，故障表现为「网络偶发不响应」，而根因在一次参数错位；
- `getcpu` / `get_mempolicy` 假成功会让 NUMA 感知的运行时按错误拓扑做决策，故障表现为难以复现的性能悬崖。

三类都符合「故障以远离根因的形式呈现」这一特征，且因为返回值是成功，日志里不会留下任何错误痕迹。

## 五、与其它缺口的边界

本报告只覆盖「臂存在且返回成功但语义未兑现」。两类相邻问题不在此列，另行取证：

- **有常量无臂**（11 条编号）属于接线缺口，调用方得到的是分发兜底错误；
- **标志位只有常量无判别**（如 `MAP_FIXED`）属于标志层面的假成功，但其表现取决于翻译层，不属于本报告的 syscall 臂分类。

## 六、对计划的意义

假成功的处置顺序应由「第三方程序启动即触达」决定，而不是由发现顺序决定：`newfstatat` 与 `select` 位于任何程序的最早路径上，`getcpu` / `get_mempolicy` / `migrate_pages` 只在特定运行时被触达。前者决定外部程序能否启动，后者决定它能否稳定运行。

同时需要一条能自动发现新假成功的机制：现有门禁对「臂返回常量」完全无感，人工四分类会随代码演进立即过期。
