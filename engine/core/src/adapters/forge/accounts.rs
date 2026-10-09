//! Encrypt per-actor forge PATs at rest. Ports `adapters/forge/accounts.py`.
//!
//! A linked Personal Access Token is the opposite of the vogt-issued tokens:
//! those are stored as a hash because a lost one is rotated, never looked up.
//! A forge PAT has to be handed back to the upstream API on the actor's
//! behalf, so it must be recoverable — encrypted, and never stored in
//! plaintext.
//!
//! The key lives in a file named by `forge_account_key_file`. If that is unset,
//! unreadable or malformed the feature is *off*: [`load_cipher`] returns the
//! typed `ForgeAccountsNotConfigured`, which the link operation turns into an
//! honest "linking is not configured" answer rather than pretending to store a
//! secret it has nowhere safe to put. Every failure to obtain a usable key is
//! that one answer, so a missing path, a missing file and a malformed key do
//! not become three shades of the same "no".
//!
//! The ciphertext is Fernet, byte-for-byte what the Python `cryptography`
//! package produces: version `0x80`, an 8-byte big-endian timestamp, a 16-byte
//! IV, AES-128-CBC with PKCS7 padding, then an HMAC-SHA256 over all of that,
//! urlsafe-base64 encoded. The signing key is the first half of the decoded
//! key and the encryption key the second.

use std::path::PathBuf;

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::engine::general_purpose::URL_SAFE;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::config::VogtConfig;
use crate::errors::VogtError;

const VERSION: u8 = 0x80;
const TIMESTAMP_LEN: usize = 8;
const IV_LEN: usize = 16;
const HMAC_LEN: usize = 32;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = 1 + TIMESTAMP_LEN + IV_LEN;

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type HmacSha256 = Hmac<Sha256>;

/// Fernet encrypt/decrypt for one instance's account-linking key.
///
/// The key halves are not part of the debug output: a cipher that prints its
/// own key into a log line has stored the secret in the place it was built to
/// keep secrets out of.
pub struct ForgeAccountCipher {
    signing_key: [u8; 16],
    encryption_key: [u8; 16],
}

impl std::fmt::Debug for ForgeAccountCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ForgeAccountCipher([redacted])")
    }
}

impl ForgeAccountCipher {
    fn new(key: &[u8]) -> Result<Self, VogtError> {
        if key.len() != KEY_LEN {
            return Err(not_configured(
                "forge account linking is not configured: the key file does not \
                 hold a valid urlsafe-base64 Fernet key",
            ));
        }
        let mut signing_key = [0u8; 16];
        let mut encryption_key = [0u8; 16];
        signing_key.copy_from_slice(&key[..16]);
        encryption_key.copy_from_slice(&key[16..]);
        Ok(Self {
            signing_key,
            encryption_key,
        })
    }

    /// The PAT as Fernet ciphertext — the only form that is ever stored.
    ///
    /// `now` is the unix timestamp the token is stamped with. Passing it in
    /// keeps the ciphertext deterministic in tests; a caller with a clock
    /// passes the real one.
    pub fn encrypt(&self, plaintext: &str, now: u64) -> String {
        let iv: [u8; IV_LEN] = random_iv();
        self.encrypt_with(plaintext, now, &iv)
    }

    fn encrypt_with(&self, plaintext: &str, now: u64, iv: &[u8; IV_LEN]) -> String {
        let mut buffer = vec![0u8; plaintext.len() + 16];
        buffer[..plaintext.len()].copy_from_slice(plaintext.as_bytes());
        let ciphertext = Aes128CbcEnc::new(&self.encryption_key.into(), iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len())
            .expect("PKCS7 padding always fits the buffer");
        let mut token = Vec::with_capacity(HEADER_LEN + ciphertext.len() + HMAC_LEN);
        token.push(VERSION);
        token.extend_from_slice(&now.to_be_bytes());
        token.extend_from_slice(iv);
        token.extend_from_slice(ciphertext);
        token.extend_from_slice(&self.mac(&token));
        // Padding kept, as Python's `urlsafe_b64encode` keeps it.
        URL_SAFE.encode(token)
    }

    /// Recover the PAT to call upstream. A key that cannot open it is the same
    /// typed refusal as a missing key: the stored token is unusable.
    pub fn decrypt(&self, ciphertext: &str) -> Result<String, VogtError> {
        let token = decode_b64(ciphertext.as_bytes()).map_err(|_| bad_token())?;
        if token.len() < HEADER_LEN + HMAC_LEN || token[0] != VERSION {
            return Err(bad_token());
        }
        let (signed, presented) = token.split_at(token.len() - HMAC_LEN);
        // Constant time, as `HMAC.verify` is: a byte-by-byte compare would
        // leak how much of a forged tag matched.
        if self.mac(signed).ct_eq(presented).unwrap_u8() != 1 {
            return Err(bad_token());
        }
        let iv: [u8; IV_LEN] = signed[1 + TIMESTAMP_LEN..HEADER_LEN].try_into().unwrap();
        let mut buffer = signed[HEADER_LEN..].to_vec();
        let plain = Aes128CbcDec::new(&self.encryption_key.into(), &iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buffer)
            .map_err(|_| bad_token())?;
        String::from_utf8(plain.to_vec()).map_err(|_| bad_token())
    }

    fn mac(&self, data: &[u8]) -> [u8; HMAC_LEN] {
        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC accepts a 16-byte key");
        mac.update(data);
        mac.finalize().into_bytes().into()
    }
}

/// Build the cipher, or refuse honestly when linking is not configured.
pub fn load_cipher(config: &VogtConfig) -> Result<ForgeAccountCipher, VogtError> {
    let Some(path) = &config.forge_account_key_file else {
        return Err(not_configured(
            "forge account linking is not configured (no key file); set \
             forge_account_key_file to a file holding a Fernet key to enable it",
        ));
    };
    let resolved = expand_user(path);
    if !resolved.is_file() {
        return Err(not_configured(format!(
            "forge account linking is not configured: the key file {} does not \
             exist, so there is nowhere safe to store a token",
            resolved.display()
        )));
    }
    let raw = std::fs::read(&resolved).map_err(|_| {
        not_configured(format!(
            "forge account linking is not configured: the key file {} could not \
             be read, so there is nowhere safe to store a token",
            resolved.display()
        ))
    })?;
    let key = decode_b64(trim_ascii(&raw)).map_err(|_| malformed())?;
    ForgeAccountCipher::new(&key)
}

/// Whether a usable key is present, without raising.
///
/// The write path asks this before it looks for a linked PAT: with no key
/// there is nothing to decrypt, so the file-token fallback is the whole answer
/// and no read of the accounts table is needed.
pub fn account_linking_enabled(config: &VogtConfig) -> bool {
    load_cipher(config).is_ok()
}

fn not_configured(message: impl Into<String>) -> VogtError {
    VogtError::ForgeAccountsNotConfigured(message.into())
}

fn malformed() -> VogtError {
    not_configured(
        "forge account linking is not configured: the key file does not hold a \
         valid urlsafe-base64 Fernet key",
    )
}

fn bad_token() -> VogtError {
    not_configured(
        "the stored forge token could not be decrypted with the configured key; \
         the key may have been rotated or replaced",
    )
}

/// `base64.urlsafe_b64decode`, which is what `cryptography` decodes a Fernet
/// key and token with.
///
/// That is wider than the urlsafe alphabet: the standard alphabet (`+`, `/`)
/// is accepted, so a key from `openssl rand -base64 32` works, and characters
/// outside either alphabet are skipped rather than rejected. It is also
/// stricter in one direction: the padding must already be correct, because
/// adding a missing `=` would accept a 43-character key that Python refuses,
/// and a rollback to Python would then find every stored token unreadable.
fn decode_b64(bytes: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
    let mut cleaned = Vec::with_capacity(bytes.len());
    for byte in bytes {
        match byte {
            b'+' => cleaned.push(b'-'),
            b'/' => cleaned.push(b'_'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'=' => cleaned.push(*byte),
            _ => {}
        }
    }
    if !cleaned.len().is_multiple_of(4) {
        return Err(base64::DecodeError::InvalidPadding);
    }
    URL_SAFE.decode(cleaned)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    &bytes[start..end]
}

fn expand_user(path: &std::path::Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix("~/") else {
        return path.to_path_buf();
    };
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(rest))
        .unwrap_or_else(|| path.to_path_buf())
}

/// 16 bytes from the system generator. A cipher without an IV source cannot
/// encrypt, so this reads `/dev/urandom` directly rather than pulling a random
/// crate in for one call.
fn random_iv() -> [u8; IV_LEN] {
    use std::io::Read;
    let mut iv = [0u8; IV_LEN];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut iv))
        .expect("the system random source is readable");
    iv
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed key and IV, so the ciphertext is comparable byte for byte with
    /// one the Python implementation produced from the same inputs.
    const KEY: &str = "MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=";
    const IV: [u8; IV_LEN] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ];

    fn cipher() -> ForgeAccountCipher {
        ForgeAccountCipher::new(&decode_b64(KEY.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn a_standard_alphabet_key_works_and_an_unpadded_one_does_not() {
        // `openssl rand -base64 32` emits `+` and `/`. Python accepts it.
        let standard = KEY.replace('-', "+").replace('_', "/");
        assert!(ForgeAccountCipher::new(&decode_b64(standard.as_bytes()).unwrap()).is_ok());
        // A space inside the key is skipped, as urlsafe_b64decode skips it.
        let spaced = format!("{} {}", &KEY[..20], &KEY[20..]);
        assert_eq!(decode_b64(spaced.as_bytes()).unwrap().len(), KEY_LEN);
        // An unpadded 43-character key is what Python refuses, so Rust must
        // too: accepting it would make a rollback unable to read the tokens.
        let unpadded = KEY.trim_end_matches('=');
        assert!(decode_b64(unpadded.as_bytes()).is_err());
    }

    #[test]
    fn the_debug_output_does_not_carry_the_key() {
        let rendered = format!("{:?}", cipher());
        assert!(!rendered.contains("0000"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn a_token_round_trips_and_matches_the_fernet_layout() {
        let cipher = cipher();
        let token = cipher.encrypt_with("ghp_secret", 1_700_000_000, &IV);
        assert_eq!(cipher.decrypt(&token).unwrap(), "ghp_secret");
        let raw = URL_SAFE.decode(&token).unwrap();
        assert_eq!(raw[0], VERSION);
        assert_eq!(&raw[1..9], &1_700_000_000u64.to_be_bytes());
        assert_eq!(&raw[9..25], &IV);
    }

    #[test]
    fn a_token_sealed_by_another_key_is_refused() {
        let other = ForgeAccountCipher::new(&[7u8; KEY_LEN]).unwrap();
        let token = cipher().encrypt("ghp_secret", 1);
        let error = other.decrypt(&token).unwrap_err();
        assert!(matches!(error, VogtError::ForgeAccountsNotConfigured(_)));
    }

    #[test]
    fn every_way_to_lack_a_key_is_the_same_refusal() {
        let mut config = VogtConfig::default();
        assert!(!account_linking_enabled(&config));
        assert!(matches!(
            load_cipher(&config),
            Err(VogtError::ForgeAccountsNotConfigured(_))
        ));

        config.forge_account_key_file = Some(PathBuf::from("/no/such/key"));
        assert!(load_cipher(&config).is_err());

        let dir = std::env::temp_dir().join(format!("vogt-forge-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("key");
        std::fs::write(&file, "not a key\n").unwrap();
        config.forge_account_key_file = Some(file.clone());
        assert!(load_cipher(&config)
            .unwrap_err()
            .message()
            .contains("urlsafe-base64"));

        std::fs::write(&file, format!("{KEY}\n")).unwrap();
        assert!(account_linking_enabled(&config));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
