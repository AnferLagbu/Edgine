//! Framekernel Bench 编排器二进制
//!
//! 运行 `framekernel_bench::run_all()` 并输出 JSON 到 stdout.
//! stdout 格式 (单行, 便于管道):
//!   `{"version":1,"results":[{"name":"...","ns_per_op":N,"ops_per_sec":N}, ...]}`
//!
//! 配套判据:
//! - `scripts/record_bench_baseline.py`  记录 baseline.json (读数 + 绝对预算)
//! - `host-tests/tests/perf_metric_test.rs`  按预算判定 (release 实测 vs `budget_ns_per_op`)

use std::process::ExitCode;

use edgine_host_tests::framekernel_bench;

fn main() -> ExitCode {
    let report = framekernel_bench::run_all();
    let json = serde_json::to_string(&report).expect("serialize");
    println!("{json}");
    ExitCode::SUCCESS
}
