use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

pub fn build_skill_archive(skill_dir: &Path) -> Result<Vec<u8>> {
    if !skill_dir.is_dir() {
        return Err(anyhow!(
            "skill directory is missing or invalid: {}",
            skill_dir.display()
        ));
    }

    let files = collect_files(skill_dir)?;
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644)
        .last_modified_time(zip::DateTime::default());

    for absolute in files {
        let rel = absolute
            .strip_prefix(skill_dir)
            .with_context(|| format!("failed to relativize path {}", absolute.display()))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }
        let mut file = fs::File::open(&absolute)
            .with_context(|| format!("failed to open {}", absolute.display()))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .with_context(|| format!("failed to read {}", absolute.display()))?;
        writer
            .start_file(rel_str, options)
            .with_context(|| format!("failed to add {} to zip", absolute.display()))?;
        writer.write_all(&buf)?;
    }

    let cursor = writer.finish().context("failed to finalize skill zip")?;
    Ok(cursor.into_inner())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let mut children = Vec::new();
            for entry in fs::read_dir(&path)
                .with_context(|| format!("failed to read directory {}", path.display()))?
            {
                children.push(entry?.path());
            }
            children.sort();
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        } else if meta.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
