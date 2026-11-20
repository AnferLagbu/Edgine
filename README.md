# Edgine Kernel

Edgine 是一个从零实现的内核，使用 Rust 编写，基于 Asterinas 框内核（Framekernel）架构与范式进行设计与开发，并在此基础上设计了一些独特的功能与特性。

Edgine 的首要身份是个人爱好与实验性内核，不以生产可用为目标，也不建议用于任何生产环境；即便日后逐渐变得稳定可靠，这一定位也不改变。定位上的实验性不构成质量上的豁免。

> **English version**: [README.en.md](README.en.md)

## 项目入口

- **人类开发者 / 阅读者**：请前往 [`docs/explain/`](docs/explain/) 目录查阅开发与架构指引文档。
- **AI / Agent 开发者**：请阅读项目根目录下的 [`AGENTS.md`](AGENTS.md) 并严格遵循其中的规则。阅读后可根据其导览阅读更多其他关联文档。

## 开发模式说明

本项目采用**人在环上（human-on-the-loop）的人机协作模式**开发：人居于环上，负责方向规划、架构决策、边界约束与最终审查，AI / Agent 在其设定的约束与监督下自主完成具体的规划与编码实施。这一模式带来了可观的开发效率，也让代码可能带有 AI 生成代码常见的痕迹与瑕疵。若你发现了任何可疑或错误的代码，欢迎指出与贡献。为约束 AI 实施质量，本项目建立了可执行的工程门禁体系：核心静态审计以 fail-closed 方式接入本地 `ci/audit.sh`（full 档）与 `Makefile.ci`，完整清单见 `AGENTS.md` §2.2。

## 外部依赖声明

本项目在设计与实现中依赖以下外部项目代码，其版权与许可条款均归各自原作者所有：

**树内并入的第三方源码**

- **smoltcp** — TCP/IP 协议栈，以 vendored（本地并入）方式引入本仓库，位于 [`src/kernel/functions/net/smoltcp/`](src/kernel/functions/net/smoltcp/)。版本 v0.14.0，采用 0BSD 许可（保留上游许可文件 `LICENSE-0BSD.txt`），并在上游基础上做了本地化适配（lint 抑制、`SAFETY` 注释、`no_std` 适配）。上游项目：<https://github.com/smoltcp-rs/smoltcp>。

**经 Cargo 依赖引入的第三方 crate**（不随本仓库分发）

- **通用生态依赖**：`spin`、`bitflags`、`zerocopy` 等，Rust `no_std` 通用基础设施 crate。
- **ed25519-dalek**（BSD-3-Clause OR Apache-2.0）：用于安全启动镜像的 Ed25519 签名验证。
- **serde**、`serde_json`：仅用于宿主端测试，不进入内核构建。

## 反馈与协作

欢迎提交问题报告与贡献：

- **主仓库**：[Gitee](https://gitee.com/AnferLagbu/Edgine)
- **镜像仓库**：[GitHub](https://github.com/AnferLagbu/Edgine)

无论是问题报告还是贡献提交，我都会关注，并由衷感谢你对本项目的关注与贡献。

### 关于评价与讨论

本项目欢迎一切指向项目本身的批评、质疑、分析与建议，包括对架构选择、代码质量，以及「这套门禁是否真能约束住 AI 实施质量」的质疑。外部视角是发现盲区的重要来源，指出问题本身就是贡献。

本项目不以「纯手写」为卖点，也不回避 AI 参与的事实：人负责方向、架构、边界与验收判据的设定和终审，AI 负责实施，而实施产物必须通过同一套可执行门禁才算完成。这套判据（静态审计、双架构构建、host 与内核单元测试、QEMU 集成、rustdoc）全部公开在仓库里，任何人都能自行复跑并据此判断质量——`bash ci/audit.sh full`、`make test-host`、`make test-kernel-host`、`make -f Makefile.ci ci-rustdoc`。

因此，请尽量用具体位置（文件 / 函数 / 行）或可复现的读数来说话，这样的意见我们照单全收。不回应的是人身攻击、动机揣测与贬损性标签：对代码的判断请用代码与读数，对作者本人的评价与项目质量无关。