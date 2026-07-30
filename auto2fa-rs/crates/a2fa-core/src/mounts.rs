//! Enumerating sshfs mounts SAFELY.
//!
//! # Why not `stat`
//!
//! The obvious way to ask "is this mounted?" is to stat the path and compare
//! device ids (what `is_mount_point` does). On a WEDGED macFUSE mount — the
//! state you land in after the network drops — that stat blocks in the kernel
//! and never returns. Anything that walks a list of mount points with stat can
//! therefore hang on the first dead one, which is precisely the situation where
//! the user most needs the app to still work.
//!
//! `/sbin/mount` reads the kernel's mount table. It never touches the
//! filesystems it reports, so it answers just as fast for a wedged mount as a
//! healthy one. It is the only enumeration primitive used here.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// One active mount under the app's mounts root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    /// Where it is mounted, e.g. `/Users/me/Mounts/k6/scratch`.
    pub mount_point: PathBuf,
    /// Host alias derived from the path (`k6`).
    pub host: String,
    /// Directory name under the host (`scratch`), i.e. the slug.
    pub slug: String,
    /// The `device` column, e.g. `k6:/scratch` — what is mounted.
    pub source: String,
}

/// Parse `mount` output, keeping only mounts under `root`.
///
/// A line looks like:
/// `k6:/scratch on /Users/me/Mounts/k6/scratch (macfuse, nodev, nosuid, ...)`
///
/// Parsing is deliberately backend-agnostic (`<source> on <point> (<opts>)`):
/// sshfs on macOS has shipped both a macFUSE and an NFS-backed implementation,
/// and the mount type differs between them.
pub fn parse_mount_output(output: &str, root: &Path) -> Vec<MountInfo> {
    let mut out = Vec::new();
    for line in output.lines() {
        // Split off the trailing " (opts)" first: a mount point can itself
        // contain " on ", so scan from the right for the options group.
        let body = match line.rfind(" (") {
            Some(i) => &line[..i],
            None => line,
        };
        // The LAST " on " separates source from mount point — a source or a
        // path may legitimately contain " on ".
        let Some(idx) = body.rfind(" on ") else { continue };
        let source = body[..idx].trim();
        let point = body[idx + 4..].trim();
        if source.is_empty() || point.is_empty() {
            continue;
        }
        let point_path = PathBuf::from(point);
        let Ok(rel) = point_path.strip_prefix(root) else { continue };
        let parts: Vec<_> = rel.components().collect();
        // Expect exactly <host>/<slug>. A bare <host> is a legacy single-mount
        // layout, reported with an empty slug so callers can migrate it.
        let (host, slug) = match parts.len() {
            1 => (parts[0].as_os_str().to_string_lossy().to_string(), String::new()),
            2 => (
                parts[0].as_os_str().to_string_lossy().to_string(),
                parts[1].as_os_str().to_string_lossy().to_string(),
            ),
            _ => continue,
        };
        out.push(MountInfo {
            mount_point: point_path,
            host,
            slug,
            source: source.to_string(),
        });
    }
    out
}

/// Every active mount under `root`, read from the kernel mount table.
///
/// Bounded: `mount` is instant, but this must never be the thing that hangs.
/// Returns an empty list if it cannot be run — callers treat that as "nothing
/// known to be mounted", which is the safe direction (it offers a mount rather
/// than claiming one exists).
pub fn list_active_mounts(root: &Path) -> Vec<MountInfo> {
    let Some(output) = crate::sys::run_cmd_bounded("/sbin/mount", &[], Duration::from_secs(5))
    else {
        log::warn!("[mounts] could not run /sbin/mount; assuming nothing is mounted");
        return Vec::new();
    };
    parse_mount_output(&String::from_utf8_lossy(&output.stdout), root)
}

/// Turn a remote path into a filesystem-safe directory name.
///
/// `/` → `root`; `/scratch/alice/project` → `scratch-alice-project`. The result
/// is the last path component prefixed by enough of its parents to stay
/// distinguishable, truncated so it cannot blow past a filesystem name limit.
pub fn slug_for(remote_path: &str) -> String {
    let trimmed = remote_path.trim().trim_matches('/');
    if trimmed.is_empty() {
        return "root".to_string();
    }
    let mut s: String = trimmed
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    // Collapse runs of '-' introduced by separators.
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        return "root".to_string();
    }
    // 255 is the usual per-component limit; stay well clear.
    if s.chars().count() > 60 {
        let tail: String = s.chars().skip(s.chars().count() - 60).collect();
        return tail.trim_matches('-').to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/Users/me/Mounts")
    }

    #[test]
    fn parses_a_typical_sshfs_line() {
        let out = "k6:/scratch on /Users/me/Mounts/k6/scratch (macfuse, nodev, nosuid)";
        let m = parse_mount_output(out, &root());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].host, "k6");
        assert_eq!(m[0].slug, "scratch");
        assert_eq!(m[0].source, "k6:/scratch");
        assert_eq!(m[0].mount_point, PathBuf::from("/Users/me/Mounts/k6/scratch"));
    }

    #[test]
    fn ignores_mounts_outside_our_root() {
        let out = "/dev/disk1s1 on / (apfs, local, journaled)\n\
                   map auto_home on /System/Volumes/Data/home (autofs, automounted)";
        assert!(parse_mount_output(out, &root()).is_empty());
    }

    /// Several mounts for the SAME host is the whole point of the new layout.
    #[test]
    fn reports_multiple_mounts_per_host() {
        let out = "k6:/scratch on /Users/me/Mounts/k6/scratch (macfuse)\n\
                   k6:/work on /Users/me/Mounts/k6/work (macfuse)\n\
                   b8:/data on /Users/me/Mounts/b8/data (macfuse)";
        let m = parse_mount_output(out, &root());
        assert_eq!(m.len(), 3);
        assert_eq!(m.iter().filter(|x| x.host == "k6").count(), 2);
    }

    /// The pre-existing single-mount layout mounted at ~/Mounts/<host> itself.
    /// It must still be REPORTED (with an empty slug) so it can be migrated,
    /// not silently ignored as if nothing were mounted.
    #[test]
    fn reports_legacy_single_mount_with_empty_slug() {
        let out = "k6:/ on /Users/me/Mounts/k6 (macfuse)";
        let m = parse_mount_output(out, &root());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].host, "k6");
        assert_eq!(m[0].slug, "");
    }

    /// A path containing " on " must not confuse the split.
    #[test]
    fn handles_paths_containing_the_separator() {
        let out = "k6:/a on b on /Users/me/Mounts/k6/a-on-b (macfuse)";
        let m = parse_mount_output(out, &root());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].source, "k6:/a on b");
        assert_eq!(m[0].slug, "a-on-b");
    }

    #[test]
    fn ignores_deeper_nesting_and_garbage() {
        let out = "x on /Users/me/Mounts/k6/a/b (macfuse)\n\
                   garbage line\n\
                   \n\
                   on /Users/me/Mounts/k6/c (macfuse)";
        assert!(parse_mount_output(out, &root()).is_empty());
    }

    #[test]
    fn slug_for_root_and_paths() {
        assert_eq!(slug_for("/"), "root");
        assert_eq!(slug_for(""), "root");
        assert_eq!(slug_for("/scratch"), "scratch");
        assert_eq!(slug_for("/scratch/alice/project"), "scratch-alice-project");
        assert_eq!(slug_for("/scratch/alice/project/"), "scratch-alice-project");
    }

    /// A slug becomes a directory name, so anything path-hostile must be
    /// replaced — a '/' would silently create a nested directory.
    #[test]
    fn slug_replaces_unsafe_characters() {
        let s = slug_for("/a b/c:d/e*f");
        assert!(!s.contains('/'), "must not contain a path separator: {s}");
        assert!(!s.contains(' '));
        assert!(!s.contains(':'));
        assert!(!s.starts_with('-') && !s.ends_with('-'));
    }

    #[test]
    fn slug_is_length_capped() {
        let long = format!("/{}", "x".repeat(500));
        assert!(slug_for(&long).chars().count() <= 60);
    }

    #[test]
    fn slug_never_empty_even_for_pathological_input() {
        assert_eq!(slug_for("///"), "root");
        assert_eq!(slug_for("/***"), "root");
    }
}
