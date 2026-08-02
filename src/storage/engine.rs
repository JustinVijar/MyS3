use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::fs::{self, File};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::db::models::EtagType;
use crate::storage::hasher::StreamingHasher;

#[derive(Debug, Clone)]
pub struct StoredObject {
    pub filepath: String,
    pub file_format: String,
    pub absolute_path: PathBuf,
    pub filesize_bytes: i64,
    pub etag: String,
    pub etag_type: EtagType,
}

#[derive(Debug, Clone)]
pub struct StorageEngine {
    storage_root: PathBuf,
    objects_dir: PathBuf,
}

impl StorageEngine {
    pub async fn init(storage_root: impl Into<PathBuf>) -> Result<Self> {
        let storage_root = storage_root.into();
        let objects_dir = storage_root.join("objects");
        fs::create_dir_all(&objects_dir)
            .await
            .with_context(|| format!("create {}", objects_dir.display()))?;
        Ok(Self {
            storage_root,
            objects_dir,
        })
    }

    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    pub fn absolute_path_for(&self, filepath: &str) -> PathBuf {
        // filepath is stored as relative "objects/<uuid>.<ext>" or just "<uuid>.<ext>"
        if filepath.starts_with("objects/") || filepath.contains('/') {
            self.storage_root.join(filepath)
        } else {
            self.objects_dir.join(filepath)
        }
    }

    pub fn relative_filepath(uuid: &Uuid, ext: &str) -> String {
        format!("objects/{uuid}.{ext}")
    }

    pub fn extension_from_filename(filename: &str) -> String {
        Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| !e.is_empty())
            .unwrap_or("bin")
            .to_lowercase()
    }

    /// Stream `reader` to a new immutable blob, hashing on the fly.
    pub async fn put_stream<R>(
        &self,
        mut reader: R,
        original_filename: &str,
        etag_type: EtagType,
    ) -> Result<StoredObject>
    where
        R: AsyncRead + Unpin,
    {
        let file_format = Self::extension_from_filename(original_filename);
        let uuid = Uuid::new_v4();
        let filepath = Self::relative_filepath(&uuid, &file_format);
        let absolute_path = self.storage_root.join(&filepath);

        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let mut file = File::create(&absolute_path)
            .await
            .with_context(|| format!("create {}", absolute_path.display()))?;
        let mut hasher = StreamingHasher::new(etag_type.clone());
        let mut buf = vec![0u8; 64 * 1024];
        let mut total: i64 = 0;

        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).await?;
            hasher.update(&buf[..n]);
            total += n as i64;
        }
        file.flush().await?;

        let etag = hasher.finalize();
        Ok(StoredObject {
            filepath,
            file_format,
            absolute_path,
            filesize_bytes: total,
            etag,
            etag_type,
        })
    }

    /// Write from an async byte stream (chunks), hashing while writing.
    pub async fn put_chunks<S>(
        &self,
        mut stream: S,
        original_filename: &str,
        etag_type: EtagType,
        expected_etag: Option<&str>,
        forced_filepath: Option<&str>,
    ) -> Result<StoredObject>
    where
        S: futures::Stream<Item = Result<bytes::Bytes, anyhow::Error>> + Unpin,
    {
        use futures::StreamExt;

        let file_format = Self::extension_from_filename(original_filename);
        let (filepath, absolute_path) = if let Some(fp) = forced_filepath {
            let abs = self.absolute_path_for(fp);
            (fp.to_string(), abs)
        } else {
            let uuid = Uuid::new_v4();
            let fp = Self::relative_filepath(&uuid, &file_format);
            let abs = self.storage_root.join(&fp);
            (fp, abs)
        };

        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let mut file = File::create(&absolute_path)
            .await
            .with_context(|| format!("create {}", absolute_path.display()))?;
        let mut hasher = StreamingHasher::new(etag_type);
        let mut total: i64 = 0;

        while let Some(item) = stream.next().await {
            let chunk = item?;
            file.write_all(&chunk).await?;
            hasher.update(&chunk);
            total += chunk.len() as i64;
        }
        file.flush().await?;

        let etag = hasher.finalize();
        if let Some(expected) = expected_etag {
            if expected != etag {
                let _ = fs::remove_file(&absolute_path).await;
                bail!("etag mismatch: expected {expected}, got {etag}");
            }
        }

        Ok(StoredObject {
            filepath,
            file_format,
            absolute_path,
            filesize_bytes: total,
            etag,
            etag_type,
        })
    }

    pub async fn open_read(&self, filepath: &str) -> Result<File> {
        let path = self.absolute_path_for(filepath);
        File::open(&path)
            .await
            .with_context(|| format!("open {}", path.display()))
    }

    /// Re-hash an existing blob with `etag_type` (used by bucket etag recalculate).
    pub async fn hash_filepath(&self, filepath: &str, etag_type: EtagType) -> Result<String> {
        let mut file = self.open_read(filepath).await?;
        let mut hasher = StreamingHasher::new(etag_type);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize())
    }

    pub async fn unlink(&self, filepath: &str) -> Result<()> {
        let path = self.absolute_path_for(filepath);
        if path.exists() {
            fs::remove_file(&path)
                .await
                .with_context(|| format!("unlink {}", path.display()))?;
        }
        Ok(())
    }

    pub async fn disk_usage(&self) -> Result<(u64, u64)> {
        // Best-effort: sum object file sizes vs a soft capacity (env or 100 GiB).
        let mut used = 0u64;
        let mut rd = fs::read_dir(&self.objects_dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            if let Ok(meta) = entry.metadata().await {
                used += meta.len();
            }
        }
        let capacity = std::env::var("STORAGE_CAPACITY_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100 * 1024 * 1024 * 1024u64);
        Ok((used, capacity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn put_stream_hashes_md5() {
        let dir = tempfile_dir();
        let engine = StorageEngine::init(&dir).await.unwrap();
        let data = b"hello world";
        let reader = BufReader::new(Cursor::new(data.as_slice()));
        let stored = engine
            .put_stream(reader, "hello.txt", EtagType::Md5)
            .await
            .unwrap();
        assert_eq!(stored.etag.len(), 32);
        assert_eq!(stored.etag, "5eb63bbbe01eeed093cb22bb8f5acdc3");
        assert!(stored.absolute_path.exists());
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mys3-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
