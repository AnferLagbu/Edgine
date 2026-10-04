# Edgine Linux 兼容设计理念

> 定义 Edgine 内核在 Linux 兼容性上的分层策略：对外 ABI 兼容，内部实现自主，借鉴但不盲从。

## 一、三层策略

### 1. 对外接口层 — 保持 Linux 兼容

对外暴露给用户态的接口遵循 Linux/POSIX 标准，确保生态兼容性：

- **syscall 编号**：直接使用 Linux x86_64 标准编号（0-299），Edgine 私有扩展使用 500+
- **文件系统接口**：/proc、/sys、/dev 遵循 Linux 标准格式，用户态工具（free/top/ps）可直接运行
- **ELF 格式**：支持 Linux ELF 二进制，PT_INTERP 检测并改写动态链接器路径
- **信号语义**：与 POSIX/Linux 一致（kill/sigaction/sigaltstack）
- **socket 接口**：AF_INET/AF_UNIX/AF_INET6 遵循 POSIX 标准
- **文件锁**：flock/POSIX record locks 遵循标准语义

**理由**：ABI 兼容是获得生态的最低成本路径。syscall 编号是 ABI 约定，不是内核实现细节。

### 2. 内核内部层 — 使用 EG 自己的方式

内核内部实现保持 Rust 纯净，借鉴 Linux 但融入 EG 自己风格：

- **进程状态**：使用 7 状态模型（Created/Ready/Running/Blocked/Zombie/Terminated/Frozen），比 Linux 的 TASK_RUNNING/TASK_INTERRUPTIBLE/TASK_UNINTERRUPTIBLE 更简洁
- **COW 实现**：简化为全局 BTreeMap 跟踪引用计数，不引入 Linux 的 per-VMA anon_vma + rmap 反向映射
- **Page Cache**：固定大小哈希表（64 桶 x 16 条目），不引入 Linux 的 xarray/radix tree
- **Swap**：固定数组 LRU，不引入 Linux 的 zone-based multi-LRU
- **CFS 调度器**：保留 vruntime + BTreeMap + 权重表核心算法，不引入 load_weight/load_avg 等复杂度
- **同步原语**：自研 SpinLock/Mutex/RwLock/RCU，不直接复制 Linux 的 locking 原语

**理由**：Linux 内部实现为大规模生产环境优化，复杂度极高。Edgine 作为学习/验证项目，应选择更简洁的实现，在保证功能正确的前提下降低维护成本。

### 3. 借鉴层 — Linux 验证过的最佳方式允许使用

Linux 验证过的成熟算法和设计模式可以在 Edgine 中使用：

- **CFS 调度算法**：vruntime 公平调度是经过 Linux 十几年验证的成熟方案
- **Tickless NO_HZ_IDLE**：空闲时停止 tick 是经过验证的省电技术
- **伙伴分配器**：经典的物理内存管理算法
- **Slab 分配器**：经典的内核对象缓存算法
- **ELF core dump 格式**：NT_PRSTATUS/NT_SIGINFO 是 ELF 标准，gdb/lldb 兼容

**理由**：不重复造轮子。Linux 社区投入了大量工程精力验证的算法，直接借鉴比从零实现更可靠。

## 二、具体判断规则

### 数据结构 — 允许简化借鉴，融入 EG 风格

| 类型 | 决策 | 示例 |
|------|------|------|
| 对外 ABI 兼容的数据结构 | ✅ 保持 Linux 布局 | PrStatus（core dump）、sigaction、sockaddr |
| 内核内部数据结构 | ⚠️ 借鉴核心思想，简化实现 | VMA（保留标志位但简化字段）、页表（保留四级结构但简化遍历） |
| Linux 高级数据结构 | ❌ 不引入 | xarray、radix tree、per-VMA anon_vma |

### 算法 — 允许简化借鉴，融入 EG 风格

| 类型 | 决策 | 示例 |
|------|------|------|
| 经典算法 | ✅ 直接借鉴核心思想 | CFS vruntime、伙伴分配、Slab、COW |
| Linux 高级优化 | ❌ 不引入 | load_weight、rmap 反向映射、zone-based LRU |
| 硬件相关算法 | ⚠️ 借鉴但简化 | IOMMU 映射（保留核心，简化 cache flush） |

### Linux 特有功能 — 移除未实现项

| 类型 | 决策 | 示例 |
|------|------|------|
| 已实现的 Linux 功能 | ✅ 保留 | CFS、epoll、futex、namespace |
| 定义但未实现的标志位 | ❌ 移除 | MADV_COLD、MADV_SOFT_OFFLINE、MADV_MERGEABLE |
| 定义但未实现的接口 | ❌ 移除或返回 EINVAL | MADV_DONTFORK、MADV_POPULATE_READ/WRITE |

### Debug 标记 — 保留经典标记

| 类型 | 决策 | 示例 |
|------|------|------|
| 堆 magic | ✅ 保留 0xDEADBEEF | kmalloc 检测 |
| 栈 canary | ✅ 保留经典值 | 进程栈保护 |
| 哈希常量 | ✅ 保留通用值 | 黄金比例、Murmur3 常量 |

### 代码风格 — 避免 Linux 命名

| 类型 | 决策 | 示例 |
|------|------|------|
| 对外接口命名 | ✅ 使用 Linux 命名 | SYS_open、EPOLLIN、AF_UNIX |
| 内核内部命名 | ❌ 避免 Linux 命名 | 用 Blocked 而非 TASK_INTERRUPTIBLE，用 EG 前缀常量 |
| 魔数 | ⚠️ 具名常量优先 | PR_SET_SECCOMP = 22 而非直接写 22 |

## 三、新功能决策流程

遇到新功能实现时，按以下步骤决策：

```
1. 先按 EG 需求设计 → 明确功能目标和接口
2. 检查是否涉及对外 ABI → 如果是，保持 Linux 兼容
3. 检查是否涉及内核内部 → 如果是，优先用 EG 自己的方式
4. 如果 EG 方案不确定 → 再参考 Linux 实现，简化后融入
5. 检查是否引入 Linux 特有复杂度 → 如果是，评估是否真正需要
```

**原则**：先设计再参考，而非先查 Linux 再简化。

## 四、与 ref-naming.md 的关系

- `ref-naming.md`：定义 ABI 层面的兼容策略（syscall 编号/路径/libc/工具链）
- 本文档：定义内核实现层面的兼容策略（内部实现/算法借鉴/复杂度控制）

两者共同构成 Edgine 的 Linux 兼容性完整立场。

## 五、交叉引用

- [ref-naming.md](./ref-naming.md) — 命名与 ABI 兼容立场
- [explain-framekernel.md](./explain-framekernel.md) — 框内核架构（privileged/functions 分层）
- [linux-compat-maintenance.md](../plan/linux-compat-maintenance.md) — Linux 兼容性维护工程
