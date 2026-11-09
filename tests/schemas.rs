//! Committed schemas match the types. `KAGU_BLESS=1` rewrites them.

use std::path::Path;

#[test]
fn committed_schemas_match_the_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
    let bless = std::env::var_os("KAGU_BLESS").is_some();
    let mut stale = Vec::new();
    let mut expected = std::collections::BTreeSet::new();
    for (rel, schema) in kagu::schema::all() {
        let path = root.join(&rel);
        expected.insert(path.clone());
        let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
        if bless {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(rel);
        }
    }
    // A committed schema with no type behind it is drift too.
    for dir in [root.clone(), root.join("events")] {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_file() && !expected.contains(&path) {
                stale.push(path.display().to_string());
            }
        }
    }
    assert!(
        stale.is_empty(),
        "schemas out of date (run `just schemas`): {stale:?}"
    );
}
