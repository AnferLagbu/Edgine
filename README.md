# Edgine Kernel

Edgine 是一个从零实现的内核，使用 Rust 编写，基于 Asterinas 框内核（Framekernel）架构与范式进行设计与开发。它目前是实验性的，并在此基础上设计了一些独特的功能与特性。

> **English version**: [README.en.md](README.en.md)

## 项目入口

- **人类开发者 / 阅读者**：请前往 [`docs/explain/`](docs/explain/) 目录查阅开发与架构指引文档。
- **AI / Agent 开发者**：请阅读项目根目录下的 [`AGENTS.md`](AGENTS.md) 并严格遵循其中的规则。阅读后可根据其导览阅读更多其他关联文档。

## 开发模式说明

本项目采用**人在回路（human-in-the-loop）的人机协作模式**开发：人负责方向规划、架构决策与最终审查，AI / Agent 负责具体的编码实施。这一模式带来了可观的开发效率，也让代码可能带有机器生成源常见的痕迹与瑕疵。若你发现了任何可疑或错误的代码，欢迎指出与贡献。

## 外部依赖声明

本项目在设计与实现中依赖以下外部项目代码，其版权与许可条款均归各自原作者所有：

**树内并入的第三方源码**

- **smoltcp** — TCP/IP 协议栈，以 vendored（本地并入）方式引入本仓库，位于 [`src/kernel/services/net/smoltcp/`](src/kernel/services/net/smoltcp/)。版本 v0.14.0，采用 0BSD 许可（保留上游许可文件 `LICENSE-0BSD.txt`），并在上游基础上做了本地化适配（lint 抑制、`SAFETY` 注释、`no_std` 适配）。上游项目：<https://github.com/smoltcp-rs/smoltcp>。

**经 Cargo 依赖引入的第三方 crate**（不随本仓库分发）

- **通用生态依赖**：`spin`、`bitflags`、`zerocopy` 等，为 Rust `no_std` 生态的标准配置，与 Asterinas 框内核所用一致。
- **ed25519-dalek**（BSD-3-Clause OR Apache-2.0）：用于安全启动镜像的 Ed25519 签名验证。
- **serde**、`serde_json`：仅用于宿主端测试，不进入内核构建。

## 反馈与协作

欢迎提交问题报告与贡献：

- **主仓库**：[Gitee](https://gitee.com/AnferLagbu/Edgine)
- **镜像仓库**：[GitHub](https://github.com/AnferLagbu/Edgine)

无论是问题报告还是贡献提交，我都会关注，并由衷感谢你对本项目的关注与贡献。