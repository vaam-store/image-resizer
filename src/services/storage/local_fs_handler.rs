use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

use crate::services::storage::core::StorageBackend;
use crate::services::storage::key_validation::split_key;

/// Number of leading hex characters of a key's hash portion used as the
/// on-disk shard directory (#38): keeps a large cache from landing as
/// millions of flat entries in one directory. Purely an on-disk layout
/// detail - the `key` callers pass to `upload_image`/`check_cache`/
/// `get_image` is unchanged.
const SHARD_PREFIX_LEN: usize = 2;

/// Monotonic counter mixed into temp-file names so concurrent writers to the
/// same key never collide on the staging file, even within the same
/// nanosecond.
static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Local file system storage implementation
pub struct LocalFSStorage {
    base_path: PathBuf,
}

impl LocalFSStorage {
    pub(crate) fn new(base_path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            base_path: base_path.into(),
        })
    }

    /// Drops `.`/`..`/empty segments and any leading root, so the result is
    /// always a plain relative path with no way to climb above wherever it's
    /// joined onto. Defence in depth (#23): the shared validation in
    /// `StorageService` is the primary guard, but this makes the join itself
    /// safe even if an unvalidated key somehow reached this backend
    /// directly (e.g. a future direct caller, or a test).
    fn sanitize_relative(input: &str) -> PathBuf {
        let mut out = PathBuf::new();
        for segment in input.split('/') {
            match segment {
                "" | "." | ".." => continue,
                other => out.push(other),
            }
        }
        out
    }

    /// Maps `key` to a shard-relative path: `<prefix>/<first N hex chars of
    /// hash>/<hash>.<ext>`. Falls back to sanitizing the raw key as a flat
    /// relative path if it doesn't have the expected `<prefix><hash>.<ext>`
    /// shape (should only happen if an unvalidated key reaches this backend
    /// directly - see `sanitize_relative`).
    fn relative_path(key: &str) -> PathBuf {
        match split_key(key) {
            Some((prefix, hash, ext)) if !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                let shard_len = hash.len().min(SHARD_PREFIX_LEN);
                let mut path = Self::sanitize_relative(prefix);
                path.push(&hash[..shard_len]);
                path.push(format!("{hash}.{ext}"));
                path
            }
            _ => Self::sanitize_relative(key),
        }
    }

    fn resolve(&self, key: &str) -> PathBuf {
        self.base_path.join(Self::relative_path(key))
    }

    /// Confirms `path` (assumed to already exist) canonicalises to somewhere
    /// inside `base_path`. Defence in depth for #23, complementing
    /// `sanitize_relative`: guards against e.g. a symlink planted inside
    /// `base_path` that resolves outside it.
    fn assert_within_base(&self, path: &Path) -> Result<PathBuf> {
        let canonical_base = self
            .base_path
            .canonicalize()
            .context("Failed to canonicalise local storage base path")?;
        let canonical = path.canonicalize().with_context(|| {
            format!(
                "Failed to canonicalise local storage path: {}",
                path.display()
            )
        })?;
        if !canonical.starts_with(&canonical_base) {
            return Err(anyhow!(
                "Resolved storage path escapes the storage base path: {}",
                path.display()
            ));
        }
        Ok(canonical)
    }
}

/// Builds a unique temp-file name for `final_name`, staged in the same
/// directory as the final path so the rename in `upload_image` is a
/// same-filesystem (and therefore POSIX-atomic) rename.
fn temp_file_name(final_name: &std::ffi::OsStr) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        ".{}.{}.{}.{}.tmp",
        final_name.to_string_lossy(),
        std::process::id(),
        nanos,
        counter
    )
}

#[async_trait]
impl StorageBackend for LocalFSStorage {
    async fn upload_image(&self, key: &str, _content_type: &str, data: Vec<u8>) -> Result<()> {
        let file_path = self.resolve(key);
        let parent = file_path
            .parent()
            .ok_or_else(|| anyhow!("Invalid storage path for key: {key}"))?;

        tokio::fs::create_dir_all(parent)
            .await
            .context("Failed to create a local storage directory")?;

        // Defence in depth (#23): confirm the shard directory we're about to
        // write into is still inside base_path.
        let canonical_base = self
            .base_path
            .canonicalize()
            .context("Failed to canonicalise local storage base path")?;
        let canonical_parent = parent
            .canonicalize()
            .context("Failed to canonicalise local storage target directory")?;
        if !canonical_parent.starts_with(&canonical_base) {
            return Err(anyhow!(
                "Resolved storage directory escapes the storage base path for key: {key}"
            ));
        }

        let file_name = file_path
            .file_name()
            .ok_or_else(|| anyhow!("Invalid storage path for key: {key}"))?;
        let tmp_path = parent.join(temp_file_name(file_name));

        // Atomic write (#38): stage the full contents in a temp file in the
        // SAME directory as the final path, fsync it so the bytes are
        // durable, then rename into place. `rename` within one directory is
        // atomic on POSIX, so a concurrent reader either sees the previous
        // complete file or the new complete file - never a partial one.
        // There's no request coalescing yet (#37), so concurrent writers to
        // the same key are an expected case here, not a rare race.
        let write_result: Result<()> = async {
            let mut file = tokio::fs::File::create(&tmp_path)
                .await
                .context("Failed to create temp file for atomic write")?;
            file.write_all(&data)
                .await
                .context("Failed to write image to temp file")?;
            file.sync_all().await.context("Failed to fsync temp file")?;
            Ok(())
        }
        .await;

        if let Err(e) = write_result {
            // Best-effort cleanup - if this fails too there's nothing more
            // useful to do than leave an orphaned temp file behind; it never
            // shadows the final path so it can't cause a partial read.
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }

        tokio::fs::rename(&tmp_path, &file_path)
            .await
            .context("Failed to atomically rename temp file into place")?;

        Ok(())
    }

    async fn check_cache(&self, key: &str) -> Result<bool> {
        let file_path = self.resolve(key);
        match tokio::fs::metadata(&file_path).await {
            // A directory is not a cache hit - `metadata(...).is_ok()` alone
            // can't tell the two apart, which used to turn what should be a
            // clean miss into a downstream read error (#38).
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => {
                Err(e).with_context(|| format!("Failed to stat local storage path for key: {key}"))
            }
        }
    }

    async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        let file_path = self.resolve(key);

        let meta = tokio::fs::metadata(&file_path).await.with_context(|| {
            format!(
                "Image not found in local file system: {}",
                file_path.display()
            )
        })?;
        if !meta.is_file() {
            return Err(anyhow!(
                "Cache path is not a regular file: {}",
                file_path.display()
            ));
        }

        let canonical = self.assert_within_base(&file_path)?;

        tokio::fs::read(&canonical).await.with_context(|| {
            format!(
                "Failed to read image from local file system: {}",
                file_path.display()
            )
        })
    }
}
