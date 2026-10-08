//! Authorization. Ports `src/vogt/core/auth.py`.
//!
//! A token is 256 bits of randomness, so it is hashed with plain SHA-256: the
//! digest of an existing token must match one the Python core stored, or the
//! swap locks everyone out. A password is chosen by a person, so it gets
//! scrypt, in the same `scrypt$n$r$p$salt$hash` form.

#![allow(dead_code)]

use subtle::ConstantTimeEq;

pub const ALL_SCOPES: [&str; 5] = ["read", "work.write", "project.write", "admin", "writeback"];
pub const TOKEN_PREFIX: &str = "vogt_";
pub const MIN_ADOPTED_SECRET_LEN: usize = 24;

pub const PASSWORD_SCHEME: &str = "scrypt";
pub const PASSWORD_SCRYPT_N: u32 = 1 << 15;
pub const PASSWORD_SCRYPT_R: u32 = 8;
pub const PASSWORD_SCRYPT_P: u32 = 1;
const PASSWORD_HASH_BYTES: usize = 32;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 1024;

pub const USERNAME_MIN_LEN: usize = 2;
pub const USERNAME_MAX_LEN: usize = 64;

/// SHA-256 hex of the UTF-8 secret. `hashlib.sha256`.
pub fn hash_token(secret: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(secret.as_bytes()))
}

/// Constant-time comparison of a secret against a stored token hash.
pub fn matches(secret: &str, token_hash: &str) -> bool {
    constant_time_eq(hash_token(secret).as_bytes(), token_hash.as_bytes())
}

/// Take an operator-supplied secret as a credential. Under 24 characters is
/// refused, with the message `adopt` raises.
pub fn adopt(secret: &str) -> Result<String, String> {
    if secret.len() < MIN_ADOPTED_SECRET_LEN {
        return Err(format!(
            "a supplied token must be at least {MIN_ADOPTED_SECRET_LEN} characters, not {} — \
             generate one with `openssl rand -hex 32`",
            secret.len()
        ));
    }
    Ok(hash_token(secret))
}

/// A comma-separated scope list. Unknown and empty are both errors.
pub fn parse_scopes(raw: &str) -> Result<Vec<&str>, String> {
    let mut parsed = Vec::new();
    for part in raw.split(',') {
        let candidate = part.trim();
        if candidate.is_empty() {
            continue;
        }
        if !ALL_SCOPES.contains(&candidate) {
            return Err(format!(
                "unknown scope '{candidate}' (known: {})",
                ALL_SCOPES.join(", ")
            ));
        }
        parsed.push(candidate);
    }
    if parsed.is_empty() {
        return Err("a token needs at least one scope".to_string());
    }
    Ok(parsed)
}

pub fn is_expired(expires_at: Option<crate::core::Moment>, now: crate::core::Moment) -> bool {
    expires_at.is_some_and(|expires| expires <= now)
}

/// The later expiry a session token in use earns. Only a `session` whose
/// remaining life is at most half its ttl slides; anything else is unchanged.
pub fn renewed_expiry(
    kind: &str,
    expires_at: Option<crate::core::Moment>,
    now: crate::core::Moment,
    ttl_seconds: i64,
) -> Option<crate::core::Moment> {
    if kind != "session" {
        return None;
    }
    let expires = expires_at?;
    let remaining = expires.seconds_since(now);
    if remaining <= 0.0 || remaining > ttl_seconds as f64 / 2.0 {
        return None;
    }
    Some(crate::core::Moment::from_unix(
        now.unix_seconds() + ttl_seconds,
        now.nanos(),
    ))
}

/// `scrypt$n$r$p$<salt>$<hash>`, the form `hash_password` stores.
pub fn hash_password(password: &str, salt: &[u8]) -> Result<String, String> {
    check_password_length(password)?;
    let digest = scrypt_of(
        password,
        salt,
        PASSWORD_SCRYPT_N,
        PASSWORD_SCRYPT_R,
        PASSWORD_SCRYPT_P,
    )
    .map_err(|err| err.to_string())?;
    Ok(format!(
        "{PASSWORD_SCHEME}${PASSWORD_SCRYPT_N}${PASSWORD_SCRYPT_R}${PASSWORD_SCRYPT_P}${}${}$",
        b64(salt),
        b64(&digest)
    )
    .trim_end_matches('$')
    .to_string())
}

/// A malformed stored value verifies as false rather than raising.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(stored) = parse_stored(stored) else {
        return false;
    };
    let Ok(candidate) = scrypt_of(password, &stored.salt, stored.n, stored.r, stored.p) else {
        return false;
    };
    constant_time_eq(&candidate, &stored.expected)
}

struct StoredPassword {
    salt: Vec<u8>,
    expected: Vec<u8>,
    n: u32,
    r: u32,
    p: u32,
}

fn parse_stored(stored: &str) -> Result<StoredPassword, String> {
    let mut parts = stored.split('$');
    let scheme = parts.next().ok_or("short")?;
    let n: u32 = parts.next().ok_or("short")?.parse().map_err(|_| "n")?;
    let r: u32 = parts.next().ok_or("short")?.parse().map_err(|_| "r")?;
    let p: u32 = parts.next().ok_or("short")?.parse().map_err(|_| "p")?;
    let salt = unb64(parts.next().ok_or("short")?)?;
    let expected = unb64(parts.next().ok_or("short")?)?;
    if scheme != PASSWORD_SCHEME || parts.next().is_some() {
        return Err("malformed".to_string());
    }
    Ok(StoredPassword {
        salt,
        expected,
        n,
        r,
        p,
    })
}

fn check_password_length(password: &str) -> Result<(), String> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(format!(
            "a password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(format!(
            "a password must be at most {MAX_PASSWORD_LEN} characters"
        ));
    }
    Ok(())
}

fn scrypt_of(password: &str, salt: &[u8], n: u32, r: u32, p: u32) -> Result<Vec<u8>, String> {
    let params = scrypt::Params::new(n.ilog2() as u8, r, p).map_err(|err| err.to_string())?;
    let mut out = vec![0u8; PASSWORD_HASH_BYTES];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut out).map_err(|err| err.to_string())?;
    Ok(out)
}

fn b64(raw: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn unb64(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|err| err.to_string())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.ct_eq(right).into()
}

/// Lower-case letters, digits, dots, dashes and underscores, 2 to 64 long,
/// starting with a letter or digit.
pub fn normalise_username(raw: &str) -> Result<String, String> {
    let username = raw.trim().to_lowercase();
    if !(USERNAME_MIN_LEN..=USERNAME_MAX_LEN).contains(&username.len()) {
        return Err(format!(
            "a username is {USERNAME_MIN_LEN} to {USERNAME_MAX_LEN} characters, not {}",
            username.len()
        ));
    }
    let ok = username
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "._-".contains(ch));
    if !ok || "._-".contains(username.chars().next().unwrap_or(' ')) {
        return Err(
            "a username is lower-case letters, digits, '.', '-' and '_', starting with a letter \
             or digit"
                .to_string(),
        );
    }
    Ok(username)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_hashes_to_the_python_digest() {
        // hashlib.sha256 of this secret, computed by the Python core.
        assert_eq!(
            hash_token("vogt_test-secret-0123456789abcdef"),
            "daf738d90b35de22b4c68868489cfb8a3a5be6951876c7f6dab581140d56e2f9"
        );
        assert!(matches(
            "vogt_test-secret-0123456789abcdef",
            "daf738d90b35de22b4c68868489cfb8a3a5be6951876c7f6dab581140d56e2f9"
        ));
        assert!(!matches(
            "vogt_other",
            &hash_token("vogt_test-secret-0123456789abcdef")
        ));
    }

    #[test]
    fn a_password_verifies_against_a_python_stored_hash() {
        // hash_password("correct horse") from the Python core.
        let stored =
            "scrypt$32768$8$1$D6LetWsIp8LWFkbfFhFTGQ$AgKiDhUsl0ma6jQJK3pav_4a0C7pVZTBpGRxhth5aOg";
        assert!(verify_password("correct horse", stored));
        assert!(!verify_password("wrong horse", stored));
        assert!(!verify_password("correct horse", "not-a-hash"));
        assert!(!verify_password("correct horse", "bcrypt$1$2$3$aa$bb"));
    }

    #[test]
    fn adopt_refuses_a_short_secret_and_scopes_reject_the_unknown() {
        let err = adopt("changeme").unwrap_err();
        assert!(err.contains("at least 24 characters, not 8"), "{err}");
        assert!(parse_scopes("read, work.write").is_ok());
        let err = parse_scopes("read, sudo").unwrap_err();
        assert!(err.contains("unknown scope 'sudo'"), "{err}");
        assert!(parse_scopes(" , ")
            .unwrap_err()
            .contains("at least one scope"));
    }

    #[test]
    fn a_username_is_folded_and_bounded() {
        assert_eq!(normalise_username(" Ada ").unwrap(), "ada");
        assert!(normalise_username(".ada").is_err());
        assert!(normalise_username("a").is_err());
        assert!(normalise_username("ada lovelace").is_err());
    }

    #[test]
    fn only_a_session_past_half_its_life_slides() {
        let now = crate::core::from_iso("2026-01-02T00:00:00Z").unwrap();
        let soon = crate::core::Moment::from_unix(now.unix_seconds() + 1000, 0);
        let fresh = crate::core::Moment::from_unix(now.unix_seconds() + 9000, 0);
        let ttl = 3600;
        assert!(renewed_expiry("session", Some(soon), now, ttl).is_some());
        assert!(renewed_expiry("session", Some(fresh), now, ttl).is_none());
        assert!(renewed_expiry("api", Some(soon), now, ttl).is_none());
        assert!(renewed_expiry("session", None, now, ttl).is_none());
    }
}
