# Bochs VBE DISPI 端口回退分支覆盖缺口报告

## 一、这份报告要说明什么

R11（DISPI 端口访问收敛到 `IoPort` 安全代理）结册时，登记了一项**运行期覆盖缺口**：
本地 `port_outw` / `port_inw` 原语已删除、端口访问路径已改经 `IoPort`，但**默认 QEMU
启动配置不保证真正执行到端口回退分支**。因此「构建通过 + QEMU 2/2 通过」只证明
**没有引入回归**，不证明**该分支被实际执行过**。

本报告把这个缺口的调用链位置、判定边界、已有证据与关闭路径一次性写清，作为后续
是否补专用运行期用例的决策输入。正文为一次性快照，修订按 `AGENTS.md` §6 另开文档或
追加修订记录。

## 二、分支在显示初始化链上的位置

显示初始化按「优先用引导器给的帧缓冲，失败才自行探测」分层：

1. `display_init()` 先读 Multiboot2 帧缓冲信息；可用则直接采用，**不进入**设备探测。
2. 不可用时调用 `probe_vga_fb_via_pci()`，遍历 `CLASS_DISPLAY` 中 `subclass = 0x00`
   的 VGA 设备，在首个探测成功处返回。
3. 单个设备先走 **MMIO / VBE 路径**；该路径失败后才调用 `read_bochs_disp_mode()`，
   即本报告关注的 **DISPI 端口回退分支**。
4. `read_bochs_disp_mode()` 通过 `privileged::ioport::IoPort` 句柄，按
   `ID → ENABLE → XRES → YRES → BPP` 顺序读 Bochs VBE DISPI 寄存器；任一前提不满足
   即返回 `None`，调用方继续下一个设备或最终放弃显示。

涉及位置：

- `src/kernel/privileged/driver/display/mod.rs`：`display_init`、`probe_vga_fb_via_pci`、
  `read_bochs_disp_mode`、`VBE_DISPI_PORT_INDEX` / `VBE_DISPI_PORT_DATA` 与
  `VBE_DISPI_DATA_OFFSET`。
- `src/kernel/privileged/ioport.rs`：`IoPort::new`、`check_offset`、`read_u16`、`write_u16`。

## 三、端口与访问语义

| 项 | 值 / 语义 |
|---|---|
| 索引口 | `VBE_DISPI_PORT_INDEX = 0x01CE` |
| 数据口 | `VBE_DISPI_PORT_DATA = 0x01CF` |
| 访问宽度 | 16 位（`in ax, dx` / `out dx, ax`） |
| 句柄区间 | 基址 0x01CE、长度 3（0x01CE..0x01D0） |
| 偏移 | 索引口 0；数据口 `VBE_DISPI_PORT_DATA - VBE_DISPI_PORT_INDEX = 1` |

16 位 in/out 各跨两个字节端口，故长度必须取 3：`IoPort::check_offset` 以
`offset + size > len` 判越界并 panic，数据口偏移 1、`size = 2`，若长度取 2 则
`1 + 2 > 2` 必然 panic。长度取 3 后，偏移 0 与偏移 1 的 16 位访问都是编译期常量，
`check_offset` 的 panic 分支不可达。该订正是 R11 施工时对计划原文（长度 2）的
必要修正。

## 四、为什么默认 QEMU 启动不保证命中该分支

`scripts/qemu_boot_test.sh` 以 `-display none` 启动 QEMU，而内核的显示初始化有两条
更早的成功路径：

1. **Multiboot2 帧缓冲可用**：GRUB 在常规启动配置下会提供 framebuffer tag，
   `display_init()` 走第一步即返回，根本不会调用 `probe_vga_fb_via_pci()`。
2. **MMIO / VBE 路径成功**：即使 Multiboot2 信息不可用，QEMU 标准 VGA 设备的 VBE
   MMIO 探测通常在端口回退之前就成功，仍然到不了 `read_bochs_disp_mode()`。

因此 full 档 QEMU 2/2 通过的含义是「启动、调度、文件系统、网络等主链路未回归」，
而不是「DISPI 端口回退分支被执行且读数正确」。

## 五、已经拿到的证据及其边界

- **编译面**：本地 `port_outw` / `port_inw` 两个 `unsafe` 原语已删除；`read_bochs_disp_mode`
  改经 `IoPort` 后，x86_64 与 aarch64 双架构 `./ci/build.sh all` 均为
  Passed 6 / Failed 0。
- **语义面**：`IoPort::write_u16` / `read_u16` 与旧原语使用同一 `out dx, ax` /
  `in ax, dx` 指令，寄存器序列与端口常量未改动，差异只在安全包装层。
- **边界面**：句柄长度取 3 的推导见第三节；固定偏移均为编译期常量，
  越界 panic 分支不可达。
- **运行面**：`bash ci/audit.sh full` RC=0（QEMU 2/2、Lockbud 4/4、未执行腿数 0），
  `make test-host` 212 套 / 1653 passed，`make test-kernel-host` 3 套 / 1431 passed，
  `ci-rustdoc` 双架构 exit 0。这些读数**不构成**端口分支已执行的证据。

结论：现有证据足以判定「改动不引入回归」，但不足以判定「端口寄存器序列在真实硬件/
固件组合下读数正确」。

## 六、风险与影响

- **语义等价**使回归风险低：指令、端口、寄存器序列均未变。
- **失败形态温和**：探测前提不满足时返回 `None`，显示初始化退化为无帧缓冲，不会
  触发 `unsafe` 违规或内存破坏。
- **缺少运行期网**：若后续有人改动寄存器序列、偏移或句柄长度，当前门禁不会在运行期
  发现读数错误；编译期只能保证类型与越界边界。
- **SMP 前提未变**：该分支的唯一消费者与「仅 BSP 启动期探测」的互斥依据仍成立，
  与本次收敛无关。

## 七、关闭缺口的可选路径

**方案 A（推荐，未施工）**：新增专用 QEMU 腿，使启动**必然**落进端口回退分支并断言
探测结果。可选手段：

- GRUB 配置 `set gfxpayload=text` 或不提供 framebuffer tag 的 multiboot header，
  使 Multiboot2 帧缓冲信息不可用；
- 在测试构建中加只读探针，记录 `display_init` 选择了哪条路径，并断言出现
  「MMIO 失败 → 端口探测」标记；
- 断言 `read_bochs_disp_mode` 返回的 `(xres, yres, bpp)` 与 QEMU 的 DISPI 配置一致。

代价：需要一条独立 QEMU 启动配置与一条日志断言；不影响现有门槛。

**方案 B（不建议）**：把端口 I/O 抽象成可注入后端 trait，host 侧用假后端验证寄存器
序列。为单一用例引入抽象层，按项目「简约实现为默认」的准则不划算。

**方案 C（当前状态）**：维持已登记缺口，不新增用例；在显示路径进入发布关键路径前
接受该风险。

建议：若显示路径后续被纳入发布关键路径，按方案 A 补运行期用例；在此之前保持方案 C，
并由本报告承担「缺口已识别、证据边界已说明」的记录职责。

## 八、复现与取证命令

```bash
# 默认启动（不保证命中端口回退分支，用于验证不回归）
./scripts/qemu_boot_test.sh all

# full 档全部门禁（含 QEMU 2/2 与未执行腿数检查）
CARGO_TERM_COLOR=never bash ci/audit.sh full

# 双架构构建
./ci/build.sh all
```

## 九、相关记录

- 已归档计划：`docs/plan/archive/safety-gap-remediation.md`（R11 条目与结册读数）。
- 活跃任务基线：`docs/plan/progress-active-tasks.md`（乙表已归档行）。
- 端口代理实现：`src/kernel/privileged/ioport.rs`。
- 显示探测实现：`src/kernel/privileged/driver/display/mod.rs`。
