import AppKit
import SwiftUI

/// AppKit menu-bar item (NSStatusItem) wrapped for SwiftUI. Lives for the
/// app lifetime. Shows tunnel-count badge in the system menu bar and a
/// dropdown menu with quick actions.
@MainActor
final class MenuBarController: NSObject, ObservableObject {
    private var statusItem: NSStatusItem!
    private weak var appState: AppState?
    private weak var window: NSWindow?
    private(set) var isInstalled = false

    func install(appState: AppState, window: NSWindow?) {
        guard !isInstalled else { return }
        isInstalled = true
        self.appState = appState
        self.window = window

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = NSImage(systemSymbolName: "point.3.connected.trianglepath.dotted",
                                    accessibilityDescription: "Auto2FA")
            button.imagePosition = .imageLeading
        }

        statusItem.menu = buildMenu()

        // Rebuild the menu when state changes (polling 1s is fine here)
        Task { @MainActor [weak self] in
            while true {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                self?.refresh()
            }
        }
    }

    private func refresh() {
        statusItem?.menu = buildMenu()
        // Update the title with the alive-tunnel count
        let alive = appState?.tunnels.filter { $0.displayState == .alive }.count ?? 0
        let total = appState?.tunnels.count ?? 0
        statusItem?.button?.title = total > 0 ? "\(alive)/\(total)" : ""
    }

    private func buildMenu() -> NSMenu {
        let menu = NSMenu()

        let header = NSMenuItem(title: "Auto2FA", action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())

        // Hosts
        let hostsHeader = NSMenuItem(title: "Hosts", action: nil, keyEquivalent: "")
        hostsHeader.isEnabled = false
        menu.addItem(hostsHeader)
        for host in appState?.hosts ?? [] {
            let title = "\(host.host)  —  \(label(for: host.displayState))"
            let item = NSMenuItem(title: title, action: #selector(toggleHost(_:)),
                                  keyEquivalent: "")
            item.target = self
            item.representedObject = host.host
            menu.addItem(item)
        }
        menu.addItem(.separator())

        // Tunnels
        let tunnelsHeader = NSMenuItem(title: "Tunnels", action: nil, keyEquivalent: "")
        tunnelsHeader.isEnabled = false
        menu.addItem(tunnelsHeader)
        if let tunnels = appState?.tunnels, !tunnels.isEmpty {
            for t in tunnels {
                let item = NSMenuItem(title: "\(t.name)  —  \(label(for: t.displayState))",
                                      action: nil, keyEquivalent: "")
                let sub = NSMenu()
                let toggle = NSMenuItem(title: t.displayState == .alive ? "Stop" : "Start",
                                        action: #selector(toggleTunnel(_:)), keyEquivalent: "")
                toggle.target = self
                toggle.representedObject = t.name
                sub.addItem(toggle)

                let copy = NSMenuItem(title: "Copy localhost:\(t.localPort)",
                                      action: #selector(copyTunnelURL(_:)), keyEquivalent: "c")
                copy.target = self
                copy.representedObject = t.url
                sub.addItem(copy)

                let delete = NSMenuItem(title: "Delete…",
                                        action: #selector(deleteTunnel(_:)), keyEquivalent: "")
                delete.target = self
                delete.representedObject = t.name
                sub.addItem(delete)

                item.submenu = sub
                menu.addItem(item)
            }
        } else {
            let none = NSMenuItem(title: "(no tunnels)", action: nil, keyEquivalent: "")
            none.isEnabled = false
            menu.addItem(none)
        }
        menu.addItem(.separator())

        // Footer
        let open = NSMenuItem(title: "Open Dashboard…", action: #selector(openMainWindow(_:)),
                              keyEquivalent: "0")
        open.target = self
        menu.addItem(open)

        let quit = NSMenuItem(title: "Quit Auto2FA", action: #selector(quit(_:)), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)
        return menu
    }

    private func label(for s: SSHHost.DisplayState) -> String {
        switch s {
        case .connected: return "● Connected"
        case .connecting: return "◐ Connecting…"
        case .failed: return "● Failed"
        case .stopped: return "○ Stopped"
        case .unknown: return "?"
        }
    }

    private func label(for s: Tunnel.DisplayState) -> String {
        switch s {
        case .alive: return "● Connected"
        case .starting: return "◐ Connecting…"
        case .stale: return "○ Stale"
        case .idle: return "○ Idle"
        case .portBusy: return "● Port busy"
        case .failed: return "● Failed"
        case .unknown: return "?"
        }
    }

    // MARK: - Action handlers

    @objc private func toggleHost(_ sender: NSMenuItem) {
        guard let name = sender.representedObject as? String,
              let host = appState?.hosts.first(where: { $0.host == name }) else { return }
        Task { await appState?.toggleHost(host) }
    }

    @objc private func toggleTunnel(_ sender: NSMenuItem) {
        guard let name = sender.representedObject as? String,
              let t = appState?.tunnels.first(where: { $0.name == name }) else { return }
        Task { await appState?.toggleTunnel(t) }
    }

    @objc private func copyTunnelURL(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? String else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(url, forType: .string)
    }

    @objc private func deleteTunnel(_ sender: NSMenuItem) {
        guard let name = sender.representedObject as? String,
              let t = appState?.tunnels.first(where: { $0.name == name }) else { return }
        Task { await appState?.deleteTunnel(t) }
    }

    @objc private func openMainWindow(_ sender: NSMenuItem) {
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc private func quit(_ sender: NSMenuItem) {
        NSApp.terminate(nil)
    }
}
