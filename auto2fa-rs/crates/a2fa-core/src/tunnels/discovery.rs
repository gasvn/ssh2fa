use crate::error::{Error, Result};

/// A single SLURM job row from `squeue -h -o '%i|%P|%j|%T|%M|%R'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub jobid: String,
    pub partition: String,
    pub name: String,
    pub state: String,
    pub time: String,
    pub node: String,
}

/// Parse the stdout of `squeue -h -o '%i|%P|%j|%T|%M|%R'`.
///
/// - Skips blank lines and malformed rows (wrong field count or empty node).
/// - Does NOT filter by state — callers can filter by `job.state == "RUNNING"`
///   if desired. (The Python reference filtered only RUNNING; we expose all
///   so that callers have the choice.)
pub fn parse_squeue(out: &str) -> Vec<Job> {
    let mut jobs = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(7, '|').collect();
        if parts.len() != 6 {
            log::debug!("skipping malformed squeue row: {:?}", line);
            continue;
        }
        let node = parts[5].trim().to_string();
        if node.is_empty() {
            log::debug!("skipping squeue row with empty node: {:?}", line);
            continue;
        }
        jobs.push(Job {
            jobid:     parts[0].trim().to_string(),
            partition: parts[1].trim().to_string(),
            name:      parts[2].trim().to_string(),
            state:     parts[3].trim().to_string(),
            time:      parts[4].trim().to_string(),
            node,
        });
    }
    jobs
}

/// Run `squeue` on the jump host via a plain `ssh` call.
///
/// The jump host's SSH master **must** already be live; this never opens a
/// new connection (it relies on `ControlPath` being supplied externally or
/// the ambient agent/known-hosts setup).  Pass the full `user@host` string
/// (or just `host`) as `jump`.
///
/// Returns `Err(Error::Discovery(_))` when the command times out, fails to
/// spawn, or squeue exits non-zero.
pub fn discover_nodes(jump: &str) -> Result<Vec<Job>> {
    use std::process::Command;
    let output = Command::new("ssh")
        .args([
            jump,
            "squeue",
            "-h",
            "-o",
            "%i|%P|%j|%T|%M|%R",
        ])
        .output()
        .map_err(|e| Error::Discovery(format!("ssh spawn failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Discovery(format!(
            "squeue failed on {jump}: {}",
            stderr.trim().chars().take(200).collect::<String>()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_squeue(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_squeue_rows() {
        let jobs = parse_squeue(
            "123|gpu|run|RUNNING|01:00:00|holygpu01\nbad row\n456|cpu|x|PENDING|0:00|\n",
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].node, "holygpu01");
        assert_eq!(jobs[0].state, "RUNNING");
    }

    #[test]
    fn empty_on_no_rows() {
        assert!(parse_squeue("").is_empty());
    }

    #[test]
    fn skips_row_with_empty_node() {
        // The 6th field (node) is blank → row must be skipped.
        let jobs = parse_squeue("456|cpu|x|PENDING|0:00|");
        assert!(jobs.is_empty());
    }

    #[test]
    fn parses_multiple_valid_rows() {
        let raw = "1|gpu|train|RUNNING|2:00:00|holygpu01\n\
                   2|cpu|eval|RUNNING|0:30:00|holycpu05\n";
        let jobs = parse_squeue(raw);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].jobid, "1");
        assert_eq!(jobs[1].node, "holycpu05");
    }

    #[test]
    fn bad_row_count_is_skipped() {
        // 5 fields instead of 6
        let jobs = parse_squeue("1|gpu|train|RUNNING|2:00:00");
        assert!(jobs.is_empty());
    }

    #[test]
    fn preserves_all_fields() {
        let jobs = parse_squeue("999|gpu|myrun|RUNNING|03:14:15|holygpu42");
        assert_eq!(jobs.len(), 1);
        let j = &jobs[0];
        assert_eq!(j.jobid, "999");
        assert_eq!(j.partition, "gpu");
        assert_eq!(j.name, "myrun");
        assert_eq!(j.state, "RUNNING");
        assert_eq!(j.time, "03:14:15");
        assert_eq!(j.node, "holygpu42");
    }
}
