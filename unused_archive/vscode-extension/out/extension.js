"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || function (mod) {
    if (mod && mod.__esModule) return mod;
    var result = {};
    if (mod != null) for (var k in mod) if (k !== "default" && Object.prototype.hasOwnProperty.call(mod, k)) __createBinding(result, mod, k);
    __setModuleDefault(result, mod);
    return result;
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.deactivate = exports.activate = void 0;
const vscode = __importStar(require("vscode"));
const fs = __importStar(require("fs"));
class Auto2FAManager {
    constructor() {
        this.hosts = {};
        // Get configuration
        const config = vscode.workspace.getConfiguration('auto2fa');
        this.socketPath = config.get('socketPath', '/tmp/auto2fa_daemon.sock');
        this.logPath = config.get('logPath', '/tmp/auto2fa_daemon.log');
        this.refreshInterval = config.get('refreshInterval', 30) * 1000;
        // Create status bar item
        this.statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
        this.statusBarItem.command = 'auto2fa.showStatus';
        this.statusBarItem.show();
        // Start monitoring
        this.startMonitoring();
    }
    async sendCommand(command) {
        return new Promise((resolve) => {
            const net = require('net');
            const client = net.createConnection(this.socketPath);
            client.on('connect', () => {
                client.write(JSON.stringify(command));
            });
            client.on('data', (data) => {
                try {
                    const response = JSON.parse(data.toString());
                    resolve(response);
                }
                catch (error) {
                    resolve({ error: 'Invalid JSON response' });
                }
                client.end();
            });
            client.on('error', (error) => {
                resolve({ error: `Connection failed: ${error.message}` });
            });
            client.on('timeout', () => {
                resolve({ error: 'Connection timeout' });
                client.end();
            });
            client.setTimeout(5000);
        });
    }
    async updateStatus() {
        try {
            const response = await this.sendCommand({ command: 'status' });
            if (response && response.hosts) {
                this.hosts = response.hosts;
                this.updateStatusBar();
            }
            else if (response && response.error) {
                this.statusBarItem.text = '$(error) Auto2FA: Error';
                this.statusBarItem.tooltip = response.error;
            }
        }
        catch (error) {
            this.statusBarItem.text = '$(error) Auto2FA: Offline';
            this.statusBarItem.tooltip = 'Daemon not responding';
        }
    }
    updateStatusBar() {
        const connectedHosts = Object.values(this.hosts).filter(h => h.connected).length;
        const totalHosts = Object.keys(this.hosts).length;
        if (totalHosts === 0) {
            this.statusBarItem.text = '$(question) Auto2FA: No hosts';
            this.statusBarItem.tooltip = 'No hosts configured';
        }
        else if (connectedHosts === totalHosts) {
            this.statusBarItem.text = '$(check) Auto2FA: All connected';
            this.statusBarItem.tooltip = `${connectedHosts}/${totalHosts} hosts connected`;
        }
        else if (connectedHosts > 0) {
            this.statusBarItem.text = '$(warning) Auto2FA: Partial';
            this.statusBarItem.tooltip = `${connectedHosts}/${totalHosts} hosts connected`;
        }
        else {
            this.statusBarItem.text = '$(error) Auto2FA: Disconnected';
            this.statusBarItem.tooltip = 'No hosts connected';
        }
    }
    startMonitoring() {
        // Initial update
        this.updateStatus();
        // Set up periodic updates
        this.refreshTimer = setInterval(() => {
            this.updateStatus();
        }, this.refreshInterval);
    }
    async showStatus() {
        await this.updateStatus();
        const panel = vscode.window.createWebviewPanel('auto2faStatus', 'Auto2FA Status', vscode.ViewColumn.One, { enableScripts: true });
        const connectedHosts = Object.entries(this.hosts).filter(([_, status]) => status.connected);
        const disconnectedHosts = Object.entries(this.hosts).filter(([_, status]) => !status.connected);
        let html = `
            <!DOCTYPE html>
            <html>
            <head>
                <style>
                    body { font-family: var(--vscode-font-family); padding: 20px; }
                    .host { margin: 10px 0; padding: 10px; border-radius: 5px; }
                    .connected { background-color: var(--vscode-testing-iconPassed); color: white; }
                    .disconnected { background-color: var(--vscode-testing-iconFailed); color: white; }
                    .status { font-weight: bold; }
                    .details { font-size: 0.9em; margin-top: 5px; }
                </style>
            </head>
            <body>
                <h2>Auto2FA Connection Status</h2>
        `;
        if (connectedHosts.length > 0) {
            html += '<h3>✅ Connected Hosts</h3>';
            connectedHosts.forEach(([name, status]) => {
                html += `
                    <div class="host connected">
                        <div class="status">${name}</div>
                        <div class="details">
                            Last check: ${status.last_check || 'Unknown'}<br>
                            Retry count: ${status.retry_count}
                        </div>
                    </div>
                `;
            });
        }
        if (disconnectedHosts.length > 0) {
            html += '<h3>❌ Disconnected Hosts</h3>';
            disconnectedHosts.forEach(([name, status]) => {
                html += `
                    <div class="host disconnected">
                        <div class="status">${name}</div>
                        <div class="details">
                            Last check: ${status.last_check || 'Unknown'}<br>
                            Retry count: ${status.retry_count}<br>
                            Error: ${status.last_error || 'None'}
                        </div>
                    </div>
                `;
            });
        }
        if (Object.keys(this.hosts).length === 0) {
            html += '<p>No hosts configured or daemon not responding.</p>';
        }
        html += '</body></html>';
        panel.webview.html = html;
    }
    async restartDaemon() {
        const response = await this.sendCommand({ command: 'restart' });
        if (response && response.success) {
            vscode.window.showInformationMessage('Auto2FA daemon restarted successfully');
        }
        else {
            vscode.window.showErrorMessage(`Failed to restart daemon: ${response?.error || 'Unknown error'}`);
        }
    }
    async reconnectHost() {
        const hostNames = Object.keys(this.hosts);
        if (hostNames.length === 0) {
            vscode.window.showWarningMessage('No hosts available');
            return;
        }
        const selectedHost = await vscode.window.showQuickPick(hostNames, {
            placeHolder: 'Select host to reconnect'
        });
        if (selectedHost) {
            const response = await this.sendCommand({
                command: 'reconnect',
                host: selectedHost
            });
            if (response && response.success) {
                vscode.window.showInformationMessage(`Reconnecting to ${selectedHost}...`);
            }
            else {
                vscode.window.showErrorMessage(`Failed to reconnect to ${selectedHost}: ${response?.error || 'Unknown error'}`);
            }
        }
    }
    async viewLogs() {
        if (fs.existsSync(this.logPath)) {
            const document = await vscode.workspace.openTextDocument(this.logPath);
            await vscode.window.showTextDocument(document);
        }
        else {
            vscode.window.showErrorMessage(`Log file not found: ${this.logPath}`);
        }
    }
    dispose() {
        if (this.refreshTimer) {
            clearInterval(this.refreshTimer);
        }
        this.statusBarItem.dispose();
    }
}
let manager;
function activate(context) {
    console.log('Auto2FA extension is now active!');
    // Initialize manager
    manager = new Auto2FAManager();
    // Register commands
    const commands = [
        vscode.commands.registerCommand('auto2fa.showStatus', () => manager.showStatus()),
        vscode.commands.registerCommand('auto2fa.restartDaemon', () => manager.restartDaemon()),
        vscode.commands.registerCommand('auto2fa.reconnectHost', () => manager.reconnectHost()),
        vscode.commands.registerCommand('auto2fa.viewLogs', () => manager.viewLogs())
    ];
    context.subscriptions.push(...commands);
    context.subscriptions.push(manager);
}
exports.activate = activate;
function deactivate() {
    if (manager) {
        manager.dispose();
    }
}
exports.deactivate = deactivate;
//# sourceMappingURL=extension.js.map