//! Property test for `mounts::slug_for`.
//!
//! The mount layout depends on this function being injective: two different
//! remote folders that share a slug share a MOUNT POINT, and mounting the
//! second then shadows the first. A collision here was a real, shipped bug (all
//! non-ASCII paths reduced to "root"), so the invariants are worth asserting
//! over a wide input space rather than a handful of hand-picked cases.
//!
//! Self-contained: paths come from a deterministic LCG, so a failure is
//! reproducible and the test needs no fixture file.

use std::collections::HashMap;

/// Deterministic pseudo-random generator — no external crate, same sequence
/// every run, so any failure is reproducible.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() as usize) % xs.len()]
    }
}

fn generated_paths() -> Vec<String> {
    // Deliberately nasty: separators, dots, spaces, quotes, CJK, emoji — the
    // characters that sanitisation destroys and therefore the ones that
    // collide.
    let atoms = [
        "a", "b", "scratch", "project", "..", ".", "-", "_", " ", "'", "\"",
        "数据", "项目", "工作", "データ", "🎉", "x/y", "A", "Z", "0", "9", "%", "*",
    ];
    let mut rng = Lcg(0x5EED_1234);
    let mut out: Vec<String> = vec![
        "/".into(), "".into(), "//".into(), "/.".into(), "/..".into(),
        "/a/../b".into(), " /x ".into(), "/".repeat(50), "/🎉".into(),
    ];
    for _ in 0..400 {
        let n = (rng.next() % 5) + 1;
        let mut p = String::new();
        for _ in 0..n {
            p.push('/');
            p.push_str(rng.pick(&atoms));
        }
        out.push(p);
    }
    out
}

#[test]
fn slug_is_always_a_safe_single_component() {
    for p in generated_paths() {
        let s = a2fa_core::mounts::slug_for(&p);
        assert!(!s.is_empty(), "empty slug for {p:?}");
        assert!(!s.contains('/'), "slug {s:?} contains a separator (from {p:?})");
        assert!(!s.contains('\0'), "slug {s:?} contains NUL (from {p:?})");
        assert!(s.chars().count() <= 60, "slug too long ({}) for {p:?}", s.chars().count());
        assert!(!s.starts_with('-') && !s.ends_with('-'), "slug {s:?} from {p:?}");
        assert!(s.is_ascii(), "slug {s:?} must be ASCII (from {p:?})");
        assert_ne!(s, ".", "slug must not be a relative directory entry");
        assert_ne!(s, "..", "slug must not be a relative directory entry");
    }
}

#[test]
fn slug_is_deterministic() {
    for p in generated_paths() {
        assert_eq!(
            a2fa_core::mounts::slug_for(&p),
            a2fa_core::mounts::slug_for(&p),
            "not deterministic for {p:?}"
        );
    }
}

/// The property the mount layout actually depends on: distinct folders get
/// distinct mount points.
#[test]
fn distinct_paths_get_distinct_slugs() {
    let mut seen: HashMap<String, String> = HashMap::new();
    for p in generated_paths() {
        // Paths that differ only by surrounding/trailing slashes or whitespace
        // ARE the same folder, so compare on that canonical form.
        let canonical = p.trim().trim_matches('/').to_string();
        let s = a2fa_core::mounts::slug_for(&p);
        if let Some(prev) = seen.insert(s.clone(), canonical.clone()) {
            assert_eq!(prev, canonical, "collision: {prev:?} and {canonical:?} both map to {s:?}");
        }
    }
}
