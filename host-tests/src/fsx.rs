//! fsx — 文件系统 exerciser (Rust 实现)
//!
//! 参考 fsx-linux 的核心思想: 通过随机操作序列测试文件系统实现的正确性.
//! 支持 ext2/exfat/overlayfs/tmpfs 等文件系统.
//!
//! ## 操作类型
//!
//! - Create: 创建新文件
//! - Write: 写入数据到文件 (覆盖整个文件)
//! - Read: 读取文件并验证数据完整性
//! - Truncate: 截断文件到指定大小
//! - Delete: 删除文件
//! - Rename: 重命名文件

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// 操作统计
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub creates: u64,
    pub writes: u64,
    pub reads: u64,
    pub truncates: u64,
    pub deletes: u64,
    pub renames: u64,
    pub errors: u64,
}

/// 文件数据跟踪 (用于完整性验证)
#[derive(Debug, Clone)]
struct FileData {
    /// 文件当前内容
    content: Vec<u8>,
}

/// 构造进程隔离的 fsx 测试目录 (名称 + 进程 ID 后缀)
///
/// 固定目录名会让并发运行共用同一目录并互相追加内容: `cargo test` 下 lib 单测与
/// 集成测试是两个进程, 且同一仓库可能被多个会话并行运行 —— 一旦共用目录, 后续
/// 运行的"空目录"初始假设即与实际内容冲突, 触发数据完整性假失败 (实测见
/// `docs/plan/archive/host-tests-fsx-tmp-isolation.md`)。进程 ID 后缀保证每个进程独占目录。
pub fn isolated_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("edgine-fsx-{}-{}", name, std::process::id()))
}

/// fsx 配置
#[derive(Debug, Clone)]
pub struct FsxConfig {
    /// 测试目录
    pub test_dir: PathBuf,
    /// 操作次数
    pub num_operations: u64,
    /// 最大文件数量
    pub max_files: usize,
    /// 最大文件大小 (字节)
    pub max_file_size: usize,
    /// 随机种子
    pub seed: u64,
    /// 是否打印详细输出
    pub verbose: bool,
}

impl Default for FsxConfig {
    fn default() -> Self {
        Self {
            test_dir: isolated_test_dir("default"),
            num_operations: 1_000_000,
            max_files: 100,
            max_file_size: 64 * 1024, // 64KB
            seed: 0,
            verbose: false,
        }
    }
}

/// 简单的 xorshift64 随机数生成器
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn next_range(&mut self, max: usize) -> usize {
        (self.next() as usize) % max
    }
}

/// fsx exerciser
pub struct FsxFs {
    config: FsxConfig,
    rng: Rng,
    stats: Stats,
    /// 文件名 -> 文件数据跟踪
    files: HashMap<String, FileData>,
    /// 当前活跃文件列表
    active_files: Vec<String>,
    /// 全局操作计数器
    ops_count: AtomicU64,
}

impl FsxFs {
    pub fn new(config: FsxConfig) -> Self {
        let rng = Rng::new(config.seed);
        Self {
            config,
            rng,
            stats: Stats::default(),
            files: HashMap::new(),
            active_files: Vec::new(),
            ops_count: AtomicU64::new(0),
        }
    }

    /// 运行 fsx 测试
    pub fn run(&mut self) -> Result<Stats, String> {
        // 先清空测试目录: 上一次异常中断 (超时被杀 / 断言失败) 留下的残留文件会与
        // 本次运行的"空目录"初始假设冲突
        let _ = fs::remove_dir_all(&self.config.test_dir);
        // 创建测试目录
        fs::create_dir_all(&self.config.test_dir)
            .map_err(|e| format!("创建测试目录失败: {}", e))?;

        if self.config.verbose {
            println!("fsx: 开始测试, 操作次数: {}", self.config.num_operations);
            println!("fsx: 测试目录: {:?}", self.config.test_dir);
        }

        for op_num in 0..self.config.num_operations {
            self.ops_count.store(op_num, Ordering::Relaxed);

            if self.config.verbose && op_num % 100_000 == 0 {
                println!("fsx: 操作 {}/{}", op_num, self.config.num_operations);
            }

            // 随机选择操作
            let op = self.rng.next_range(6);
            match op {
                0 => self.op_create(),
                1 => self.op_write(),
                2 => self.op_read(),
                3 => self.op_truncate(),
                4 => self.op_delete(),
                5 => self.op_rename(),
                _ => unreachable!(),
            }
        }

        // 清理: 删除所有测试文件
        self.cleanup();

        if self.config.verbose {
            println!("fsx: 测试完成");
            println!("fsx: 统计: {:?}", self.stats);
        }

        Ok(self.stats.clone())
    }

    /// 创建新文件
    fn op_create(&mut self) {
        if self.active_files.len() >= self.config.max_files {
            return;
        }

        let filename = format!("file_{}", self.rng.next());
        let filepath = self.config.test_dir.join(&filename);

        // 生成随机内容
        let size = self.rng.next_range(self.config.max_file_size) + 1;
        let content: Vec<u8> = (0..size).map(|_| self.rng.next() as u8).collect();

        match fs::write(&filepath, &content) {
            Ok(_) => {
                self.files.insert(filename.clone(), FileData { content });
                self.active_files.push(filename);
                self.stats.creates += 1;
            }
            Err(_) => {
                self.stats.errors += 1;
            }
        }
    }

    /// 写入数据到文件 (覆盖整个文件)
    fn op_write(&mut self) {
        if self.active_files.is_empty() {
            return;
        }

        let idx = self.rng.next_range(self.active_files.len());
        let filename = &self.active_files[idx];
        let filepath = self.config.test_dir.join(filename);

        // 生成新的随机内容 (完全覆盖)
        let size = self.rng.next_range(self.config.max_file_size) + 1;
        let content: Vec<u8> = (0..size).map(|_| self.rng.next() as u8).collect();

        match fs::write(&filepath, &content) {
            Ok(_) => {
                if let Some(data) = self.files.get_mut(filename) {
                    data.content = content;
                }
                self.stats.writes += 1;
            }
            Err(_) => {
                self.stats.errors += 1;
            }
        }
    }

    /// 读取文件并验证数据完整性
    fn op_read(&mut self) {
        if self.active_files.is_empty() {
            return;
        }

        let idx = self.rng.next_range(self.active_files.len());
        let filename = &self.active_files[idx];
        let filepath = self.config.test_dir.join(filename);

        match fs::read(&filepath) {
            Ok(content) => {
                if let Some(file_data) = self.files.get(filename) {
                    // 验证数据完整性: 内容必须完全匹配
                    if content != file_data.content {
                        self.stats.errors += 1;
                        eprintln!(
                            "fsx: 数据完整性验证失败: {} (disk_len={}, expected_len={})",
                            filepath.display(),
                            content.len(),
                            file_data.content.len()
                        );
                        return;
                    }
                    self.stats.reads += 1;
                } else {
                    // 文件在磁盘上存在但不在跟踪表中 (不应该发生)
                    self.stats.errors += 1;
                    eprintln!("fsx: 文件未跟踪: {}", filepath.display());
                }
            }
            Err(_) => {
                self.stats.errors += 1;
            }
        }
    }

    /// 截断文件
    fn op_truncate(&mut self) {
        if self.active_files.is_empty() {
            return;
        }

        let idx = self.rng.next_range(self.active_files.len());
        let filename = &self.active_files[idx];
        let filepath = self.config.test_dir.join(filename);

        let new_size = self.rng.next_range(self.config.max_file_size);

        // 使用真正的 truncate 操作 (需要写权限)
        match fs::OpenOptions::new().write(true).open(&filepath) {
            Ok(file) => {
                match file.set_len(new_size as u64) {
                    Ok(_) => {
                        // 更新跟踪的内容
                        if let Some(data) = self.files.get_mut(filename) {
                            data.content.resize(new_size, 0);
                        }
                        self.stats.truncates += 1;
                    }
                    Err(_) => {
                        self.stats.errors += 1;
                    }
                }
            }
            Err(_) => {
                self.stats.errors += 1;
            }
        }
    }

    /// 删除文件
    fn op_delete(&mut self) {
        if self.active_files.is_empty() {
            return;
        }

        let idx = self.rng.next_range(self.active_files.len());
        let filename = self.active_files.remove(idx);
        let filepath = self.config.test_dir.join(&filename);

        match fs::remove_file(&filepath) {
            Ok(_) => {
                self.files.remove(&filename);
                self.stats.deletes += 1;
            }
            Err(_) => {
                self.stats.errors += 1;
            }
        }
    }

    /// 重命名文件
    fn op_rename(&mut self) {
        if self.active_files.is_empty() {
            return;
        }

        let idx = self.rng.next_range(self.active_files.len());
        let old_filename = self.active_files[idx].clone();
        let new_filename = format!("renamed_{}", self.rng.next());

        let old_path = self.config.test_dir.join(&old_filename);
        let new_path = self.config.test_dir.join(&new_filename);

        match fs::rename(&old_path, &new_path) {
            Ok(_) => {
                if let Some(data) = self.files.remove(&old_filename) {
                    self.files.insert(new_filename.clone(), data);
                    self.active_files[idx] = new_filename;
                    self.stats.renames += 1;
                } else {
                    // 文件不在跟踪表中 (不应该发生)
                    self.stats.errors += 1;
                    eprintln!("fsx: 重命名失败 - 文件未跟踪: {}", old_path.display());
                }
            }
            Err(_) => {
                self.stats.errors += 1;
            }
        }
    }

    /// 清理测试目录
    fn cleanup(&self) {
        let _ = fs::remove_dir_all(&self.config.test_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fsx_basic() {
        let config = FsxConfig {
            test_dir: isolated_test_dir("unit-basic"),
            num_operations: 1000,
            max_files: 10,
            max_file_size: 1024,
            seed: 42,
            verbose: false,
        };

        let mut fsx = FsxFs::new(config);
        let stats = fsx.run().unwrap();

        println!("fsx basic test stats: {:?}", stats);
        assert!(stats.errors == 0, "fsx 测试出现错误: {:?}", stats);
    }

    #[test]
    fn test_fsx_stress() {
        let config = FsxConfig {
            test_dir: isolated_test_dir("unit-stress"),
            num_operations: 100_000,
            max_files: 50,
            max_file_size: 4096,
            seed: 12345,
            verbose: false,
        };

        let mut fsx = FsxFs::new(config);
        let stats = fsx.run().unwrap();

        println!("fsx stress test stats: {:?}", stats);
        assert!(stats.errors == 0, "fsx 压力测试出现错误: {:?}", stats);
    }
}
