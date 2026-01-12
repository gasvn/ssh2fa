# Auto2FA VSCode Extension - 安装指南

## 概述

Auto2FA 是一个鲁棒的 SSH 自动登录系统，专为 VSCode Remote-SSH 设计。它通过后台守护进程维护持久的 SSH 连接，确保 VSCode 连接稳定可靠。

## 系统要求

- macOS 10.14 或更高版本
- Python 3.7 或更高版本
- VSCode 1.74 或更高版本
- VSCode Remote-SSH 扩展

## 安装步骤

### 1. 安装 Python 依赖

```bash
cd /Users/shgao/logs/auto2fa_dev
pip3 install pexpect pyotp
```

### 2. 配置 SSH 和密码文件

确保以下文件存在并正确配置：

#### SSH 配置文件 (`~/.ssh/config`)
```bash
# 使用提供的模板
cp /Users/shgao/logs/auto2fa_dev/ssh_config_template ~/.ssh/config
# 然后编辑文件，添加你的服务器配置
```

#### 密码配置文件 (`~/.ssh/passwords.json`)
```json
{
  "your-server": {
    "password": "your_password",
    "otpauthUrl": "otpauth://totp/YourService:user@example.com?secret=YOUR_SECRET&issuer=YourService"
  }
}
```

### 3. 安装系统服务

```bash
cd /Users/shgao/logs/auto2fa_dev
./install_service.sh
```

这将：
- 安装 launchd 服务配置
- 启动 Auto2FA 守护进程
- 设置开机自启动

### 4. 安装 VSCode 扩展

#### 方法 A：从源码安装（推荐）

```bash
cd /Users/shgao/logs/auto2fa_dev/vscode-extension
npm install
npm run compile
npm install -g vsce
vsce package
```

然后在 VSCode 中：
1. 按 `Cmd+Shift+P`
2. 输入 "Extensions: Install from VSIX"
3. 选择 `auto2fa-1.0.0.vsix` 文件

#### 方法 B：开发模式

1. 在 VSCode 中打开 `vscode-extension` 目录
2. 按 `F5` 启动调试模式
3. 在新窗口中测试扩展

### 5. 验证安装

1. 检查守护进程状态：
   ```bash
   launchctl list | grep auto2fa
   ```

2. 查看日志：
   ```bash
   tail -f /tmp/auto2fa_daemon.log
   ```

3. 在 VSCode 中：
   - 查看状态栏是否显示 Auto2FA 状态
   - 按 `Cmd+Shift+P`，输入 "Auto2FA" 查看可用命令

## 使用指南

### 状态栏指示器

VSCode 状态栏会显示连接状态：
- ✅ **All connected**: 所有主机已连接
- ⚠️ **Partial**: 部分主机连接
- ❌ **Disconnected**: 无主机连接
- ❓ **No hosts**: 未配置主机

### 命令面板命令

按 `Cmd+Shift+P` 输入 "Auto2FA" 可看到：

- **Show Status**: 显示详细连接状态
- **Restart Daemon**: 重启守护进程
- **Reconnect Host**: 重连指定主机
- **View Logs**: 在 VSCode 中查看日志

### VSCode Remote-SSH 使用

1. 确保 Auto2FA 守护进程正在运行
2. 在 VSCode 中按 `Cmd+Shift+P`
3. 选择 "Remote-SSH: Connect to Host"
4. 选择已配置的主机
5. 连接应该立即成功，无需输入密码或 2FA

## 故障排除

### 守护进程未启动

```bash
# 检查服务状态
launchctl list | grep auto2fa

# 手动启动
launchctl load ~/Library/LaunchAgents/com.auto2fa.daemon.plist

# 查看日志
tail -f /tmp/auto2fa_daemon.log
```

### VSCode 连接失败

1. 检查 SSH 配置中的 ControlPath 设置
2. 确保守护进程正在运行
3. 查看守护进程日志中的错误信息
4. 尝试手动重启守护进程

### 2FA 验证失败

1. 检查系统时间是否同步
2. 验证 `passwords.json` 中的 OTP URL 格式
3. 确保 OTP 密钥正确

### 权限问题

```bash
# 确保 socket 文件权限正确
ls -la /tmp/auto2fa_daemon.sock

# 如果需要，修复权限
sudo chmod 666 /tmp/auto2fa_daemon.sock
```

## 配置选项

### 守护进程配置 (`daemon_config.json`)

```json
{
  "daemon": {
    "check_interval": 30,        // 检查间隔（秒）
    "max_retries": 3,           // 最大重试次数
    "retry_delay": 5            // 重试延迟（秒）
  },
  "ssh": {
    "server_alive_interval": 15, // SSH 保活间隔
    "server_alive_count_max": 6  // 最大保活失败次数
  }
}
```

### VSCode 扩展配置

在 VSCode 设置中搜索 "auto2fa"：

- `auto2fa.socketPath`: 守护进程 socket 路径
- `auto2fa.logPath`: 日志文件路径
- `auto2fa.statusBarEnabled`: 是否显示状态栏
- `auto2fa.refreshInterval`: 状态刷新间隔

## 卸载

### 停止并移除服务

```bash
# 停止服务
launchctl unload ~/Library/LaunchAgents/com.auto2fa.daemon.plist

# 删除配置文件
rm ~/Library/LaunchAgents/com.auto2fa.daemon.plist

# 清理临时文件
rm -f /tmp/auto2fa_daemon.*
```

### 卸载 VSCode 扩展

1. 在 VSCode 中按 `Cmd+Shift+X`
2. 搜索 "Auto2FA"
3. 点击卸载

## 支持

如果遇到问题：

1. 查看日志文件：`/tmp/auto2fa_daemon.log`
2. 检查守护进程状态：`launchctl list | grep auto2fa`
3. 验证配置文件格式
4. 确保所有依赖已正确安装

## 更新

要更新到新版本：

1. 停止当前服务
2. 替换文件
3. 重新安装服务
4. 重启 VSCode

```bash
# 停止服务
launchctl unload ~/Library/LaunchAgents/com.auto2fa.daemon.plist

# 更新文件后重新安装
./install_service.sh
```
