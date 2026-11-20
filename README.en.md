# Edgine Kernel

Edgine is a from-scratch kernel implemented in Rust, designed and developed based on the Asterinas Framekernel architecture and paradigm, with some unique features designed specifically for it.

Edgine is first and foremost a hobby and experimental kernel. It does not aim to be production-ready and is not recommended for any production use; that identity holds even if it grows stable and reliable over time. Being experimental is not an exemption from quality.

> **中文版本**: [README.md](README.md)

## Project Entry Points

- **Human developers / readers**: Please refer to the [`docs/explain/`](docs/explain/) directory for development and architecture guidance.
- **AI / Agent developers**: Please read [`AGENTS.md`](AGENTS.md) in the project root and strictly follow the rules therein. After doing so, follow its guidance to read further related documents.

## A Note on Development Mode

This project is built in a **human-on-the-loop, human-AI collaborative model**: the human sits above the loop, owning direction, architecture decisions, boundary constraints, and final review, while AI / agents autonomously carry out planning and implementation within the constraints and oversight the human sets. The model yields high development throughput, and the code may therefore carry the traces and flaws common to AI-generated code. If you find any questionable or erroneous code, your reports and contributions are welcome. To keep AI implementation in check, this project enforces an executable engineering gate: the core static audits are wired as fail-closed checks into the local `ci/audit.sh` (full tier) and `Makefile.ci` (full list in [`AGENTS.md`](AGENTS.md) §2.2).

## License

Edgine itself is released under the Mozilla Public License 2.0; see [LICENSE](LICENSE).

## External Dependency Notice

This project relies on the following external project code in its design and implementation. All copyright and license terms remain with their respective authors:

**Third-party source vendored into this repository**

- **smoltcp** — the TCP/IP stack, vendored (inlined) into this repository at [`src/kernel/functions/net/smoltcp/`](src/kernel/functions/net/smoltcp/). Version v0.14.0, licensed under 0BSD (the upstream `LICENSE-0BSD.txt` is retained), with local adaptations on top of upstream (lint suppressions, `SAFETY` comments, `no_std` adaptation). Upstream project: <https://github.com/smoltcp-rs/smoltcp>.

**Third-party crates pulled in via Cargo** (not distributed with this repository)

- **Common ecosystem dependencies**: `spin`, `bitflags`, `zerocopy`, etc. — standard building blocks of the Rust `no_std` ecosystem.
- **ed25519-dalek** (BSD-3-Clause OR Apache-2.0): used for Ed25519 signature verification of secure-boot images.
- **serde**, `serde_json`: used only for host-side tests and excluded from the kernel build.

## Feedback and Contributions

Bug reports and contributions are welcome:

- **Primary repository**: [Gitee](https://gitee.com/AnferLagbu/Edgine)
- **Mirror repository**: [GitHub](https://github.com/AnferLagbu/Edgine)

Both bug reports and contributions are appreciated — thank you for your interest and contributions to this project.

### On Criticism and Discussion

Criticism, skepticism, analysis and suggestions aimed at the project itself are welcome — including challenges to its architecture, its code quality, and whether these gates really do constrain the quality of AI-produced code. Outside perspectives surface blind spots; pointing out a problem is itself a contribution.

Work here is divided as follows: the human owns direction, architecture, boundaries, acceptance criteria and final review; the AI implements; and no implementation counts as done until it passes the same executable gates. Those criteria — static audits, dual-architecture builds, host and kernel unit tests, QEMU integration, rustdoc — are all public in this repository, and anyone can re-run them to judge quality for themselves: `bash ci/audit.sh full`, `make test-host`, `make test-kernel-host`, `make -f Makefile.ci ci-rustdoc`.

So please argue with specific locations (file / function / line) or reproducible readings; such feedback is accepted in full. What we do not respond to is personal attacks, speculation about motives, and dismissive labels.