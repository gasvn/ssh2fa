import Foundation

/// Pure, view-agnostic text matching for the global search field. Lives apart
/// from any SwiftUI / model type so it can be unit-tested headlessly (compiled
/// directly into the test bundle, like SyncCore). Used by HostsView, TunnelsView.
enum SearchFilter {
    /// True if `query` is blank (after trimming), or any non-nil field contains
    /// it (case-insensitive). nil fields are skipped.
    static func matches(query: String, in fields: [String?]) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if q.isEmpty { return true }
        return fields.contains { ($0 ?? "").lowercased().contains(q) }
    }
}
