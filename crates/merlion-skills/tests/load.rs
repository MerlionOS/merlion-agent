//! Integration tests for `merlion-skills`.
//!
//! These exercise the public API via real on-disk skill layouts created
//! in a `tempfile::tempdir`.

use std::fs;
use std::path::Path;

use merlion_skills::SkillSet;
use tempfile::tempdir;

/// Write `contents` to `path`, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

const FOO_DIR_SKILL: &str = "\
---
name: foo
description: The foo skill
---

# Foo

Body of foo.
";

const BAR_FLAT_SKILL: &str = "\
---
name: bar
description: The bar skill
---

Body of bar.
";

#[test]
fn loads_directory_skill() {
    let root = tempdir().unwrap();
    write(&root.path().join("foo").join("SKILL.md"), FOO_DIR_SKILL);

    let set = SkillSet::load(&[root.path()]).unwrap();

    let foo = set.get("foo").expect("foo skill should load");
    assert_eq!(foo.name, "foo");
    assert_eq!(foo.description, "The foo skill");
    assert!(foo.body.contains("Body of foo."));
    assert!(foo.body.starts_with("# Foo"));
    assert_eq!(set.len(), 1);
    assert_eq!(set.names(), vec!["foo"]);
}

#[test]
fn loads_flat_skill() {
    let root = tempdir().unwrap();
    write(&root.path().join("bar.md"), BAR_FLAT_SKILL);

    let set = SkillSet::load(&[root.path()]).unwrap();

    let bar = set.get("bar").expect("bar skill should load");
    assert_eq!(bar.name, "bar");
    assert_eq!(bar.description, "The bar skill");
    assert!(bar.body.contains("Body of bar."));
}

#[test]
fn malformed_front_matter_is_skipped_without_error() {
    let root = tempdir().unwrap();

    // No front-matter at all.
    write(
        &root.path().join("plain.md"),
        "# Just a heading\n\nNo front matter here.\n",
    );

    // Front-matter that isn't valid YAML (mapping value of a mapping value).
    write(
        &root.path().join("broken.md"),
        "---\nname: : :\ndescription: [unterminated\n---\n\nbody\n",
    );

    // A valid skill alongside the bad ones — it should still load.
    write(&root.path().join("ok.md"), BAR_FLAT_SKILL.replace("bar", "ok").as_str());

    let set = SkillSet::load(&[root.path()]).unwrap();

    assert!(set.get("plain").is_none(), "missing front-matter should be skipped");
    assert!(set.get("broken").is_none(), "broken YAML should be skipped");
    assert!(set.get("ok").is_some(), "valid skill should still load");
}

#[test]
fn user_root_overrides_bundled_root() {
    let bundled = tempdir().unwrap();
    let user = tempdir().unwrap();

    write(
        &bundled.path().join("shared.md"),
        "---\nname: shared\ndescription: bundled version\n---\n\nbundled body\n",
    );
    write(
        &user.path().join("shared.md"),
        "---\nname: shared\ndescription: user version\n---\n\nuser body\n",
    );

    // User root comes second → it should win.
    let set = SkillSet::load(&[bundled.path(), user.path()]).unwrap();

    let shared = set.get("shared").expect("shared skill should be present");
    assert_eq!(shared.description, "user version");
    assert!(shared.body.contains("user body"));
    assert!(!shared.body.contains("bundled body"));
}

#[test]
fn slug_validation_rejects_uppercase_and_spaces() {
    let root = tempdir().unwrap();

    // Uppercase letter in filename → rejected.
    write(
        &root.path().join("BadSlug.md"),
        "---\nname: BadSlug\ndescription: nope\n---\n\nbody\n",
    );

    // Space in filename → rejected.
    write(
        &root.path().join("bad slug.md"),
        "---\nname: bad slug\ndescription: nope\n---\n\nbody\n",
    );

    // Uppercase in a directory-style skill → rejected.
    write(
        &root.path().join("BadDir").join("SKILL.md"),
        "---\nname: BadDir\ndescription: nope\n---\n\nbody\n",
    );

    // A control: a valid sibling that *should* load.
    write(
        &root.path().join("good.md"),
        "---\nname: good\ndescription: yes\n---\n\nbody\n",
    );

    let set = SkillSet::load(&[root.path()]).unwrap();

    assert!(set.get("BadSlug").is_none());
    assert!(set.get("bad slug").is_none());
    assert!(set.get("BadDir").is_none());
    assert_eq!(set.names(), vec!["good"]);
}
