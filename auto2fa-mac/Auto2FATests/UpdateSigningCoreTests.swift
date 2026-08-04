import XCTest
import CryptoKit

final class UpdateSigningCoreTests: XCTestCase {
    private let digest = String(repeating: "a", count: 64)

    private func signedManifest(key: Curve25519.Signing.PrivateKey,
                                version: String = "1.5.12",
                                build: String = "162",
                                size: Int64 = 7_500_000,
                                sha256: String? = nil,
                                keyID: String = "test-key") throws
        -> UpdateSigningCore.Manifest {
        try UpdateSigningCore.Manifest.signed(
            version: version, build: build, size: size,
            sha256: sha256 ?? digest, keyID: keyID, privateKey: key)
    }

    func testValidManifestVerifiesWithPinnedPublicKey() throws {
        let key = Curve25519.Signing.PrivateKey()
        let manifest = try signedManifest(key: key)
        XCTAssertNil(manifest.validationProblem(
            advertisedVersion: "v1.5.12",
            trustedKeys: ["test-key": key.publicKey.rawRepresentation]))
        XCTAssertTrue(manifest.digestMatches(digest, actualSize: 7_500_000))
    }

    func testTamperedDigestInvalidatesSignature() throws {
        let key = Curve25519.Signing.PrivateKey()
        let valid = try signedManifest(key: key)
        let tampered = UpdateSigningCore.Manifest(
            schema: valid.schema, version: valid.version, build: valid.build,
            asset: valid.asset, size: valid.size,
            sha256: String(repeating: "b", count: 64),
            keyID: valid.keyID, signature: valid.signature)
        XCTAssertEqual(tampered.validationProblem(
            advertisedVersion: "1.5.12",
            trustedKeys: ["test-key": key.publicKey.rawRepresentation]),
            .signatureMismatch)
    }

    func testTamperedSizeInvalidatesSignature() throws {
        let key = Curve25519.Signing.PrivateKey()
        let valid = try signedManifest(key: key)
        let tampered = UpdateSigningCore.Manifest(
            schema: valid.schema, version: valid.version, build: valid.build,
            asset: valid.asset, size: valid.size + 1, sha256: valid.sha256,
            keyID: valid.keyID, signature: valid.signature)
        XCTAssertEqual(tampered.validationProblem(
            advertisedVersion: "1.5.12",
            trustedKeys: ["test-key": key.publicKey.rawRepresentation]),
            .signatureMismatch)
    }

    func testWrongAndUnknownKeysAreRejected() throws {
        let signer = Curve25519.Signing.PrivateKey()
        let wrong = Curve25519.Signing.PrivateKey()
        let manifest = try signedManifest(key: signer)
        XCTAssertEqual(manifest.validationProblem(
            advertisedVersion: "1.5.12", trustedKeys: [:]), .unknownKey)
        XCTAssertEqual(manifest.validationProblem(
            advertisedVersion: "1.5.12",
            trustedKeys: ["test-key": wrong.publicKey.rawRepresentation]),
            .signatureMismatch)
    }

    func testReplayUnderAnotherVersionIsRejected() throws {
        let key = Curve25519.Signing.PrivateKey()
        let manifest = try signedManifest(key: key, version: "1.5.12")
        XCTAssertEqual(manifest.validationProblem(
            advertisedVersion: "1.5.13",
            trustedKeys: ["test-key": key.publicKey.rawRepresentation]),
            .versionMismatch)
    }

    func testMalformedSignatureAndFieldsAreRejected() throws {
        let key = Curve25519.Signing.PrivateKey()
        let valid = try signedManifest(key: key)
        let malformedSignature = UpdateSigningCore.Manifest(
            schema: valid.schema, version: valid.version, build: valid.build,
            asset: valid.asset, size: valid.size, sha256: valid.sha256,
            keyID: valid.keyID, signature: "not-base64")
        XCTAssertEqual(malformedSignature.validationProblem(
            advertisedVersion: "1.5.12",
            trustedKeys: ["test-key": key.publicKey.rawRepresentation]),
            .invalidSignatureEncoding)

        XCTAssertThrowsError(try signedManifest(key: key, version: "1.5.12\nsize:1"))
        XCTAssertThrowsError(try signedManifest(key: key, build: "16x"))
        XCTAssertThrowsError(try signedManifest(key: key, build: "１６２"))
        XCTAssertThrowsError(try signedManifest(key: key, version: "1.５.12"))
        XCTAssertThrowsError(try signedManifest(key: key, size: 0))
    }

    func testManifestJSONRoundTripsAndMalformedJSONFailsClosed() throws {
        let key = Curve25519.Signing.PrivateKey()
        let manifest = try signedManifest(key: key)
        let data = try JSONEncoder().encode(manifest)
        XCTAssertEqual(try UpdateSigningCore.decodeManifest(data).get(), manifest)
        XCTAssertEqual(UpdateSigningCore.decodeManifest(Data("{}".utf8)),
                       .failure(.malformed))
    }

    func testDigestRequiresBothExactHashAndSize() throws {
        let key = Curve25519.Signing.PrivateKey()
        let manifest = try signedManifest(key: key)
        XCTAssertFalse(manifest.digestMatches(String(repeating: "b", count: 64),
                                              actualSize: manifest.size))
        XCTAssertFalse(manifest.digestMatches(digest,
                                              actualSize: manifest.size - 1))
    }
}
