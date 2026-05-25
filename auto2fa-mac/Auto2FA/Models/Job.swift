import Foundation

/// A running SLURM job as reported by `squeue` via the daemon.
/// Named `SqueueJob` to avoid colliding with Swift's `_Concurrency.Job`.
struct SqueueJob: Identifiable, Codable, Equatable, Hashable {
    let jobid: String
    let partition: String
    let name: String
    let state: String
    let time: String
    let node: String

    var id: String { jobid }
}
