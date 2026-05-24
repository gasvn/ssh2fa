import Foundation

/// A running SLURM job as reported by `squeue` via the daemon.
struct Job: Identifiable, Codable, Equatable, Hashable {
    let jobid: String
    let partition: String
    let name: String
    let state: String
    let time: String
    let node: String

    var id: String { jobid }
}
