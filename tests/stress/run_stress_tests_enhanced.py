#!/usr/bin/env python3
"""
压力测试 (Stress Tests)
长时间运行的压力测试，验证系统稳定性
"""

import subprocess
import sys
import os
import time
import threading
import multiprocessing
from pathlib import Path
from datetime import datetime
from dataclasses import dataclass, field
from typing import List, Callable
import random

PROJECT_ROOT = Path(__file__).parent.parent.parent
REPORTS_DIR = PROJECT_ROOT / "other" / "tests-reports"

@dataclass
class StressTestResult:
    name: str
    duration: float
    operations: int
    errors: int
    success_rate: float
    details: List[str] = field(default_factory=list)

class StressTest:
    def __init__(self, name: str, duration: float):
        self.name = name
        self.duration = duration
        self.operations = 0
        self.errors = 0
        self.running = True

    def stop(self):
        self.running = False

    def run(self, func: Callable) -> StressTestResult:
        """运行压力测试"""
        start_time = time.time()
        end_time = start_time + self.duration
        
        while time.time() < end_time and self.running:
            try:
                func()
                self.operations += 1
            except Exception as e:
                self.errors += 1
        
        actual_duration = time.time() - start_time
        success_rate = (self.operations / (self.operations + self.errors) * 100 
                       if (self.operations + self.errors) > 0 else 0)
        
        return StressTestResult(
            name=self.name,
            duration=actual_duration,
            operations=self.operations,
            errors=self.errors,
            success_rate=success_rate
        )

def stress_memory_allocation() -> StressTestResult:
    """内存分配压力测试"""
    test = StressTest("Memory Allocation Stress", 10.0)
    
    allocations = []
    
    def alloc_free():
        if random.random() > 0.5:
            allocations.append(bytearray(random.randint(1024, 65536)))
        elif allocations:
            allocations.pop(random.randint(0, len(allocations) - 1))
    
    result = test.run(alloc_free)
    result.details.append(f"峰值内存对象: {len(allocations)}")
    result.details.append(f"操作速率: {result.operations / result.duration:.0f} ops/s")
    return result

def stress_file_io() -> StressTestResult:
    """文件I/O压力测试"""
    test = StressTest("File I/O Stress", 10.0)
    
    test_dir = PROJECT_ROOT / "tests" / "stress_test_tmp"
    test_dir.mkdir(exist_ok=True)
    
    file_counter = 0
    
    def file_ops():
        nonlocal file_counter
        file_counter += 1
        test_file = test_dir / f"stress_{file_counter % 100}.tmp"
        
        with open(test_file, 'wb') as f:
            f.write(os.urandom(4096))
        
        with open(test_file, 'rb') as f:
            _ = f.read()
    
    result = test.run(file_ops)
    
    for f in test_dir.glob("*.tmp"):
        f.unlink()
    test_dir.rmdir()
    
    result.details.append(f"操作速率: {result.operations / result.duration:.0f} ops/s")
    return result

def stress_subprocess() -> StressTestResult:
    """子进程压力测试"""
    test = StressTest("Subprocess Stress", 10.0)
    
    def subprocess_ops():
        subprocess.run(
            ['echo', 'stress'],
            capture_output=True,
            timeout=1
        )
    
    result = test.run(subprocess_ops)
    result.details.append(f"操作速率: {result.operations / result.duration:.0f} ops/s")
    return result

def stress_threading() -> StressTestResult:
    """多线程压力测试"""
    test = StressTest("Threading Stress", 10.0)
    
    counter = [0]
    lock = threading.Lock()
    
    def thread_ops():
        with lock:
            counter[0] += 1
    
    threads = []
    for _ in range(10):
        t = threading.Thread(target=lambda: [
            thread_ops() for _ in range(1000)
        ])
        threads.append(t)
        t.start()
    
    for t in threads:
        t.join()
    
    result = test.run(thread_ops)
    result.details.append(f"计数器值: {counter[0]}")
    result.details.append(f"操作速率: {result.operations / result.duration:.0f} ops/s")
    return result

def stress_multiprocessing() -> StressTestResult:
    """多进程压力测试"""
    test = StressTest("Multiprocessing Stress", 5.0)
    
    def worker(n):
        return n * n
    
    processed = [0]
    
    def mp_ops():
        try:
            with multiprocessing.Pool(2, maxtasksperchild=100) as pool:
                result = pool.map(worker, range(10))
                processed[0] += len(result)
        except Exception:
            pass
    
    result = test.run(mp_ops)
    result.details.append(f"处理数据: {processed[0]}")
    result.details.append(f"操作速率: {result.operations / result.duration:.0f} ops/s")
    return result

def stress_mixed_operations() -> StressTestResult:
    """混合操作压力测试"""
    test = StressTest("Mixed Operations Stress", 10.0)
    
    data = {}
    
    def mixed_ops():
        op = random.randint(0, 3)
        
        if op == 0:
            key = f"key_{random.randint(0, 100)}"
            data[key] = random.randint(0, 1000)
        elif op == 1 and data:
            key = random.choice(list(data.keys()))
            _ = data[key]
        elif op == 2 and data:
            key = random.choice(list(data.keys()))
            del data[key]
        else:
            _ = sum(data.values())
    
    result = test.run(mixed_ops)
    result.details.append(f"最终数据大小: {len(data)}")
    result.details.append(f"操作速率: {result.operations / result.duration:.0f} ops/s")
    return result

def print_header(title: str):
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}\n")

def print_stress_result(result: StressTestResult):
    status = "✅" if result.errors == 0 else "⚠️"
    print(f"  {status} {result.name}")
    print(f"     持续时间: {result.duration:.2f}s")
    print(f"     操作次数: {result.operations}")
    print(f"     错误次数: {result.errors}")
    print(f"     成功率: {result.success_rate:.2f}%")
    for detail in result.details:
        print(f"     • {detail}")
    print()

def main():
    print_header("QueenX 压力测试")
    
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    
    print("🔥 运行压力测试...\n")
    
    # 运行所有压力测试
    results = []
    
    print("  [1/6] 内存分配压力测试...")
    results.append(stress_memory_allocation())
    
    print("  [2/6] 文件I/O压力测试...")
    results.append(stress_file_io())
    
    print("  [3/6] 子进程压力测试...")
    results.append(stress_subprocess())
    
    print("  [4/6] 多线程压力测试...")
    results.append(stress_threading())
    
    print("  [5/6] 多进程压力测试...")
    results.append(stress_multiprocessing())
    
    print("  [6/6] 混合操作压力测试...")
    results.append(stress_mixed_operations())
    
    # 打印结果
    print("\n📊 压力测试结果:\n")
    for result in results:
        print_stress_result(result)
    
    # 总结
    print(f"\n{'='*60}\n")
    total_errors = sum(r.errors for r in results)
    total_ops = sum(r.operations for r in results)
    
    print(f"  📈 总结:")
    print(f"     总操作次数: {total_ops}")
    print(f"     总错误次数: {total_errors}")
    print(f"     总体成功率: {(total_ops / (total_ops + total_errors) * 100):.2f}%")
    
    if total_errors == 0:
        print(f"\n  ✅ 所有压力测试通过!")
        return 0
    else:
        print(f"\n  ⚠️  发现 {total_errors} 个错误")
        return 1

if __name__ == "__main__":
    sys.exit(main())
