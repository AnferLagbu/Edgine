# panic 可触达面评估

> 结论：framekernel 的保证是「无 UB」，不是「无 panic」；一次可触达 panic 等价于拒绝服务。当前生产代码（不含 vendored smoltcp、截断文件内测试模块）共有 **314 处** panic 源，vendored smoltcp 另有 **479 处**；其中至少 6 处位于用户态可直接触达的路径。更关键的是**没有任何门禁腿覆盖 panic 面**——新增一处 `unwrap` 不会使任何一条审计变红。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读；计数由只读脚本完成，未执行构建。

## 一、统计口径

- 范围：`src/kernel/**/*.rs`，排除 vendored `src/kernel/functions/net/smoltcp/` 与 `src/kernel/privileged/tests/`。
- 截断：每个文件在首个 `#[cfg(test)]` 模块标记处截断，避免把测试代码计入生产面。
- 计入形态：`.unwrap()`、`.expect(`、`panic!`、`unreachable!`、`assert!` / `assert_eq!` / `debug_assert*!`。

## 二、分层读数

| 类别 | 生产代码 | vendored smoltcp |
|---|---|---|
| `.unwrap()` | 57 | 466 |
| `.expect(` | 59 | 13 |
| `panic!` | 13 | — |
| `unreachable!` | 3 | — |
| `assert*!` | 182 | — |
| **合计** | **314** | **479** |

生产代码热点文件（按合计降序）：

| 位置 | 条数 | 形态 |
|---|---|---|
| `privileged/sync/lockdep.rs` | 73 | 全为 `assert*!` |
| `kernel_tests/test_uds.rs` | 31 | 全为 `unwrap`（测试载体，实际风险低） |
| `privileged/iomem.rs` | 16 | `expect` 8 + `assert` 8 |
| `privileged/mm/kpti_aarch64.rs` | 14 | `assert` |
| `privileged/ioport.rs` | 12 | `expect` |
| `privileged/mm/pmm.rs` | 9 | `expect` 6 + `assert` 3 |
| `privileged/sync/once_lock.rs` | 7 | `expect` 2 + `panic` 2 + `unreachable` 1 + `assert` 2 |
| `privileged/proc/coredump_trait.rs` | 7 | `expect` 1 + `assert` 6 |
| `privileged/cpu_local.rs` | 7 | `expect` 2 + `assert` 5 |
| `privileged/pci/msi.rs` | 6 | `unwrap` |
| `privileged/mm/kpti.rs` | 6 | `assert` |
| `privileged/debug/ftrace.rs` | 6 | `unwrap` |

读法：`assert*!` 的 182 处中 73 处集中在 lockdep（调试设施），14 + 6 处在 KPTI 初始化，属于启动期一次性路径，风险低于字面计数给人的印象。真正需要逐个核定可达性的是 57 处 `unwrap` 与 59 处 `expect`。

## 三、用户态可触达的 panic 点

### 3.1 入向 IPv4 分片重组

`proto-ipv4-fragmentation` 在两棵树上都已启用：`functions/Cargo.toml:21-38`、`privileged/Cargo.toml:22-39`。于是以下代码被编译进生产：

- `functions/net/smoltcp/src/iface/fragmentation.rs:171`：`let total_size = self.total_size.unwrap();`
- `functions/net/smoltcp/src/iface/interface/mod.rs`（ARP/NDP 响应路径）：`neighbor_addr.expect("non-IP response packet")`

`fragmentation.rs:170` 的注释写着「`is_complete` 已经检查过，因此可以 unwrap」。这条保证是**注释级**的：`is_complete()` 与 `total_size` 的一致性没有由类型系统或分支结构强制，vendored 代码一旦演进（升级版本）即可能失效，而当前没有测试覆盖「畸形分片序列 → 重组完成但 total_size 为 None」这一组合。

### 3.2 socket 句柄到 SocketSet 的取值

`iface/socket_set.rs` 对句柄的处理全是 panic 语义：

- `:83` `panic!("adding a socket to a full SocketSet")`
- `:101` / `:115` `.expect("handle refers to a socket of a wrong type")`
- `:103` / `:116` / `:128` `panic!("handle does not refer to a valid socket")`

而特权侧的消费方式是直接调用会 panic 的 API（`privileged/net/init/raw.rs` 对 `sockets.get_mut::<TcpSocket>(handle)` 与 `sockets.remove(handle)` 的调用）。句柄由用户 fd 派生，因此「用户 close 与 send 并发」「用错误类型的 fd 调用另一协议的操作」都能把可恢复的 `EBADF`/`ENOTSOCK` 变成内核 panic。

这条链不依赖网络入向报文，只依赖本地 syscall 序列，是本次读数中最短、最稳定可复现的一条。

### 3.3 用户缓冲区与出参

`privileged/mm/numa.rs` 的若干入口可直接从 syscall 到达（见 `functions/syscall/dispatch.rs:1142-1205` 的分发）。这些函数当前不含 panic，但同族代码（`check_user_buf` 之后的 `read_unaligned`）依赖前置校验；一旦新增字段读取而漏掉校验，panic 面会随功能增长而增长，而门禁不会提示。

## 四、算术溢出面

`src/kernel/Cargo.toml:23` 与 `:27` 只声明了 `panic = "abort"`，**没有显式设置 `overflow-checks`**。未设置即采用 profile 默认：dev 开启、release 关闭（静默 wrapping）。这意味着 release 构建下整数溢出不会 panic，而是产生错误值继续运行——对内核而言这比 panic 更隐蔽。

仓库内已有一处已知的算术敏感点：`functions/syscall/dispatch.rs` 的时间换算路径。此类代码在 release 与 dev 下行为不同，测试通常跑在 dev/host 侧，因此差值不会被常规测试暴露。

## 五、门禁覆盖现状

- `scripts/` 下不存在任何 `*panic*` 审计脚本；逐一检查现有审计腿的关键字，没有任何一条以「可触达 panic」为判据。
- `kani-harness` 只覆盖 `functions/wasm/leb128.rs` 一个纯逻辑模块，且未见作为执行腿接入 `ci/audit.sh`（`ci/audit.sh:274` 仅在某条审计的扫描根说明中提到 `kani-harness` 目录）。
- 死代码门禁（F9）、unsafe 覆盖（F4）、不变式断言（I1-I6）都不涉及 panic 可触达性。

结论：panic 面目前处于**无执法**状态。任何新增 `unwrap` 都能静默合入，且不会有读数变化。

## 六、最短链路推演（两条）

**链路甲（本地，无需网络）**：用户程序对一个 UDP socket fd 执行 TCP 语义操作 → 分发到 `get_mut::<TcpSocket>(handle)` → `socket_set.rs:101` 的 `expect("handle refers to a socket of a wrong type")` → panic → 整机 abort。可用一条 20 行用户态程序触发；对手只需能运行任意程序。

**链路乙（远程）**：向启用分片的接口发送一组构造分片，使重组状态机进入 `is_complete()` 为真而 `total_size` 未设置的组合（依赖 vendored 实现的状态迁移细节）→ `fragmentation.rs:171` unwrap → panic。此链路的可达性取决于 smoltcp 0.14 的具体状态机，需以构造包实测确认；但它位于**入向**路径，一旦可达即为远程拒绝服务。

## 七、对计划的意义

panic 面与其它缺口的性质不同：它不是「功能没做」，而是「已有功能在异常输入下的行为不可接受」。因此它不能靠补功能消解，只能靠**执法面 + 结构性消除**两条同时推进：

- 执法面：需要一条以 AST 判定（而非文本正则）的腿，统计生产代码与 syscall 可达闭包内的 panic 源，并对新增做棘轮；
- 结构性消除：句柄取值改为 `get` / `try_*` 语义并在内核侧做类型判别，把 panic 分支变成 `EBADF` / `ENOTSOCK` 返回值；
- 入向路径：需要决定是前置校验（不改 vendored）还是最小本地化补丁（改 vendored，须授权并同步 purity 腿口径）。

在执法面建立之前，任何「无 panic 启动」的观测都只是单次运行结果，不构成保证。
