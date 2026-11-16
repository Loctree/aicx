//! Small, identity-bound digest cache shared by payload and model verification.
//! Callers enforce their own allowed roots before requesting a digest.

use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result, ensure};

const MAX_CACHED_FILES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
struct StrongIdentity {
    volume: u64,
    file_id: [u8; 16],
    modified: [i64; 2],
    changed: [i64; 2],
    attributes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    strong: Option<StrongIdentity>,
}

fn stamp(file: &File) -> Result<FileStamp> {
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "digest target must be a regular file");
    Ok(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        created: metadata.created().ok(),
        strong: strong_identity(file, &metadata),
    })
}

#[cfg(unix)]
fn strong_identity(_file: &File, metadata: &Metadata) -> Option<StrongIdentity> {
    use std::os::unix::fs::MetadataExt;
    (metadata.ino() != 0).then(|| StrongIdentity {
        volume: metadata.dev(),
        file_id: u128::from(metadata.ino()).to_le_bytes(),
        modified: [metadata.mtime(), metadata.mtime_nsec()],
        changed: [metadata.ctime(), metadata.ctime_nsec()],
        attributes: metadata.mode(),
    })
}

#[cfg(windows)]
fn strong_identity(file: &File, _metadata: &Metadata) -> Option<StrongIdentity> {
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FILE_ID_INFO, FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::uninit();
    let mut id = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the borrowed File keeps the handle valid. Each writable buffer
    // has precisely the requested C layout/size and is read only on success.
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    // SAFETY: same handle/buffer guarantees as the call above.
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            id.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if basic_ok == 0 || id_ok == 0 {
        // Some filesystems lack a stable file ID/change stamp. Hash every
        // time there rather than weakening mutation detection for a cache hit.
        return None;
    }
    // SAFETY: both successful calls initialized their respective structures.
    let (basic, id) = unsafe { (basic.assume_init(), id.assume_init()) };
    id.FileId
        .Identifier
        .iter()
        .any(|byte| *byte != 0)
        .then_some(StrongIdentity {
            volume: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
            modified: [basic.LastWriteTime, 0],
            changed: [basic.ChangeTime, 0],
            attributes: basic.FileAttributes,
        })
}

#[cfg(not(any(unix, windows)))]
fn strong_identity(_file: &File, _metadata: &Metadata) -> Option<StrongIdentity> {
    None
}

struct VerifiedDigest {
    stamp: FileStamp,
    digest: [u8; 32],
}

#[derive(Default)]
struct DigestSlot {
    verified: Mutex<Option<VerifiedDigest>>,
}

#[derive(Default)]
struct DigestCache {
    slots: Mutex<HashMap<PathBuf, Arc<DigestSlot>>>,
    #[cfg(test)]
    hash_reads: std::sync::atomic::AtomicUsize,
}

fn open_digest_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open digest target {}", path.display()))
}

impl DigestCache {
    fn digest(&self, path: &Path) -> Result<[u8; 32]> {
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve digest target {}", path.display()))?;
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(|error| {
                let mut slots = error.into_inner();
                slots.clear();
                slots
            });
            if !slots.contains_key(&path) && slots.len() >= MAX_CACHED_FILES {
                slots.clear();
            }
            slots.entry(path.clone()).or_default().clone()
        };
        // Serialize only the same file: concurrent cold queries hash it once.
        let mut verified = slot.verified.lock().unwrap_or_else(|error| {
            let mut value = error.into_inner();
            *value = None;
            value
        });
        let mut file = open_digest_file(&path)?;
        let before = stamp(&file)?;
        if before.strong.is_some()
            && let Some(cached) = verified.as_ref()
            && cached.stamp == before
        {
            ensure!(
                stamp(&open_digest_file(&path)?)? == before,
                "digest target replaced during verification"
            );
            return Ok(cached.digest);
        }
        *verified = None;
        #[cfg(test)]
        self.hash_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let after = stamp(&file)?;
        ensure!(
            before == after && stamp(&open_digest_file(&path)?)? == after,
            "digest target changed during verification; retry"
        );
        let digest = *hasher.finalize().as_bytes();
        if after.strong.is_some() {
            *verified = Some(VerifiedDigest {
                stamp: after,
                digest,
            });
        }
        Ok(digest)
    }
}

/// Return the BLAKE3 digest, reusing verification only while the file identity,
/// size, write time and change time remain unchanged. Cache entries contain
/// digests/metadata, never mappings, model buffers, or retained open handles.
pub fn verified_file_digest(path: &Path) -> Result<[u8; 32]> {
    static CACHE: OnceLock<DigestCache> = OnceLock::new();
    CACHE.get_or_init(DigestCache::default).digest(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn warm_file_digest_avoids_hashing_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, b"payload bytes").unwrap();
        let cache = DigestCache::default();
        let expected = *blake3::hash(b"payload bytes").as_bytes();
        assert_eq!(cache.digest(&path).unwrap(), expected);
        assert_eq!(cache.digest(&path).unwrap(), expected);
        let supported = stamp(&open_digest_file(&path).unwrap())
            .unwrap()
            .strong
            .is_some();
        assert_eq!(
            cache.hash_reads.load(Ordering::SeqCst),
            if supported { 1 } else { 2 }
        );
    }

    #[test]
    fn file_digest_detects_same_size_mutation_with_restored_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, b"original").unwrap();
        let modified = filetime::FileTime::from_last_modification_time(&path.metadata().unwrap());
        let cache = DigestCache::default();
        let before = cache.digest(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        std::fs::write(&path, b"modified").unwrap();
        filetime::set_file_mtime(&path, modified).unwrap();
        let after = cache.digest(&path).unwrap();
        assert_ne!(before, after);
        assert_eq!(after, *blake3::hash(b"modified").as_bytes());
        assert_eq!(cache.hash_reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn file_digest_detects_atomic_replacement_with_restored_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        let replacement = dir.path().join("replacement");
        std::fs::write(&path, b"original").unwrap();
        let modified = filetime::FileTime::from_last_modification_time(&path.metadata().unwrap());
        let cache = DigestCache::default();
        let before = cache.digest(&path).unwrap();
        std::fs::write(&replacement, b"replaced").unwrap();
        filetime::set_file_mtime(&replacement, modified).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_ne!(cache.digest(&path).unwrap(), before);
        assert_eq!(cache.hash_reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn simultaneous_warmup_hashes_a_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload");
        std::fs::write(&path, vec![42; 2 * 1024 * 1024]).unwrap();
        if stamp(&open_digest_file(&path).unwrap())
            .unwrap()
            .strong
            .is_none()
        {
            return;
        }
        let cache = Arc::new(DigestCache::default());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let cache = &cache;
                let path = &path;
                scope.spawn(move || {
                    cache.digest(path).unwrap();
                });
            }
        });
        assert_eq!(cache.hash_reads.load(Ordering::SeqCst), 1);
    }
}
