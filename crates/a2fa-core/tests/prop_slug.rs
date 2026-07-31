#[test]
fn property_slug_invariants() {
    let raw = include_str!("/tmp/paths.json");
    let paths: Vec<String> = serde_json::from_str(raw).unwrap();
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for p in &paths {
        let s = a2fa_core::mounts::slug_for(p);
        assert!(!s.is_empty(), "empty slug for {p:?}");
        assert!(!s.contains('/'), "slug {s:?} has a separator (from {p:?})");
        assert!(!s.contains('\0'), "slug {s:?} has a NUL");
        assert!(s.chars().count() <= 60, "slug too long: {s:?}");
        assert!(!s.starts_with('-') && !s.ends_with('-'), "slug {s:?}");
        assert_eq!(s, a2fa_core::mounts::slug_for(p), "not deterministic for {p:?}");
        // Distinct trimmed paths must not share a slug.
        let key = p.trim().trim_matches('/').to_string();
        if let Some(prev) = seen.insert(s.clone(), key.clone()) {
            assert_eq!(prev, key, "collision: {prev:?} vs {key:?} both -> {s:?}");
        }
    }
}
