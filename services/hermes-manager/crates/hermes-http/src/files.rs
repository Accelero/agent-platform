//! Bounded reads of the files configuration points at. Contents and paths
//! are never echoed; a missing, empty or oversized file is `invalid_request`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hermes_core::app::error::AppError;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

/// Reads at most `maximum` bytes; empty or larger files are refused.
pub async fn read(path: &Path, maximum: usize) -> Result<Zeroizing<Vec<u8>>, AppError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| AppError::invalid())?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| AppError::invalid())?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(AppError::invalid());
    }
    Ok(bytes)
}

/// A UTF-8 file without surrounding whitespace.
pub async fn read_text(path: &Path, maximum: usize) -> Result<Zeroizing<String>, AppError> {
    let bytes = read(path, maximum).await?;
    let text = std::str::from_utf8(&bytes).map_err(|_| AppError::invalid())?;
    Ok(Zeroizing::new(text.trim().to_owned()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Keyring {
    active_key_id: String,
    key_files: BTreeMap<String, PathBuf>,
}

/// A session keyring: a file naming key files, each exactly 32 bytes.
/// Returns the active key ID and every key.
pub async fn keyring(path: &Path) -> Result<(String, BTreeMap<String, Vec<u8>>), AppError> {
    let keyring: Keyring =
        serde_json::from_slice(&read(path, 16 * 1024).await?).map_err(|_| AppError::invalid())?;
    if keyring.key_files.is_empty() || keyring.key_files.len() > 16 {
        return Err(AppError::invalid());
    }
    let mut keys = BTreeMap::new();
    for (id, file) in keyring.key_files {
        let key = read(&file, 32).await?;
        if key.len() != 32 {
            return Err(AppError::invalid());
        }
        keys.insert(id, key.to_vec());
    }
    Ok((keyring.active_key_id, keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keyrings_are_bounded_and_need_exact_32_byte_keys() {
        let directory = std::env::temp_dir().join(format!("hermes-files-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let key = directory.join("current");
        let keyring_file = directory.join("keyring.json");
        let write_keyring = |files: serde_json::Value| {
            let path = keyring_file.clone();
            async move {
                let body = serde_json::json!({"activeKeyId": "current", "keyFiles": files});
                tokio::fs::write(path, body.to_string()).await.unwrap();
            }
        };
        write_keyring(serde_json::json!({"current": key})).await;
        for length in [31, 33] {
            tokio::fs::write(&key, vec![9; length]).await.unwrap();
            assert!(keyring(&keyring_file).await.is_err(), "{length}");
        }
        tokio::fs::write(&key, vec![9; 32]).await.unwrap();
        let (active, keys) = keyring(&keyring_file).await.unwrap();
        assert_eq!((active.as_str(), keys["current"].len()), ("current", 32));
        let many: serde_json::Map<_, _> = (0..17)
            .map(|i| (format!("key-{i}"), serde_json::json!(key)))
            .collect();
        write_keyring(serde_json::Value::Object(many)).await;
        assert!(keyring(&keyring_file).await.is_err());
        write_keyring(serde_json::json!({})).await;
        assert!(keyring(&keyring_file).await.is_err());
        tokio::fs::remove_dir_all(&directory).await.unwrap();
    }
}
