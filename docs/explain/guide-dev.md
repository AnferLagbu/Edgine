# 框内核开发与维护指导

> 本文档给本项目（Edgine）维护者与贡献者：在框内核（framework/ + services/）双子树架构下，新代码放哪里，改代码先改哪里，何时两边都改。配套 [explain-framekernel.md](./explain-framekernel.md) 阅读。适用读者：维护 framework/ 与 services/ 的内核开发者，以及首次提 PR 的新贡献者。

## 这是什么

### 范围与配套

本项目内核代码的归属决策与变更流程。范围：本项目内核代码的归属决策与变更流程；不讨论 OSTD/Asterinas 理论；不涵盖：用户态工具链，构建系统，测试工具（另有文档）；配套：explain-framekernel.md（框内核是什么，为什么这样设计）/ README.md §1 文件归属规则（文档归属，本文是代码归属）/ scripts/audit_services_boundary.py（边界审计脚本的精确白/黑名单）。

## 为什么这样设计

### 一句话判据

简单的资源敏感性判据。要 unsafe 吗？要 → framework。不要 → services。进一步：涉及硬件/MMU/中断/上下文切换？→ framework。纯算法/策略/业务？→ services。

### 资源分类判据（星绽 ATC 论文）

按资源敏感性分类。资源被篡改是否可导致内核内存安全违反（UB）？是 → 敏感资源，归 framework；资源被篡改最坏仅导致逻辑错误？是 → 非敏感资源，归 services；敏感资源示例：内核态 CPU 状态（CR3/GDT/IDT）、内核页表项、内核堆元数据、APIC/IOMMU 寄存器；非敏感资源示例：用户态 CPU 状态、用户内存页、外设寄存器（通过 safe 代理）、调度策略、文件系统数据结构；开发时自检：写代码前问"如果这段代码有 bug，最坏后果是什么？" 如果是 UB（UAF/OOB/数据竞争），那它必须在 framework；如果只是功能错误（返回错误码/调度不公平），它可以放 services。

### 6 安全不变式约束

framework 代码的硬约束。I1 内核态 CPU 状态不可被 services 篡改（新增寄存器操作必须在 framework::arch 内部，不暴露 raw 访问）；I2 内核内存不可被 services 非法访问（新增内存管理 API 必须返回强类型 Frame/&T，不返回裸指针）；I3 用户态 CPU 状态只能通过 framework 安全入口修改（新增用户态交互必须走 usermode/userctx）；I4 用户内存只能通过 framework 安全代理访问（新增用户数据访问必须走 copy_from_user/copy_to_user）；I5 外设 MMIO/PIO 只能通过 framework 安全代理访问（新增设备驱动必须通过 iomem/ioport 代理）；I6 外设 DMA 不可写入内核内存（IOMMU 配置）。

## 如何使用

### 决策流程（5 步）

5 步决策流程：(1) 判定资源敏感性：涉及硬件/中断/上下文切换？→ framework / 否则 services；(2) 不变式自检：6 条安全不变式是否满足？不满足 → 必须重新设计；(3) 现有架构搜索：类似功能是否已存在？如果存在，复用；(4) 边界审计：完成后跑 audit_services_boundary.py；(5) 文档同步：改模块接口或依赖关系时，必须同步更新 guide-dev.md 和 audit-*.md。

### 6 安全不变式自检清单

每次修改 framework 代码时逐项确认。I1 本次修改是否暴露了新的内核态 CPU 状态操作给 services？不应暴露；I2 本次修改是否让 services 能直接访问内核内存？不应允许；I3 本次修改是否绕过了 usermode/userctx 进入用户态？不应绕过；I4 本次修改是否让 services 能直接引用用户内存？不应允许；I5 本次修改是否让 services 能直接操作设备寄存器？不应允许；I6 本次修改是否让 DMA 能写入内核内存？不应允许；任何一项回答"是" = 本次修改违反安全不变式，必须重新设计。

### 5 项 TCB 维护原则

TCB 维护 5 条原则：(1) 暴露最强类型，避免裸指针 framework pub fn 返回 &T/&mut T/UFrame/Frame，不返回裸指针；(2) 机制与策略分离 调度器（上下文切换=机制，CFS 算法=策略）/ 帧分配器（页表映射=机制，伙伴系统=策略）/ 网络协议栈（网卡寄存器=机制，TCP 状态机=策略）策略部分应提取到 services 通过 trait 注入；(3) 第三方库的 TCB 影响评估 引入第三方库到 framework 前，评估 unsafe？代码量？替代方案？；(4) TCB 度量纳入 CI 每次 PR 报告 TCB 占比变化，上升需在 PR 描述说明原因；(5) 逐步提取，不做大爆炸重构 渐进式工作，每次提取一个子系统，确保功能不变 + TCB 占比下降 + 6 安全不变式仍然满足。

### 常见子系统归属示例

8 个常见子系统。进程管理（机制：上下文切换/页表切换 framework；策略：CFS/MLFQ 调度 services）/ 内存管理（机制：物理页分配/页表映射 framework；策略：VMA 合并/页面回收 services）/ 中断处理（机制：IDT/中断控制器 framework；策略：路由/分发 services）/ 文件系统（机制：inode 操作表 framework；策略：VFS 后端/RamFS/UNKFS services）/ 设备驱动（机制：iomem/ioport 代理 framework；策略：设备行为 services）/ 网络协议栈（机制：smoltcp 接口 framework；策略：TCP 状态机/DHCP services）/ 同步原语（机制：SpinLock/Mutex framework；策略：锁顺序/Lockdep services）/ 凭据安全（机制：creds/pwm 校验 framework；策略：会话/授权 services）。

## 工作原理

### TCB 边界机制

3 层保护机制。编译期：services/mod.rs 顶部 #![deny(unsafe_code)]，cargo build 阶段排除 services unsafe；静态检查：scripts/audit_services_boundary.py TCB 公开 API 白名单 + 内部模块黑名单；人工审查：code review 流程覆盖 OSTD 四准则 + 6 不变式 + safe API 形式化。

### 数据流方向

单向数据流。services safe Rust → 调用 framework::* 安全 API → framework 内部 unsafe 块配 // SAFETY: 注释 → 硬件（MMU/DMA/中断/CPU 寄存器）；跨层接口全部走 framework pub fn（无 pub unsafe fn）；framework 内部子模块之间不受 #![deny(unsafe_code)] 限制，允许 unsafe，但要写 // SAFETY:。

### TCB 演进路径

渐进式 TCB 缩减。步骤 1 度量当前 TCB 占比（audit_tcb_ratio.py）→ 步骤 2 识别可提取策略（D-01~D-06/B-01~B-05/T-01~T-05 评估）→ 步骤 3 评估提取可行性（策略 vs 机制分离，0 unsafe 边界）→ 步骤 4 提取 + 验证（双架构 0w0e + 三审计 + host-tests）→ 步骤 5 重复直到 TCB 占比 < 30%。

## 注意事项

### 不要在 services 写 unsafe

即便"只用一行"也不放行。一旦放行，后续开发者会以"先例"继续扩散，几天后 TCB 边界形同虚设；正确做法：把 unsafe 移到 framework 并暴露为 safe API。

### 不要让 services 直接 pub use framework 内部模块

TCB 内部细节不能绕过顶层 API。例如 framework::sync::raw / framework::arch::x86_64 / framework::mm::pmm 等内部模块，只能通过 framework 顶层 API 调用；CI 脚本会拦截。

### 改 framework 是大改

任何 framework 改动需重新审视下游 services。提交前补 docs/report/audit-*.md 审计（历史审计文档仍在 docs/plan/ 原位保留）。

### 不为将来可能用预留 framework 模块

准则 §0 同样适用。否则 framework 不可避免地膨胀，TCB 失控。

### 策略提取优先于机制重构

TCB 缩减最稳妥路径。优先提取明显策略（调度/帧分配/Slab/网络/VFS）→ 通过 trait 注入到 services；机制（CR3 切换/中断控制器）重构风险大收益小，延后。

### 新增框架抽象严格按 OSTD 四准则评审

Soundness/Expressiveness/Minimalism/Efficiency。Soundness 准则：任何 safe Rust 调用不可触发 UB；Expressiveness 准则：支持写设备驱动；Minimalism 准则：能放 services 不放 framework；Efficiency 准则：zero-cost abstraction。

## 交叉引用

### 依赖清单

6 个依赖源：explain-framekernel.md（框内核定义与原理，必读背景）/ spec-engineering.md（工程纪律性规范，权威规则定义）/ docs/README.md（文档格式规范）/ src/kernel/framework/mod.rs（framework 入口与 SAFETY 规范）/ src/kernel/services/mod.rs（services 入口与 Safe Rust 契约）/ scripts/audit_services_boundary.py（边界审计，白/黑名单权威源）。

### 被引用清单

1 个被引用源：commit message 记录变更。

### 外部参考

2 个外部参考：OSTD 官方书 — The Framekernel Architecture（原始定义）/ Asterinas USENIX ATC 2025 论文 §3 Framekernel 详细架构。