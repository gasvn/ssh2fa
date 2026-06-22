import Foundation

/// Best-effort parser for "ssh -L localPort:host:remotePort user@node" style
/// strings the user might have in their clipboard (often pasted from
/// docs / chat / their own .ssh/config notes). Also recognises common
/// notebook URLs like "http://localhost:8888/?token=abc" and treats the
/// port + token suffix as the tunnel target.
enum SSHCommandParser {
    struct Parsed: Equatable {
        var suggestedName: String?
        var localPort: Int?
        var remotePort: Int?
        var node: String?
        var user: String?
        /// Optional path/query to append after the host (for jupyter etc.).
        var pathQuery: String?
    }

    static func parse(_ raw: String) -> Parsed? {
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return nil }

        // Notebook URL form: http://localhost:8888/?token=...
        if let url = URL(string: s),
           let scheme = url.scheme?.lowercased(),
           scheme == "http" || scheme == "https",
           let host = url.host,
           host == "localhost" || host == "127.0.0.1" {
            var p = Parsed()
            p.localPort = url.port ?? (scheme == "https" ? 443 : 80)
            var path = url.path
            if let q = url.query, !q.isEmpty {
                path = path + "?" + q
            }
            p.pathQuery = path.isEmpty ? nil : path
            p.suggestedName = "notebook"
            return p
        }

        // SSH command form: ssh [opts] -L <lp>:<h>:<rp> [opts] user@node [cmd]
        // Tokenise on whitespace (good enough — quoted paths are rare here).
        let tokens = s.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        guard tokens.first == "ssh" || tokens.first == "ssh.exe" else { return nil }

        var p = Parsed()
        var i = 1
        while i < tokens.count {
            let tok = tokens[i]
            if tok == "-L" && i + 1 < tokens.count {
                // -L localPort:host:remotePort  OR  -L localPort:host:remotePort:bind
                let parts = tokens[i + 1].split(separator: ":").map(String.init)
                if parts.count >= 3,
                   let lp = Int(parts[0]),
                   let rp = Int(parts[parts.count - 1]) {
                    p.localPort = lp
                    p.remotePort = rp
                }
                i += 2
                continue
            }
            if tok.hasPrefix("-") {
                // Skip flag (might consume next token if it's a value, but the
                // common forwarding flags -L/-N/-T are zero-arg or already
                // handled. We can be liberal here.)
                if tok == "-i" || tok == "-o" || tok == "-p" || tok == "-J" {
                    i += 2
                } else {
                    i += 1
                }
                continue
            }
            // First positional non-flag = user@host
            if let at = tok.firstIndex(of: "@") {
                p.user = String(tok[tok.startIndex..<at])
                p.node = String(tok[tok.index(after: at)...])
            } else {
                p.node = tok
            }
            break
        }
        // Need at least a host to be useful.
        guard p.node != nil else { return nil }
        // Default name from forwarded port if not otherwise specified.
        if let lp = p.localPort {
            switch lp {
            case 8888: p.suggestedName = "jupyter"
            case 6006: p.suggestedName = "tensorboard"
            case 8080, 8000: p.suggestedName = "web"
            default:   p.suggestedName = "p\(lp)"
            }
        }
        return p
    }
}
