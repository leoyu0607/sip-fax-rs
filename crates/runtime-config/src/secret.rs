//! Encryption of the secret fields stored in the database (docs/design/m4-web.md section 1.6).
//!
//! `conf/secret.key` holds 32 random bytes (mode 0600). A secret is stored as
//! `enc:v1:<base64(nonce || ciphertext || tag)>`, AES-256-GCM with a fresh 96-bit nonce per
//! value; the section name and the field path are authenticated data (`<section>/<path>`, for
//! example `trunk:cht/register.password`), so a ciphertext cannot be moved to another field.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

/// Prefix of a stored (encrypted) secret.
pub const PREFIX: &str = "enc:v1:";
/// Size of the key file.
pub const KEY_LEN: usize = 32;

/// Why a secret could not be encrypted or decrypted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The key file was missing or unusable when the server started.
    #[error("the secret key is not available: {0}")]
    NoKey(String),
    /// The value is not `enc:v1:<base64>` or is too short.
    #[error("the stored secret is malformed")]
    Malformed,
    /// Wrong key (the file was replaced) or the value was changed.
    #[error("the stored secret cannot be decrypted with the current secret.key")]
    Undecryptable,
}

/// What happened to `secret.key` at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    /// An existing key was loaded.
    Loaded,
    /// There was no key: a new one was generated.
    Created,
    /// The file had the wrong size: it was moved aside (the path) and a new key generated.
    Replaced(String),
    /// No usable key and none could be created (the reason).
    Unavailable(String),
}

/// The loaded key.
pub struct SecretBox {
    key: Option<LessSafeKey>,
    raw: Option<[u8; KEY_LEN]>,
    status: KeyStatus,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBox({:?})", self.status)
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    SystemRandom::new()
        .fill(&mut b)
        .expect("the system random generator works");
    b
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(data)
}

/// Writes `data` to `path` with mode 0600 (replacing an existing file). Used for the key and for
/// `conf/initial-admin-password`.
///
/// # Errors
/// The file cannot be created.
pub fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    write_private(path, data)
}

impl SecretBox {
    /// Loads `path`, creating the key when the file does not exist.
    pub fn load_or_create(path: &Path) -> SecretBox {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() == KEY_LEN => {
                let mut raw = [0u8; KEY_LEN];
                raw.copy_from_slice(&bytes);
                Self::from_raw(raw, KeyStatus::Loaded)
            }
            Ok(_) => {
                // never overwrite what could be somebody's key: move it aside
                let mut aside = path.as_os_str().to_owned();
                aside.push(format!(".bad-{}", stamp()));
                let aside = std::path::PathBuf::from(aside);
                match std::fs::rename(path, &aside) {
                    Ok(()) => Self::create(path, KeyStatus::Replaced(aside.display().to_string())),
                    Err(e) => Self::unavailable(format!(
                        "{} has the wrong size and cannot be moved aside: {e}",
                        path.display()
                    )),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Self::create(path, KeyStatus::Created)
            }
            Err(e) => Self::unavailable(format!("cannot read {}: {e}", path.display())),
        }
    }

    fn create(path: &Path, status: KeyStatus) -> SecretBox {
        let raw: [u8; KEY_LEN] = random_bytes();
        match write_private_file(path, &raw) {
            Ok(()) => Self::from_raw(raw, status),
            Err(e) => Self::unavailable(format!("cannot create {}: {e}", path.display())),
        }
    }

    fn from_raw(raw: [u8; KEY_LEN], status: KeyStatus) -> SecretBox {
        let key = UnboundKey::new(&AES_256_GCM, &raw)
            .map(LessSafeKey::new)
            .expect("32 bytes are a valid AES-256 key");
        SecretBox {
            key: Some(key),
            raw: Some(raw),
            status,
        }
    }

    /// A box without a key: every encrypt and decrypt fails with [`SecretError::NoKey`].
    pub fn unavailable(reason: impl Into<String>) -> SecretBox {
        SecretBox {
            key: None,
            raw: None,
            status: KeyStatus::Unavailable(reason.into()),
        }
    }

    /// A box with a given key (tests).
    pub fn from_key(raw: [u8; KEY_LEN]) -> SecretBox {
        Self::from_raw(raw, KeyStatus::Loaded)
    }

    /// What happened at load.
    pub fn status(&self) -> &KeyStatus {
        &self.status
    }

    /// Whether secrets can be encrypted and decrypted.
    pub fn available(&self) -> bool {
        self.key.is_some()
    }

    /// The key bytes (only for tests of the on-disk format).
    #[doc(hidden)]
    pub fn raw_key(&self) -> Option<[u8; KEY_LEN]> {
        self.raw
    }

    /// Encrypts `plain` for the field `context` (for example `trunk:cht/register.password`).
    ///
    /// # Errors
    /// [`SecretError::NoKey`] when the key is not available.
    pub fn encrypt(&self, context: &str, plain: &str) -> Result<String, SecretError> {
        let key = self.key.as_ref().ok_or_else(|| self.no_key())?;
        let nonce_bytes: [u8; NONCE_LEN] = random_bytes();
        let mut buf = plain.as_bytes().to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(context.as_bytes()),
            &mut buf,
        )
        .map_err(|_| SecretError::Malformed)?;
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&buf);
        Ok(format!("{PREFIX}{}", B64.encode(out)))
    }

    /// Decrypts a value written by [`SecretBox::encrypt`] for the same `context`.
    ///
    /// # Errors
    /// No key, a malformed value, or a value that does not authenticate (wrong key or context).
    pub fn decrypt(&self, context: &str, stored: &str) -> Result<String, SecretError> {
        let key = self.key.as_ref().ok_or_else(|| self.no_key())?;
        let b64 = stored.strip_prefix(PREFIX).ok_or(SecretError::Malformed)?;
        let data = B64.decode(b64).map_err(|_| SecretError::Malformed)?;
        if data.len() < NONCE_LEN + 16 {
            return Err(SecretError::Malformed);
        }
        let (nonce, rest) = data.split_at(NONCE_LEN);
        let mut nb = [0u8; NONCE_LEN];
        nb.copy_from_slice(nonce);
        let mut buf = rest.to_vec();
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nb),
                Aad::from(context.as_bytes()),
                &mut buf,
            )
            .map_err(|_| SecretError::Undecryptable)?;
        String::from_utf8(plain.to_vec()).map_err(|_| SecretError::Malformed)
    }

    fn no_key(&self) -> SecretError {
        match &self.status {
            KeyStatus::Unavailable(r) => SecretError::NoKey(r.clone()),
            _ => SecretError::NoKey("no key".into()),
        }
    }
}

fn stamp() -> String {
    // seconds since the epoch are unique enough for a file name that is only a backup
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .to_string()
}
