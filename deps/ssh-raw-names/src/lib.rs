//! Paths and file names whose bytes are not UTF-8, carried in a `str`.
//!
//! libssh and libssh2 take and return names as C strings: bytes, in
//! whatever encoding the files on the server were named in. The Rust APIs
//! over them (libssh-rs, ssh2) hand out `&str` and `Path`, so a name in GBK,
//! say, could be neither listed nor opened. Here each byte that is not part
//! of valid UTF-8 travels as one character of a private use range,
//! U+F780..=U+F7FF (U+F700 plus the byte), and becomes that byte again on
//! the way back to the library. Valid UTF-8 passes through unchanged. Both
//! vendored crates (deps/libssh-rs, deps/ssh2) use this one mapping, so a
//! name means the same whichever backend listed it.
//!
//! The price: a name that really contains characters of that range is sent
//! as the bytes they stand for, so such a file can't be reached. Private use
//! characters in file names are rare enough to accept that.

use std::borrow::Cow;

const BASE: u32 = 0xF700;

/// Whether `c` stands for a byte that is not UTF-8.
pub fn is_raw_byte(c: char) -> bool {
    (0xF780..=0xF7FF).contains(&(c as u32))
}

/// Whether `s` carries any bytes that are not UTF-8.
pub fn has_raw_bytes(s: &str) -> bool {
    s.chars().any(is_raw_byte)
}

/// The name libssh handed back, as a `str` that `to_bytes` turns back into
/// exactly these bytes.
pub fn from_bytes(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(err) => {
                let (valid, after) = rest.split_at(err.valid_up_to());
                out.push_str(std::str::from_utf8(valid).expect("checked by valid_up_to"));
                // The bytes of a broken sequence are all 0x80 and up: ASCII
                // is always valid on its own.
                let stray = err.error_len().unwrap_or(after.len());
                for &b in &after[..stray] {
                    out.push(char::from_u32(BASE + b as u32).expect("in the private use area"));
                }
                rest = &after[stray..];
            }
        }
    }
    Cow::Owned(out)
}

/// The bytes to hand libssh for a name that `from_bytes` produced, or that
/// was typed: plain UTF-8 unless it carries raw bytes.
pub fn to_bytes(s: &str) -> Cow<'_, [u8]> {
    if !has_raw_bytes(s) {
        return Cow::Borrowed(s.as_bytes());
    }
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        if is_raw_byte(c) {
            out.push((c as u32 - BASE) as u8);
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod test {
    use super::*;

    // "测试" in GBK.
    const GBK: &[u8] = b"\xb2\xe2\xca\xd4";

    #[test]
    fn utf8_passes_through_untouched() {
        assert!(matches!(
            from_bytes("目录/a.txt".as_bytes()),
            Cow::Borrowed("目录/a.txt")
        ));
        assert!(matches!(to_bytes("目录/a.txt"), Cow::Borrowed(b) if b == "目录/a.txt".as_bytes()));
    }

    #[test]
    fn other_bytes_come_back_exactly() {
        let mut path = b"/tmp/".to_vec();
        path.extend_from_slice(GBK);
        path.extend_from_slice(b"/ok.txt");
        let name = from_bytes(&path);
        assert!(has_raw_bytes(&name));
        assert!(name.starts_with("/tmp/"), "{:?}", name);
        assert!(name.ends_with("/ok.txt"), "{:?}", name);
        assert_eq!(to_bytes(&name).as_ref(), path.as_slice());
    }

    #[test]
    fn each_stray_byte_is_one_character() {
        // A lead byte cut short, then ASCII, then a lone continuation byte,
        // then a sequence cut off at the end.
        let bytes = b"a\xe6b\x80c\xe6\x88";
        let name = from_bytes(bytes);
        assert_eq!(name.chars().filter(|&c| is_raw_byte(c)).count(), 4);
        assert_eq!(
            name.chars()
                .filter(|&c| !is_raw_byte(c))
                .collect::<String>(),
            "abc"
        );
        assert_eq!(to_bytes(&name).as_ref(), &bytes[..]);
    }

    #[test]
    fn every_byte_that_can_stray_round_trips() {
        for b in 0x80..=0xffu8 {
            let bytes = [b'x', b, b'y'];
            assert_eq!(
                to_bytes(&from_bytes(&bytes)).as_ref(),
                &bytes[..],
                "byte {:#x}",
                b
            );
        }
    }
}
