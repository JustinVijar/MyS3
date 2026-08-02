use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::fs;

/// True if `dir` exists and contains any entry.
pub async fn dir_nonempty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if !path.is_dir() {
        bail!("{} exists and is not a directory", path.display());
    }
    let mut rd = fs::read_dir(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    Ok(rd.next_entry().await?.is_some())
}

/// Ensure `path/objects` exists (empty storage root layout).
pub async fn ensure_storage_layout(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("objects"))
        .await
        .with_context(|| format!("create objects under {}", root.display()))?;
    Ok(())
}

/// Move an entire storage root to `dest` (rename, or copy+remove on cross-device).
pub async fn relocate_storage_root(src: &Path, dest: &Path) -> Result<()> {
    if src == dest {
        return Ok(());
    }
    if !src.exists() {
        bail!("current storage root {} does not exist", src.display());
    }
    if dest.exists() {
        if dir_nonempty(dest).await? {
            bail!(
                "destination {} already exists and is not empty",
                dest.display()
            );
        }
        // Empty destination dir: remove so rename can replace it.
        fs::remove_dir(dest)
            .await
            .with_context(|| format!("remove empty destination {}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create parent of {}", dest.display()))?;
    }

    match fs::rename(src, dest).await {
        Ok(()) => {
            ensure_storage_layout(dest).await?;
            return Ok(());
        }
        Err(err) => {
            // Cross-device rename fails with EXDEV; fall back to copy+remove for that case.
            if !is_exdev(&err) {
                return Err(err).with_context(|| {
                    format!("rename {} -> {}", src.display(), dest.display())
                });
            }
        }
    }

    copy_dir_recursive(src, dest)
        .await
        .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
    ensure_storage_layout(dest).await?;
    remove_dir_recursive(src)
        .await
        .with_context(|| format!("remove old storage root {}", src.display()))?;
    Ok(())
}

fn is_exdev(err: &std::io::Error) -> bool {
    // Linux/macOS EXDEV; Windows ERROR_NOT_SAME_DEVICE = 17.
    matches!(err.raw_os_error(), Some(18) | Some(17))
}

async fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)
        .await
        .with_context(|| format!("create {}", dest.display()))?;
    let mut rd = fs::read_dir(src)
        .await
        .with_context(|| format!("read {}", src.display()))?;
    while let Some(entry) = rd.next_entry().await? {
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = entry.metadata().await?;
        if meta.is_dir() {
            Box::pin(copy_dir_recursive(&from, &to)).await?;
        } else {
            fs::copy(&from, &to)
                .await
                .with_context(|| format!("copy file {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

async fn remove_dir_recursive(path: &Path) -> Result<()> {
    fs::remove_dir_all(path)
        .await
        .with_context(|| format!("remove {}", path.display()))?;
    Ok(())
}

/// Absolute display path for the current storage root.
pub fn absolute_display(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}
