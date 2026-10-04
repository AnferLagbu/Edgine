# Edgine 远期工程规划

> 远期任务规划，当前阶段不实施。待核心功能稳定后启动。

---

## WASM WASI 接入 ✅ 已完成

- **描述**: 实现 WASI snapshot_preview1 标准接口，使 Edgine 可运行 WASI 编译的 WASM 模块
- **完成日期**: 2026-07-20
- **实现**: 独立 WASI 适配层 (functions/wasm/wasi/) + 复用底层 POSIX 服务
- **规模**: ~2,500 行新增代码 (WASI 适配层 + 解释器增强 + 测试)
- **文档**: [wasm-wasi-integration-design.md](./wasm-wasi-integration-design.md), [wasm-wasi-integration-plan.md](./wasm-wasi-integration-plan.md)

---

## F1: mdBook 文档体系

- **描述**: 建立完整的内核文档体系
- **内容**: 5 个部分
  - kernel-handbook (内核使用手册: 构建/架构/内存/进程/文件系统/网络/驱动)
  - functions-handbook (服务层 API: syscall/VFS/Error)
  - architecture (架构说明: 迁移 docs/explain/)
  - rfcs (设计文档: 迁移 docs/plan/)
  - contributing (贡献指南: 从 AGENTS.md 提取)
- **工作量**: 预计 2 周

---

## F2: RISC-V 架构支持

- **描述**
  - Asterinas 支持 x86_64 + riscv64 + loongarch64；Edgine 仅 x86_64 + aarch64
  - 方案: RISC-V 64 启动 (OpenSBI) + 页表 (Sv39) + 异常 (stvec/sepc/scause) + 中断 (PLIC/CLINT) + 调度切换

- **工作量**: ~3000-5000 行；预计 6-8 周

---

## F3: TDX 机密计算支持

- **描述**
  - 方案: TDX module 检测 (CPUID 0x21) + tdcall 指令封装 + attest quote + 内存加密

- **工作量**: ~2300 行；预计 4-6 周

---

## F4: NFS 网络文件共享

- **描述**
  - 网络文件共享支持, 允许 Edgine 作为 NFS 客户端/服务器
  - 采用 Edgine 原生方式 (非 Linux 通用 syscall)

- **实现方式: Edgine 原生 (推荐)**
  - NFS 协议解析和业务逻辑在 **functions 层** (safe Rust)
  - 文件操作通过 **FileSystem trait** 委托给 VFS
  - 网络 I/O 通过框架层安全代理
  - 即使 NFS 逻辑 panic, 框架层可捕获并恢复
  - 不需要 name_to_handle_at / open_by_handle_at syscall

- **与 FreeBSD 方式的对比**
  - FreeBSD: NFS 内核模块在内核态运行, 模块崩溃 = 内核 panic
  - Edgine: NFS 在 functions 层 (safe Rust), panic = 进程终止, 更安全

- **前提条件**
  - OpenFile 基础设施 ✅
  - FileSystem trait ✅
  - 网络 socket API ✅
  - 用户态通信接口 (可选, 用于 nfsiod)

- **工作量**: 预计 6-8 周

---

## F5: IPv4/IPv6 双栈支持

- **描述**: 将 NetEndpoint/NetStack 抽象从 IPv4-only 升级为 IPv4/IPv6 双栈, 支持绑定/连接/收发 IPv6 套接字
- **设计决策**: enum IpAddr { V4, V6 } 破坏性改造 (DECISION-032), 详见 [ipv6-dual-stack.md](./ipv6-dual-stack.md)
- **规模**: ~930 行新增 / ~280 行修改 (excl. DHCPv6), 9 文件
- **工作量**: 1-2 周 (Phase 1-5, 7-8), Phase 6 (DHCPv6) 远期
- **状态**: [X] (Phase 1-5 + 7-8 已实施, 2026-08-02; Phase 6 DHCPv6/SLAAC 远期待办)

---

## 实施时间线

```text
Week 1-2:   F1 mdBook 文档体系
Week 3-10:  F2 RISC-V / F3 TDX
Week 11+:   F4 NFS (需内核模块框架)
已完成:     F5 IPv6 双栈 (DECISION-032, Phase 1-5 + 7-8, 2026-08-02; Phase 6 DHCPv6 远期)
```
