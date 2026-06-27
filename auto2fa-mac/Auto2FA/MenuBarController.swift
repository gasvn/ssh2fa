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
        statusItem.autosaveName = "com.ssh2fa.menubar"
        statusItem.behavior = .removalAllowed

        if let button = statusItem.button {
            button.toolTip = "SSH2FA — left-click to show window, right-click for menu"
            // Custom click handler so we get BOTH left and right clicks.
            // We deliberately do NOT set statusItem.menu — if menu is set,
            // macOS swallows clicks and pops the menu, losing left-vs-right
            // distinction. Instead we present the menu manually on right-click.
            button.action = #selector(buttonClicked(_:))
            button.target = self
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
            // Initial image with neutral tint; refresh() recolors as state changes.
            renderButton()
            NSLog("[SSH2FA] MenuBar status item installed")
        } else {
            NSLog("[SSH2FA] MenuBar statusItem.button is nil — system denied a slot")
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
                            accessibilityDescription: "SSH2FA")
                ?? NSImage(systemSymbolName: "network", accessibilityDescription: "SSH2FA")
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
        // The icon's COLOR already conveys aggregate health, so the numeric
        // alive/total badge was dropped (it added noise without much meaning).
        // Keep only a small ⬆︎ marker when a newer release is waiting.
        let mark = (appState?.availableUpdate != nil) ? " ⬆︎" : ""
        button.title = mark
        // Give the otherwise-cryptic ⬆︎ a hover tooltip + VoiceOver label, and
        // restore the default ones when no update is pending.
        if let upd = appState?.availableUpdate {
            let v = UpdateCheckCore.displayVersion(upd.version)
            button.toolTip = "SSH2FA update available — \(v). Click for options."
            button.image?.accessibilityDescription = "SSH2FA, update available \(v)"
        } else {
            button.toolTip = "SSH2FA — click for menu"
            button.image?.accessibilityDescription = "SSH2FA"
        }
    }

    @objc private func buttonClicked(_ sender: Any?) {
        // BOTH left- and right-click present the menu (per user request — a
        // normal click on the logo opens the menu). The dashboard window is
        // reached via the "Open SSH2FA" item at the top of that menu.
        //
        // Detach the menu AFTER it closes (via NSMenuDelegate.menuDidClose),
        // not on the next runloop tick — the previous async approach could
        // detach while the user was still keyboard-navigating, dismissing it.
        if let menu = buildMenuOptional(), let button = statusItem?.button {
            menu.delegate = self
            statusItem.menu = menu
            button.performClick(nil)
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

        let header = NSMenuItem(title: "SSH2FA", action: nil, keyEquivalent: "")
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())

        // Primary action: open the desktop dashboard window. Now that a click on
        // the logo shows this menu (instead of opening the window directly), this
        // is how the user reaches the app window.
        let openApp = NSMenuItem(title: "Open SSH2FA", action: #selector(openMainWindow(_:)),
                                 keyEquivalent: "0")
        openApp.target = self
        openApp.toolTip = "Open the SSH2FA dashboard window."
        menu.addItem(openApp)
        menu.addItem(.separator())

        // Update available (notify-only) — surfaced at the top. A submenu gives
        // the user an actual path: how to update (for their install method),
        // release notes, and a way to skip a version they don't want. The app
        // never downloads or installs on its own.
        if let upd = appState?.availableUpdate {
            let v = UpdateCheckCore.displayVersion(upd.version)
            let item = NSMenuItem(title: "⬆︎ Update available — \(v)", action: nil, keyEquivalent: "")
            item.toolTip = "A newer SSH2FA release is available."
            let sub = NSMenu()
            let how = NSMenuItem(title: "How to update…",
                                 action: #selector(openUpdateInstructions(_:)), keyEquivalent: "")
            how.target = self
            how.toolTip = "Open Settings → About for one-step update instructions."
            sub.addItem(how)
            let notes = NSMenuItem(title: "View release notes",
                                   action: #selector(viewReleaseNotes(_:)), keyEquivalent: "")
            notes.target = self
            notes.representedObject = upd.url
            sub.addItem(notes)
            sub.addItem(.separator())
            let skip = NSMenuItem(title: "Skip \(v)",
                                  action: #selector(skipUpdateVersion(_:)), keyEquivalent: "")
            skip.target = self
            skip.representedObject = upd.version
            sub.addItem(skip)
            item.submenu = sub
            menu.addItem(item)
            menu.addItem(.separator())
        }

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
                let toggle = NSMenuItem(title: (t.displayState == .alive || t.displayState == .starting) ? "Stop" : "Start",
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

        // Footer ("Open SSH2FA" now lives at the top of the menu as the primary
        // action, so it's no longer repeated here.)
        // Settings is the single entry point to the advanced stuff — logs live
        // on a toolbar button in the dashboard, and Troubleshoot / Uninstall are
        // tabs/sections inside Settings. The quick menu stays focused on
        // everyday actions (open app, hosts, tunnels).
        let prefs = NSMenuItem(title: "Settings…",
                               action: #selector(openSettings(_:)), keyEquivalent: ",")
        prefs.target = self
        menu.addItem(prefs)

        menu.addItem(.separator())

        let quit = NSMenuItem(title: "Quit SSH2FA", action: #selector(quit(_:)), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)
        return menu
    }

    // A menu (unlike the dashboard) can't color these dots, so each state gets a
    // DISTINCT glyph + plain-English word — no raw "Stale"/"Port busy" jargon,
    // and states no longer look identical at a glance.
    private func label(for s: SSHHost.DisplayState) -> String {
        switch s {
        case .connected: return "● Connected"
        case .connecting: return "◌ Connecting…"
        case .failed: return "✕ Failed"
        case .stopped: return "○ Off"
        case .unknown: return "– Unknown"
        }
    }

    private func label(for s: Tunnel.DisplayState) -> String {
        switch s {
        case .alive: return "● Connected"
        case .starting: return "◌ Connecting…"
        case .stale: return "◑ Reconnecting…"
        case .idle: return "○ Off"
        case .portBusy: return "⚠ Port in use"
        case .failed: return "✕ Failed"
        case .unknown: return "– Unknown"
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

    /// "How to update…" → deep-link to Settings → About, which now shows
    /// copy-paste update commands for both Homebrew and manual installs.
    @objc private func openUpdateInstructions(_ sender: Any?) {
        UserDefaults.standard.set(SettingsTab.about, forKey: SettingsKey.settingsTab)
        showSettingsWindow()
    }

    @objc private func viewReleaseNotes(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        NSWorkspace.shared.open(url)
    }

    @objc private func skipUpdateVersion(_ sender: NSMenuItem) {
        guard let v = sender.representedObject as? String else { return }
        appState?.skipUpdate(v)
        refresh()   // drop the menu-bar marker right away
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
        } else if let any = NSApp.windows.first(where: { $0.title == "SSH2FA" }) {
            any.makeKeyAndOrderFront(nil)
            self.window = any
        }
    }

    @objc private func openSettings(_ sender: Any?) {
        UserDefaults.standard.set(SettingsTab.general, forKey: SettingsKey.settingsTab)
        showSettingsWindow()
    }

    private func showSettingsWindow() {
        NSApp.activate(ignoringOtherApps: true)
        // The private showSettingsWindow: selector is a no-op on macOS 26, so
        // hand off to SwiftUI: a view opens the Settings scene via
        // @Environment(\.openSettings) on receiving this.
        NotificationCenter.default.post(name: .a2fShowSettings, object: nil)
    }

    @objc private func quit(_ sender: NSMenuItem) {
        NSApp.terminate(nil)
    }
}

/// The interactive Uninstall flow, shared so it can live in Settings (it used to
/// be a menu-bar item). Confirms (with an opt-in to also delete saved hosts/
/// tunnels), removes the daemon + LaunchAgent + Keychain creds, reveals the app
/// for the user to trash, then quits.
@MainActor
enum UninstallFlow {
    static func runInteractive() {
        let alert = NSAlert()
        alert.messageText = "Uninstall SSH2FA?"
        alert.informativeText = "This stops and removes the background daemon, deletes its LaunchAgent, and removes every credential SSH2FA saved in your Keychain. Afterward, drag SSH2FA.app to the Trash yourself."
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
        done.messageText = "SSH2FA uninstalled"
        done.informativeText = "The daemon, LaunchAgent and Keychain credentials are removed. Drag SSH2FA.app (now revealed in Finder) to the Trash to finish."
        done.addButton(withTitle: "Quit")
        done.runModal()
        NSApp.terminate(nil)
    }
}
