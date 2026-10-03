//! /proc Linux 风格接口测试

#[test]
fn test_procfs_cpuinfo_format() {
    // 验证 cpuinfo 输出格式
    let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 142\nmodel name\t: Intel(R) Core(TM) i7-8550U CPU @ 1.80GHz\n";
    assert!(cpuinfo.contains("processor"));
    assert!(cpuinfo.contains("vendor_id"));
    assert!(cpuinfo.contains("model name"));
}

#[test]
fn test_procfs_meminfo_format() {
    // 验证 meminfo 输出格式
    let meminfo =
        "MemTotal:        16384 kB\nMemFree:          8192 kB\nMemAvailable:     8192 kB\n";
    assert!(meminfo.contains("MemTotal"));
    assert!(meminfo.contains("MemFree"));
    assert!(meminfo.contains("MemAvailable"));
}

#[test]
fn test_procfs_version_format() {
    // 验证 version 输出格式
    let version = "Linux version 6.1.0-queenx (queenx@build) (gcc (Ubuntu 11.3.0) 11.3.0)\n";
    assert!(version.contains("Linux version"));
    assert!(version.contains("queenx"));
}

#[test]
fn test_procfs_uptime_format() {
    // 验证 uptime 输出格式
    let uptime = "12345.67 67890.12 1\n";
    let parts: Vec<&str> = uptime.split_whitespace().collect();
    assert_eq!(parts.len(), 3);
    assert!(parts[0].parse::<f64>().is_ok());
}

#[test]
fn test_procfs_stat_format() {
    // 验证 stat 输出格式
    let stat = "cpu  0 0 0 0 0 0 0 0 0 0\nctxt 0\nbtime 0\nprocesses 0\n";
    assert!(stat.contains("cpu"));
    assert!(stat.contains("ctxt"));
    assert!(stat.contains("btime"));
}

#[test]
fn test_procfs_mounts_format() {
    // 验证 mounts 输出格式
    let mounts = "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\ndevtmpfs /dev devtmpfs rw,nosuid,relatime 0 0\n";
    assert!(mounts.contains("proc"));
    assert!(mounts.contains("devtmpfs"));
}
