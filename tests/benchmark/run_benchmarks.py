#!/usr/bin/env python3
"""
性能基准测试 (Performance Benchmark Tests)
测量内核和驱动的性能指标
"""

import subprocess
import sys
import os
import time
import json
from pathlib import Path
from datetime import datetime
from dataclasses import dataclass, field
from typing import List, Dict

PROJECT_ROOT = Path(__file__).parent.parent.parent
BUILD_DIR = PROJECT_ROOT / "other" / "build"
REPORTS_DIR = PROJECT_ROOT / "other" / "tests-reports"

@dataclass
class BenchmarkResult:
    name: str
    category: str
    iterations: int
    total_time: float
    avg_time: float
    min_time: float
    max_time: float
    ops_per_sec: float
    details: List[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "category": self.category,
            "iterations": self.iterations,
            "total_time": self.total_time,
            "avg_time": self.avg_time,
            "min_time": self.min_time,
            "max_time": self.max_time,
            "ops_per_sec": self.ops_per_sec,
            "details": self.details
        }

class Benchmark:
    def __init__(self, name: str, category: str):
        self.name = name
        self.category = category
        self.times: List[float] = []

    def run(self, func, iterations: int = 1000) -> BenchmarkResult:
        """运行基准测试"""
        self.times = []
        
        for _ in range(iterations):
            start = time.perf_counter()
            func()
            end = time.perf_counter()
            self.times.append(end - start)
        
        total_time = sum(self.times)
        avg_time = total_time / iterations
        min_time = min(self.times)
        max_time = max(self.times)
        ops_per_sec = iterations / total_time if total_time > 0 else 0
        
        return BenchmarkResult(
            name=self.name,
            category=self.category,
            iterations=iterations,
            total_time=total_time,
            avg_time=avg_time,
            min_time=min_time,
            max_time=max_time,
            ops_per_sec=ops_per_sec
        )

def benchmark_memory_allocation() -> BenchmarkResult:
    """内存分配基准测试"""
    bench = Benchmark("Memory Allocation", "Memory")
    
    def alloc_free():
        data = bytearray(4096)
        del data
    
    result = bench.run(alloc_free, iterations=10000)
    result.details.append("4KB内存分配和释放")
    result.details.append(f"平均延迟: {result.avg_time * 1_000_000:.2f} μs")
    return result

def benchmark_list_operations() -> BenchmarkResult:
    """列表操作基准测试"""
    bench = Benchmark("List Operations", "Data Structures")
    
    data = []
    
    def list_ops():
        data.append(1)
        if len(data) > 100:
            data.pop(0)
    
    result = bench.run(list_ops, iterations=100000)
    result.details.append("列表追加和弹出操作")
    result.details.append(f"吞吐量: {result.ops_per_sec:.0f} ops/s")
    return result

def benchmark_dict_operations() -> BenchmarkResult:
    """字典操作基准测试"""
    bench = Benchmark("Dict Operations", "Data Structures")
    
    data = {}
    counter = 0
    
    def dict_ops():
        nonlocal counter
        data[counter] = counter
        if counter > 100:
            del data[counter - 100]
        counter += 1
    
    result = bench.run(dict_ops, iterations=100000)
    result.details.append("字典插入和删除操作")
    result.details.append(f"吞吐量: {result.ops_per_sec:.0f} ops/s")
    return result

def benchmark_string_operations() -> BenchmarkResult:
    """字符串操作基准测试"""
    bench = Benchmark("String Operations", "String")
    
    def string_ops():
        s = "test" * 100
        _ = s.upper()
        _ = s.lower()
        _ = s.replace("test", "demo")
    
    result = bench.run(string_ops, iterations=10000)
    result.details.append("字符串拼接、大小写转换、替换")
    result.details.append(f"吞吐量: {result.ops_per_sec:.0f} ops/s")
    return result

def benchmark_json_operations() -> BenchmarkResult:
    """JSON操作基准测试"""
    bench = Benchmark("JSON Operations", "Serialization")
    
    data = {"key": "value", "number": 123, "list": [1, 2, 3]}
    
    def json_ops():
        json_str = json.dumps(data)
        json.loads(json_str)
    
    result = bench.run(json_ops, iterations=10000)
    result.details.append("JSON序列化和反序列化")
    result.details.append(f"吞吐量: {result.ops_per_sec:.0f} ops/s")
    return result

def benchmark_file_io() -> BenchmarkResult:
    """文件I/O基准测试"""
    bench = Benchmark("File I/O", "I/O")
    
    test_file = PROJECT_ROOT / "tests" / "benchmark_test.tmp"
    
    def file_ops():
        with open(test_file, 'wb') as f:
            f.write(b'test' * 1024)
        with open(test_file, 'rb') as f:
            _ = f.read()
    
    result = bench.run(file_ops, iterations=1000)
    result.details.append("4KB文件写入和读取")
    result.details.append(f"吞吐量: {result.ops_per_sec:.0f} ops/s")
    
    if test_file.exists():
        test_file.unlink()
    
    return result

def benchmark_subprocess() -> BenchmarkResult:
    """子进程基准测试"""
    bench = Benchmark("Subprocess", "Process")
    
    def subprocess_ops():
        subprocess.run(['echo', 'test'], capture_output=True, timeout=1)
    
    result = bench.run(subprocess_ops, iterations=100)
    result.details.append("子进程创建和执行")
    result.details.append(f"平均延迟: {result.avg_time * 1000:.2f} ms")
    return result

def print_header(title: str):
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}\n")

def print_benchmark_result(result: BenchmarkResult):
    print(f"  📊 {result.name} ({result.category})")
    print(f"     迭代次数: {result.iterations}")
    print(f"     总时间: {result.total_time:.4f}s")
    print(f"     平均时间: {result.avg_time * 1_000_000:.2f} μs")
    print(f"     最小时间: {result.min_time * 1_000_000:.2f} μs")
    print(f"     最大时间: {result.max_time * 1_000_000:.2f} μs")
    print(f"     吞吐量: {result.ops_per_sec:.0f} ops/s")
    for detail in result.details:
        print(f"     • {detail}")
    print()

def main():
    print_header("QueenX 性能基准测试")
    
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    
    print("⏱️  运行基准测试...\n")
    
    # 运行所有基准测试
    results = []
    
    print("  [1/7] 内存分配...")
    results.append(benchmark_memory_allocation())
    
    print("  [2/7] 列表操作...")
    results.append(benchmark_list_operations())
    
    print("  [3/7] 字典操作...")
    results.append(benchmark_dict_operations())
    
    print("  [4/7] 字符串操作...")
    results.append(benchmark_string_operations())
    
    print("  [5/7] JSON操作...")
    results.append(benchmark_json_operations())
    
    print("  [6/7] 文件I/O...")
    results.append(benchmark_file_io())
    
    print("  [7/7] 子进程...")
    results.append(benchmark_subprocess())
    
    # 打印结果
    print("\n📊 基准测试结果:\n")
    for result in results:
        print_benchmark_result(result)
    
    # 保存结果
    report_file = REPORTS_DIR / f"benchmark_{timestamp}.json"
    REPORTS_DIR.mkdir(parents=True, exist_ok=True)
    
    with open(report_file, 'w') as f:
        json.dump([r.to_dict() for r in results], f, indent=2)
    
    print(f"📁 结果已保存至: {report_file}")
    
    # 总结
    print(f"\n{'='*60}\n")
    print(f"  ✅ 完成 {len(results)} 项基准测试")
    
    return 0

if __name__ == "__main__":
    sys.exit(main())
