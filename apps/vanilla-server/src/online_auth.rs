//! Minecraft Java 1.7.10 online-mode login. Never fall back to offline identity.
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use rand::{rngs::OsRng, RngCore};
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::ops::{Deref, DerefMut};
use std::time::Duration;

use crate::{receive, send, Input};

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

/// Stateful AES-128-CFB8, with one shift register per wire direction.
/// Stream state is continuous across frames, including packet length VarInts.
struct Cfb8 {
    block: Aes128,
    shift: [u8; 16],
    encrypting: bool,
}
impl Cfb8 {
    fn new(secret: &[u8; 16], encrypting: bool) -> Self {
        Self {
            block: Aes128::new_from_slice(secret).expect("16-byte AES-128 key"),
            shift: *secret,
            encrypting,
        }
    }
    fn apply(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            let input = *byte;
            let mut block = aes::cipher::Block::<Aes128>::clone_from_slice(&self.shift);
            self.block.encrypt_block(&mut block);
            let output = input ^ block[0];
            self.shift.copy_within(1.., 0);
            // CFB always feeds ciphertext back, not the plaintext.
            self.shift[15] = if self.encrypting { output } else { input };
            *byte = output;
        }
    }
}

/// Acts as a TcpStream for timeouts and socket metadata, plus Read/Write with
/// encryption enabled only after the unencrypted Encryption Response frame.
pub struct Connection {
    socket: TcpStream,
    reader: Option<Cfb8>,
    writer: Option<Cfb8>,
}
impl Connection {
    pub fn new(socket: TcpStream) -> Self {
        Self {
            socket,
            reader: None,
            writer: None,
        }
    }
    fn encrypted(&mut self, secret: &[u8; 16]) {
        self.reader = Some(Cfb8::new(secret, false));
        self.writer = Some(Cfb8::new(secret, true));
    }
}
impl Deref for Connection {
    type Target = TcpStream;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}
impl DerefMut for Connection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.socket
    }
}
impl Read for Connection {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let n = self.socket.read(output)?;
        if let Some(cipher) = self.reader.as_mut() {
            cipher.apply(&mut output[..n]);
        }
        Ok(n)
    }
}
impl Write for Connection {
    fn write(&mut self, plaintext: &[u8]) -> io::Result<usize> {
        // A partially written ciphertext would make a retry corrupt the CFB
        // state. Write the whole encrypted span or fail the connection.
        for chunk in plaintext.chunks(8192) {
            let mut bytes = chunk.to_vec();
            if let Some(cipher) = self.writer.as_mut() {
                cipher.apply(&mut bytes);
            }
            self.socket.write_all(&bytes)?;
        }
        Ok(plaintext.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

pub struct VerifiedProfile {
    pub name: String,
    pub uuid: String,
}

pub struct ServerIdentity {
    rsa: RsaPrivateKey,
    der: Vec<u8>,
}
impl ServerIdentity {
    pub fn generate() -> io::Result<Self> {
        let rsa = RsaPrivateKey::new(&mut OsRng, 1024).map_err(io::Error::other)?;
        let der = RsaPublicKey::from(&rsa)
            .to_public_key_der()
            .map_err(io::Error::other)?
            .as_ref()
            .to_vec();
        Ok(Self { rsa, der })
    }
    pub fn authenticate(
        &self,
        socket: &mut Connection,
        username: &str,
    ) -> io::Result<VerifiedProfile> {
        let mut challenge = [0u8; 4];
        OsRng.fill_bytes(&mut challenge);
        let public_len = u16::try_from(self.der.len()).map_err(|_| invalid("RSA key too long"))?;
        let mut request = Vec::new();
        legacy_protocol::write_string("", &mut request)?;
        request.extend_from_slice(&public_len.to_be_bytes());
        request.extend_from_slice(&self.der);
        request.extend_from_slice(&(challenge.len() as u16).to_be_bytes());
        request.extend_from_slice(&challenge);
        send(socket, 0x01, &request)?;

        let (id, response) = receive(socket)?;
        if id != 0x01 {
            return Err(invalid("expected encryption response"));
        }
        let mut body = Input(&response);
        let secret_len = body.u16()? as usize;
        if secret_len == 0 || secret_len > 512 {
            return Err(invalid("invalid encrypted secret"));
        }
        let encrypted_secret = body.take(secret_len)?;
        let token_len = body.u16()? as usize;
        if token_len == 0 || token_len > 512 {
            return Err(invalid("invalid encrypted token"));
        }
        let encrypted_token = body.take(token_len)?;
        body.finish()?;
        let secret = self
            .rsa
            .decrypt(Pkcs1v15Encrypt, encrypted_secret)
            .map_err(|_| invalid("RSA secret decryption failed"))?;
        let returned_token = self
            .rsa
            .decrypt(Pkcs1v15Encrypt, encrypted_token)
            .map_err(|_| invalid("RSA token decryption failed"))?;
        // Do not ever enable online-mode with a missing or incorrect token.
        let token_difference = returned_token.len() ^ challenge.len();
        let mut difference = token_difference as u8;
        for (i, expected) in challenge.iter().enumerate() {
            difference |= expected ^ returned_token.get(i).copied().unwrap_or(0);
        }
        if difference != 0 || secret.len() != 16 {
            return Err(invalid("invalid encryption response"));
        }
        let key: [u8; 16] = secret.try_into().map_err(|_| invalid("invalid AES key"))?;
        socket.encrypted(&key);
        // The client switches to AES-CFB8 immediately after sending its reply.
        let server_hash = signed_server_hash(&key, &self.der);
        verify_has_joined(username, &server_hash)
    }
}

fn signed_hex(digest: &[u8; 20]) -> String {
    let negative = digest[0] & 0x80 != 0;
    let mut magnitude = *digest;
    if negative {
        for b in &mut magnitude {
            *b = !*b;
        }
        for b in magnitude.iter_mut().rev() {
            let (next, carry) = b.overflowing_add(1);
            *b = next;
            if !carry {
                break;
            }
        }
    }
    let first = magnitude.iter().position(|b| *b != 0).unwrap_or(19);
    let mut output = format!("{:x}", magnitude[first]);
    for b in &magnitude[first + 1..] {
        output.push_str(&format!("{b:02x}"));
    }
    if negative {
        format!("-{output}")
    } else {
        output
    }
}
fn signed_server_hash(secret: &[u8; 16], der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(secret);
    hasher.update(der);
    let digest: [u8; 20] = hasher.finalize().into();
    signed_hex(&digest)
}

fn verify_has_joined(username: &str, hash: &str) -> io::Result<VerifiedProfile> {
    if !crate::whitelist::valid_name(username) {
        return Err(invalid("invalid account name"));
    }
    let url = format!(
        "https://sessionserver.mojang.com/session/minecraft/hasJoined?username={username}&serverId={hash}"
    );
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .redirects(0)
        .build();
    let response = agent
        .get(&url)
        .call()
        .map_err(|_| invalid("session verification unavailable"))?;
    if response.status() != 200 {
        return Err(invalid("account not authenticated by session server"));
    }
    let mut response_body = Vec::new();
    response
        .into_reader()
        .take(16_385)
        .read_to_end(&mut response_body)?;
    if response_body.len() > 16_384 {
        return Err(invalid("oversized session response"));
    }
    let object: serde_json::Value =
        serde_json::from_slice(&response_body).map_err(|_| invalid("invalid session response"))?;
    let name = object
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|name| name.eq_ignore_ascii_case(username) && crate::whitelist::valid_name(name))
        .ok_or_else(|| invalid("session username mismatch"))?;
    let id = object
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("invalid authenticated UUID"))?;
    let id = id.to_ascii_lowercase();
    Ok(VerifiedProfile {
        name: name.to_owned(),
        uuid: format!(
            "{}-{}-{}-{}-{}",
            &id[0..8],
            &id[8..12],
            &id[12..16],
            &id[16..20],
            &id[20..32]
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cfb8_round_trip_across_arbitrary_fragment_boundaries() {
        let key = [0x4b; 16];
        let plain: Vec<u8> = (0..5000).map(|v| (v % 251) as u8).collect();
        let mut encrypted = plain.clone();
        let mut send = Cfb8::new(&key, true);
        for piece in encrypted.chunks_mut(3) {
            send.apply(piece);
        }
        assert_ne!(encrypted, plain);
        let mut recv = Cfb8::new(&key, false);
        for piece in encrypted.chunks_mut(79) {
            recv.apply(piece);
        }
        assert_eq!(encrypted, plain);
    }
    #[test]
    fn minecraft_signed_hash_uses_java_twos_complement_format() {
        let mut sha = Sha1::new();
        sha.update(b"Notch");
        let digest: [u8; 20] = sha.finalize().into();
        assert_eq!(
            signed_hex(&digest),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
        let mut sha = Sha1::new();
        sha.update(b"jeb_");
        let digest: [u8; 20] = sha.finalize().into();
        assert_eq!(
            signed_hex(&digest),
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        );
    }
}
