import Foundation

/// A running SLURM job as reported by `squeue` via the daemon.
/// Named `SqueueJob` to avoid colliding with Swift's `_Concurrency.Job`.
struct SqueueJob: Identifiable, Codable, Equatable, Hashable {
    let jobid: String
    let partition: String
    let name: String
    let state: String
    let time: String
    /// SLURM TIME_LEFT (`%L`). Optional — an older daemon doesn't send it, in
    /// which case there's simply no expiry countdown.
    let timeLeft: String?
    let node: String

    enum CodingKeys: String, CodingKey {
        case jobid, partition, name, state, time, node
        case timeLeft = "time_left"
    }

    var id: String { jobid }
}
