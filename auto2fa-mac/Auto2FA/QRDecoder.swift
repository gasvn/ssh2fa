import AppKit
import Vision

/// Decodes a QR code from an image (clipboard or a dropped file) — used to read
/// a 2FA `otpauth://` URL off a screenshot instead of hand-pasting it, the #1
/// onboarding friction.
enum QRDecoder {
    /// First QR payload that's an `otpauth://` URL, else the first QR payload, else nil.
    static func decode(from image: NSImage) -> String? {
        guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
            return nil
        }
        let request = VNDetectBarcodesRequest()
        request.symbologies = [.qr]
        let handler = VNImageRequestHandler(cgImage: cg, options: [:])
        try? handler.perform([request])
        let payloads = (request.results ?? []).compactMap { $0.payloadStringValue }
        return payloads.first(where: { $0.lowercased().hasPrefix("otpauth://") }) ?? payloads.first
    }

    /// Read a QR off the current clipboard image (⌘⇧⌃4 copies a screenshot to
    /// the clipboard). nil if there's no image or no QR in it.
    static func decodeFromClipboard() -> String? {
        guard let img = NSPasteboard.general
            .readObjects(forClasses: [NSImage.self], options: nil)?.first as? NSImage else {
            return nil
        }
        return decode(from: img)
    }
}
