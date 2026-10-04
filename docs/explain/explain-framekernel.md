# 框内核（Framekernel）

> 框内核是 Asterinas 项目（USENIX ATC 2025）提出的 OS 架构，通过"语言层面的内核内特权分离"，用单一地址空间同时获得宏内核性能与微内核级安全。适用读者：维护 privileged/ 和 functions/ 两个子树的开发者，评估子系统归属（改哪里）与新功能入口的贡献者。

## 这是什么

### 框内核定义

单一地址空间 + 源码层特权分离的 OS 架构，宏内核性能 + 微内核级安全。OS Framework（特权半，允许 unsafe）把 MMU/DMA/中断/上下文切换封装为 safe Rust API（TCB）；OS Services（去特权半，禁止 unsafe）在 privileged 之上实现 OS 功能（系统调用/进程管理/文件系统/设备驱动/网络协议栈）；单一地址空间：functions↔privileged 走普通函数调用 + 共享内存，无 IPC 性能损失。

### 起源与历史

蚂蚁集团 2022 中构想，中关村实验室/蚂蚁/北大/南科大联合发布 Asterinas，USENIX ATC 2025 论文正式命名。具体：蚂蚁集团田洪亮团队构想"用 Rust 写完整 OS，把 unsafe 关进最小 TCB"；中关村实验室/蚂蚁集团/北大/南科大联合发布 Asterinas 作为首个工业级实现；星绽 OS 在 HackerNews 公开发布；USENIX ATC 论文 Asterinas: A Linux ABI-Compatible, Rust-Based Framekernel OS with a Small and Sound TCB（arxiv:2506.03876）正式命名；北大 CortenMM（Asterinas 内存管理子系统）获 SOSP 2025 最佳论文。

### 范畴边界

框内核不是第三种"微内核变体"，也不是"全 safe Rust 内核"。不是微内核变体：单地址空间 + 函数调用替代微内核 IPC；不是全 safe Rust：承认必须有 unsafe，集中到 TCB；与宏内核差异：单地址空间依旧，多了源码层特权/去特权边界。

## 为什么这样设计

### 问题驱动

Rust OS ≠ Safe OS，需系统性控制 unsafe 扩散。CrowdStrike 事件（全球数百万 Windows 蓝屏）内核驱动 OOB 访问，证明商业 OS 巨头也无法靠工程规范根除内存安全漏洞；系统软件 60-70% 高危漏洞根因是内存安全；USENIX ATC 论文统计 unsafe 比例：Linux+RFL 55%（6/111）/ Tock 93%（91/98）/ RedLeaf 62%（36/58）/ Theseus 32%（54/171）；仅仅"用 Rust 写 OS" 不够，必须系统性控制 unsafe。

### 替代方案对比

5 个替代方案与放弃原因：(1) 全 C 内核+沙箱 gVisor：与现有生态兼容，但性能损耗大沙箱本身仍需验证；(2) 全 safe Rust+inline asm：无 unsafe 块，但编译器无法验证硬件交互，等价于"信任所有代码"；(3) 微内核 seL4：TCB 最小已形式化验证，但 IPC 性能损失，不支持富功能 OS；(4) Rust for Linux（混合）：渐进式引入 Rust，但 unsafe 散落，TCB ≈ Linux 全量，失去"安全"卖点；(5) 框内核 Framekernel：宏内核性能 + 微内核级 TCB — 选定。

### OSTD 四准则

privileged 必须同时满足 4 条准则：(1) Soundness 安全性：任何用 privileged 安全 API 写的 safe Rust 都不可触发 UB；(2) Expressiveness 表达力：必须支持写设备驱动等"内核主体"功能；(3) Minimalism 极小性：能放在 functions 层的就不放 privileged；(4) Efficiency 效率：安全 API 应是 zero-cost abstraction，几乎无运行时开销。

### 6 条安全不变式

形式化 Soundness 准则的 6 条不变式。I1 内核态 CPU 状态不可被 functions 篡改（CR3/GDT/IDT/MSR 只能 privileged safe API 修改）privileged::arch 内部；I2 内核内存不可被 functions 非法访问（内核页表/内核堆元数据）privileged::mm + privileged::page_table 内部；I3 用户态 CPU 状态只能通过 privileged 安全入口修改（进入/退出用户态走 usermode/userctx）privileged::usermode + privileged::userctx；I4 用户内存只能通过 privileged 安全代理访问（copy_from_user/copy_to_user）privileged::userptr；I5 外设 MMIO/PIO 只能通过 privileged 安全代理访问 privileged::iomem + privileged::ioport；I6 外设 DMA 不可写入内核内存（IOMMU 配置）privileged::dma_buf + IOMMU 映射；违反任一 = privileged safe API 不 Sound = 整个内核内存安全保证失效。

### 资源分类：敏感 vs 非敏感

内核资源分类决定归属。敏感资源：被篡改可导致内核内存安全违反，归 privileged（TCB），例 内核态 CPU 状态/内核页表/APIC/IOMMU 寄存器/内核堆元数据；非敏感资源：被篡改仅导致逻辑错误（非 UB），归 functions（safe Rust），例 用户态 CPU 状态/用户内存页/外设寄存器（通过 safe 代理）/调度策略；关键洞察：非敏感资源可被 functions 安全管理，即使 functions 有 bug，最坏结果也是功能错误（如进程调度不公平），不会导致内存安全漏洞。

### UFrame / USegment 抽象

外部可变内存的类型级安全抽象。强化 Invariant I4；Pod trait（Plain Old Data）标记 Copy+无指针+无内部可变性，实现于 u8/u16/u32/u64/i8/i16/i32/i64/usize/isize/bool + [T; N]；UFrame 封装用户物理帧（4KB）提供 read_pod/write_pod/read_bytes/write_bytes 禁止暴露 &[u8] 引用；USegment 封装连续用户虚拟内存段同理；安全保证：所有访问通过 copy_from_user/copy_to_user（带异常表恢复）+ 偏移量边界检查 saturating_add 防溢出 + 不暴露长期引用，防止 TOCTOU 攻击 + Pod trait 防止内核指针泄露到用户空间。

## 如何使用

### 模块归属决策

新增代码应放 privileged 还是 functions。放 functions（策略）：算法选择（CFS 权重/buddy 阶数）/ 数据结构管理（VMA 合并/调度队列）/ 策略参数（rlimit/时间片/OOM 评分）/ 协议逻辑（信号投递/seccomp 过滤链）/ 格式解析（ELF 验证/cpio 解包）；放 privileged（机制）：硬件操作（CR3 切换/页表写入/上下文切换）/ unsafe 内存操作（copy_from/to_user/物理页操作）/ 原子指令/内存屏障 / 中断控制器编程（APIC/GIC）/ 寄存器读写/MMIO；不为将来可能用到预留章节（准则 §0）。

**归属决策树（2026-09-11 依 Asterinas 标准补全 Q2/Q3，修正"要 unsafe → privileged"的片面理解）**：

```
Q1: 该功能必须 unsafe 吗（直接碰硬件/页表/裸内存）？
 ├─ 否 → functions（纯策略/功能）✅
 └─ 是 → Q2: 它是"机制"还是"功能"？
       ├─ 机制（页表/上下文切换/寄存器原语/同步/安全代理）→ privileged ✅
       └─ 功能（驱动/文件系统/网络栈/进程/信号/syscall）→ Q3
             Q3: 能否封装为 safe API 供 functions 用？
              ├─ 能 → privileged 留机制原语 + 封装 safe API
             │        （IoMem::from_pci_bar / IoPort::new_safe /
             │         DmaStream / UserPtr safe 构造），
             │        功能实现在 functions（0 unsafe 调用 safe API）✅
              └─ 不能（self-referential / no_mangle FFI ABI / 中断上下文）
                   → privileged 留薄层（如 smoltcp 集成层 / FFI 边界）
```

> 关键洞察：**驱动等"功能"即使要 unsafe（MMIO/DMA），也不该直接进 privileged**——Asterinas 的做法是 privileged 提供安全代理（IoMem/IoPort/DmaStream），驱动在 functions 以 0 unsafe 实现。下沉路径 = 先补 safe API 缺口 → 策略层下沉 → 驱动经 safe API 下沉。

### safe API 失败回滚模式

失败回滚专用入口的设计模式。分配入口 safe API（返回 Result，失败自动清理已分配资源）；释放入口 safe API（失败回滚专用）；调用点 LIFO 反序（先 UserProcess 再 Process，避免 NonNull<Process> 悬挂）；模式出处：Asterinas 论文 §4.3 "Privilege Separation" 的工程落地 unsafe 只在 privileged 一次出现，业务层只看到 safe API。

### 与 Asterinas 原版的本地化差异

3 项本地化调整：(1) 项目未引入 OSTD 完整 crate，privileged 直接做成 src/kernel/privileged/，公共 API 集中体现在 privileged/mod.rs 顶层模块；(2) functions/mod.rs 顶部的 "Safe Rust 契约" 段落把"每文件头声明 @SAFE"写成显式规范，方便人工/脚本审计；(3) "TCB 公开 API 白名单 + 内部模块黑名单" 落在 scripts/audit_functions_boundary.py 是 Asterinas 论文未单列但本项目新增的工程约束。

## 工作原理

### 编译期强制

functions 层 0 unsafe 通过编译时 deny 强制。src/kernel/functions/mod.rs 顶部 #![deny(unsafe_code)]；任何子模块中写 unsafe 即触发编译错误；效果：cargo build 阶段即排除 functions 层 unsafe 代码。

### 调用边界

跨层接口全部走 privileged safe API。functions safe Rust → 调用 privileged::* 安全 API → privileged 内部 unsafe 块配 // SAFETY: 注释 → MMU/DMA/中断控制器/CPU 寄存器；跨层接口必须全部走 privileged pub fn（无 pub unsafe fn）；privileged 内部子模块之间不受 #![deny(unsafe_code)] 限制，允许 unsafe，但要写 // SAFETY:。

### 内存安全 TCB 定义

TCB 边界。TCB = privileged 的所有 unsafe 块 + 围绕它们的 safe API 类型/函数签名；只要（a）privileged 的 unsafe 块全部遵守 SAFETY 契约，（b）privileged 的 safe API 形式化（或经严格审计）正确，则整个 OS 内核的内存安全都成立；functions 层出 bug，顶多产生"逻辑错误"（如 VFS 返回错误码），不会导致 UAF/OOB 等内存安全漏洞。

### 与宏内核/微内核对比

5 维度对比。地址空间（宏：单一 / 微：多个 / 框：单一）；跨组件通信（宏：函数调用 / 微：IPC 高开销 / 框：函数调用）；TCB 大小（宏：整个内核千万 LoC / 微：微内核 10K LoC / 框：privileged ~15K LoC Asterinas 实测）；设备驱动位置（宏：内核态 / 微：用户态 / 框：内核态 safe Rust）；富功能支持（宏：强 / 微：弱 / 框：强 Linux ABI 兼容）；内存安全保证（宏：无 / 微：强可验证 / 框：强 TCB 内可验证）。

## 注意事项

### 不要在 functions 写 unsafe

即便"只用一行"也不放行。一旦放行，后续开发者会以"先例"继续扩散，几天后 TCB 边界形同虚设；正确做法：把 unsafe 移到 privileged 并暴露为 safe API。

### 不要让 functions 直接 pub use privileged::sync::raw 或 privileged::arch::x86_64

这些是 TCB 内部细节。只能通过 privileged 顶层 API 调用，CI 脚本会拦截。

### 改 privileged 是大改

任何 privileged 的 unsafe 块改动需重新审视下游 functions。提交前补 docs/report/audit-*.md 审计（历史审计文档仍在 docs/plan/ 原位保留）。

### 不要把内层抽象暴露成 privileged 公开 API

例如 PageTableChecker 之于 functions::fs::page_cache。functions 拿到 VmSpace / Frame，不直接拿 checker；见 privileged/mod.rs 顶层公开 API 范围。

### 不要为将来可能用预留 privileged 模块

准则 §0 在这里同样适用。否则 privileged 不可避免地膨胀，TCB 失控。

### 新增框架抽象时，严格按 OSTD 四准则评审

Soundness/Expressiveness/Minimalism/Efficiency。通过评审再合并。

## 交叉引用

### 依赖清单

5 个依赖源：docs/README.md §6 explain 文档格式（写本文所遵循的格式规范）/ src/kernel/privileged/mod.rs（privileged 子树入口，含完整模块清单与 SAFETY 注释规范）/ src/kernel/functions/mod.rs（functions 子树入口，包含 #![deny(unsafe_code)] 与 Safe Rust 契约）/ scripts/audit_functions_boundary.py（TCB 公开 API 白名单 + 内部模块黑名单）/ framekernel-compliance.md（框内核合规工程书）。

### 被引用清单

2 个被引用源：docs/plan/fix-report-issue1.md（若存在）（进程分配失败回滚修复报告，框内核"safe API 失败回滚"模式样例）。

### 外部参考

6 个外部参考：Asterinas 框内核架构（官方书）/ USENIX ATC 论文 PDF / arXiv 预印本 2506.03876 / Kernel Memory Safety Mission Accomplished（Asterinas 博客）/ Asterinas A Rust-Based Framekernel to Reimagine Linux（login）/ 星绽 OS 登顶 SOSP 框内核的技术解析。