use chrono::{TimeZone, Utc};
use merlion_memory::{Memory, MemoryStore, MemoryType};
use tempfile::tempdir;

fn sample_memory(name: &str, description: &str, kind: MemoryType, body: &str) -> Memory {
    Memory {
        name: name.to_string(),
        description: description.to_string(),
        kind,
        body: body.to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    }
}

#[test]
fn open_creates_dir_and_index() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("mem");
    assert!(!dir.exists());
    let store = MemoryStore::open(&dir).unwrap();
    assert!(dir.exists());
    assert!(dir.join("MEMORY.md").exists());
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn write_then_read_roundtrip_preserves_fields() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    let m = sample_memory(
        "user-role",
        "User prefers terse responses",
        MemoryType::User,
        "The user has stated they like short answers. Cross-link: [[other]].\n",
    );
    store.write(&m).unwrap();

    let got = store.read("user-role").unwrap();
    assert_eq!(got.name, m.name);
    assert_eq!(got.description, m.description);
    assert_eq!(got.kind, MemoryType::User);
    assert_eq!(got.body.trim_end(), m.body.trim_end());
    // updated_at should have been refreshed by write().
    assert!(got.updated_at >= m.updated_at);
}

#[test]
fn metadata_type_roundtrips_for_all_variants() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    for (slug, kind) in [
        ("u", MemoryType::User),
        ("f", MemoryType::Feedback),
        ("p", MemoryType::Project),
        ("r", MemoryType::Reference),
    ] {
        let m = sample_memory(slug, "desc", kind.clone(), "body\n");
        store.write(&m).unwrap();
        let got = store.read(slug).unwrap();
        assert_eq!(got.kind, kind, "kind mismatch for {}", slug);
    }
}

#[test]
fn write_updates_index_with_new_line() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    let a = sample_memory("alpha", "first hook", MemoryType::Project, "a\n");
    let b = sample_memory("beta", "second hook", MemoryType::Reference, "b\n");
    store.write(&a).unwrap();
    store.write(&b).unwrap();

    let rows = store.list().unwrap();
    let names: Vec<_> = rows.iter().map(|r| r.name.clone()).collect();
    assert!(names.contains(&"alpha".to_string()), "missing alpha: {:?}", names);
    assert!(names.contains(&"beta".to_string()), "missing beta: {:?}", names);

    let alpha_row = rows.iter().find(|r| r.name == "alpha").unwrap();
    assert_eq!(alpha_row.file, "alpha.md");
    assert_eq!(alpha_row.hook, "first hook");
}

#[test]
fn write_overwrite_preserves_created_at_and_updates_index_in_place() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    let m1 = sample_memory("alpha", "old hook", MemoryType::Project, "v1\n");
    store.write(&m1).unwrap();
    let after_first = store.read("alpha").unwrap();
    let original_created = after_first.created_at;

    // Sleep tiny bit to ensure updated_at differs.
    std::thread::sleep(std::time::Duration::from_millis(5));

    let m2 = sample_memory("alpha", "new hook", MemoryType::Project, "v2\n");
    store.write(&m2).unwrap();
    let after_second = store.read("alpha").unwrap();

    assert_eq!(after_second.created_at, original_created, "created_at must be preserved");
    assert!(after_second.updated_at >= after_first.updated_at);
    assert_eq!(after_second.body.trim_end(), "v2");
    assert_eq!(after_second.description, "new hook");

    // Index must still only have one alpha row, and its hook should be updated.
    let rows = store.list().unwrap();
    let alpha_rows: Vec<_> = rows.iter().filter(|r| r.name == "alpha").collect();
    assert_eq!(alpha_rows.len(), 1);
    assert_eq!(alpha_rows[0].hook, "new hook");
}

#[test]
fn delete_is_idempotent() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    let m = sample_memory("gone", "soon", MemoryType::User, "bye\n");
    store.write(&m).unwrap();
    assert!(tmp.path().join("gone.md").exists());

    store.delete("gone").unwrap();
    assert!(!tmp.path().join("gone.md").exists());

    // Second call should also succeed.
    store.delete("gone").unwrap();

    // Index should no longer list `gone`.
    let rows = store.list().unwrap();
    assert!(rows.iter().all(|r| r.name != "gone"));
}

#[test]
fn rejects_invalid_slug() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    for bad in ["", "-leading", "UPPER", "has space", "has_underscore", "foo!", "ÿ"] {
        let m = sample_memory(bad, "desc", MemoryType::User, "body\n");
        assert!(
            store.write(&m).is_err(),
            "expected slug `{}` to be rejected",
            bad
        );
    }

    // Reads with bad slugs should also fail before hitting disk.
    assert!(store.read("Bad Name").is_err());
}

#[test]
fn render_context_block_honors_max_chars() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    // Empty store -> empty string.
    assert_eq!(store.render_context_block(1000).unwrap(), "");

    for i in 0..5 {
        let m = sample_memory(
            &format!("mem-{}", i),
            &format!("hook number {}", i),
            MemoryType::User,
            "body\n",
        );
        store.write(&m).unwrap();
    }

    let unlimited = store.render_context_block(10_000).unwrap();
    assert!(unlimited.starts_with("# Persistent memory (5 entries)"));
    // All five hooks should appear when unbounded.
    for i in 0..5 {
        assert!(
            unlimited.contains(&format!("hook number {}", i)),
            "missing hook {} in:\n{}",
            i,
            unlimited
        );
    }

    // Tight budget: must not exceed max_chars and must still start with header.
    let header_len = "# Persistent memory (5 entries)\n".len();
    let tight = store.render_context_block(header_len + 20).unwrap();
    assert!(tight.len() <= header_len + 20, "block too long: {} bytes", tight.len());
    assert!(tight.starts_with("# Persistent memory (5 entries)"));

    // Budget smaller than the header alone: returns empty.
    let too_small = store.render_context_block(5).unwrap();
    assert_eq!(too_small, "");
}

#[test]
fn list_preserves_non_matching_lines_on_rewrite() {
    let tmp = tempdir().unwrap();
    let store = MemoryStore::open(tmp.path()).unwrap();

    // Prepend some user-authored header lines to MEMORY.md.
    let index_path = tmp.path().join("MEMORY.md");
    let custom = "# Memory\n\nThese are my notes.\n\n## Index\n";
    std::fs::write(&index_path, custom).unwrap();

    let m = sample_memory("kept", "still here", MemoryType::Project, "body\n");
    store.write(&m).unwrap();

    let after = std::fs::read_to_string(&index_path).unwrap();
    assert!(after.contains("# Memory"));
    assert!(after.contains("These are my notes."));
    assert!(after.contains("## Index"));
    assert!(after.contains("[kept](kept.md)"));
}
