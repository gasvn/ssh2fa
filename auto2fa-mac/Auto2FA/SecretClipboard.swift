import Foundation
import AppKit

/// Copying a *secret* to the clipboard, with the two safeguards a plain
/// `NSPasteboard.setString` does not give you.
///
/// 1. **Stays on this Mac.** The general pasteboard is shared with your other
///    Apple devices via Universal Clipboard, so a plain copy puts your SSH
///    password on your iPhone. `prepareForNewContents(with: .currentHostOnly)`
///    marks the contents local-only.
/// 2. **Expires.** A secret that sits in the clipboard until something else
///    happens to overwrite it is a secret you have forgotten about. It is
///    cleared after `lifetime`.
///
/// The clear is CONDITIONAL — see `ClipboardExpiry.shouldClear`. If you copied
/// something else in the meantime, wiping the pasteboard would destroy your
/// data to protect ours.
enum SecretClipboard {
    /// How long a copied secret survives before being wiped.
    static let lifetime: TimeInterval = 45

    /// Copy `secret`, local-only, and schedule a conditional wipe.
    /// Returns the deadline, so the UI can tell the user when it expires.
    @discardableResult
    @MainActor
    static func copy(_ secret: String, lifetime: TimeInterval = SecretClipboard.lifetime) -> Date {
        let pb = NSPasteboard.general
        // Local-only + the change count that identifies OUR write.
        pb.prepareForNewContents(with: .currentHostOnly)
        pb.setString(secret, forType: .string)
        let stamp = pb.changeCount

        Task {
            try? await Task.sleep(nanoseconds: UInt64(lifetime * 1_000_000_000))
            let pb = NSPasteboard.general
            guard ClipboardExpiry.shouldClear(writtenChangeCount: stamp,
                                              currentChangeCount: pb.changeCount) else { return }
            pb.clearContents()
        }
        return Date().addingTimeInterval(lifetime)
    }
}

/// Pure decision logic for the conditional wipe (Foundation-only, unit-tested).
enum ClipboardExpiry {
    /// Clear only if the clipboard still holds what we put there.
    ///
    /// `NSPasteboard.changeCount` increments on every write by any process. If
    /// it still equals the value from our own write, ours is the current
    /// content and clearing it is right. If it has moved on, the user (or
    /// another app) copied something else — clearing then would silently
    /// destroy unrelated clipboard data, which is a worse bug than a lingering
    /// secret.
    static func shouldClear(writtenChangeCount: Int, currentChangeCount: Int) -> Bool {
        writtenChangeCount == currentChangeCount
    }
}
