use blake2::{Blake2b, Blake2s256};
use digest::consts::U16;
use digest::Digest;
use md5::Md5;
use sha2::{Sha256, Sha512};

use crate::db::models::EtagType;

type Blake2b128 = Blake2b<U16>;

/// Streaming multi-algorithm hasher used during object ingestion.
pub struct StreamingHasher {
    etag_type: EtagType,
    inner: HasherInner,
}

enum HasherInner {
    Md5(Md5),
    Sha256(Sha256),
    Sha512(Sha512),
    Blake2_128(Blake2b128),
    Blake2_256(Blake2s256),
    Blake3(blake3::Hasher),
}

impl StreamingHasher {
    pub fn new(etag_type: EtagType) -> Self {
        let inner = match etag_type {
            EtagType::Md5 => HasherInner::Md5(Md5::new()),
            EtagType::Sha256 => HasherInner::Sha256(Sha256::new()),
            EtagType::Sha512 => HasherInner::Sha512(Sha512::new()),
            EtagType::Blake2_128 => HasherInner::Blake2_128(Blake2b128::new()),
            EtagType::Blake2_256 => HasherInner::Blake2_256(Blake2s256::new()),
            EtagType::Blake3_128 | EtagType::Blake3_256 => {
                HasherInner::Blake3(blake3::Hasher::new())
            }
        };
        Self { etag_type, inner }
    }

    pub fn update(&mut self, data: &[u8]) {
        match &mut self.inner {
            HasherInner::Md5(h) => Digest::update(h, data),
            HasherInner::Sha256(h) => Digest::update(h, data),
            HasherInner::Sha512(h) => Digest::update(h, data),
            HasherInner::Blake2_128(h) => Digest::update(h, data),
            HasherInner::Blake2_256(h) => Digest::update(h, data),
            HasherInner::Blake3(h) => {
                h.update(data);
            }
        }
    }

    pub fn finalize(self) -> String {
        match (self.etag_type, self.inner) {
            (EtagType::Md5, HasherInner::Md5(h)) => hex::encode(h.finalize()),
            (EtagType::Sha256, HasherInner::Sha256(h)) => hex::encode(h.finalize()),
            (EtagType::Sha512, HasherInner::Sha512(h)) => hex::encode(h.finalize()),
            (EtagType::Blake2_128, HasherInner::Blake2_128(h)) => hex::encode(h.finalize()),
            (EtagType::Blake2_256, HasherInner::Blake2_256(h)) => hex::encode(h.finalize()),
            (EtagType::Blake3_128, HasherInner::Blake3(h)) => {
                let mut out = [0u8; 16];
                h.finalize_xof().fill(&mut out);
                hex::encode(out)
            }
            (EtagType::Blake3_256, HasherInner::Blake3(h)) => {
                hex::encode(h.finalize().as_bytes())
            }
            _ => unreachable!("hasher/etag_type mismatch"),
        }
    }

    pub fn digest_hex(etag_type: EtagType, data: &[u8]) -> String {
        let mut h = Self::new(etag_type);
        h.update(data);
        h.finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_produces_32_hex_chars() {
        let hex = StreamingHasher::digest_hex(EtagType::Md5, b"hello world");
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hex, "5eb63bbbe01eeed093cb22bb8f5acdc3");
    }

    #[test]
    fn blake3_128_produces_32_hex_chars() {
        let hex = StreamingHasher::digest_hex(EtagType::Blake3_128, b"hello world");
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hex.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn sha256_produces_64_hex_chars() {
        let hex = StreamingHasher::digest_hex(EtagType::Sha256, b"hello world");
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn sha512_produces_128_hex_chars() {
        let hex = StreamingHasher::digest_hex(EtagType::Sha512, b"hello world");
        assert_eq!(hex.len(), 128);
    }
}
