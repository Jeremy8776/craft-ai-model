//! Passphrase encryption for the secret file.
//!
//! Key: Argon2id over the passphrase and a fresh 16 byte salt. Cipher: XChaCha20-Poly1305 with a
//! fresh 24 byte nonce on every write. The header (version and KDF settings and salt) is bound
//! into the authentication tag as associated data, so editing any of it makes decryption fail.
//!
//! Cost settings live in the file so they can be raised later without breaking old files. They
//! are also capped on read, so a hostile file cannot make a reader allocate gigabytes.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use craft_ai_core::SecretString;
use craft_ai_core::payload::{base64_decode, base64_encode};
use serde::{Deserialize, Serialize};

/// The only envelope version this build reads and writes.
pub(crate) const VERSION: u32 = 1;
const KDF_NAME: &str = "argon2id";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// Argon2id cost settings.
///
/// Real use takes [`KdfParams::default`] (64 MiB, 3 passes, about a quarter of a second). Tests use
/// [`KdfParams::new`] with tiny values so they stay fast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
}

impl KdfParams {
    const MAX_M_COST_KIB: u32 = 256 * 1024;
    const MAX_T_COST: u32 = 10;
    const MAX_P_COST: u32 = 8;

    /// Memory in KiB, passes, lanes. `None` if outside the range this crate accepts.
    pub fn new(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Option<Self> {
        let ok = (1..=Self::MAX_P_COST).contains(&p_cost)
            && (1..=Self::MAX_T_COST).contains(&t_cost)
            && (m_cost_kib >= 8 * p_cost && m_cost_kib <= Self::MAX_M_COST_KIB);
        ok.then_some(Self {
            m_cost_kib,
            t_cost,
            p_cost,
        })
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            m_cost_kib: 64 * 1024,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

/// Why sealing or opening failed. Carries no secret and no plaintext.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CryptoError {
    /// Not a file this code wrote, or its fields are out of range.
    BadFormat,
    /// Written by a newer version.
    TooNew(u32),
    /// Wrong passphrase, or the file was altered or damaged. Indistinguishable by design.
    WrongKeyOrDamaged,
    /// The system could not supply randomness or run the KDF.
    Failed,
}

/// The encrypted file as stored: JSON with base64 fields.
#[derive(Serialize, Deserialize)]
pub(crate) struct Envelope {
    version: u32,
    kdf: String,
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

impl Envelope {
    fn aad(&self, salt: &[u8]) -> Vec<u8> {
        let mut aad = format!(
            "craft-ai-secrets/v{}/{}/{}/{}/{}/",
            self.version, self.kdf, self.m_cost_kib, self.t_cost, self.p_cost
        )
        .into_bytes();
        aad.extend_from_slice(salt);
        aad
    }
}

fn derive_key(
    passphrase: &SecretString,
    salt: &[u8],
    m: u32,
    t: u32,
    p: u32,
) -> Result<[u8; KEY_LEN], CryptoError> {
    let params = Params::new(m, t, p, Some(KEY_LEN)).map_err(|_| CryptoError::BadFormat)?;
    let mut key = [0u8; KEY_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.expose().as_bytes(), salt, &mut key)
        .map_err(|_| CryptoError::Failed)?;
    Ok(key)
}

fn cipher(key: &[u8; KEY_LEN]) -> Result<XChaCha20Poly1305, CryptoError> {
    XChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::Failed)
}

/// Encrypts `plaintext` under a key derived from `passphrase`.
pub(crate) fn seal(
    passphrase: &SecretString,
    params: KdfParams,
    plaintext: &[u8],
) -> Result<Envelope, CryptoError> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| CryptoError::Failed)?;
    getrandom::fill(&mut nonce).map_err(|_| CryptoError::Failed)?;
    let mut env = Envelope {
        version: VERSION,
        kdf: KDF_NAME.to_owned(),
        m_cost_kib: params.m_cost_kib,
        t_cost: params.t_cost,
        p_cost: params.p_cost,
        salt: base64_encode(&salt),
        nonce: base64_encode(&nonce),
        ciphertext: String::new(),
    };
    let key = derive_key(
        passphrase,
        &salt,
        params.m_cost_kib,
        params.t_cost,
        params.p_cost,
    )?;
    let aad = env.aad(&salt);
    let xnonce = XNonce::try_from(&nonce[..]).map_err(|_| CryptoError::Failed)?;
    let ct = cipher(&key)?
        .encrypt(
            &xnonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::Failed)?;
    env.ciphertext = base64_encode(&ct);
    Ok(env)
}

/// Decrypts an envelope. Every structural problem is [`CryptoError::BadFormat`]; a tag failure is
/// [`CryptoError::WrongKeyOrDamaged`].
pub(crate) fn open(passphrase: &SecretString, env: &Envelope) -> Result<Vec<u8>, CryptoError> {
    if env.version == 0 {
        return Err(CryptoError::BadFormat);
    }
    if env.version > VERSION {
        return Err(CryptoError::TooNew(env.version));
    }
    if env.kdf != KDF_NAME {
        return Err(CryptoError::BadFormat);
    }
    // Same range check as `KdfParams::new`, so a hostile file cannot demand huge memory.
    if KdfParams::new(env.m_cost_kib, env.t_cost, env.p_cost).is_none() {
        return Err(CryptoError::BadFormat);
    }
    let salt = base64_decode(&env.salt).ok_or(CryptoError::BadFormat)?;
    let nonce = base64_decode(&env.nonce).ok_or(CryptoError::BadFormat)?;
    let ct = base64_decode(&env.ciphertext).ok_or(CryptoError::BadFormat)?;
    if salt.len() != SALT_LEN || nonce.len() != NONCE_LEN {
        return Err(CryptoError::BadFormat);
    }
    let xnonce = XNonce::try_from(&nonce[..]).map_err(|_| CryptoError::BadFormat)?;
    let key = derive_key(passphrase, &salt, env.m_cost_kib, env.t_cost, env.p_cost)?;
    let aad = env.aad(&salt);
    cipher(&key)?
        .decrypt(
            &xnonce,
            Payload {
                msg: &ct,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::WrongKeyOrDamaged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> KdfParams {
        KdfParams::new(64, 1, 1).unwrap()
    }

    fn pass(s: &str) -> SecretString {
        SecretString::new(s)
    }

    #[test]
    fn round_trip() {
        let env = seal(&pass("pw"), fast(), b"hello secrets").unwrap();
        assert_eq!(open(&pass("pw"), &env).unwrap(), b"hello secrets");
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let env = seal(&pass("pw"), fast(), b"sk-live-needle").unwrap();
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("needle"));
    }

    #[test]
    fn wrong_passphrase_fails_cleanly() {
        let env = seal(&pass("right"), fast(), b"data").unwrap();
        assert_eq!(
            open(&pass("wrong"), &env).unwrap_err(),
            CryptoError::WrongKeyOrDamaged
        );
    }

    #[test]
    fn two_seals_of_the_same_input_differ() {
        let a = seal(&pass("pw"), fast(), b"data").unwrap();
        let b = seal(&pass("pw"), fast(), b"data").unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn tampering_with_any_field_is_detected_not_trusted() {
        let good = seal(&pass("pw"), fast(), b"data").unwrap();
        let mutate = |f: &dyn Fn(&mut Envelope)| {
            let mut e: Envelope =
                serde_json::from_str(&serde_json::to_string(&good).unwrap()).unwrap();
            f(&mut e);
            open(&pass("pw"), &e)
        };
        // Cost settings are authenticated: lowering them cannot weaken the key silently.
        assert!(mutate(&|e| e.t_cost = 2).is_err());
        assert!(mutate(&|e| e.m_cost_kib = 128).is_err());
        assert!(mutate(&|e| e.salt = base64_encode(&[7u8; SALT_LEN])).is_err());
        assert!(mutate(&|e| e.nonce = base64_encode(&[7u8; NONCE_LEN])).is_err());
        // Flip one ciphertext bit.
        let flipped = {
            let mut ct = base64_decode(&good.ciphertext).unwrap();
            ct[0] ^= 1;
            base64_encode(&ct)
        };
        assert_eq!(
            mutate(&|e| e.ciphertext = flipped.clone()).unwrap_err(),
            CryptoError::WrongKeyOrDamaged
        );
    }

    #[test]
    fn hostile_headers_are_refused_before_any_work() {
        let good = seal(&pass("pw"), fast(), b"data").unwrap();
        let reparse = || -> Envelope {
            serde_json::from_str(&serde_json::to_string(&good).unwrap()).unwrap()
        };
        let mut e = reparse();
        e.m_cost_kib = u32::MAX;
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::BadFormat);
        let mut e = reparse();
        e.t_cost = u32::MAX;
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::BadFormat);
        let mut e = reparse();
        e.version = 99;
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::TooNew(99));
        let mut e = reparse();
        e.version = 0;
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::BadFormat);
        let mut e = reparse();
        e.kdf = "scrypt".to_owned();
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::BadFormat);
        let mut e = reparse();
        e.salt = "not base64!".to_owned();
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::BadFormat);
        let mut e = reparse();
        e.nonce = base64_encode(&[1, 2, 3]);
        assert_eq!(open(&pass("pw"), &e).unwrap_err(), CryptoError::BadFormat);
    }

    #[test]
    fn params_are_range_checked() {
        assert!(KdfParams::new(0, 1, 1).is_none());
        assert!(KdfParams::new(64, 0, 1).is_none());
        assert!(KdfParams::new(64, 1, 0).is_none());
        assert!(KdfParams::new(4, 1, 1).is_none());
        assert!(KdfParams::new(u32::MAX, 1, 1).is_none());
        assert!(KdfParams::new(64, 1, 1).is_some());
    }
}
