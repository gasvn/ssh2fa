import Foundation
import AppKit
import UniformTypeIdentifiers

/// Tunnel config export / import via NSSavePanel / NSOpenPanel.
/// Format is a small JSON object with a schema version, so future schema
/// changes don't silently corrupt user backups.
///
/// Credentials are NOT exported — only tunnel definitions (name, ports,
/// jump candidates, tags, post-connect command, last node, direct host).
/// Hosts and their passwords stay in passwords.json, which the user
/// re-creates via the Add Host wizard on the importing machine.
enum TunnelExportImport {
    struct ExportedTunnel: Codable {
        let name: String
        let local_port: Int
        let remote_port: Int
        let jump_candidates: [String]?
        let last_node: String?
        let last_user: String?
        let auto_start: Bool
        let post_connect_cmd: String?
        let tags: [String]
        /// Browser URL suffix (e.g. a jupyter "?token=…"). Optional so files
        /// exported before this field existed still decode (nil).
        var url_path: String?
        /// Direct-mode target host (forward to this registered host's own
        /// localhost, no jump/node). Optional so older exports decode (nil =
        /// a normal SLURM compute tunnel). Added WITHOUT bumping the schema —
        /// a missing key decodes to nil, exactly like url_path.
        var direct_host: String?
    }

    struct ExportFile: Codable {
        let schema: Int   // bump when the wire format changes
        let exported_at: Date
        let tunnels: [ExportedTunnel]
    }

    static let currentSchema = 1

    /// Show a Save panel, write JSON to the chosen file. Returns nil on
    /// success, error string on failure (or "cancelled" if user backed out).
    @MainActor
    static func exportToFile(_ tunnels: [Tunnel]) -> String? {
        let panel = NSSavePanel()
        panel.title = "Export tunnels"
        panel.message = "Save tunnel definitions (credentials NOT included)"
        panel.nameFieldStringValue = "auto2fa-tunnels-\(stamp()).json"
        if #available(macOS 11.0, *) {
            panel.allowedContentTypes = [.json]
        }
        guard panel.runModal() == .OK, let url = panel.url else { return "cancelled" }
        let payload = ExportFile(
            schema: currentSchema,
            exported_at: Date(),
            tunnels: tunnels.map(toExported)
        )
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        enc.dateEncodingStrategy = .iso8601
        do {
            let data = try enc.encode(payload)
            try data.write(to: url)
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    /// Show an Open panel, load JSON, return parsed tunnels (or nil + error).
    @MainActor
    static func importFromFile() -> (tunnels: [ExportedTunnel]?, error: String?) {
        let panel = NSOpenPanel()
        panel.title = "Import tunnels"
        panel.message = "Pick an auto2fa export JSON"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if #available(macOS 11.0, *) {
            panel.allowedContentTypes = [.json]
        }
        guard panel.runModal() == .OK, let url = panel.url else {
            return (nil, "cancelled")
        }
        do {
            let data = try Data(contentsOf: url)
            let dec = JSONDecoder()
            dec.dateDecodingStrategy = .iso8601
            let file = try dec.decode(ExportFile.self, from: data)
            if file.schema != currentSchema {
                return (nil, "Unsupported schema \(file.schema), this app expects \(currentSchema)")
            }
            return (file.tunnels, nil)
        } catch {
            return (nil, "Failed to parse JSON: \(error.localizedDescription)")
        }
    }

    private static func toExported(_ t: Tunnel) -> ExportedTunnel {
        ExportedTunnel(
            name: t.name,
            local_port: t.localPort,
            remote_port: t.remotePort,
            jump_candidates: t.jumpCandidates,
            last_node: t.lastNode,
            last_user: t.lastUser,
            auto_start: t.autoStart,
            post_connect_cmd: t.postConnectCmd,
            tags: t.tags,
            url_path: t.urlPath,
            direct_host: t.directHost
        )
    }

    private static func stamp() -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyyMMdd-HHmmss"
        return f.string(from: Date())
    }
}
