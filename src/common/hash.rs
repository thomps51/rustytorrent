use sha1::{Digest, Sha1};

pub const SHA1_HASH_LENGTH: usize = 20;
pub type Sha1Hash = [u8; SHA1_HASH_LENGTH];

pub fn hash_to_uri_str(data: &[u8]) -> String {
    // let mut hasher = Sha1::new();
    // hasher.update(data);
    // let hash = hasher.finalize();
    // let mut result = String::new();
    // for byte in hash.as_slice() {
    //     let c = *byte as char;
    //     if c.is_ascii_alphanumeric() {
    //         result.push(c);
    //     } else {
    //         let hex = format!("%{:02X}", byte);
    //         result.push_str(&hex);
    //     }
    // }
    // result
    bytes_to_uri(&hash_to_bytes(data))
}

pub fn hash_to_bytes(data: &[u8]) -> Sha1Hash {
    let mut hasher = Sha1::new();
    hasher.update(data);
    let hash = hasher.finalize();
    hash.into()
}

pub fn bytes_to_uri(data: &[u8]) -> String {
    let mut result = String::new();
    for byte in data {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() {
            result.push(c);
        } else {
            let hex = format!("%{:02X}", byte);
            result.push_str(&hex);
        }
    }
    result
}

pub fn is_valid_piece(piece: &[u8], index: usize, piece_hashes: &Vec<Sha1Hash>) -> bool {
    let actual = hash_to_bytes(piece);
    let expected = piece_hashes[index];
    actual == expected
}
