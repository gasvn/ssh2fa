//! The handful of places where macOS and Linux genuinely differ.
//!
//! Everything else in this crate is portable: the daemon drives the system
//! `ssh` binary, speaks over a unix socket, and stores JSON under
//! `SSH_CONFIG_PATH`. Only FUSE plumbing, desktop integration, and credential
//! storage (see `creds`) need to know which OS they are on — so they are all
//! collected here instead of being sprinkled through the handlers as `cfg!`
//! branches that are easy to miss when adding a third platform.

/// The `mount` binary that prints the kernel mount table.
///
/// macOS keeps it at `/sbin/mount`; Ubuntu/Debian have `/bin/mount` and NO
/// `/sbin/mount` at all. Getting this wrong is silent rather than loud:
/// `list_active_mounts` treats an unrunnable command as "nothing is mounted",
/// so the daemon would cheerfully offer to mount an already-mounted folder and
/// report every real mount as absent.
pub const fn mount_table_bin() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "/sbin/mount"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "/bin/mount"
    }
}

/// Command + leading args that unmount a FUSE filesystem, as an unprivileged
/// user.
///
/// macOS: `umount -f <point>`.
/// Linux: `fusermount3 -u <point>` — plain `umount` needs root there, while
/// `fusermount` is setuid precisely so the user who mounted it can unmount it.
/// Callers append the mount point.
pub fn unmount_command() -> (&'static str, &'static [&'static str]) {
    #[cfg(target_os = "macos")]
    {
        ("umount", &["-f"])
    }
    #[cfg(not(target_os = "macos"))]
    {
        // fusermount3 ships with libfuse3 (Ubuntu 22.04+). `unmount_fallbacks`
        // covers older boxes that only have the libfuse2 binary.
        ("fusermount3", &["-u"])
    }
}

/// Additional unmount commands to try when the first one is missing or fails.
///
/// A lazy unmount (`-z`) is the last resort for a WEDGED mount whose server is
/// gone: it detaches the tree immediately so nothing else blocks on it, and the
/// kernel finishes when the last reference goes away.
pub fn unmount_fallbacks() -> &'static [(&'static str, &'static [&'static str])] {
    #[cfg(target_os = "macos")]
    {
        &[("diskutil", &["unmount", "force"])]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[("fusermount", &["-u"]), ("fusermount3", &["-uz"])]
    }
}

/// Extra `sshfs -o` options that only make sense on this platform.
///
/// `volname` is macFUSE-only — Linux sshfs rejects unknown options outright, so
/// passing it there fails the mount rather than being ignored.
pub fn sshfs_platform_opts(host: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        format!("volname={host},")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = host;
        String::new()
    }
}

/// Process-name markers identifying a leaked FUSE backend for a failed mount.
///
/// macOS sshfs daemonizes a separate `go-nfsv4` backend that survives the
/// client; Linux sshfs IS the process holding the mount.
pub fn fuse_process_markers() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &["go-nfsv4", "sshfs"]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &["sshfs"]
    }
}

/// Command that shows a desktop notification, with the title/body appended.
/// `None` when this platform has no notifier we can rely on.
pub fn notify_command(title: &str, body: &str) -> Option<(&'static str, Vec<String>)> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        Some(("osascript", vec!["-e".into(), script]))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some((
            "notify-send",
            vec!["--app-name=SSH2FA".into(), title.into(), body.into()],
        ))
    }
}

/// Clipboard writers to try in order; the text arrives on stdin.
///
/// Linux has no single answer — Wayland and X11 need different tools, and a
/// headless box has neither, so the caller must tolerate all of them failing.
pub fn clipboard_commands() -> &'static [(&'static str, &'static [&'static str])] {
    #[cfg(target_os = "macos")]
    {
        &[("pbcopy", &[])]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mount binary must be one that exists on THIS machine, or mount
    /// detection silently reports "nothing mounted" forever.
    #[test]
    fn the_mount_table_binary_exists_here() {
        assert!(
            std::path::Path::new(mount_table_bin()).exists(),
            "{} does not exist on this platform",
            mount_table_bin()
        );
    }

    #[test]
    fn unmount_uses_an_unprivileged_tool_on_linux() {
        let (cmd, args) = unmount_command();
        if cfg!(target_os = "macos") {
            assert_eq!((cmd, args), ("umount", &["-f"][..]));
        } else {
            // Plain `umount` would need root; the whole point is that the user
            // who mounted it can unmount it.
            assert_eq!(cmd, "fusermount3");
            assert!(args.contains(&"-u"));
        }
    }

    /// volname is macFUSE-only: Linux sshfs FAILS on an unknown option rather
    /// than ignoring it, so passing it there would break every mount.
    #[test]
    fn volname_is_macos_only() {
        let opts = sshfs_platform_opts("k6");
        if cfg!(target_os = "macos") {
            assert_eq!(opts, "volname=k6,");
        } else {
            assert!(opts.is_empty(), "got {opts:?}");
        }
    }

    /// Whatever the platform, the option fragment must splice into a longer
    /// comma-separated list without producing an empty option (`,,`), which
    /// sshfs rejects.
    #[test]
    fn platform_opts_splice_cleanly() {
        let joined = format!("{}reconnect,ConnectTimeout=10", sshfs_platform_opts("k6"));
        assert!(!joined.contains(",,"), "{joined}");
        assert!(!joined.starts_with(','), "{joined}");
        assert!(joined.contains("reconnect"));
    }

    #[test]
    fn every_platform_has_a_notifier_and_a_clipboard_candidate() {
        assert!(notify_command("t", "b").is_some());
        assert!(!clipboard_commands().is_empty());
    }
}
