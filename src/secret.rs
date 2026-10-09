//! Values Mylar stores with `encrypt_passwords` on (`mylar/encrypted.py`):
//! `^~$z$` + standard padded base64 of the UTF-8 secret followed by 8 random
//! salt bytes. It is an obfuscation, not encryption: decoding needs no key.

const PREFIX: &str = "^~$z$";
const SALT_LEN: usize = 8;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `secret` in Mylar's stored form, with a fresh salt.
pub fn encode(secret: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).expect("OS random source unavailable");
    encode_with_salt(secret, salt)
}

/// The plaintext of a stored value; `None` if it is not in Mylar's form.
pub fn decode(stored: &str) -> Option<String> {
    let mut bytes = base64_decode(stored.strip_prefix(PREFIX)?)?;
    let len = bytes.len().checked_sub(SALT_LEN)?;
    bytes.truncate(len);
    String::from_utf8(bytes).ok()
}

fn encode_with_salt(secret: &str, salt: [u8; SALT_LEN]) -> String {
    let mut bytes = secret.as_bytes().to_vec();
    bytes.extend_from_slice(&salt);
    format!("{PREFIX}{}", base64_encode(&bytes))
}

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Strict: Python's `b64decode` rejects bad padding but skips stray characters;
/// rejecting those too only matters for values Mylar never writes.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(4) {
        return None;
    }
    let body = s
        .strip_suffix("==")
        .or_else(|| s.strip_suffix('='))
        .unwrap_or(s);
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in body.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_resalts() {
        for s in [
            "",
            "hunter2",
            "pä$$ wörd=/+",
            "a much longer api key 0123456789",
        ] {
            let stored = encode(s);
            assert!(stored.starts_with(PREFIX));
            assert_eq!(decode(&stored).as_deref(), Some(s));
        }
        assert_ne!(encode("hunter2"), encode("hunter2"));
    }

    #[test]
    fn decodes_a_vector_built_by_upstream_algorithm() {
        // Python: base64.b64encode('hunter2é'.encode('utf-8') + bytes(range(1, 9)))
        let stored = "^~$z$aHVudGVyMsOpAQIDBAUGBwg=";
        assert_eq!(decode(stored).as_deref(), Some("hunter2é"));
        assert_eq!(
            encode_with_salt("hunter2é", [1, 2, 3, 4, 5, 6, 7, 8]),
            stored
        );
    }

    #[test]
    fn a_value_without_the_prefix_is_none() {
        assert_eq!(decode("aHVudGVyMsOpAQIDBAUGBwg="), None);
        assert_eq!(decode("hunter2"), None);
    }

    #[test]
    fn bad_base64_is_none() {
        assert_eq!(decode("^~$z$not base64!"), None);
        assert_eq!(decode("^~$z$aHVudGVyMsOpAQIDBAUGBwg"), None);
        // Valid base64 shorter than the salt.
        assert_eq!(decode("^~$z$AAAA"), None);
    }
}
