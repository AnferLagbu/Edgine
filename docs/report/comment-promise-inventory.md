# 代码注释中的承诺与缺口清单

> 结论：全仓有 **222 处强信号**的「承诺 / 已知缺口 / 简化」注释，分散在 247 个文件里，且**没有任何机器可读索引**——它们既不被审计腿覆盖，也不被文档路径检查覆盖，因此无法随代码演进自动更新。更严重的一处合规缺口是：项目自定的 `SIMPLIFIED` 三段格式（简化点 / 影响面 / 何时需扩展）在执行面几乎只落了两段——142 条 `SIMPLIFIED` 中仅 14 条写了影响面，**0 条写了「何时需扩展」**。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。只读扫描，未修改任何源码。

## 一、扫描方法与读数

范围：`src/kernel/**/*.rs`，排除 vendored `functions/net/smoltcp/`；只统计注释行（`//` / `//!` / `///`）。

| 关键词族 | 命中 |
|---|---|
| `尚未` | 209（弱信号，多数为「尚未初始化」等正常描述） |
| `后续` | 150（弱信号，多数为叙述性） |
| `SIMPLIFIED` | **142** |
| `不支持` | 83（含 errno 文档等非缺口） |
| `未实现` | 25 |
| `未实装` | 19 |
| `近似` | 15 |
| `留待` | 7 |
| `暂不支持` | 6 |
| `未接线` | 4 |
| `不接线` | 1 |
| `待实现` | 1 |
| `残留边界` | 1 |
| `单独立项` | 1 |

强信号合计 **222 处**（142 条 `SIMPLIFIED` + 80 处缺口关键词命中；缺口类按行去重为 47 行）。另有 `TODO` / `todo!()` / `unimplemented!()` **0 处**，与 F9 口径一致。

## 二、关键合规发现：`SIMPLIFIED` 的第三段全部缺失

`AGENTS.md` §11.3 规定格式：

```
// SIMPLIFIED: <具体简化点>; <影响面>; <何时需扩展>
```

实测 142 条 `SIMPLIFIED`：

- 含「影响 / 影响面」：**14 条**（9.9%）；
- 含「何时」：**0 条**。

也就是说，最需要追踪的那一段——「什么条件下必须回头扩展」——在全部 142 条里一次都没有写。直接后果是：这些注释只能告诉后来者「这里简化了」，不能告诉他「现在是不是已经到了该扩展的时候」。这与「在茫茫注释中难以找到」是同一问题的两面：不仅难找，找到了也缺判据。

对照一个合格样本（`functions/fs/overlayfs.rs:260-261`）：

```
// SIMPLIFIED: 目录/符号链接/设备的 copy_up 未实装 (目录需递归复制子树);
//            影响面 = 仅普通文件支持写打开; 待后续按需扩展目录复制.
```

它写全了简化点与影响面，第三段仍是模糊的「按需」——这正是需要被替换成可判定条件的部分。

## 三、清单 A：显式未实装 / 未实现

| 位置 | 承诺内容 |
|---|---|
| `functions/fs/overlayfs.rs:238`、`:260-261`、`:483`、`:695` | 目录/符号链接/设备 `copy_up` 未实装；metadata copy-up 未实装（时间戳写回被显式拒绝） |
| `functions/fs/io.rs:298` | `O_CLOEXEC` 未实现，`flags` 当前被忽略 |
| `functions/fs/file_ops.rs:245` | `AT_SYMLINK_NOFOLLOW` 被忽略；目录 fd 解析暂不支持 |
| `functions/fs/mod.rs:412` | mmap 文件映射后端凭据未接线 ⇒ `ENODEV` 拒绝 |
| `functions/fs/misc.rs:204`、`:267` | `times` 的 cutime/cstime 恒 0；`VIRTUAL` / `PROF` 未实现 |
| `functions/fs/initramfs.rs:288` | 设备文件等类型暂不支持 |
| `functions/proc/pidfd.rs:29` | `pidfd_getfd` 未实装，返回 `ENOSYS` |
| `functions/proc/memfd.rs:34` | 大页暂不支持 |
| `functions/proc/table.rs:17` | 「进程创建/析构 — 留待 Phase 2.5.3」（**疑似过期**，见清单 E） |
| `functions/wasm/interpreter.rs:466` | 未实装 opcode 返回 `UnknownOpcode` |
| `functions/driver/usb/xhci.rs:1066` | 等时传输暂不支持 |
| `privileged/io/iouring.rs:307` | 未实装占位（登记分册 9 B09-10） |
| `privileged/mm/uffd.rs:39` | 事件未实装，`UFFDIO_API.features` 回填 0 |
| `privileged/mm/vma.rs:917` | 含 `MREMAP_FIXED` 等未实现位 ⇒ 显式 `EINVAL`（**正确形态**） |
| `privileged/proc/process.rs:1401`、`:1418` | 生命周期 pin / refcount 尚未实装，已立项；误激活警报已就位 |
| `privileged/proc/proc_ops.rs:542` | 唤醒后重睡同一等待字，本架构尚未接线 |
| `privileged/syscall/futex.rs:409` | `FUTEX_CLOCK_REALTIME` 与 val2 位集过滤本轮不接线 |
| `privileged/timer/sleep.rs:30` | 忙等待实现（Busy-wait） |
| `privileged/net/init/raw.rs:560` | Raw / Dhcpv4 / Dhcpv6 / Dns 用户态不可见，暂不支持 |
| `privileged/idt/handlers.rs:255` | `copy_from_user` 的异常表 / 恢复点机制当前未接线 |
| `privileged/arch/aarch64/exception.rs:863-873` | uffd 出口未接线；缺页风暴无重试限界，单独立项 |
| `functions/fs/vfs_poll_policy.rs:47` | Dev 不报告 `POLLIN/POLLOUT`，留待驱动层（`DRIVER-2` 触发时扩展） |

## 四、清单 B：近似实现（正确性弱化）

| 位置 | 近似内容 |
|---|---|
| `privileged/net/init/sm_fi.rs:874`、`:2107` | 关闭窗口统一近似为 `ECONNREFUSED` |
| `privileged/debug/ebpf.rs:632` | 时间戳使用 TSC 近似 |
| `privileged/dma/engine.rs:702` | 同步 DMA 用内存复制近似，缺硬件完成回调释放 |
| `privileged/mm/swap.rs:14` | 「近似 LRU 回收」（固定窗口，视野非全局） |
| `functions/ipc/async_ipc.rs:345` | 毫秒 → TSC ticks 近似换算 |
| `functions/driver/display/hdmi.rs:290` | 刷新率为近似值 |
| `privileged/proc/eevdf.rs:1165` | 「近似匹配：找最接近」 |
| `privileged/mm/vmm_x86_64.rs:719` | 未激活时退化为复制内核高半区 |
| `privileged/dtb.rs:14` | 仅提取 memory / uart / gicv3，未实现 phandle |
| `privileged/dtb.rs:385` | CPU 枚举上限 8 核，超出不记录 |

## 五、清单 C：未接线（有实现、无调用者）

| 位置 | 事实 |
|---|---|
| `functions/fs/mod.rs:412` | mmap 文件映射后端凭据 |
| `privileged/idt/handlers.rs:255` | 用户拷贝异常表 / 恢复点 |
| `privileged/arch/aarch64/exception.rs:865` | uffd 出口 |
| `privileged/proc/proc_ops.rs:542` | 唤醒后重睡等待字 |
| `privileged/syscall/futex.rs:409` | `FUTEX_CLOCK_REALTIME` / val2 过滤 |

## 六、清单 D：残留边界与单独立项

| 位置 | 内容 |
|---|---|
| `privileged/arch/aarch64/exception.rs:871-873` | 缺页风暴无重试计数限界；与 x86_64 同形，列为单独立项 |
| `privileged/sync/rwlock.rs:421` | `concurrent_readers` 未覆盖归零与阻塞判据（测试迁移说明） |

## 七、清单 E：过期或可疑承诺

注释最危险的形态不是「说缺什么」，而是「说的是过去」。本次发现两条：

1. **`privileged/sched/mod.rs:16`**：注释自述「『task 抽象在 Phase 1.4.2 计划中但尚未实现』已过期，实装早于……」——作者已经手动纠正，说明这类问题真实存在。
2. **`functions/proc/table.rs:17`**：`- [ ] 进程创建/析构 — 留待 Phase 2.5.3`。同一列表中 `remove_and_free`、`allocate_pid`、引用计数均已勾选且代码存在，该未勾选项与相邻条目口径不一致，需人工核对是「表级 create/destroy 未做」还是「文档未更新」。

这两条提示：注释承诺会随代码演进静默失真，且目前没有任何机制发现它。

## 八、为什么这些承诺会丢失

三重原因叠加：

1. **无机器索引**：`audit_*.py` 检查 unsafe、死代码、锁序、注释语言，**没有一条**统计或提取「已知缺口」注释；
2. **`check_doc_paths.py` 只查路径新鲜度**，不查注释内容与代码现状的一致性；
3. **`SIMPLIFIED` 缺第三段**，导致即使被提取，也没有「何时该处理」的判据。

结果：222 条承诺的唯一载体是注释本身，读者只能靠正则碰运气——这正是「茫茫注释中很难找到」的成因。

## 九、对计划的意义

这份清单本身就是第一版索引，但它会随代码演进立即过期。要让「追踪代码承诺」成为可持续能力，需要三件事（按依赖顺序）：

1. **可提取**：一条只读脚本，按上表的分组与关键词，把 `SIMPLIFIED` 与缺口注释抽成机器可读清单（文件、行号、类别、原文），并纳入 CI 作为「新增承诺可见」的读数；
2. **可判定**：把 `SIMPLIFIED` 第三段从「按需」改成可观测条件（例如「当 `O_CLOEXEC` 被第三方程序使用时」「当 page cache 命中率低于 X 时」），否则清单只能列事实、不能驱动排期；
3. **可收敛**：对清单 A/C 的每一项设定「消除方式」——实现使用路径、改为显式 `EINVAL`、或从能力面移除。`MREMAP_FIXED` 的显式拒绝是正确范式，`MAP_FIXED` 的静默忽略是反例。

在这三件事落地前，本清单应作为后续计划的**输入基线**：任何新增能力宣称都应先对照清单 A/C 判断是否在重复承诺一个尚未兑现的缺口。
