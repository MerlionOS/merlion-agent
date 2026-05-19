//! `merlion backup` / `merlion import` — tar.gz archive + restore of
//! `~/.merlion/`. Useful for moving merlion between machines or rolling
//! back after a misconfiguration.
//!
//! Secrets (`.env`) are excluded by default; `--include-secrets` opts in.
//! `logs/` and any stray `target/` are always skipped — neither carries
//! durable state worth backing up.
//!
//! See the wiring spec at the bottom of this file for how this hooks into
//! `main.rs`.

// The module is intentionally not yet dispatched from `main.rs` — the wiring
// is documented at the bottom of the file. Suppress dead-code lints until the
// caller is wired up so `cargo clippy -- -D warnings` stays green.
#![allow(dead_code)]

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Archive, Builder};

/// Top-level directory name inside the tarball. The archive contains a
/// single root entry called `merlion/` so the layout is obvious when a user
/// opens the tarball by hand.
const ARCHIVE_ROOT: &str = "merlion";

#[derive(Debug, Args)]
pub struct BackupArgs {
    /// Where to write the tarball. Default: ./merlion-backup-<timestamp>.tar.gz
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Include `~/.merlion/.env` (API keys). Off by default for safety.
    #[arg(long)]
    pub include_secrets: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Path to a tarball previously produced by `merlion backup`.
    pub archive: PathBuf,
    /// Overwrite `~/.merlion/` if it exists. Default is to refuse.
    #[arg(long)]
    pub force: bool,
}

pub async fn run_backup(args: BackupArgs) -> Result<()> {
    let home = merlion_config::merlion_home();
    let output = args.output.clone().unwrap_or_else(|| {
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        PathBuf::from(format!("merlion-backup-{ts}.tar.gz"))
    });
    let summary = backup_to(&home, &output, args.include_secrets)?;
    println!(
        "wrote {} ({} file(s), {})",
        output.display(),
        summary.file_count,
        humanize_bytes(summary.total_bytes),
    );
    if !summary.excluded.is_empty() {
        println!("excluded:");
        for p in &summary.excluded {
            println!("  - {}", p.display());
        }
    }
    Ok(())
}

pub async fn run_import(args: ImportArgs) -> Result<()> {
    let home = merlion_config::merlion_home();
    let summary = import_to(&home, &args.archive, args.force)?;
    if let Some(backup) = &summary.moved_existing_to {
        println!("moved existing {} → {}", home.display(), backup.display());
    }
    println!(
        "restored to {} ({} file(s), {})",
        home.display(),
        summary.file_count,
        humanize_bytes(summary.total_bytes),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Core impl — pure functions parameterised by `home` so tests can drive them
// against a tempdir without env mutation.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct BackupSummary {
    file_count: u64,
    total_bytes: u64,
    excluded: Vec<PathBuf>,
}

#[derive(Debug)]
struct ImportSummary {
    file_count: u64,
    total_bytes: u64,
    moved_existing_to: Option<PathBuf>,
}

fn backup_to(home: &Path, output: &Path, include_secrets: bool) -> Result<BackupSummary> {
    if !home.exists() {
        anyhow::bail!(
            "nothing to back up — {} does not exist. Run `merlion setup` first.",
            home.display()
        );
    }

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output dir {}", parent.display()))?;
        }
    }

    let out_file =
        File::create(output).with_context(|| format!("create archive {}", output.display()))?;
    let encoder = GzEncoder::new(BufWriter::new(out_file), Compression::default());
    let mut builder = Builder::new(encoder);
    builder.follow_symlinks(false);

    let mut file_count: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut excluded: Vec<PathBuf> = Vec::new();

    walk_and_add(
        home,
        home,
        &mut builder,
        include_secrets,
        &mut file_count,
        &mut total_bytes,
        &mut excluded,
    )?;

    let encoder = builder.into_inner().context("finalise tar")?;
    encoder.finish().context("finalise gzip")?;

    Ok(BackupSummary {
        file_count,
        total_bytes,
        excluded,
    })
}

fn walk_and_add(
    home: &Path,
    dir: &Path,
    builder: &mut Builder<GzEncoder<BufWriter<File>>>,
    include_secrets: bool,
    file_count: &mut u64,
    total_bytes: &mut u64,
    excluded: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("dir entry under {}", dir.display()))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(home)
            .with_context(|| format!("strip_prefix {}", path.display()))?
            .to_path_buf();

        if should_skip(&rel, include_secrets) {
            excluded.push(rel.clone());
            continue;
        }

        let file_type = entry
            .file_type()
            .with_context(|| format!("file_type {}", path.display()))?;
        let archive_path = PathBuf::from(ARCHIVE_ROOT).join(&rel);

        if file_type.is_dir() {
            walk_and_add(
                home,
                &path,
                builder,
                include_secrets,
                file_count,
                total_bytes,
                excluded,
            )?;
        } else if file_type.is_file() {
            let mut f = File::open(&path).with_context(|| format!("open {}", path.display()))?;
            let meta = f
                .metadata()
                .with_context(|| format!("metadata {}", path.display()))?;
            *file_count += 1;
            *total_bytes += meta.len();
            builder
                .append_file(&archive_path, &mut f)
                .with_context(|| format!("append {}", path.display()))?;
        }
        // Skip symlinks and other special files — they're rarely meaningful
        // inside ~/.merlion and tar would need extra care to reproduce them.
    }
    Ok(())
}

fn should_skip(rel: &Path, include_secrets: bool) -> bool {
    let first = rel.components().next().and_then(|c| c.as_os_str().to_str());
    if matches!(first, Some("logs") | Some("target")) {
        return true;
    }
    if !include_secrets && rel == Path::new(".env") {
        return true;
    }
    false
}

fn import_to(home: &Path, archive: &Path, force: bool) -> Result<ImportSummary> {
    if !archive.exists() {
        anyhow::bail!("archive not found: {}", archive.display());
    }

    let mut moved_existing_to: Option<PathBuf> = None;
    if home.exists() {
        if !force {
            anyhow::bail!(
                "{} already exists. Pass --force to overwrite (the current dir will be renamed to <home>.bak-<timestamp>).",
                home.display()
            );
        }
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let backup = home.with_file_name(format!(
            "{}.bak-{ts}",
            home.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(".merlion")
        ));
        std::fs::rename(home, &backup)
            .with_context(|| format!("rename {} → {}", home.display(), backup.display()))?;
        moved_existing_to = Some(backup);
    }

    if let Some(parent) = home.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent {}", parent.display()))?;
        }
    }
    std::fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;

    let input =
        File::open(archive).with_context(|| format!("open archive {}", archive.display()))?;
    let decoder = GzDecoder::new(BufReader::new(input));
    let mut tar = Archive::new(decoder);

    let mut file_count: u64 = 0;
    let mut total_bytes: u64 = 0;

    for entry in tar.entries().context("read tar entries")? {
        let mut entry = entry.context("tar entry")?;
        let path_in_archive = entry.path().context("entry path")?.into_owned();

        let rel = match path_in_archive.strip_prefix(ARCHIVE_ROOT) {
            Ok(r) => r.to_path_buf(),
            Err(_) => path_in_archive.clone(),
        };
        if rel.as_os_str().is_empty() {
            continue;
        }

        let dest = home.join(&rel);
        let header_kind = entry.header().entry_type();
        if header_kind.is_dir() {
            std::fs::create_dir_all(&dest).with_context(|| format!("mkdir {}", dest.display()))?;
            continue;
        }
        if !header_kind.is_file() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let size = entry.header().size().unwrap_or(0);
        file_count += 1;
        total_bytes += size;
        entry
            .unpack(&dest)
            .with_context(|| format!("unpack {}", dest.display()))?;
    }

    Ok(ImportSummary {
        file_count,
        total_bytes,
        moved_existing_to,
    })
}

fn humanize_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.2} GiB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MiB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KiB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn fixture_home(root: &Path) -> PathBuf {
        let home = root.join("home");
        write_file(
            &home.join("config.yaml"),
            "model:\n  id: openai:gpt-4o-mini\n",
        );
        write_file(&home.join(".env"), "OPENAI_API_KEY=sk-secret\n");
        write_file(&home.join("memory").join("a.md"), "hello memory\n");
        write_file(
            &home.join("skills").join("greet").join("SKILL.md"),
            "# greet\n",
        );
        write_file(&home.join("logs").join("agent.log"), "noise\n");
        write_file(&home.join("target").join("debug").join("junk"), "junk\n");
        home
    }

    #[test]
    fn roundtrip_preserves_file_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fixture_home(tmp.path());
        let archive = tmp.path().join("backup.tar.gz");

        let summary = backup_to(&home, &archive, /*include_secrets=*/ true).unwrap();
        assert!(archive.exists());
        // config.yaml, .env, memory/a.md, skills/greet/SKILL.md = 4 files.
        // logs/* and target/* are unconditionally skipped.
        assert_eq!(
            summary.file_count, 4,
            "files in archive: {}",
            summary.file_count
        );

        let restored = tmp.path().join("restored");
        let restore_summary = import_to(&restored, &archive, /*force=*/ false).unwrap();
        assert_eq!(restore_summary.file_count, 4);

        assert_eq!(
            fs::read_to_string(restored.join("config.yaml")).unwrap(),
            "model:\n  id: openai:gpt-4o-mini\n",
        );
        assert_eq!(
            fs::read_to_string(restored.join(".env")).unwrap(),
            "OPENAI_API_KEY=sk-secret\n",
        );
        assert_eq!(
            fs::read_to_string(restored.join("memory").join("a.md")).unwrap(),
            "hello memory\n",
        );
        assert_eq!(
            fs::read_to_string(restored.join("skills").join("greet").join("SKILL.md")).unwrap(),
            "# greet\n",
        );
        // logs and target must not have come along for the ride.
        assert!(!restored.join("logs").exists());
        assert!(!restored.join("target").exists());
    }

    #[test]
    fn secrets_excluded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fixture_home(tmp.path());
        let archive = tmp.path().join("backup.tar.gz");

        let summary = backup_to(&home, &archive, /*include_secrets=*/ false).unwrap();
        assert!(
            summary.excluded.iter().any(|p| p == Path::new(".env")),
            "expected .env in excluded list, got: {:?}",
            summary.excluded
        );

        let restored = tmp.path().join("restored");
        import_to(&restored, &archive, /*force=*/ false).unwrap();
        assert!(
            !restored.join(".env").exists(),
            "default backup must not contain .env"
        );
        assert!(restored.join("config.yaml").exists());
    }

    #[test]
    fn import_refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fixture_home(tmp.path());
        let archive = tmp.path().join("backup.tar.gz");
        backup_to(&home, &archive, true).unwrap();

        // Pre-existing dir at the destination.
        let dest = tmp.path().join("dest");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("preexisting.txt"), "keep me").unwrap();

        let err = import_to(&dest, &archive, /*force=*/ false).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("already exists"),
            "expected refusal message, got: {msg}"
        );
        // Original content must survive a refused import.
        assert_eq!(
            fs::read_to_string(dest.join("preexisting.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn import_with_force_moves_existing_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fixture_home(tmp.path());
        let archive = tmp.path().join("backup.tar.gz");
        backup_to(&home, &archive, true).unwrap();

        let dest = tmp.path().join("dest");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("preexisting.txt"), "keep me").unwrap();

        let summary = import_to(&dest, &archive, /*force=*/ true).unwrap();
        let backup = summary.moved_existing_to.expect("expected a side-rename");
        assert!(backup.exists(), "renamed-aside dir should exist");
        assert_eq!(
            fs::read_to_string(backup.join("preexisting.txt")).unwrap(),
            "keep me",
        );
        // Fresh contents from the archive landed in `dest`.
        assert!(dest.join("config.yaml").exists());
        assert!(!dest.join("preexisting.txt").exists());
    }
}

// -----------------------------------------------------------------------------
// Wiring spec — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top:
//
//        mod backup_cmd;
//
// 2. Add two new variants to the `Command` enum. Both use `clap::Args` so
//    they get the flag-style invocation (`merlion backup --output …`) without
//    a nested subcommand:
//
//        /// Archive ~/.merlion/ to a tar.gz for moving or rollback.
//        Backup(backup_cmd::BackupArgs),
//        /// Restore ~/.merlion/ from a `merlion backup` tarball.
//        Import(backup_cmd::ImportArgs),
//
// 3. Add dispatch arms in the `match cli.command.unwrap_or(...)` block in
//    `main`:
//
//        Command::Backup(args) => backup_cmd::run_backup(args).await,
//        Command::Import(args) => backup_cmd::run_import(args).await,
//
// -----------------------------------------------------------------------------
