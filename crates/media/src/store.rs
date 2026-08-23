//! Storage abstraction. Local filesystem today, S3-compatible behind the
//! same trait later (ARCHITECTURE.md §5: "Storage trait LocalStore|S3Store").

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use sha2::{Digest, Sha256};
use tokio::io::AsyncRead;

use crate::error::MediaError;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A stored object: its logical key plus the absolute on-disk location the
/// media pipeline can hand to ffmpeg/ffprobe.
#[derive(Debug, Clone)]
pub struct StoredFile {
    pub key: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// Backend-agnostic object storage. Implementations must be `Send + Sync`
/// so they can be shared behind `Arc<dyn Store>` across worker tasks.
pub trait Store: Send + Sync {
    /// Persist `data` under `key`, creating any parent directories.
    fn save_bytes<'a>(
        &'a self,
        key: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<StoredFile, MediaError>>;

    /// Stream an async reader into `key` without buffering the whole object.
    fn save_stream<'a>(
        &'a self,
        key: &'a str,
        reader: &'a mut (dyn AsyncRead + Unpin + Send),
    ) -> BoxFuture<'a, Result<StoredFile, MediaError>>;

    /// Remove the object under `key`. Missing objects are not an error.
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), MediaError>>;

    /// Return location/size metadata for `key`; `MediaError::NotFound` when absent.
    fn open<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<StoredFile, MediaError>>;
}

/// Filesystem-backed [`Store`] rooted at a configurable directory.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a logical key to an absolute path, refusing traversal.
    fn abs(&self, key: &str) -> Result<PathBuf, MediaError> {
        if key.contains("..") || key.starts_with('/') {
            return Err(MediaError::Probe(format!("invalid storage key: {key}")));
        }
        let path = self.root.join(key);
        if !path.starts_with(&self.root) {
            return Err(MediaError::Probe(format!("invalid storage key: {key}")));
        }
        Ok(path)
    }
}

impl Store for LocalStore {
    fn save_bytes<'a>(
        &'a self,
        key: &'a str,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<StoredFile, MediaError>> {
        Box::pin(async move {
            let path = self.abs(key)?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, data).await?;
            Ok(StoredFile {
                key: key.to_string(),
                path,
                size_bytes: data.len() as u64,
            })
        })
    }

    fn save_stream<'a>(
        &'a self,
        key: &'a str,
        reader: &'a mut (dyn AsyncRead + Unpin + Send),
    ) -> BoxFuture<'a, Result<StoredFile, MediaError>> {
        Box::pin(async move {
            let path = self.abs(key)?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let mut file = tokio::fs::File::create(&path).await?;
            let copied = tokio::io::copy(reader, &mut file).await?;
            Ok(StoredFile {
                key: key.to_string(),
                path,
                size_bytes: copied,
            })
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), MediaError>> {
        Box::pin(async move {
            let path = self.abs(key)?;
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn open<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<StoredFile, MediaError>> {
        Box::pin(async move {
            let path = self.abs(key)?;
            let meta = match tokio::fs::metadata(&path).await {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(MediaError::NotFound(key.to_string()))
                }
                Err(e) => return Err(e.into()),
            };
            Ok(StoredFile {
                key: key.to_string(),
                path,
                size_bytes: meta.len(),
            })
        })
    }
}

/// Hex-encoded SHA-256 of the input, used for local-upload dedup.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}
