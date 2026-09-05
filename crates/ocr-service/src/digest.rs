pub(crate) fn hex_encode(value: impl AsRef<[u8]>) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let bytes = value.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        encoded.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(crate) fn sha256_digest(value: impl AsRef<[u8]>) -> String {
    format!("sha256:{}", hex_encode(value))
}
