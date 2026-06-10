import AppKit
import SwiftUI

/// AppKit menu-bar item (NSStatusItem) wrapped for SwiftUI. Lives for the
/// app lifetime. Shows tunnel-count badge in the system menu bar and a
/// dropdown menu with quick actions.
@MainActor
final class MenuBarController: NSObject, ObservableObject, NSMenuDelegate {
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
        statusItem.autosaveName = "com.auto2fa.menubar"
        statusItem.behavior = .removalAllowed

        if let button = statusItem.button {
            button.toolTip = "Auto2FA — left-click to show window, right-click for menu"
            // Custom click handler so we get BOTH left and right clicks.
            // We deliberately do NOT set statusItem.menu — if menu is set,
            // macOS swallows clicks and pops the menu, losing left-vs-right
            // distinction. Instead we present the menu manually on right-click.
            button.action = #selector(buttonClicked(_:))
            button.target = self
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            // Initial image with neutral tint; refresh() recolors as state changes.
            renderButton()
            NSLog("[Auto2FA] MenuBar status item installed")
        } else {
            NSLog("[Auto2FA] MenuBar statusItem.button is nil — system denied a slot")
        }

        // Refresh the icon tint + count badge once a second. Cheap.
        Task { @MainActor [weak self] in
            // Exit when the controller deallocates or the task is cancelled —
            // `while true` with `self?.` spun the 1 Hz loop forever even after
            // the controller was gone (weak self prevented the retain cycle
            // but not the loop).
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
                guard let self else { return }
                self.refresh()
            }
        }
    }

    private func refresh() {
        renderButton()
    }

    /// Pick a tint based on aggregate state across hosts + tunnels:
    ///   - red   if anything is failed
    ///   - yellow if anything is connecting/starting
    ///   - green if all enabled things are healthy
    ///   - grey  if nothing is enabled / idle
    private func aggregateTint() -> NSColor {
        guard let appState else { return .secondaryLabelColor }
        // Explicit loops instead of compound contains() expressions —
        // Swift's type checker would otherwise time out on this body
        // (it tries every overload of `==` for each variant).
        var anyFailed = false
        var anyBusy = false
        var anyAlive = false
        for h in appState.hosts {
            switch h.displayState {
            case .failed: anyFailed = true
            case .connecting: anyBusy = true
            case .connected: anyAlive = true
            default: break
            }
        }
        for t in appState.tunnels {
            switch t.displayState {
            case .failed, .portBusy: anyFailed = true
            case .starting: anyBusy = true
            case .alive: anyAlive = true
            default: break
            }
        }
        if anyFailed { return .systemRed }
        if anyBusy { return .systemYellow }
        return anyAlive ? .systemGreen : .secondaryLabelColor
    }

    private func renderButton() {
        guard let button = statusItem?.button else { return }
        let tint = aggregateTint()
        var image = NSImage(systemSymbolName: "point.3.connected.trianglepath.dotted",
                            accessibilityDescription: "Auto2FA")
                ?? NSImage(systemSymbolName: "network", accessibilityDescription: "Auto2FA")
        if let img = image {
            // .palette renders the symbol in our chosen color (and respects
            // dark/light mode automatically because we're working in NSColor).
            let cfg = NSImage.SymbolConfiguration(paletteColors: [tint])
            image = img.withSymbolConfiguration(cfg)
            button.image = image
            button.imagePosition = .imageLeading
        } else {
            button.title = "A2F"
        }
        // Always show a small alive/total count to the right of the icon so
        // the user can see at a glance how many tunnels are up.
        let alive = appState?.tunnels.filter { $0.displayState == .alive }.count ?? 0
        let total = appState?.tunnels.count ?? 0
        button.title = total > 0 ? " \(alive)/\(total)" : ""
    }

    @objc private func buttonClicked(_ sender: Any?) {
        let event = NSApp.currentEvent
        let isRightClick = event?.type == .rightMouseUp
                        || (event?.modifierFlags.contains(.control) ?? false)
        if isRightClick {
            // Present the menu manually at the button's location. Detach
            // AFTER the menu closes (via NSMenuDelegate.menuDidClose), not
            // on the next runloop tick — the previous async approach
            // could detach while the user was still keyboard-navigating
            // the menu, causing AppKit to dismiss it unexpectedly.
            if let menu = buildMenuOptional(), let button = statusItem?.button {
                menu.delegate = self
                statusItem.menu = menu
                button.performClick(nil)
            }
        } else {
            // Left click → bring the main window forward (or open one if
            // user previously closed it).
            NSApp.activate(ignoringOtherApps: true)
            if let win = window {
                win.makeKeyAndOrderFront(nil)
            } else if let any = NSApp.windows.first(where: { $0.title == "Auto2FA" }) {
                any.makeKeyAndOrderFront(nil)
                self.window = any
            }
        }
    }

    /// NSMenuDelegate: clean up the attached menu only after AppKit has
    /// fully dismissed it, so the next left-click is again treated as a
    /// click (not a menu-pop).
    nonisolated func menuDidClose(_ menu: NSMenu) {
        Task { @MainActor [weak self] in
            self?.statusItem?.menu = nil
        }
    }

    /// Wrapper so we can fail-soft when appState isn't ready yet.
    private func buildMenuOptional() -> NSMenu? {
        return buildMenu()
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

        let logs = NSMenuItem(title: "Show Daemon Logs…",
                              action: #selector(openLogs(_:)), keyEquivalent: "l")
        logs.keyEquivalentModifierMask = [.command, .shift]
        logs.target = self
        menu.addItem(logs)

        let prefs = NSMenuItem(title: "Settings…",
                               action: #selector(openSettings(_:)), keyEquivalent: ",")
        prefs.target = self
        menu.addItem(prefs)

        let troubleshoot = NSMenuItem(title: "Troubleshoot…",
                                      action: #selector(openSettings(_:)), keyEquivalent: "")
        troubleshoot.target = self
        troubleshoot.toolTip = "Open Settings → Troubleshoot to run health checks."
        menu.addItem(troubleshoot)

        menu.addItem(.separator())

        let uninstall = NSMenuItem(title: "Uninstall Auto2FA…",
                                   action: #selector(uninstall(_:)), keyEquivalent: "")
        uninstall.target = self
        menu.addItem(uninstall)

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
        // Confirm first: the menu item says "Delete…" (ellipsis = dialog),
        // the undo snackbar only renders inside the main window (usually
        // closed when using the menu bar), and the old direct call made
        // menu-bar deletion instant AND effectively un-undoable.
        let alert = NSAlert()
        alert.messageText = "Delete tunnel \u{201C}\(name)\u{201D}?"
        alert.informativeText = "This stops the tunnel and removes its configuration."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await appState?.deleteTunnel(t) }
    }

    @objc private func openMainWindow(_ sender: NSMenuItem) {
        NSApp.activate(ignoringOtherApps: true)
        if let win = window {
            win.makeKeyAndOrderFront(nil)
        } else if let any = NSApp.windows.first(where: { $0.title == "Auto2FA" }) {
            any.makeKeyAndOrderFront(nil)
            self.window = any
        }
    }

    @objc private func openLogs(_ sender: Any?) {
        // Bring main window forward + signal the ContentView to open the
        // logs WindowGroup via the SwiftUI Environment(\.openWindow) API.
        // Previously we tried `newWindowForTab:` (a Safari-style selector
        // that doesn't apply to non-tabbed SwiftUI windows) — silently
        // no-op'd on macOS 14+.
        NSApp.activate(ignoringOtherApps: true)
        if let win = NSApp.windows.first(where: { $0.title == "Auto2FA Logs" }) {
            win.makeKeyAndOrderFront(nil)
            return
        }
        NotificationCenter.default.post(name: .a2fShowLogs, object: nil)
    }

    @objc private func openSettings(_ sender: Any?) {
        NSApp.activate(ignoringOtherApps: true)
        // macOS 14+: SwiftUI Settings scene is exposed via this selector.
        if #available(macOS 14, *) {
            NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
        } else {
            NSApp.sendAction(Selector(("showPreferencesWindow:")), to: nil, from: nil)
        }
    }

    @objc private func quit(_ sender: NSMenuItem) {
        NSApp.terminate(nil)
    }

    @objc private func uninstall(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = "Uninstall Auto2FA?"
        alert.informativeText = "This stops and removes the background daemon, deletes its LaunchAgent, and removes every credential Auto2FA saved in your Keychain. Afterward, drag Auto2FA.app to the Trash yourself."
        alert.alertStyle = .warning
        let purge = NSButton(checkboxWithTitle: "Also delete my saved hosts & tunnels (passwords.json, tunnels.json)",
                             target: nil, action: nil)
        purge.state = .off
        alert.accessoryView = purge
        alert.addButton(withTitle: "Uninstall")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        DaemonProcess.shared.performUninstall(purgeConfig: purge.state == .on)

        // Reveal the app so the user can drag it to the Trash, then quit.
        NSWorkspace.shared.activateFileViewerSelecting([Bundle.main.bundleURL])
        let done = NSAlert()
        done.messageText = "Auto2FA uninstalled"
        done.informativeText = "The daemon, LaunchAgent and Keychain credentials are removed. Drag Auto2FA.app (now revealed in Finder) to the Trash to finish."
        done.addButton(withTitle: "Quit")
        done.runModal()
        NSApp.terminate(nil)
    }
}
