//! Build zip / tar.gz / 7z archives of folder contents for download.

use std::fs::File as StdFile;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{bail, Context as _, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use sevenz_rust::{SevenZArchiveEntry, SevenZWriter};
use tokio::io::{AsyncRead, ReadBuf};
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;
use zip::ZipWriter;

use crate::db::models::ObjectRecord;
use crate::storage::engine::StorageEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
    SevenZ,
}

impl ArchiveFormat {
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "zip" => Ok(Self::Zip),
            "tar.gz" | "tgz" => Ok(Self::TarGz),
            "7z" => Ok(Self::SevenZ),
            _ => Err("format must be zip, tar.gz, or 7z"),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
            Self::SevenZ => "7z",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Zip => "application/zip",
            Self::TarGz => "application/gzip",
            Self::SevenZ => "application/x-7z-compressed",
        }
    }
}

/// Drop `.keep` marker objects; returns `(archive_entry_name, absolute_path)`.
pub fn filter_archive_entries(
    engine: &StorageEngine,
    prefix: &str,
    records: &[ObjectRecord],
) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for r in records {
        let key = &r.original_filename;
        let basename = key.rsplit('/').next().unwrap_or(key);
        if basename == ".keep" {
            continue;
        }
        let Some(rel) = key.strip_prefix(prefix) else {
            continue;
        };
        if rel.is_empty() || rel.contains('\0') {
            continue;
        }
        // Reject path traversal in relative entry names.
        if rel.split('/').any(|s| s == "." || s == "..") {
            continue;
        }
        out.push((rel.to_string(), engine.absolute_path_for(&r.filepath)));
    }
    out
}

pub fn folder_download_basename(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if name.is_empty() {
        "folder".to_string()
    } else {
        name.to_string()
    }
}

/// Temp archive that deletes itself when dropped (after the HTTP body finishes).
pub struct TempArchive {
    file: tokio::fs::File,
    path: PathBuf,
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl AsyncRead for TempArchive {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

impl TempArchive {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let file = tokio::fs::File::open(&path)
            .await
            .with_context(|| format!("open archive {}", path.display()))?;
        Ok(Self { file, path })
    }

    pub async fn len(&self) -> Result<u64> {
        Ok(self.file.metadata().await?.len())
    }
}

fn ensure_tmpdir(storage_root: &Path) -> Result<PathBuf> {
    let dir = storage_root.join("tmp");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Build an archive on disk under `{storage_root}/tmp/` and return its path.
pub async fn build_archive(
    engine: &StorageEngine,
    format: ArchiveFormat,
    entries: Vec<(String, PathBuf)>,
) -> Result<PathBuf> {
    if entries.is_empty() {
        bail!("folder is empty");
    }
    let tmpdir = ensure_tmpdir(engine.storage_root())?;
    let ext = format.extension();
    let out_path = tmpdir.join(format!(
        "archive-{}-{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4(),
        ext
    ));

    let path_for_write = out_path.clone();
    tokio::task::spawn_blocking(move || match format {
        ArchiveFormat::Zip => write_zip(&path_for_write, &entries),
        ArchiveFormat::TarGz => write_tar_gz(&path_for_write, &entries),
        ArchiveFormat::SevenZ => write_7z(&path_for_write, &entries),
    })
    .await
    .context("archive worker panicked")??;

    Ok(out_path)
}

fn write_zip(out: &Path, entries: &[(String, PathBuf)]) -> Result<()> {
    let file = StdFile::create(out).with_context(|| format!("create {}", out.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, abs) in entries {
        zip.start_file(name, options)
            .with_context(|| format!("zip start_file {name}"))?;
        let mut src = StdFile::open(abs).with_context(|| format!("open {}", abs.display()))?;
        io::copy(&mut src, &mut zip).with_context(|| format!("zip copy {name}"))?;
    }
    zip.finish().context("zip finish")?;
    Ok(())
}

fn write_tar_gz(out: &Path, entries: &[(String, PathBuf)]) -> Result<()> {
    let file = StdFile::create(out).with_context(|| format!("create {}", out.display()))?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut archive = tar::Builder::new(enc);
    for (name, abs) in entries {
        archive
            .append_path_with_name(abs, name)
            .with_context(|| format!("tar append {name}"))?;
    }
    let enc = archive.into_inner().context("tar finish")?;
    let mut file = enc.finish().context("gzip finish")?;
    file.flush()?;
    Ok(())
}

fn write_7z(out: &Path, entries: &[(String, PathBuf)]) -> Result<()> {
    let mut sz = SevenZWriter::create(out).map_err(|e| anyhow::anyhow!("7z create: {e}"))?;
    for (name, abs) in entries {
        let src = Path::new(abs);
        let reader = StdFile::open(src).with_context(|| format!("open {}", abs.display()))?;
        sz.push_archive_entry(SevenZArchiveEntry::from_path(src, name.clone()), Some(reader))
            .map_err(|e| anyhow::anyhow!("7z entry {name}: {e}"))?;
    }
    sz.finish()
        .map_err(|e| anyhow::anyhow!("7z finish: {e}"))?;
    Ok(())
}
