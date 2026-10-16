//! Important-artifact registry.
//!
//! LJ Hub records *where* a deliverable lives and what it contains (size +
//! SHA-256) so the next agent can prove it is looking at the same bytes.
//!
//! Two deliberate limits:
//!  - nothing is scanned or hashed unless the user registers it explicitly;
//!  - registration is **not** a backup. `backup_verified` stays false until a
//!    real backup is confirmed elsewhere.

use crate::model::{now_iso, ArtifactRef};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Files larger than this are registered without a hash unless the caller
/// explicitly allows it, so a multi-gigabyte model file cannot freeze the UI.
pub const DEFAULT_HASH_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Verification {
    pub accessible: bool,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub matches_recorded: Option<bool>,
    pub detail: String,
}

fn resolve(root: &Path, input: &str) -> PathBuf {
    let p = Path::new(input);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// Stream a file through SHA-256 without loading it into memory.
pub fn hash_file(path: &Path) -> Result<(u64, String), String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("无法读取：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size: u64 = 0;
    loop {
        let read = file.read(&mut buf).map_err(|e| format!("读取中断：{e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        size += read as u64;
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

pub fn register(
    root: &Path,
    id: &str,
    input: &str,
    kind: &str,
    note: &str,
    allow_large: bool,
) -> Result<ArtifactRef, String> {
    let absolute = resolve(root, input);
    let meta = std::fs::metadata(&absolute)
        .map_err(|e| format!("文件不存在或不可访问：{}（{e}）", absolute.display()))?;
    let size = if meta.is_file() {
        Some(meta.len())
    } else {
        None
    };

    let mut sha256 = None;
    let mut accessible = Some(true);
    let mut extra_note = String::new();
    match size {
        Some(len) if meta.is_file() && (len <= DEFAULT_HASH_LIMIT || allow_large) => {
            match hash_file(&absolute) {
                Ok((_, digest)) => sha256 = Some(digest),
                Err(e) => {
                    accessible = Some(false);
                    extra_note = format!("哈希失败：{e}");
                }
            }
        }
        Some(len) => {
            extra_note = format!(
                "文件 {:.2} GiB 超过默认哈希上限（{:.0} GiB），未计算 SHA-256。可按需强制校验。",
                len as f64 / 1_073_741_824.0,
                DEFAULT_HASH_LIMIT as f64 / 1_073_741_824.0
            );
        }
        None if meta.is_dir() => {
            extra_note = "登记的是目录：只记录路径，目录内容未哈希。".to_string();
        }
        None => {}
    }

    Ok(ArtifactRef {
        id: id.to_string(),
        path: input.to_string(),
        absolute_path: absolute.display().to_string(),
        kind: kind.to_string(),
        size,
        sha256,
        accessible,
        // Registration is never evidence of a backup.
        backup_verified: false,
        note: if extra_note.is_empty() {
            note.to_string()
        } else if note.is_empty() {
            extra_note
        } else {
            format!("{note} {extra_note}")
        },
        registered_at: now_iso(),
    })
}

/// Re-check a registered artifact on demand (the receiving side of a handoff
/// uses this to prove the file is still the same).
pub fn verify(artifact: &ArtifactRef, force_hash: bool) -> Verification {
    let path = PathBuf::from(&artifact.absolute_path);
    if !path.exists() {
        return Verification {
            accessible: false,
            size: None,
            sha256: None,
            matches_recorded: None,
            detail: format!("文件缺失：{}", artifact.absolute_path),
        };
    }
    let size = std::fs::metadata(&path).ok().map(|m| m.len());
    let should_hash = force_hash
        || artifact.sha256.is_some()
        || size.map(|s| s <= DEFAULT_HASH_LIMIT).unwrap_or(false);
    if !should_hash {
        return Verification {
            accessible: true,
            size,
            sha256: None,
            matches_recorded: None,
            detail: "文件存在；因体积超过哈希上限，本次未校验内容（登记时也未记录哈希）。"
                .to_string(),
        };
    }
    match hash_file(&path) {
        Ok((_, digest)) => {
            let matches = artifact.sha256.as_ref().map(|recorded| *recorded == digest);
            Verification {
                accessible: true,
                size,
                sha256: Some(digest),
                matches_recorded: matches,
                detail: match matches {
                    Some(true) => "内容与登记时的 SHA-256 一致。".to_string(),
                    Some(false) => "内容已变化：SHA-256 与登记时不一致。".to_string(),
                    None => "文件存在，已计算 SHA-256（登记时没有哈希可比较）。".to_string(),
                },
            }
        }
        Err(e) => Verification {
            accessible: false,
            size,
            sha256: None,
            matches_recorded: None,
            detail: e,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_records_size_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.bin");
        std::fs::write(&file, b"hello").unwrap();
        let art = register(dir.path(), "a1", "out.bin", "binary", "", false).unwrap();
        assert_eq!(art.size, Some(5));
        assert_eq!(
            art.sha256.as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert!(!art.backup_verified, "登记不等于备份");
    }

    #[test]
    fn missing_file_is_an_error_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let result = register(dir.path(), "a1", "nope.bin", "binary", "", false);
        assert!(result.is_err(), "缺失文件必须报错，不能当成通过");
    }

    #[test]
    fn verification_detects_changed_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.bin");
        std::fs::write(&file, b"hello").unwrap();
        let art = register(dir.path(), "a1", "out.bin", "binary", "", false).unwrap();
        std::fs::write(&file, b"changed").unwrap();
        let v = verify(&art, true);
        assert_eq!(v.matches_recorded, Some(false));
        std::fs::remove_file(&file).unwrap();
        let v = verify(&art, true);
        assert!(!v.accessible);
    }
}
