import Foundation
import CryptoKit

/// Independent release authentication for SSH2FA's free, self-signed builds.
///
/// Apple code signing still gives the daemon a stable Keychain identity, but a
/// self-signed certificate is intentionally outside macOS's system trust roots.
/// Update authenticity therefore uses a separate Ed25519 key owned by this
/// project. The private key never ships; the public key is pinned below.
enum UpdateSigningCore {
    static let schema = 1
    static let manifestAssetName = "SSH2FA.update.json"
    static let dmgAssetName = "SSH2FA.dmg"
    static let currentKeyID = "ssh2fa-ed25519-2026-01"

    /// Public keys are raw 32-byte Ed25519 keys encoded as Base64. Keeping a
    /// keyring (rather than one scalar) permits a future release signed by the
    /// old key to introduce its successor without stranding installed users.
    // Public, safe in Git. The matching private key lives only in the release
    // maintainer's login Keychain under com.ssh2fa.release.update-signing.
    static let trustedPublicKeysBase64: [String: String] = [
        "ssh2fa-ed25519-2026-01": "T4RS7svFaNgqHSoqY1lWw/+6t3IBN/osrCTvt50ojAI="
    ]

    static var trustedPublicKeys: [String: Data] {
        trustedPublicKeysBase64.reduce(into: [:]) { result, item in
            if let data = Data(base64Encoded: item.value), data.count == 32 {
                result[item.key] = data
            }
        }
    }

    enum ManifestProblem: Error, Equatable {
        case malformed
        case unsupportedSchema
        case invalidVersion
        case versionMismatch
        case invalidBuild
        case wrongAsset
        case invalidSize
        case invalidDigest
        case unknownKey
        case invalidSignatureEncoding
        case signatureMismatch
    }

    struct Manifest: Codable, Equatable {
        let schema: Int
        let version: String
        let build: String
        let asset: String
        let size: Int64
        let sha256: String
        let keyID: String
        let signature: String

        enum CodingKeys: String, CodingKey {
            case schema, version, build, asset, size, sha256
            case keyID = "key_id"
            case signature
        }

        /// The exact bytes signed by the release key. Every free-form field is
        /// syntax-checked before verification, so newline delimiters are
        /// unambiguous and reproducible across Swift/Foundation versions.
        var signingPayload: Data {
            Data("""
            SSH2FA-UPDATE-MANIFEST-V1
            schema:\(schema)
            version:\(version)
            build:\(build)
            asset:\(asset)
            size:\(size)
            sha256:\(sha256.lowercased())
            key_id:\(keyID)

            """.utf8)
        }

        func validationProblem(advertisedVersion: String,
                               trustedKeys: [String: Data]) -> ManifestProblem? {
            guard schema == UpdateSigningCore.schema else { return .unsupportedSchema }
            guard Self.isVersion(version) else { return .invalidVersion }
            guard Self.normalizedVersion(version) == Self.normalizedVersion(advertisedVersion)
            else { return .versionMismatch }
            guard Self.isASCIIDigits(build) else { return .invalidBuild }
            guard asset == UpdateSigningCore.dmgAssetName else { return .wrongAsset }
            guard size > 0 else { return .invalidSize }
            guard Self.isSHA256(sha256) else { return .invalidDigest }
            guard Self.isIdentifier(keyID), let rawKey = trustedKeys[keyID]
            else { return .unknownKey }
            guard let rawSignature = Data(base64Encoded: signature), rawSignature.count == 64
            else { return .invalidSignatureEncoding }
            guard let publicKey = try? Curve25519.Signing.PublicKey(rawRepresentation: rawKey),
                  publicKey.isValidSignature(rawSignature, for: signingPayload)
            else { return .signatureMismatch }
            return nil
        }

        func digestMatches(_ actual: String, actualSize: Int64) -> Bool {
            actualSize == size && actual.lowercased() == sha256.lowercased()
        }

        static func signed(version: String,
                           build: String,
                           asset: String = UpdateSigningCore.dmgAssetName,
                           size: Int64,
                           sha256: String,
                           keyID: String,
                           privateKey: Curve25519.Signing.PrivateKey) throws -> Manifest {
            let unsigned = Manifest(schema: UpdateSigningCore.schema,
                                    version: version,
                                    build: build,
                                    asset: asset,
                                    size: size,
                                    sha256: sha256.lowercased(),
                                    keyID: keyID,
                                    signature: "")
            guard unsigned.validationSyntaxProblem == nil else {
                throw SigningError.invalidManifestFields
            }
            let signature = try privateKey.signature(for: unsigned.signingPayload)
            return Manifest(schema: unsigned.schema,
                            version: unsigned.version,
                            build: unsigned.build,
                            asset: unsigned.asset,
                            size: unsigned.size,
                            sha256: unsigned.sha256,
                            keyID: unsigned.keyID,
                            signature: signature.base64EncodedString())
        }

        private var validationSyntaxProblem: ManifestProblem? {
            guard schema == UpdateSigningCore.schema else { return .unsupportedSchema }
            guard Self.isVersion(version) else { return .invalidVersion }
            guard Self.isASCIIDigits(build) else { return .invalidBuild }
            guard asset == UpdateSigningCore.dmgAssetName else { return .wrongAsset }
            guard size > 0 else { return .invalidSize }
            guard Self.isSHA256(sha256) else { return .invalidDigest }
            guard Self.isIdentifier(keyID) else { return .unknownKey }
            return nil
        }

        private static func normalizedVersion(_ value: String) -> String? {
            var value = value.trimmingCharacters(in: .whitespacesAndNewlines)
            if value.first?.lowercased() == "v" { value.removeFirst() }
            return isVersion(value) ? value : nil
        }

        private static func isVersion(_ value: String) -> Bool {
            let parts = value.split(separator: ".", omittingEmptySubsequences: false)
            return (2...4).contains(parts.count)
                && parts.allSatisfy { isASCIIDigits(String($0)) }
        }

        private static func isSHA256(_ value: String) -> Bool {
            value.utf8.count == 64 && value.utf8.allSatisfy {
                (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
            }
        }

        private static func isASCIIDigits(_ value: String) -> Bool {
            !value.isEmpty && value.utf8.allSatisfy { (48...57).contains($0) }
        }

        private static func isIdentifier(_ value: String) -> Bool {
            !value.isEmpty && value.count <= 80 && value.allSatisfy {
                $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_")
            }
        }
    }

    enum SigningError: Error {
        case invalidManifestFields
    }

    static func decodeManifest(_ data: Data) -> Result<Manifest, ManifestProblem> {
        do {
            return .success(try JSONDecoder().decode(Manifest.self, from: data))
        } catch {
            return .failure(.malformed)
        }
    }
}
