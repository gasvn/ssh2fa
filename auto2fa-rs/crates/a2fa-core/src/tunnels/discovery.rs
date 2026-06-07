use crate::error::{Error, Result};
use regex::Regex;

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

/// Expand a SLURM nodelist string to its first concrete node.
///
/// SLURM often returns a compressed nodelist such as `holygpu[01-03]` or
/// `holygpu[01,03,05]` rather than a single hostname.  This function extracts
/// the first node from the list so it can be used directly as an SSH target.
///
/// # Return value
///
/// Returns `(first_node, is_range)`:
/// - `is_range` is `true` when bracket notation was present (the caller may
///   want to surface that to the user).
/// - `is_range` is `false` when the input looks like a plain hostname.
///
/// # Examples
///
/// ```
/// use a2fa_core::tunnels::expand_first_node;
///
/// assert_eq!(expand_first_node("holygpu01"),          ("holygpu01".into(),                   false));
/// assert_eq!(expand_first_node("holygpu[01-03]"),     ("holygpu01".into(),                   true));
/// assert_eq!(expand_first_node("holygpu[01,03,05]"),  ("holygpu01".into(),                   true));
/// assert_eq!(
///     expand_first_node("holygpu[01-03].rc.fas.harvard.edu"),
///     ("holygpu01.rc.fas.harvard.edu".into(), true)
/// );
/// ```
///
/// # Fallback
///
/// Any input that does not contain a well-formed bracket expression is returned
/// unchanged with `is_range = false`.  This mirrors the Python reference
/// implementation in `auto2fa/tunnels.py`.
pub fn expand_first_node(nodelist: &str) -> (String, bool) {
    // Pattern mirrors the Python reference:
    //   ^([a-zA-Z0-9_.-]+)\[([^\]]+)\](.*)$
    // Group 1 — prefix (e.g. "holygpu")
    // Group 2 — bracket contents (e.g. "01-03" or "01,03,05")
    // Group 3 — optional suffix after the bracket (e.g. ".rc.fas.harvard.edu")
    let re = Regex::new(r"^([a-zA-Z0-9_.\\-]+)\[([^\]]+)\](.*)$")
        .expect("expand_first_node regex is valid");

    match re.captures(nodelist) {
        None => (nodelist.to_owned(), false),
        Some(caps) => {
            let prefix = &caps[1];
            let inside = &caps[2];
            let suffix = &caps[3];
            // Take the first comma-separated chunk, then the first dash-separated
            // element from that chunk (handles both ranges and comma lists).
            let first_chunk = inside.split(',').next().unwrap_or("").trim();
            let first_num  = first_chunk.split('-').next().unwrap_or("").trim();
            (format!("{prefix}{first_num}{suffix}"), true)
        }
    }
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

/// Run `squeue` on the jump host, **reusing an existing SSH ControlMaster**.
///
/// This is the variant used by the daemon's `discover_nodes` handler: it passes
/// `ControlPath=<path>` so `ssh` multiplexes over the already-authenticated
/// master socket instead of opening a new connection (which would trigger 2FA
/// again).
///
/// `control_path` must be the path returned by
/// `a2fa_core::ssh::control::active_symlink_path(host)`.
///
/// Returns `Err(Error::Discovery(_))` on any failure.
pub fn discover_nodes_via_control(jump: &str, control_path: &std::path::Path) -> Result<Vec<Job>> {
    use std::process::Command;
    let cp = control_path.to_string_lossy();
    let output = Command::new("ssh")
        .args([
            "-o",
            &format!("ControlPath={cp}"),
            // Disable ControlMaster so we don't accidentally try to become a
            // new master if the socket has vanished.
            "-o",
            "ControlMaster=no",
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
            "squeue via control failed on {jump}: {}",
            stderr.trim().chars().take(200).collect::<String>()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_squeue(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- expand_first_node ---------------------------------------------

    #[test]
    fn expand_first_node_table() {
        // Each tuple: (input, expected_node, expected_is_range)
        let cases: &[(&str, &str, bool)] = &[
            // Plain hostname — no brackets → returned unchanged, not a range.
            ("holygpu01",                          "holygpu01",                          false),
            // Dash range → first element extracted, is_range = true.
            ("holygpu[01-03]",                     "holygpu01",                          true),
            // Comma list → first element extracted.
            ("holygpu[01,03,05]",                  "holygpu01",                          true),
            // Suffix after the closing bracket must be preserved.
            ("holygpu[01-03].rc.fas.harvard.edu",  "holygpu01.rc.fas.harvard.edu",       true),
            // Malformed / no brackets → returned unchanged, not a range.
            ("holygpu[unclosed",                   "holygpu[unclosed",                   false),
            // Empty string → returned unchanged.
            ("",                                   "",                                   false),
        ];

        for (input, want_node, want_range) in cases {
            let (got_node, got_range) = expand_first_node(input);
            assert_eq!(
                got_node, *want_node,
                "node mismatch for input {input:?}"
            );
            assert_eq!(
                got_range, *want_range,
                "is_range mismatch for input {input:?}"
            );
        }
    }

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
