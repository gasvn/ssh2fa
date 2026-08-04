import Foundation
import CryptoKit
import Security

/// Release-only utility. The Ed25519 private key is generated and consumed
/// inside macOS Keychain APIs; it is never printed or passed through argv.
@main
struct UpdateSigner {
    private static let service = "com.ssh2fa.release.update-signing"
    private static let keyID = UpdateSigningCore.currentKeyID

    enum ToolError: Error, CustomStringConvertible {
        case usage
        case keychain(OSStatus)
        case missingKey
        case invalidKey
        case invalidManifest
        case publicKeyMismatch
        case unreadableFile(String)

        var description: String {
            switch self {
            case .usage:
                return "usage: update-signer initialize | public-key | sign-manifest <dmg> <version> <build> <output> | verify-manifest <manifest> <dmg> <version>"
            case .keychain(let status):
                let message = SecCopyErrorMessageString(status, nil) as String? ?? "unknown"
                return "Keychain error \(status): \(message)"
            case .missingKey:
                return "release signing key is missing; run update-signer initialize"
            case .invalidKey:
                return "stored release signing key is invalid"
            case .invalidManifest:
                return "update manifest or disk image failed verification"
            case .publicKeyMismatch:
                return "stored private key does not match the public key pinned in SSH2FA"
            case .unreadableFile(let path):
                return "cannot read \(path)"
            }
        }
    }

    static func main() {
        do {
            let args = Array(CommandLine.arguments.dropFirst())
            guard let command = args.first else { throw ToolError.usage }
            switch command {
            case "initialize" where args.count == 1:
                let key = try loadKey() ?? createKey()
                print("key-id=\(keyID)")
                print("public-key=\(key.publicKey.rawRepresentation.base64EncodedString())")
            case "public-key" where args.count == 1:
                guard let key = try loadKey() else { throw ToolError.missingKey }
                print("key-id=\(keyID)")
                print("public-key=\(key.publicKey.rawRepresentation.base64EncodedString())")
            case "sign-manifest" where args.count == 5:
                guard let key = try loadKey() else { throw ToolError.missingKey }
                try signManifest(dmgPath: args[1], version: args[2], build: args[3],
                                 outputPath: args[4], key: key)
            case "verify-manifest" where args.count == 4:
                try verifyManifest(manifestPath: args[1], dmgPath: args[2],
                                   advertisedVersion: args[3])
            default:
                throw ToolError.usage
            }
        } catch {
            FileHandle.standardError.write(Data("ERROR: \(error)\n".utf8))
            exit(1)
        }
    }

    private static func loadKey() throws -> Curve25519.Signing.PrivateKey? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: keyID,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw ToolError.keychain(status) }
        guard let raw = item as? Data,
              let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: raw)
        else { throw ToolError.invalidKey }
        return key
    }

    private static func createKey() throws -> Curve25519.Signing.PrivateKey {
        let key = Curve25519.Signing.PrivateKey()
        let item: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: keyID,
            kSecAttrLabel: "SSH2FA release update signing key",
            kSecAttrDescription: "Ed25519 private key for signed SSH2FA update manifests",
            kSecAttrAccessible: kSecAttrAccessibleWhenUnlocked,
            kSecValueData: key.rawRepresentation
        ]
        let status = SecItemAdd(item as CFDictionary, nil)
        guard status == errSecSuccess else { throw ToolError.keychain(status) }
        return key
    }

    private static func signManifest(dmgPath: String,
                                     version: String,
                                     build: String,
                                     outputPath: String,
                                     key: Curve25519.Signing.PrivateKey) throws {
        let dmg = URL(fileURLWithPath: dmgPath)
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: dmg.path),
              let sizeNumber = attrs[.size] as? NSNumber else {
            throw ToolError.unreadableFile(dmgPath)
        }
        guard UpdateSigningCore.trustedPublicKeys[keyID]
                == key.publicKey.rawRepresentation else {
            throw ToolError.publicKeyMismatch
        }
        let digest = try sha256(dmg)
        let manifest = try UpdateSigningCore.Manifest.signed(
            version: version,
            build: build,
            size: sizeNumber.int64Value,
            sha256: digest,
            keyID: keyID,
            privateKey: key)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        var data = try encoder.encode(manifest)
        data.append(0x0A)
        try data.write(to: URL(fileURLWithPath: outputPath), options: .atomic)
        print("signed-manifest=\(outputPath)")
        print("key-id=\(keyID)")
        print("sha256=\(digest)")
    }

    private static func verifyManifest(manifestPath: String,
                                       dmgPath: String,
                                       advertisedVersion: String) throws {
        let manifestData: Data
        do { manifestData = try Data(contentsOf: URL(fileURLWithPath: manifestPath)) }
        catch { throw ToolError.unreadableFile(manifestPath) }
        let manifest: UpdateSigningCore.Manifest
        switch UpdateSigningCore.decodeManifest(manifestData) {
        case .success(let value): manifest = value
        case .failure: throw ToolError.invalidManifest
        }
        guard manifest.validationProblem(
            advertisedVersion: advertisedVersion,
            trustedKeys: UpdateSigningCore.trustedPublicKeys) == nil else {
            throw ToolError.invalidManifest
        }
        let dmg = URL(fileURLWithPath: dmgPath)
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: dmg.path),
              let size = (attrs[.size] as? NSNumber)?.int64Value else {
            throw ToolError.unreadableFile(dmgPath)
        }
        let digest = try sha256(dmg)
        guard manifest.digestMatches(digest, actualSize: size) else {
            throw ToolError.invalidManifest
        }
        print("verified-manifest=\(manifestPath)")
        print("key-id=\(manifest.keyID)")
        print("sha256=\(digest)")
    }

    private static func sha256(_ url: URL) throws -> String {
        guard let handle = try? FileHandle(forReadingFrom: url) else {
            throw ToolError.unreadableFile(url.path)
        }
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 1 << 20), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}
