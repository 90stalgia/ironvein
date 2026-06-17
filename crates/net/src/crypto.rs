//! crypto.rs — settler identity: one secp256k1 keypair per player.
//!
//! The same key signs lockstep frames (BIP-340 Schnorr over a tagged hash)
//! and, later, Nostr signaling events — Nostr uses exactly this curve and
//! signature scheme, which is why it was chosen. The sim never sees any of
//! this: to `crates/sim` a key is 32 opaque bytes on a Player.
//!
//! No `getrandom` dependency on purpose: its wasm path assumes
//! wasm-bindgen glue that the miniquad runtime doesn't have. Entropy is
//! /dev/urandom natively and an `ivn_random` JS import in the browser.

use k256::schnorr::signature::hazmat::PrehashVerifier;
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// 32-byte x-only public key — a settler's identity, a Nostr pubkey.
pub type PubKey = [u8; 32];

pub const ZERO_KEY: PubKey = [0u8; 32];

// ----------------------------------------------------------------------
// Entropy & wall time (platform services, shimmed on wasm)
// ----------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
extern "C" {
    fn ivn_random(ptr: *mut u8, len: usize);
    fn ivn_now_ms() -> f64;
}

/// Fill `buf` with cryptographically strong random bytes.
#[cfg(not(target_arch = "wasm32"))]
pub fn fill_random(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // Last-resort fallback (no /dev/urandom): stretch OS hasher seeds +
    // time through SHA-256. Documented as weaker; only reachable on
    // platforms without urandom.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut pool = Sha256::new();
    for i in 0..8u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        pool.update(h.finish().to_le_bytes());
    }
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        pool.update(d.as_nanos().to_le_bytes());
    }
    let mut out = pool.finalize();
    for chunk in buf.chunks_mut(32) {
        chunk.copy_from_slice(&out[..chunk.len()]);
        out = Sha256::digest(out);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn fill_random(buf: &mut [u8]) {
    unsafe { ivn_random(buf.as_mut_ptr(), buf.len()) }
}

// Route k256/rand_core's `getrandom` through the same JS entropy source, so
// the curve library works on wasm without wasm-bindgen.
#[cfg(target_arch = "wasm32")]
fn ivn_getrandom(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    fill_random(buf);
    Ok(())
}
#[cfg(target_arch = "wasm32")]
getrandom::register_custom_getrandom!(ivn_getrandom);

/// Milliseconds since the Unix epoch (sequence-number baseline, Nostr
/// created_at). Wall time never feeds the sim.
#[cfg(not(target_arch = "wasm32"))]
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
pub fn unix_millis() -> u64 {
    unsafe { ivn_now_ms() as u64 }
}

// ----------------------------------------------------------------------
// Hashing
// ----------------------------------------------------------------------

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// BIP-340 style domain-separated hash: sha256(sha256(tag) ‖ sha256(tag) ‖ data).
pub fn tagged_hash(tag: &str, parts: &[&[u8]]) -> [u8; 32] {
    let t = Sha256::digest(tag.as_bytes());
    let mut h = Sha256::new();
    h.update(t);
    h.update(t);
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

// ----------------------------------------------------------------------
// Identity
// ----------------------------------------------------------------------

#[derive(Clone)]
pub struct Identity {
    sk: SigningKey,
    pub pk: PubKey,
}

impl Identity {
    pub fn generate() -> Identity {
        loop {
            let mut secret = [0u8; 32];
            fill_random(&mut secret);
            if let Some(id) = Identity::from_secret(&secret) {
                return id;
            }
            // out-of-range secret (probability ~2^-128): roll again
        }
    }

    pub fn from_secret(secret: &[u8; 32]) -> Option<Identity> {
        let sk = SigningKey::from_bytes(secret).ok()?;
        let pk: PubKey = sk.verifying_key().to_bytes().into();
        Some(Identity { sk, pk })
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.sk.to_bytes().into()
    }

    /// BIP-340 Schnorr over a 32-byte digest.
    pub fn sign(&self, digest: &[u8; 32]) -> [u8; 64] {
        let mut aux = [0u8; 32];
        fill_random(&mut aux);
        let sig = self
            .sk
            .sign_raw(digest, &aux)
            .expect("schnorr signing is infallible for valid keys");
        sig.to_bytes()
    }

    /// Load a persisted identity, or mint one and persist it. A settler's
    /// base can only be reclaimed by this key, so it must survive restarts.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_or_create(path: &std::path::Path) -> Identity {
        if let Ok(bytes) = std::fs::read(path) {
            if bytes.len() == 32 {
                let mut secret = [0u8; 32];
                secret.copy_from_slice(&bytes);
                if let Some(id) = Identity::from_secret(&secret) {
                    return id;
                }
            }
        }
        let id = Identity::generate();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, id.secret_bytes());
        id
    }
}

/// Verify a BIP-340 signature over a 32-byte digest against an x-only key.
pub fn verify(pk: &PubKey, digest: &[u8; 32], sig: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(pk) else { return false };
    let Ok(sig) = Signature::try_from(sig.as_slice()) else { return false };
    vk.verify_prehash(digest, &sig).is_ok()
}

// ----------------------------------------------------------------------
// ECDH + AEAD — encrypted Nostr signaling (kind 29000)
// ----------------------------------------------------------------------

impl Identity {
    /// Shared secret with another x-only key, à la NIP-04: the SHA-256 of
    /// the x-coordinate of the ECDH point. An x-only pubkey implies even y
    /// (BIP-340), so we reconstruct the compressed point with a 0x02 prefix.
    #[allow(deprecated)]
    pub fn shared_key(&self, their_xonly: &PubKey) -> Option<[u8; 32]> {
        use k256::ecdh::diffie_hellman;
        let sk = k256::SecretKey::from_bytes((&self.secret_bytes()).into()).ok()?;
        let mut sec1 = [0u8; 33];
        sec1[0] = 0x02;
        sec1[1..].copy_from_slice(their_xonly);
        let pk = k256::PublicKey::from_sec1_bytes(&sec1).ok()?;
        let shared = diffie_hellman(sk.to_nonzero_scalar(), pk.as_affine());
        let raw = shared.raw_secret_bytes();
        Some(sha256(&[&raw[..]]))
    }

    /// Encrypt `plaintext` to `their_xonly`. Output is `nonce(12) || ct`.
    pub fn encrypt_to(&self, their_xonly: &PubKey, plaintext: &[u8]) -> Option<Vec<u8>> {
        let key = self.shared_key(their_xonly)?;
        Some(aead_seal(&key, plaintext))
    }

    /// Decrypt a `nonce(12) || ct` blob from `their_xonly`. ECDH is
    /// symmetric, so the same shared key opens what the peer sealed.
    pub fn decrypt_from(&self, their_xonly: &PubKey, blob: &[u8]) -> Option<Vec<u8>> {
        let key = self.shared_key(their_xonly)?;
        aead_open(&key, blob)
    }
}

// The chacha20poly1305 0.10 line re-exports generic-array 0.14 types, which
// emit upstream deprecation noise we can't fix without bumping the crate.
#[allow(deprecated)]
fn aead_seal(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; 12];
    fill_random(&mut nonce);
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).expect("aead encrypt");
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

#[allow(deprecated)]
fn aead_open(key: &[u8; 32], blob: &[u8]) -> Option<Vec<u8>> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    if blob.len() < 12 {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher.decrypt(Nonce::from_slice(&blob[..12]), &blob[12..]).ok()
}

// ----------------------------------------------------------------------
// Hex (pubkeys travel as text in addresses and Nostr events)
// ----------------------------------------------------------------------

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 15) as u32, 16).unwrap());
    }
    s
}

pub fn hex_decode32(s: &str) -> Option<PubKey> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let id = Identity::generate();
        let digest = sha256(&[b"the world is a save file"]);
        let sig = id.sign(&digest);
        assert!(verify(&id.pk, &digest, &sig));
        // tampered digest fails
        let bad = sha256(&[b"the world is a lie"]);
        assert!(!verify(&id.pk, &bad, &sig));
        // tampered sig fails
        let mut sig2 = sig;
        sig2[10] ^= 1;
        assert!(!verify(&id.pk, &digest, &sig2));
        // wrong key fails
        let other = Identity::generate();
        assert!(!verify(&other.pk, &digest, &sig));
    }

    #[test]
    fn identity_is_stable_from_secret() {
        let id = Identity::generate();
        let again = Identity::from_secret(&id.secret_bytes()).unwrap();
        assert_eq!(id.pk, again.pk);
    }

    #[test]
    fn hex_roundtrip() {
        let id = Identity::generate();
        assert_eq!(hex_decode32(&hex_encode(&id.pk)), Some(id.pk));
    }

    #[test]
    fn ecdh_is_symmetric() {
        let a = Identity::generate();
        let b = Identity::generate();
        // both sides derive the same shared key
        assert_eq!(a.shared_key(&b.pk), b.shared_key(&a.pk));
        assert!(a.shared_key(&b.pk).is_some());
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let a = Identity::generate();
        let b = Identity::generate();
        let msg = b"v=0\r\no=- 42 2 IN IP4 127.0.0.1\r\n(an SDP offer)";
        let blob = a.encrypt_to(&b.pk, msg).unwrap();
        // b opens what a sealed
        assert_eq!(b.decrypt_from(&a.pk, &blob).unwrap(), msg);
        // a third party cannot
        let eve = Identity::generate();
        assert!(eve.decrypt_from(&a.pk, &blob).is_none());
        // tampered ciphertext fails the AEAD tag
        let mut bad = blob.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(b.decrypt_from(&a.pk, &bad).is_none());
    }
}
