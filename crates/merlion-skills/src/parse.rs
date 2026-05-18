//! Parser for the agentskills.io `SKILL.md` / `<slug>.md` format.
//!
//! Layout:
//!
//! ```text
//! ---
//! name: my-skill
//! description: One-line summary
//! ---
//!
//! # Skill body
//! ...
//! ```
//!
//! The front-matter is delimited by lines that are exactly `---` (after
//! trimming trailing whitespace). It is parsed as YAML.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::Skill;

#[derive(Debug, Error)]
pub(crate) enum ParseError {
    #[error("missing front-matter delimiters")]
    MissingFrontMatter,
    #[error("malformed YAML front-matter: {0}")]
    BadYaml(#[from] serde_yaml::Error),
    #[error("front-matter missing required field `description`")]
    MissingDescription,
}

#[derive(Debug, Deserialize)]
struct FrontMatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Parse a raw skill file into a [`Skill`].
///
/// `slug` is the slug derived from the filename / dirname. If the
/// front-matter contains a `name` that differs from `slug`, the slug
/// wins and a warning is emitted. The body is everything after the
/// closing `---` delimiter, with at most one leading blank line trimmed.
pub(crate) fn parse_skill(
    slug: &str,
    source_path: &Path,
    raw: &str,
) -> Result<Skill, ParseError> {
    let (front, body) = split_front_matter(raw).ok_or(ParseError::MissingFrontMatter)?;

    let fm: FrontMatter = serde_yaml::from_str(front)?;

    if let Some(ref fm_name) = fm.name {
        if fm_name != slug {
            tracing::warn!(
                skill.slug = %slug,
                skill.front_matter_name = %fm_name,
                source = %source_path.display(),
                "skill front-matter `name` does not match filename slug; using slug",
            );
        }
    }

    let description = fm.description.ok_or(ParseError::MissingDescription)?;

    Ok(Skill {
        name: slug.to_string(),
        description: description.trim().to_string(),
        body: body.to_string(),
        source_path: source_path.to_path_buf(),
    })
}

/// Split a raw skill file into `(front_matter_yaml, body)`. Returns
/// `None` if the file does not start with `---` or has no closing `---`.
fn split_front_matter(raw: &str) -> Option<(&str, &str)> {
    // Strip an optional UTF-8 BOM.
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);

    // Skip leading blank lines so a file beginning with a blank line still
    // parses cleanly.
    let mut lines = raw.split_inclusive('\n');
    let first = loop {
        let line = lines.next()?;
        if line.trim().is_empty() {
            continue;
        }
        break line;
    };

    if first.trim_end() != "---" {
        return None;
    }

    // Find the closing delimiter. We work with byte offsets into `raw` so
    // we can return string slices.
    let after_first = first.as_ptr() as usize - raw.as_ptr() as usize + first.len();
    let rest = &raw[after_first..];

    let mut offset = after_first;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let front = &raw[after_first..offset];
            let body_start = offset + line.len();
            let body = raw.get(body_start..).unwrap_or("");
            // Trim one leading newline from the body so callers see clean text.
            let body = body.strip_prefix("\r\n").or_else(|| body.strip_prefix('\n')).unwrap_or(body);
            return Some((front, body));
        }
        offset += line.len();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_basic_front_matter() {
        let raw = "---\nname: foo\ndescription: bar\n---\n\nhello\n";
        let (fm, body) = split_front_matter(raw).unwrap();
        assert!(fm.contains("name: foo"));
        // The single blank line between front-matter and body is trimmed.
        assert_eq!(body, "hello\n");
    }

    #[test]
    fn rejects_missing_front_matter() {
        assert!(split_front_matter("no front matter here\n").is_none());
    }

    #[test]
    fn rejects_unterminated_front_matter() {
        assert!(split_front_matter("---\nname: foo\nstill going\n").is_none());
    }
}
