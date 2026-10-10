//! SHA-256, streaming — the content hash behind the protected-path snapshot.
//!
//! It must be a CRYPTOGRAPHIC hash, not `DefaultHasher`: the worker chooses the bytes it writes, and
//! a hash it can collide is a gate it can walk through. The implementation is RustCrypto's `sha2`,
//! not a hand-rolled one: a security check is the wrong place for a home-made primitive. The test
//! vectors below are the NIST ones.

use sha2::Digest;

/// An incremental SHA-256 hasher (a thin wrapper, so callers never name the crate).
#[derive(Default)]
pub struct Sha256(sha2::Sha256);

impl Sha256 {
    /// Feed more bytes.
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    /// The lowercase hex digest of everything fed so far.
    pub fn hex(self) -> String {
        self.0.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The hex digest of a whole byte slice.
pub fn hex(data: &[u8]) -> String {
    let mut s = Sha256::default();
    s.update(data);
    s.hex()
}

/// The hex digest of a file's contents, streamed in 64 KiB chunks so a large golden file is never
/// held in memory whole.
pub fn file_hex(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut s = Sha256::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(s.hex());
        }
        s.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The NIST FIPS 180-4 example vectors, plus a multi-block input fed in awkward chunk sizes so
    /// the buffering across `update` calls is exercised, not just the one-shot path.
    #[test]
    fn matches_the_nist_vectors() {
        assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        let mut s = Sha256::default();
        for chunk in million_a.chunks(997) {
            s.update(chunk);
        }
        assert_eq!(s.hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn file_hex_agrees_with_the_in_memory_digest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(file_hex(&p).unwrap(), hex(&data));
    }
}
