//! Directory walking + slug validation for skill discovery.

use std::path::{Path, PathBuf};

use regex::Regex;

/// Returns true iff `slug` matches `^[a-z0-9][a-z0-9-]*$`.
pub(crate) fn is_valid_slug(slug: &str) -> bool {
    // Compiled once per process.
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]*$").unwrap());
    re.is_match(slug)
}

/// A discovered candidate skill file, paired with its slug.
pub(crate) struct Candidate {
    pub slug: String,
    pub path: PathBuf,
}

/// Enumerate top-level skill candidates in `root`.
///
/// - `<root>/<X>/SKILL.md` → slug = `X`
/// - `<root>/<X>.md`       → slug = `X` (filename without `.md`)
///
/// Other files (README.md, hidden entries, etc.) are ignored. A missing
/// root is not an error and returns an empty vector.
pub(crate) fn discover(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();

    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    root = %root.display(),
                    error = %err,
                    "failed to read skills root; skipping",
                );
            }
            return out;
        }
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(n) => n,
            None => continue,
        };

        // Skip hidden entries.
        if name.starts_with('.') {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            let skill_md = entry.path().join("SKILL.md");
            if skill_md.is_file() {
                out.push(Candidate {
                    slug: name.to_string(),
                    path: skill_md,
                });
            }
        } else if file_type.is_file() {
            // Flat skill: `<slug>.md`. Ignore non-md files and README.md.
            if let Some(stem) = name.strip_suffix(".md") {
                // Filter out README at the top level so users can document
                // their skills root without it being treated as a skill.
                if stem.eq_ignore_ascii_case("README") {
                    continue;
                }
                out.push(Candidate {
                    slug: stem.to_string(),
                    path: entry.path(),
                });
            }
        }
    }

    // Deterministic order helps tests and `help_index` rendering.
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_regex_accepts_normal_slugs() {
        assert!(is_valid_slug("foo"));
        assert!(is_valid_slug("foo-bar"));
        assert!(is_valid_slug("foo123"));
        assert!(is_valid_slug("9lives"));
    }

    #[test]
    fn slug_regex_rejects_garbage() {
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("Foo"));
        assert!(!is_valid_slug("foo bar"));
        assert!(!is_valid_slug("-foo"));
        assert!(!is_valid_slug("foo_bar"));
        assert!(!is_valid_slug("foo/bar"));
    }
}
