//! 원문 저장소(계획서 6.2): `eml\{h[0..2]}\{h[2..4]}\{sha256}.eml.zst`, zstd 레벨 3.
//! 같은 원문은 한 번만 저장하고, 쓰기는 임시 파일 → fsync → rename으로 원자적으로 한다.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

const LEVEL: i32 = 3;

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct EmlStore {
    dir: PathBuf,
}

impl EmlStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, hash: &str) -> PathBuf {
        let (a, b) = (
            hash.get(..2).unwrap_or("00"),
            hash.get(2..4).unwrap_or("00"),
        );
        self.dir.join(a).join(b).join(format!("{hash}.eml.zst"))
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.path(hash).is_file()
    }

    /// 원문을 저장하고 SHA-256(소문자 16진)을 돌려준다. 이미 있으면 쓰지 않는다.
    pub fn put(&self, raw: &[u8]) -> io::Result<String> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let hash = sha256_hex(raw);
        let path = self.path(&hash);
        if path.is_file() {
            return Ok(hash);
        }
        let parent = path.parent().unwrap_or(&self.dir);
        fs::create_dir_all(parent)?;
        let compressed = zstd::encode_all(raw, LEVEL)?;
        let tmp = parent.join(format!(
            "{hash}.{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&compressed)?;
            f.sync_all()?;
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            // 다른 작업이 같은 원문을 먼저 저장했으면 성공으로 본다.
            if !path.is_file() {
                return Err(e);
            }
        }
        Ok(hash)
    }

    pub fn get(&self, hash: &str) -> io::Result<Vec<u8>> {
        zstd::decode_all(File::open(self.path(hash))?)
    }

    /// 압축 해제 뒤 해시까지 맞는지 확인한다.
    pub fn get_verified(&self, hash: &str) -> io::Result<Vec<u8>> {
        let raw = self.get(hash)?;
        if sha256_hex(&raw) != hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("원문 해시 불일치: {hash}"),
            ));
        }
        Ok(raw)
    }

    pub fn remove(&self, hash: &str) -> io::Result<()> {
        match fs::remove_file(self.path(hash)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// 저장된 파일 수와 디스크 크기(압축 상태).
    pub fn usage(&self) -> io::Result<(u64, u64)> {
        fn walk(dir: &Path, acc: &mut (u64, u64)) -> io::Result<()> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let meta = entry.metadata()?;
                if meta.is_dir() {
                    walk(&entry.path(), acc)?;
                } else if entry.file_name().to_string_lossy().ends_with(".eml.zst") {
                    acc.0 += 1;
                    acc.1 += meta.len();
                }
            }
            Ok(())
        }
        let mut acc = (0, 0);
        if self.dir.is_dir() {
            walk(&self.dir, &mut acc)?;
        }
        Ok(acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_dedupes_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = EmlStore::new(dir.path().join("eml"));
        let raw = b"Subject: x\r\n\r\nbody".repeat(100);
        let h1 = store.put(&raw).unwrap();
        let h2 = store.put(&raw).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(
            store
                .path(&h1)
                .ends_with(format!("{}/{}/{h1}.eml.zst", &h1[..2], &h1[2..4]))
        );
        assert_eq!(store.get_verified(&h1).unwrap(), raw);
        assert_eq!(store.usage().unwrap().0, 1);
        // 임시 파일이 남지 않는다.
        let leftovers = fs::read_dir(store.path(&h1).parent().unwrap())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        store.remove(&h1).unwrap();
        store.remove(&h1).unwrap();
        assert!(!store.contains(&h1));
    }

    #[test]
    fn corrupt_file_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EmlStore::new(dir.path());
        let h = store.put(b"original").unwrap();
        let other = zstd::encode_all(&b"tampered"[..], LEVEL).unwrap();
        fs::write(store.path(&h), other).unwrap();
        assert!(store.get_verified(&h).is_err());
    }
}
