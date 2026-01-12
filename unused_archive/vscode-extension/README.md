# Auto2FA VSCode Extension

A robust VSCode extension for managing SSH connections with 2FA authentication, designed to work seamlessly with VSCode Remote-SSH.

## Features

- **Status Bar Integration**: Real-time connection status display
- **Daemon Management**: Start, stop, and restart the Auto2FA daemon
- **Host Management**: Reconnect individual hosts or all hosts
- **Log Viewing**: View daemon logs directly in VSCode
- **Automatic Monitoring**: Continuous status updates

## Installation

### Prerequisites

- VSCode 1.74.0 or higher
- Auto2FA daemon running (see main INSTALL.md)
- Node.js and npm (for development)

### Development Installation

1. Clone or download the extension files
2. Install dependencies:
   ```bash
   cd vscode-extension
   npm install
   ```
3. Compile the extension:
   ```bash
   npm run compile
   ```
4. Press F5 in VSCode to run the extension in a new window

### Production Installation

1. Compile the extension:
   ```bash
   cd vscode-extension
   npm run compile
   ```
2. Package the extension:
   ```bash
   npm install -g vsce
   vsce package
   ```
3. Install the generated .vsix file in VSCode

## Usage

### Status Bar

The extension adds a status indicator to VSCode's status bar:

- ✅ **All connected**: All configured hosts are connected
- ⚠️ **Partial**: Some hosts are connected
- ❌ **Disconnected**: No hosts are connected
- ❓ **No hosts**: No hosts configured

Click the status bar item to view detailed connection information.

### Commands

Access commands via the Command Palette (`Cmd+Shift+P`):

- **Auto2FA: Show Status** - Display detailed connection status in a webview
- **Auto2FA: Restart Daemon** - Restart the Auto2FA daemon
- **Auto2FA: Reconnect Host** - Reconnect a specific host
- **Auto2FA: View Logs** - Open daemon logs in VSCode

### Configuration

Configure the extension in VSCode settings:

```json
{
  "auto2fa.socketPath": "/tmp/auto2fa_daemon.sock",
  "auto2fa.logPath": "/tmp/auto2fa_daemon.log",
  "auto2fa.statusBarEnabled": true,
  "auto2fa.refreshInterval": 30
}
```

## Architecture

The extension communicates with the Auto2FA daemon via Unix socket:

```
VSCode Extension ←→ Unix Socket ←→ Auto2FA Daemon ←→ SSH Connections
```

### Communication Protocol

Commands sent to daemon:
```json
{"command": "status"}                    // Get connection status
{"command": "reconnect", "host": "name"} // Reconnect specific host
{"command": "restart"}                   // Restart all connections
```

Daemon responses:
```json
{
  "hosts": {
    "host1": {
      "connected": true,
      "last_check": "2024-10-17T10:30:00",
      "retry_count": 0,
      "last_error": null
    }
  }
}
```

## Development

### Project Structure

```
vscode-extension/
├── package.json          # Extension manifest
├── tsconfig.json         # TypeScript configuration
├── src/
│   └── extension.ts      # Main extension code
└── out/                  # Compiled JavaScript (generated)
```

### Building

```bash
npm run compile          # Compile TypeScript
npm run watch           # Watch mode for development
```

### Testing

1. Press F5 to launch a new VSCode window with the extension
2. Test commands and status updates
3. Check the Developer Console for any errors

## Troubleshooting

### Extension Not Loading

1. Check that the extension compiled successfully
2. Look for errors in the Developer Console
3. Verify the daemon is running: `launchctl list | grep auto2fa`

### Status Not Updating

1. Check daemon socket: `ls -la /tmp/auto2fa_daemon.sock`
2. Verify daemon is responding: `echo '{"command":"status"}' | nc -U /tmp/auto2fa_daemon.sock`
3. Check daemon logs: `tail -f /tmp/auto2fa_daemon.log`

### Commands Not Working

1. Ensure daemon is running and accessible
2. Check socket permissions
3. Verify configuration settings

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Test thoroughly
5. Submit a pull request

## License

This extension is part of the Auto2FA project. See the main project for license information.
