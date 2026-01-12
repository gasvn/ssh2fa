#!/usr/bin/env python3
"""
Auto2FA Connection Monitor
Simple script to monitor the status of Auto2FA connections
"""

import os
import sys
import time
import subprocess
from datetime import datetime

def check_log_file():
    """Check the Auto2FA log file for recent activity"""
    log_file = "/tmp/auto2fa.log"
    
    if not os.path.exists(log_file):
        print("  ❌ 日志文件不存在")
        return False
    
    try:
        # Get last 10 lines of log
        result = subprocess.run(['tail', '-10', log_file], 
                              capture_output=True, text=True)
        
        if result.returncode == 0:
            print("  📋 最近的活动记录:")
            for line in result.stdout.strip().split('\n'):
                if line:
                    print(f"     {line}")
            return True
        else:
            print("  ❌ 无法读取日志文件")
            return False
            
    except Exception as e:
        print(f"  ❌ 读取错误: {e}")
        return False

def check_ssh_processes():
    """Check for active SSH processes that might be from Auto2FA"""
    try:
        result = subprocess.run(['ps', 'aux'], capture_output=True, text=True)
        
        if result.returncode == 0:
            ssh_lines = [line for line in result.stdout.split('\n') 
                        if 'ssh' in line.lower() and 'auto2fa' not in line.lower() 
                        and 'grep' not in line.lower()]
            
            if ssh_lines:
                print("  🔐 检测到活动的SSH连接:")
                for line in ssh_lines[:3]:  # Show max 3 connections
                    parts = line.split()
                    if len(parts) > 10:
                        print(f"     • {' '.join(parts[10:13])}")
                if len(ssh_lines) > 3:
                    print(f"     ... 还有 {len(ssh_lines) - 3} 个连接")
                return True
            else:
                print("  ℹ️  当前无SSH连接")
                return False
                
    except Exception as e:
        print(f"  ❌ 检查错误: {e}")
        return False

def check_network_connectivity():
    """Check basic network connectivity"""
    try:
        result = subprocess.run(['ping', '-c', '1', '8.8.8.8'], 
                              capture_output=True, text=True, timeout=5)
        
        if result.returncode == 0:
            print("  ✅ 网络连接正常")
            return True
        else:
            print("  ❌ 网络连接失败")
            return False
            
    except subprocess.TimeoutExpired:
        print("  ⏰ 网络连接超时")
        return False
    except Exception as e:
        print(f"  ❌ 检查错误: {e}")
        return False

def main():
    """Main monitoring function"""
    print("\n" + "="*60)
    print("🔍 Auto2FA 连接监控工具")
    print("="*60)
    print(f"⏰ 检查时间: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    print("="*60 + "\n")
    
    # Check network first
    print("1️⃣  网络连接检查")
    print("-" * 60)
    network_ok = check_network_connectivity()
    print()
    
    # Check for SSH processes
    print("2️⃣  SSH进程检查")
    print("-" * 60)
    ssh_active = check_ssh_processes()
    print()
    
    # Check log file
    print("3️⃣  日志文件检查")
    print("-" * 60)
    log_ok = check_log_file()
    print()
    
    # Summary
    print("="*60)
    print("📊 状态汇总")
    print("="*60)
    print(f"  网络状态: {'✅ 正常' if network_ok else '❌ 异常'}")
    print(f"  SSH连接: {'✅ 活动中' if ssh_active else 'ℹ️  无连接'}")
    print(f"  日志文件: {'✅ 正常' if log_ok else '❌ 未找到'}")
    print("="*60 + "\n")

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\n👋 监控工具已退出\n")
    except Exception as e:
        print(f"\n❌ 监控错误: {e}\n")
        sys.exit(1)