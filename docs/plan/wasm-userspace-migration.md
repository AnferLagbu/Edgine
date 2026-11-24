# WASM 退用户态迁移计划 (对接 Linux 生态运行时)

> 把 Edgine 自研内核内 WASM 解释器（functions/wasm + `EG_WASM_*` 766-779 + wasmstub）
> 退役，迁移到**用户态生态运行时**。归属已改判为**监管疏忽偏离**（第 7 个真偏离），
> 方向已裁为**退用户态、对接 Linux 生态运行时**。权威依据见
> [service-placement-design.md](../design/service-placement-design.md) §四 与
> [wasm-runtime-design.md](../design/wasm-runtime-design.md) §七；证据快照见
> [in-kernel-service-placement-assessment.md](../report/in-kernel-service-placement-assessment.md)
> 修订记录。实现路径已裁定: **相对完整 (B)**——wasmi 用户态运行时 + 用户态 alloc 前提
> + 完整 WASI 38 函数 syscall 翻译 + 错误分支闭合 + 单测/集成 + 干净退役内核侧。
> 首目标 **wasmi**（no_std 纯解释器，契合 Edgine 无 libc/std 用户态）；**Wasmtime JIT/AOT 延后**
> （需用户态 libc/std 成熟 + #PF→SIGSEGV 投递修复，见开放议题）。

---

## 背景与归属裁定

WASM 内核内 safe 解释器原被论证为「刻意例外」（F1 排除 JIT → 解释器、单地址空间性能立场）。
经归属审查**改判为监管疏忽偏离**：Linux 模型下通用 WASM 一律用户态（Wasmtime / Wasmer /
WasmEdge / wasmi），内核内沙箱字节码是 eBPF（verifier + hook 受限）而非通用 WASM；
「F1 排除 JIT → 解释器」只是「已落内核」条件下的形态论证，属事后合理化，兼重复造轮子
（生态已有成熟用户态运行时）。与 SGEG 反向：SGEG 被误述为偏离、实则对齐（本体留内核）；
WASM 被误述为对齐例外、实则偏离。

方向已裁：**退用户态、对接 Linux 生态运行时**，自研内核解释器为过渡形态、待退役
（不立即删码，用户态前提齐备前保留为兜底）。本册是该方向的可执行分解，**不是**重裁归属。

## 调研结论：用户态 ABI 前提与缺口

迁移不能凭空发生——wasmi 要在 Edgine 用户态跑起来，须先核实 ABI 托管能力。源码调研结论：

| 前提面 | 现状（源码证据） | 对迁移的影响 |
|---|---|---|
| 用户态 alloc | [lib.rs](../../src/user/lib/src/lib.rs) `#![no_std]`，仅 fs/io/str/sys 四模块；`src/user` 全树 `global_allocator` / `alloc::` / `extern crate alloc` **零命中** | wasmi 需 `no_std + alloc`，**用户态堆分配是首前提缺口** |
| mmap 用户面 | [sys.rs](../../src/user/lib/src/sys.rs) 无 `SYS_MMAP` wrapper（仅 `SYS_FB_MMAP`=721 帧缓冲专用）；内核 `SYS_MMAP`=9 已实装 | alloc 须先补 mmap wrapper；interpreter 只需 `PROT_READ\|WRITE`，不需 exec 内存 |
| mmap PROT_EXEC | [mprotect.rs:24-28](../../src/kernel/functions/mm/mprotect.rs#L24-L28) `PROT_EXEC` 清 NX | wasmi 纯解释**不用** exec 内存，绕开 JIT 的取指 #PF 问题 |
| mmap SGEG 能力门 | [mmap.rs:345-348](../../src/kernel/functions/mm/mmap.rs#L345-L348) `pwm_has_capability(pwm, 7, 0x01)` 否则 EACCES | wasm 运行时进程须获授 mmap 能力位 |
| 用户 #PF 投递 | [handlers.rs](../../src/kernel/privileged/idt/handlers.rs) 用户 #PF → `TerminateProcess`（**非** SIGSEGV 投递） | wasmi 解释器不触发（无 exec/越界取指）；**Wasmtime JIT trap 处理是延后缺口** |
| WASI 翻译目标 | [sys.rs](../../src/user/lib/src/sys.rs) 已有 read/write/open/close/lseek/stat/mkdir/socket 族/clock_gettime/nanosleep/clone_thread/futex 等 wrapper | 38 函数多数有落点；缺口 = random_get / fd_prestat / path_* 映射 |
| 退役面（用户侧） | [sys.rs:335-361](../../src/user/lib/src/sys.rs#L335-L361) `wasm_load/run/get_info/unload` + [sys.rs:684-688](../../src/user/lib/src/sys.rs#L684-L688) `edgine_abi::wasm` re-export | P4 须连同内核侧一并退役 |
| 退役面（内核侧） | [edgine-abi/src/wasm.rs](../../src/rust/edgine-abi/src/wasm.rs) `EG_WASM_*`=766-769；[dispatch.rs:1261](../../src/kernel/functions/syscall/dispatch.rs#L1261) `dispatch_wasm`；[functions/wasm/](../../src/kernel/functions/wasm/mod.rs) 九子模块；[wasmstub](../../src/user/wasmstub/src/main.rs) | P4 干净退役（F9 真删，非 allow(dead_code)） |
| exec 魔数派发 | [exec.rs:106-119](../../src/kernel/functions/proc/exec.rs#L106-L119) `\0asm` → 装 wasmstub → EG_WASM 回环 | P1 起改指用户态 wasmi 运行时（过渡共存） |

**选型裁定**：wasmi 是近期唯一可行目标——`no_std + alloc` 契合 Edgine 用户态（无 libc/std）；
Wasmtime/Wasmer 需 std/libc，Edgine 用户态当前不具备，故 JIT/AOT 路线延后（见开放议题）。

## 设计决策

### D1: 首目标 wasmi，Wasmtime JIT 延后

- **描述**: 迁移首阶段对接 wasmi（no_std 纯解释器），不追求 Wasmtime JIT/AOT。
- **方案**: wasmi vendored 进 `src/user/`（锁定版本，类比 smoltcp vendored 治理）；
  Wasmtime 路线作为开放议题延后，待用户态 libc/std 成熟 + #PF→SIGSEGV 修复后另立。
- **状态**: []
- **详情**: wasmi 被 Polkadot/Substrate 采用，no_std+alloc 形态成熟。Edgine 用户态
  无 std/libc（[lib.rs](../../src/user/lib/src/lib.rs) `#![no_std]`），stock Wasmtime 不可跑。
  解释器路线同时绕开 JIT 的 exec 内存与 #PF trap 投递两个缺口，de-risk 最优。

### D2: 用户态 alloc 是首前提，先于 wasmi 集成

- **描述**: wasmi 需 alloc，当前用户态零堆分配能力，须先补。
- **方案**: 补 mmap wrapper（`SYS_MMAP`=9 / `SYS_MUNMAP`=11）+ 最小 `#[global_allocator]`
  （bump 或 linked-list over mmap，no_std）；核实 SGEG mmap 能力门授权路径。
- **状态**: [X]
- **详情**: 缺口实测——`src/user` 全树 `global_allocator`/`SYS_MMAP`/`alloc::` 零命中。
  已实装（P0）：sys.rs 增 mmap/munmap wrapper + heap.rs free-list `#[global_allocator]`，
  10 host 单测全绿。interpreter 只需 `PROT_READ|WRITE`，不需 `PROT_EXEC`。

### D3: WASI 38 函数全量翻译到既有 POSIX syscall

- **描述**: 内核 [functions/wasm/wasi](../../src/kernel/functions/wasm/wasi/mod.rs) 的
  38 函数 8 组语义，在用户态 wasmi host 层全量翻译到 Edgine POSIX syscall。
- **方案**: 逐组映射（G1 进程 / G2 时钟随机 / G3 环境参数 / G4 FD 管理 / G5 FD I/O /
  G6 路径 / G7 高级 FD / G8 socket）→ [sys.rs](../../src/user/lib/src/sys.rs) 既有 wrapper；
  缺口（random_get / fd_prestat / path_*）补齐；错误分支闭合。
- **状态**: []
- **详情**: 翻译目标多数已存在（read/write/open/socket/clock_gettime/nanosleep/clone/futex）。
  38 函数对照见 [explain-wasm-runtime.md](../explain/explain-wasm-runtime.md) WASI 适配层表。
  这是「相对完整」与「简约 demo」的分界——P1 只最小 WASI de-risk，P2 才全量。

### D4: 干净退役，遵循 design §七 次序，不立即删码

- **描述**: 生态运行时接管后退役自研内核侧；过渡期共存，靠 exec 派发切换。
- **方案**: 次序 = 补前提（P0）→ 接生态（P1-P3）→ 退自研（P4）。退役面 = `EG_WASM_*`
  766-779 + wasmstub + 内核 functions/wasm + user lib wasm_* wrapper。F9 真删，非抑制。
- **状态**: []
- **详情**: design §七「不立即删码，用户态前提齐备前保留为兜底」。相对完整路径终态退役，
  但 P1-P3 期间内核解释器保留为引导 fallback。退役须真删（F9 死代码零容忍），
  不得 `#[allow(dead_code)]` 压制。

## 施工分解

### P0: 用户态 alloc 前提 + mmap 接线

- **描述**: 补用户态堆分配能力，作为 wasmi 集成的首前提（D2）。
- **方案**:
  - [sys.rs](../../src/user/lib/src/sys.rs) 增 `SYS_MMAP`=9 / `SYS_MUNMAP`=11 wrapper
    （与内核 [types.rs](../../src/kernel/privileged/syscall/types.rs) 同源）。
  - 新增最小 `#[global_allocator]`（bump/linked-list over mmap，no_std），落 user lib 或新 crate。
  - 核实 SGEG mmap 能力门（bit 7.0x01）对 wasm 运行时进程的授权放行路径。
  - 单测：alloc 基本路径（分配/释放/对齐）+ mmap 失败分支。
- **状态**: [X]
- **详情**: 已实装。heap.rs = K&R 地址升序 first-fit free-list（块分裂 + 相邻合并 +
  高对齐回填块头指针），AtomicBool 自旋锁保护，mmap 64KiB 粒度增长；FreeList 纯指针核心
  host 可测（静态 arena），mmap 后端 host 桩替代（CR4 限制，真映射只在 QEMU 用户态生效）。
  sys.rs 增 SYS_MMAP=9 / SYS_MUNMAP=11 wrapper。10 host 单测全绿（分配/释放/对齐/分裂/
  合并/双释放幂等/OOM 传播/高对齐/零尺寸/realloc）。SGEG mmap 能力门（domain 7=CAP_DOMAIN_MEM,
  bit 0x01）已核实：init 以 PWM 0 bootstrap 身份运行，pwm_has_capability 对 PWM 0 恒真
  （CapBits::ALL 全域），wasm 运行时经 bootstrap 身份即放行；非 bootstrap 身份须显式 grant
  domain 7。七门槛全绿 + x86_64 QEMU 6/6 启动零 vec14。alloc 前提就绪，P1 wasmi 为首个真实
  消费者（运行时 mmap 行使随 P1）。interpreter 不需 PROT_EXEC，绕开 JIT 缺口。

### P1: wasmi vendored 集成 + stdio demo (de-risk 首里程碑)

- **描述**: wasmi vendored 进 `src/user/`，跑通真实工具链 wasm 模块的 stdio 端到端，
  作为迁移可行性 de-risk（D1）。
- **方案**:
  - 新增 user crate（如 `wasmrun`），加入 [workspace members](../../src/user/Cargo.toml#L7)；
    wasmi vendored 锁定版本。
  - 最小 WASI 子集（proc_exit / fd_write / args_get / environ_get）→ 既有 syscall 翻译。
  - exec 魔数 `\0asm` 派发改指用户态 wasmi 运行时（过渡期与内核解释器共存）。
  - QEMU 双架构跑通 stdio demo。
- **状态**: [~] (2026-11-24 重开为 active，blocked-by-mm-M1；载体已重做，stdio 端到端在 mm M1 裁定前不提前施工、不判绿)
- **详情**: 首阶段只最小 WASI，de-risk 优先；全量 WASI 在 P2。
  验收 = 一个 wasi-sdk 编译的 hello 模块在用户态 wasmi 下 stdio 输出正确。
  载体变更注记: P1 的 x86_64 载体腿受 K-08 (< 4 MiB 内核镜像) 阻塞——wasmrun 桩 + 载荷叠在
  内嵌 initramfs 上顶破上限。先由插队工程 [userspace-decoupling.md](./userspace-decoupling.md)
  把用户态内容移出内核镜像 (boot-module initrd，严格零嵌入)；P1 的**载体相关接线**（demo 载荷
  cpio 条目 / wasmi 探针 / wasmi 启动腿 / feature 选桩形态）在该解耦落地后于新载体上重做，
  carrier 无关资产 (wasmrun crate / wasmi vendoring / wasi fixture) 保留。
- **载体接线已重做 (解耦 D4-d/D4-e 之后)**: K-08 阻塞已消失 (内核零用户态字节,
  x86 `kernel_end=0x3E8000`); demo 载荷 `bin/hello_wasi.wasm` 与两桩恒入 cpio,
  exec 按 `wasm_userspace` **选路径**读 VFS (不再编译期选桩), init 增 `[WASMI] exit=N`
  事实报告探针, 专项腿 `scripts/wasmi_boot_test.sh` 双架构真构建真启动.
  `wasmrun.bin` 经按架构 `objcopy --strip-all` 后 692,400 B (x86) / 793,640 B (aarch64),
  均在 `ELF_MAX_SIZE = 1 MiB` 内 (未剥离时双双超限, 是载体腿红的直接成因).
- **stdio 端到端暂停 = mm 生命周期缺口 (非 wasm 专属)**: 双架构真跑红, 归因为生产 ELF 装载
  从不安装 `MmStruct` ⇒ `mmap` 无 mm 回退返**物理地址** ⇒ wasmi 首次堆写即翻译故障
  (x86 `vec=14 err=0x6 cr2=0x1F800000` / aarch64 `ESR=0x92000045 FAR=0x5FA00000`,
  两架构均 `[WASMI] exit=5632`). wasmi 是**第一个真行使 mmap 的用户程序**, 故此前无腿暴露.
  用户裁定单独立项 ⇒ [mm-lifecycle-project.md](./mm-lifecycle-project.md) (含 I2/I4 安全面与
  简约/相对完整两方案, 待裁定). 该工程落地前 P1 端到端不判绿, wasmi 腿**不接 `ci/audit.sh`**
  (脚本 `err()` 致命退出, 接已知红腿会掩盖其余门槛读数); 脚本与 fixture 作为资产保留。

### P2: 完整 WASI 38 函数 syscall 翻译层

- **描述**: 38 函数 8 组全量翻译 + 错误分支闭合（D3），达到「相对完整」的稳定完备。
- **方案**:
  - 8 组逐组映射到 [sys.rs](../../src/user/lib/src/sys.rs) 既有 wrapper。
  - 缺口补齐：random_get（getrandom 或 /dev/urandom）、fd_prestat/path_* 映射、sock_* 映射。
  - 错误分支闭合（WasmError→Errno 等价映射在用户态重建）。
- **状态**: []
- **详情**: 路径/边界/错误分支完整闭合，可长期稳定运行（§11.3「相对完整」定义）。
  验收 = 38 函数逐组有测试覆盖，错误分支不静默截断。

### P3: 测试覆盖 + 真实载荷验收

- **描述**: 单测 + host 集成 + QEMU 端到端 + 真实工具链载荷验收。
- **方案**:
  - wasmi host 层单测（no_std）+ host-tests 集成。
  - QEMU 双架构 exec 端到端（替代既有 `[WASM] PASS` 腿）。
  - 真实工具链载荷（wasi-sdk 编译）stdio + 文件 + socket 验收。
- **状态**: []
- **详情**: 契合 §2.3 七门槛。验收 = 双架构 QEMU 端到端绿 + 真实载荷跑通。

### P4: 干净退役内核侧 + 收口

- **描述**: 生态运行时接管后退役自研内核侧，文档/台账同步（D4）。
- **方案**:
  - 退 `EG_WASM_*` 766-779（[edgine-abi/src/wasm.rs](../../src/rust/edgine-abi/src/wasm.rs)
    + [registry.rs](../../src/kernel/functions/wasm/registry.rs)
    + [dispatch.rs:1261](../../src/kernel/functions/syscall/dispatch.rs#L1261) `dispatch_wasm`
    + [sys.rs:335-361](../../src/user/lib/src/sys.rs#L335-L361) wasm_* wrapper）。
  - 退 [wasmstub](../../src/user/wasmstub/src/main.rs) crate + 内核
    [functions/wasm](../../src/kernel/functions/wasm/mod.rs) 子树。
  - exec 魔数派发完全改指用户态 wasmi。
  - 文档同步（[explain-wasm-runtime.md](../explain/explain-wasm-runtime.md) 归属注记→退役完成；
    design §七 收口）+ 台账。
- **状态**: []
- **详情**: F9 真删，非 `allow(dead_code)`。退役次序遵循 design §七，不立即删码；
  共存过渡靠 exec 派发切换。验收 = 内核侧零 wasm 残留，七门槛全绿。

## 验收判据

- **底线**: §2.3 七门槛全绿（双架构 check/clippy/fmt + 核心审计 + host-tests + QEMU +
  内核 host 单测 + rustdoc），任何阶段不可豁免。
- **功能**: 真实工具链 wasm 载荷在用户态 wasmi 下 stdio/文件/socket 跑通（design §六 判据 3）。
- **退役**: 内核侧 `EG_WASM_*` / wasmstub / functions/wasm 零残留（F9 真删）。
- **文档**: explain/design/台账同步，归属注记收口为「退役完成」。

## 开放议题 (不在本轮施工)

- **Wasmtime JIT/AOT 对接**: 需用户态 libc/std 成熟 + 用户 #PF→SIGSEGV 投递修复
  （当前 [handlers.rs](../../src/kernel/privileged/idt/handlers.rs) 用户 #PF→TerminateProcess）。
  wasmi 解释器先行，JIT 延后另立。
- **内核解释器引导 fallback 去留**: design §七 保留为兜底至前提齐备；相对完整路径终态退役，
  过渡期共存。fallback 何时彻底移除，随 P4 收口裁定。
- **wasmi vendored 治理**: 锁定版本与升级授权流程，类比 smoltcp vendored（升级须用户授权）。