//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! SCRAM-SHA-256 server-side authentication (RFC 5802 / RFC 7677).
//!
//! PostgreSQL 10+ clients negotiate SCRAM-SHA-256 during startup. This module
//! implements the server half of the exchange using only pure-Rust primitives
//! (SHA-256, HMAC-SHA-256, PBKDF2-HMAC-SHA-256 and Base64), consistent with the
//! rest of the codebase which hand-rolls its cryptographic helpers and does not
//! depend on external crypto crates.
//!
//! # Wire flow
//!
//! ```text
//! server: AuthenticationSASL (mechanism list)
//! client: SASLInitialResponse (mechanism + "n,,n=user,r=clientnonce")
//! server: AuthenticationSASLContinue ("r=...,s=...,i=...")
//! client: SASLResponse ("c=...,r=...,p=...")
//! server: AuthenticationSASLFinal ("v=...") then AuthenticationOk
//! ```
//!
//! No plaintext password is ever written to the wire or to logs.

/// SHA-256 round constants.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 initial hash state.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Computes the SHA-256 digest of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let padded_len = (data.len() + 9).div_ceil(64) * 64;
    if padded_len <= 256 {
        let mut message = [0u8; 256];
        message[..data.len()].copy_from_slice(data);
        message[data.len()] = 0x80;
        message[padded_len - 8..padded_len]
            .copy_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        return sha256_blocks(&message[..padded_len]);
    }
    let mut message = Vec::with_capacity(padded_len);
    message.extend_from_slice(data);
    message.push(0x80);
    message.resize(padded_len - 8, 0);
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    sha256_blocks(&message)
}

fn sha256_blocks(message: &[u8]) -> [u8; 32] {
    let mut h = H0;
    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().expect("64-byte chunk"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// HMAC-SHA-256 (RFC 2104) over `data` with `key`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use plomid_core::SCRAM_HMAC_BLOCK as BLOCK;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = sha256(key);
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    if data.len() <= 64 {
        let mut inner = [0u8; 128];
        inner[..BLOCK].copy_from_slice(&ipad);
        inner[BLOCK..BLOCK + data.len()].copy_from_slice(data);
        let inner_hash = sha256(&inner[..BLOCK + data.len()]);
        let mut outer = [0u8; 96];
        outer[..BLOCK].copy_from_slice(&opad);
        outer[BLOCK..BLOCK + 32].copy_from_slice(&inner_hash);
        return sha256(&outer[..BLOCK + 32]);
    }
    let mut inner = Vec::with_capacity(BLOCK + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let inner_hash = sha256(&inner);
    let mut outer = Vec::with_capacity(BLOCK + 32);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

/// PBKDF2-HMAC-SHA-256 (RFC 8018) producing `dk_len` bytes.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, dk_len: usize) -> Vec<u8> {
    use plomid_core::SCRAM_HASH_LEN as H_LEN;
    let mut out = Vec::with_capacity(dk_len);
    let mut block_index: u32 = 1;
    while out.len() < dk_len {
        let mut salt_block = Vec::with_capacity(salt.len() + 4);
        salt_block.extend_from_slice(salt);
        salt_block.extend_from_slice(&block_index.to_be_bytes());
        let mut u = hmac_sha256(password, &salt_block);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for i in 0..H_LEN {
                t[i] ^= u[i];
            }
        }
        out.extend_from_slice(&t);
        block_index = block_index.wrapping_add(1);
    }
    out.truncate(dk_len);
    out
}

/// Standard Base64 encoding (RFC 4648), used for SCRAM attribute values.
pub fn base64_encode(data: &[u8]) -> String {
    use plomid_core::SCRAM_BASE64_ALPHABET as ALPHABET;
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Standard Base64 decoding (RFC 4648). Returns `None` on invalid input.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|b| *b != b'=' && !b.is_ascii_whitespace())
        .collect();
    if cleaned.is_empty() {
        return Some(Vec::new());
    }
    if cleaned.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        let mut n = 0u32;
        for (i, byte) in chunk.iter().enumerate() {
            n |= u32::from(value(*byte)?) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() >= 3 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() >= 4 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Errors raised while parsing or verifying a SCRAM exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScramError {
    /// The client message could not be parsed.
    Malformed,
    /// The client proof did not match the expected proof.
    InvalidProof,
    /// The client supplied a nonce that does not match the server nonce.
    NonceMismatch,
}

impl std::fmt::Display for ScramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "malformed SCRAM message"),
            Self::InvalidProof => write!(f, "invalid SCRAM proof"),
            Self::NonceMismatch => write!(f, "SCRAM nonce mismatch"),
        }
    }
}

/// Parses a comma-separated `name=value` attribute list into an ordered vec.
fn parse_attributes(message: &str) -> Vec<(&str, &str)> {
    message
        .split(',')
        .filter_map(|part| part.split_once('='))
        .collect()
}
/// The server-side state of a single SCRAM-SHA-256 authentication exchange.
///
/// A fresh instance is created per authentication attempt, so no state leaks
/// between connections.
pub struct ScramSession {
    password: String,
    salt: [u8; 16],
    iterations: u32,
    client_first_bare: String,
    server_first: String,
    server_nonce: String,
    salted_password: Option<[u8; 32]>,
}

impl ScramSession {
    /// Creates a new server-side SCRAM exchange for `password`.
    ///
    /// The salt is derived from the current time and a caller-supplied
    /// per-connection nonce seed so that each exchange uses a distinct salt.
    pub fn new(password: &str, salt_seed: u64) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut salt = [0u8; 16];
        let mix = now ^ salt_seed.rotate_left(17);
        for (i, byte) in salt.iter_mut().enumerate() {
            *byte = ((mix >> ((i % 8) * 8)) ^ (salt_seed >> ((i % 5) * 8))) as u8;
        }
        Self {
            password: password.to_owned(),
            salt,
            iterations: 4096,
            client_first_bare: String::new(),
            server_first: String::new(),
            server_nonce: String::new(),
            salted_password: None,
        }
    }

    pub fn with_verifier(password: &str, salt: [u8; 16], salted_password: &[u8]) -> Self {
        let mut session = Self::new(password, 0);
        session.salt = salt;
        session.salted_password = salted_password.try_into().ok();
        session
    }

    /// Processes the SASLInitialResponse payload (the `n,,n=user,r=nonce`
    /// client-first message) and returns the server-first message.
    pub fn handle_initial(&mut self, client_first: &str) -> Result<String, ScramError> {
        // Strip the gs2 header. The header is `flag,authzid,`; with no
        // authzid the client sends `n,,`. Splitting on commas and rejoining
        // everything after the second field yields the client-first-bare.
        let parts = client_first.split(',').collect::<Vec<_>>();
        if parts.len() < 3 {
            return Err(ScramError::Malformed);
        }
        let bare = parts[2..].join(",");
        let attrs = parse_attributes(&bare);
        let client_nonce = attrs
            .iter()
            .find(|(name, _)| *name == "r")
            .map(|(_, value)| *value)
            .ok_or(ScramError::Malformed)?;
        if client_nonce.is_empty() {
            return Err(ScramError::Malformed);
        }

        // Server nonce = client nonce + server-generated suffix.
        let suffix = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0)
            ^ 0x5a17_4a11)
            .to_string();
        let server_nonce = format!("{client_nonce}{suffix}");

        self.client_first_bare = bare.to_owned();
        self.server_nonce = server_nonce.clone();
        self.server_first = format!(
            "r={},s={},i={}",
            server_nonce,
            base64_encode(&self.salt),
            self.iterations
        );
        Ok(self.server_first.clone())
    }

    /// Processes the SASLResponse payload (the `c=...,r=...,p=...` client-final
    /// message) and returns the server-final message (`v=...`) on success.
    pub fn handle_final(&self, client_final: &str) -> Result<String, ScramError> {
        let attrs = parse_attributes(client_final);
        let client_nonce = attrs
            .iter()
            .find(|(name, _)| *name == "r")
            .map(|(_, value)| *value)
            .ok_or(ScramError::Malformed)?;
        if client_nonce != self.server_nonce {
            return Err(ScramError::NonceMismatch);
        }
        let client_proof_b64 = attrs
            .iter()
            .find(|(name, _)| *name == "p")
            .map(|(_, value)| *value)
            .ok_or(ScramError::Malformed)?;
        let client_proof = base64_decode(client_proof_b64).ok_or(ScramError::Malformed)?;

        // client-final-without-proof = everything up to the ",p=" marker.
        let without_proof = client_final
            .split(",p=")
            .next()
            .ok_or(ScramError::Malformed)?;

        let salted_password = self.salted_password.unwrap_or_else(|| {
            pbkdf2_sha256(self.password.as_bytes(), &self.salt, self.iterations, 32)
                .try_into()
                .expect("PBKDF2-SHA-256 must produce 32 bytes")
        });
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);

        let auth_message = format!(
            "{},{},{}",
            self.client_first_bare, self.server_first, without_proof
        );
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let mut expected_proof = [0u8; 32];
        for i in 0..32 {
            expected_proof[i] = client_key[i] ^ client_signature[i];
        }
        if expected_proof.as_slice() != client_proof.as_slice() {
            return Err(ScramError::InvalidProof);
        }

        let server_key = hmac_sha256(&salted_password, b"Server Key");
        let server_signature = hmac_sha256(&server_key, auth_message.as_bytes());
        Ok(format!("v={}", base64_encode(&server_signature)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223...
        let digest = sha256(b"abc");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_empty_and_long_input() {
        let empty = sha256(b"");
        assert_eq!(
            empty,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ]
        );
        // RFC 4231 test case 1: key=0x0b*20, data="Hi There"
        let hmac = hmac_sha256(&[0x0b; 20], b"Hi There");
        let hex: String = hmac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn base64_round_trip() {
        for input in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
        ] {
            let encoded = base64_encode(input);
            let decoded = base64_decode(&encoded).unwrap();
            assert_eq!(decoded, input, "round trip failed for {input:?}");
        }
        assert_eq!(base64_encode(b"n,,"), "biws");
    }

    #[test]
    fn scram_exchange_verifies_valid_client() {
        // Simulate a full client/server SCRAM exchange.
        let mut server = ScramSession::new("secret", 42);
        let client_first = "n,,n=alice,r=abcdef";
        let server_first = server.handle_initial(client_first).unwrap();
        let attrs = parse_attributes(&server_first);
        let salt = attrs.iter().find(|(n, _)| *n == "s").unwrap().1;
        let iterations: u32 = attrs
            .iter()
            .find(|(n, _)| *n == "i")
            .unwrap()
            .1
            .parse()
            .unwrap();
        let nonce = attrs.iter().find(|(n, _)| *n == "r").unwrap().1;
        let salt_bytes = base64_decode(salt).unwrap();

        // Client-side computation.
        let salted = pbkdf2_sha256(b"secret", &salt_bytes, iterations, 32);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let client_first_bare = "n=alice,r=abcdef";
        let without_proof = format!("c=biws,r={nonce}");
        let auth_message = format!("{client_first_bare},{server_first},{without_proof}");
        let client_sig = hmac_sha256(&stored_key, auth_message.as_bytes());
        let mut proof = [0u8; 32];
        for i in 0..32 {
            proof[i] = client_key[i] ^ client_sig[i];
        }
        let client_final = format!("{},p={}", without_proof, base64_encode(&proof));

        let server_final = server.handle_final(&client_final).unwrap();
        assert!(server_final.starts_with("v="), "got {server_final}");

        // Verify the server signature independently.
        let server_key = hmac_sha256(&salted, b"Server Key");
        let server_sig = hmac_sha256(&server_key, auth_message.as_bytes());
        assert_eq!(server_final, format!("v={}", base64_encode(&server_sig)));
    }

    #[test]
    fn scram_rejects_wrong_password() {
        let mut server = ScramSession::new("correct", 7);
        let server_first = server.handle_initial("n,,n=alice,r=nonce1").unwrap();
        let attrs = parse_attributes(&server_first);
        let salt = base64_decode(attrs.iter().find(|(n, _)| *n == "s").unwrap().1).unwrap();
        let iterations: u32 = attrs
            .iter()
            .find(|(n, _)| *n == "i")
            .unwrap()
            .1
            .parse()
            .unwrap();
        let nonce = attrs.iter().find(|(n, _)| *n == "r").unwrap().1;

        // Client uses the wrong password.
        let salted = pbkdf2_sha256(b"wrong", &salt, iterations, 32);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let auth_message = format!("n=alice,r=nonce1,{server_first},c=biws,r={nonce}");
        let client_sig = hmac_sha256(&stored_key, auth_message.as_bytes());
        let mut proof = [0u8; 32];
        for i in 0..32 {
            proof[i] = client_key[i] ^ client_sig[i];
        }
        let client_final = format!("c=biws,r={nonce},p={}", base64_encode(&proof));
        assert_eq!(
            server.handle_final(&client_final),
            Err(ScramError::InvalidProof)
        );
    }

    #[test]
    fn scram_rejects_nonce_mismatch() {
        let mut server = ScramSession::new("secret", 3);
        let _ = server.handle_initial("n,,n=alice,r=nonceA").unwrap();
        let client_final = "c=biws,r=nonceB,p=AAAA";
        assert_eq!(
            server.handle_final(client_final),
            Err(ScramError::NonceMismatch)
        );
    }

    #[test]
    fn scram_rejects_malformed_initial() {
        let mut server = ScramSession::new("secret", 1);
        assert_eq!(
            server.handle_initial("not-a-scram-message"),
            Err(ScramError::Malformed)
        );
        assert_eq!(server.handle_initial("n,,r="), Err(ScramError::Malformed));
    }
}
