//! Id minting and slugify. Ports the id helpers of `src/vogt/core/`.

const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const TIME_CHARS: usize = 10;
const RANDOM_CHARS: usize = 16;

pub fn new_ulid(timestamp_ms: u64, randomness: u128) -> String {
    let mut out = encode(timestamp_ms, TIME_CHARS);
    // Randomness is 80 bits. Truncate so a wider integer cannot leak into the
    // time half.
    out.push_str(&encode(randomness & ((1_u128 << 80) - 1), RANDOM_CHARS));
    out
}

fn encode(value: impl Into<u128>, length: usize) -> String {
    let mut value = value.into();
    let mut chars = vec![0_u8; length];
    for slot in chars.iter_mut().rev() {
        *slot = ALPHABET[(value & 0x1F) as usize];
        value >>= 5;
    }
    String::from_utf8(chars).expect("alphabet is ascii")
}

/// `{prefix}_{26 chars}`. The caller supplies the millisecond clock and 80
/// bits of randomness; Python's `new_id` reads both from the process.
pub fn new_id(prefix: &str, timestamp_ms: u64, randomness: u128) -> String {
    format!("{prefix}_{}", new_ulid(timestamp_ms, randomness))
}

/// A fresh id from the process clock and randomness. This is what `init` uses
/// when `VOGT_TEST_IDS` is unset, so two plain inits never share an instance
/// id. The hook is the only path to a deterministic one.
pub fn fresh_id(prefix: &str) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    fresh_id_at(prefix, millis, fill_entropy)
}

/// 80 bits from the OS. A failure is fatal: the old `/dev/urandom` read left the
/// buffer zeroed, which minted a timestamp-only id anyone can guess and that
/// collides inside one millisecond.
fn fill_entropy(bytes: &mut [u8]) -> Result<(), getrandom::Error> {
    getrandom::getrandom(bytes)
}

pub(super) fn fresh_id_at(
    prefix: &str,
    millis: u64,
    fill: fn(&mut [u8]) -> Result<(), getrandom::Error>,
) -> String {
    let mut bytes = [0u8; 10];
    fill(&mut bytes).unwrap_or_else(|err| {
        panic!("the operating system refused to supply random bytes for a fresh id: {err}")
    });
    let mut randomness = 0u128;
    for byte in bytes {
        randomness = (randomness << 8) | u128::from(byte);
    }
    new_id(prefix, millis, randomness)
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut previous_was_sep = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            previous_was_sep = false;
        } else if !previous_was_sep {
            out.push('-');
            previous_was_sep = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::TIME_CHARS;

    #[test]
    fn fresh_id_uses_entropy_and_never_a_zero_random_half() {
        let first = fresh_id("tst");
        let second = fresh_id("tst");
        assert_ne!(first, second, "two fresh ids collided");
        for id in [&first, &second] {
            let body = id.strip_prefix("tst_").expect("prefix");
            assert_eq!(body.len(), 26);
            assert_ne!(
                &body[TIME_CHARS..],
                "0000000000000000",
                "random half is all zeros, which is the old /dev/urandom fallback"
            );
        }
    }

    #[test]
    #[should_panic(expected = "refused to supply random bytes")]
    fn fresh_id_panics_when_entropy_is_unavailable() {
        fn refuse(_: &mut [u8]) -> Result<(), getrandom::Error> {
            Err(getrandom::Error::UNSUPPORTED)
        }
        let _ = fresh_id_at("tst", 1_700_000_000_000, refuse);
    }

    #[test]
    fn ulids_match_python_and_sort() {
        let earlier = new_ulid(1_700_000_000_000, 0);
        let later = new_ulid(1_700_000_001_000, 0);
        assert_eq!(earlier, "01HF7YAT000000000000000000");
        assert_eq!(later, "01HF7YATZ80000000000000000");
        assert!(earlier < later);
    }

    #[test]
    fn ids_carry_their_prefix() {
        let generated = new_id("prj", 1_700_000_000_000, 0);
        assert!(generated.starts_with("prj_"));
        assert_eq!(generated.len(), "prj_".len() + 26);
    }

    #[test]
    fn slugify_matches_the_table() {
        for (name, expected) in [
            ("Vogt", "vogt"),
            ("  Rust NZB  ", "rust-nzb"),
            ("nzb-core", "nzb-core"),
            ("A//B", "a-b"),
            ("///", ""),
            ("Region 2 (west)", "region-2-west"),
        ] {
            assert_eq!(slugify(name), expected, "{name}");
        }
    }
}
