# QueenX Kernel

QueenX is a from-scratch kernel implemented in Rust, designed and developed based on the Asterinas Framekernel architecture and paradigm. It is currently experimental, and includes some unique features designed specifically for it.

> **中文版本**: [README.md](README.md)

## Project Entry Points

- **Human developers / readers**: Please refer to the [`docs/explain/`](docs/explain/) directory for development and architecture guidance.
- **AI / Agent developers**: Please read [`AGENTS.md`](AGENTS.md) in the project root and strictly follow the rules therein. After doing so, follow its guidance to read further related documents.

## A Note on Development Mode

This project is built in a **human-in-the-loop, human-AI collaborative model**: the human owns direction, architecture decisions, and final review, while AI / agents carry out the implementation. The model yields high development throughput, and the code may therefore carry the traces and flaws common to machine-generated sources. If you find any questionable or erroneous code, your reports and contributions are welcome.

## External Dependency Notice

This project relies on the following external project code in its design and implementation. All copyright and license terms remain with their respective authors:

**Third-party source vendored into this repository**

- **smoltcp** — the TCP/IP stack, vendored (inlined) into this repository at [`src/kernel/services/net/smoltcp/`](src/kernel/services/net/smoltcp/). Version v0.14.0, licensed under 0BSD (the upstream `LICENSE-0BSD.txt` is retained), with local adaptations on top of upstream (lint suppressions, `SAFETY` comments, `no_std` adaptation). Upstream project: <https://github.com/smoltcp-rs/smoltcp>.

**Third-party crates pulled in via Cargo** (not distributed with this repository)

- **Common ecosystem dependencies**: `spin`, `bitflags`, `zerocopy`, etc. — standard building blocks of the Rust `no_std` ecosystem, consistent with those used by the Asterinas framekernel.
- **ed25519-dalek** (BSD-3-Clause OR Apache-2.0): used for Ed25519 signature verification of secure-boot images.
- **serde**, `serde_json`: used only for host-side tests and excluded from the kernel build.

## Feedback and Contributions

Bug reports and contributions are welcome:

- **Primary repository**: [Gitee](https://gitee.com/AnferLagbu/QueenX)
- **Mirror repository**: [GitHub](https://github.com/AnferLagbu/QueenX)

Both bug reports and contributions are appreciated — thank you for your interest and contributions to this project.