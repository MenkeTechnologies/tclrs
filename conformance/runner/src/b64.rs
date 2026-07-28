//! Base64, because every field that crosses the Tcl/Rust boundary is arbitrary
//! bytes — test bodies contain tabs, newlines, NULs and lone surrogates — and a
//! line-oriented format needs them to survive intact.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        let digits = [n >> 18, n >> 12 & 63, n >> 6 & 63, n & 63];
        for (i, d) in digits.iter().enumerate() {
            out.push(if i <= chunk.len() {
                ALPHABET[*d as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or_else(|| format!("not base64: byte {c:#04x}"))?;
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_length_and_byte() {
        for len in 0..=64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let text = encode(&bytes);
            assert_eq!(decode(&text).expect("decodes"), bytes, "length {len}");
            assert_eq!(text.len() % 4, 0, "length {len} is not padded");
        }
    }

    #[test]
    fn matches_known_vectors() {
        // RFC 4648 section 10.
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(decode("Zm9vYmFy").expect("decodes"), b"foobar");
    }
}
