//! Password hashing (argon2id, OWASP parameters m = 19 MiB, t = 2, p = 1) and the password rules
//! (docs/design/m4-web.md section 2.3).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use ring::rand::{SecureRandom, SystemRandom};

/// Memory cost in KiB.
pub const MEMORY_KIB: u32 = 19 * 1024;
/// Iterations.
pub const ITERATIONS: u32 = 2;
/// Lanes.
pub const PARALLELISM: u32 = 1;
/// Shortest accepted password, in characters.
pub const MIN_LEN: usize = 10;
/// Longest accepted password, in characters (bounds the cost of a hash).
pub const MAX_LEN: usize = 128;

/// Why a password was refused. [`PasswordError::code`] is what the API returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// Fewer than [`MIN_LEN`] characters.
    #[error("the password must have at least 10 characters")]
    TooShort,
    /// More than [`MAX_LEN`] characters.
    #[error("the password must have at most 128 characters")]
    TooLong,
    /// Equal to the account name.
    #[error("the password must not be the same as the account name")]
    SameAsUsername,
}

impl PasswordError {
    /// The fixed error code.
    pub fn code(self) -> &'static str {
        match self {
            PasswordError::TooShort => "password_too_short",
            PasswordError::TooLong => "password_too_long",
            PasswordError::SameAsUsername => "password_same_as_username",
        }
    }
}

/// Checks the rules.
///
/// # Errors
/// The first rule that is broken.
pub fn check_rules(username: &str, password: &str) -> Result<(), PasswordError> {
    let n = password.chars().count();
    if n < MIN_LEN {
        return Err(PasswordError::TooShort);
    }
    if n > MAX_LEN {
        return Err(PasswordError::TooLong);
    }
    if password.eq_ignore_ascii_case(username) || password == username {
        return Err(PasswordError::SameAsUsername);
    }
    Ok(())
}

fn hasher() -> Argon2<'static> {
    let params =
        Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, None).expect("the parameters are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hashes a password into a PHC string (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`).
pub fn hash(password: &str) -> String {
    let mut salt = [0u8; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .expect("the system random generator works");
    let salt = SaltString::encode_b64(&salt).expect("16 bytes are a valid salt");
    hasher()
        .hash_password(password.as_bytes(), &salt)
        .expect("hashing with valid parameters cannot fail")
        .to_string()
}

/// Whether `password` matches the PHC string. A string that is not a valid hash never matches.
pub fn verify(phc: &str, password: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(p) => Argon2::default()
            .verify_password(password.as_bytes(), &p)
            .is_ok(),
        Err(_) => false,
    }
}

/// A random password of `len` characters without look-alike characters (for the first
/// administrator).
pub fn random_password(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let rng = SystemRandom::new();
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 1];
    while out.len() < len {
        rng.fill(&mut buf).expect("the system random generator works");
        // reject values that would bias the choice (256 is not a multiple of the alphabet size)
        let limit = 256 - (256 % ALPHABET.len());
        if (buf[0] as usize) < limit {
            out.push(ALPHABET[buf[0] as usize % ALPHABET.len()] as char);
        }
    }
    out
}
