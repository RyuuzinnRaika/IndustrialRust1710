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
use std::time::{Duration, Instant};

use crate::{send, Input};

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

// A socket read timeout only bounds each individual read(2). Without an
// absolute deadline, a peer can hold a pending online login indefinitely by
// trickling one encrypted-response byte just before each socket timeout.
const ENCRYPTION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

struct DeadlineReader<'a> {
    connection: &'a mut Connection,
    expires_at: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            let remaining = self.expires_at.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.connection.set_read_timeout(Some(remaining))?;
            match self.connection.read(output) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    // Poll again only while some of the absolute budget remains.
                }
                other => return other,
            }
        }
    }
}

/// Read one complete unencrypted EncryptionResponse within one total budget,
/// including its VarInt prefix and packet body. Restore the previous socket
/// timeout before enabling AES or starting the Mojang session lookup.
fn receive_encryption_response(
    connection: &mut Connection,
    budget: Duration,
) -> io::Result<(i32, Vec<u8>)> {
    let previous_timeout = connection.read_timeout()?;
    let result = (|| {
        let mut reader = DeadlineReader {
            connection,
            expires_at: Instant::now() + budget,
        };
        let frame = legacy_protocol::read_frame(&mut reader)?;
        let mut data = frame.as_slice();
        let id = legacy_protocol::read_varint(&mut data)? as i32;
        Ok((id, data.to_vec()))
    })();
    connection.set_read_timeout(previous_timeout)?;
    result
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

        let (id, response) = receive_encryption_response(socket, ENCRYPTION_RESPONSE_TIMEOUT)?;
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
        // Each connection has a freshly generated challenge. A response from
        // any earlier login (or a different connection) must never authenticate.
        verify_challenge_token(&challenge, &returned_token)?;
        if secret.len() != 16 {
            return Err(invalid("invalid encryption response"));
        }
        let key: [u8; 16] = secret.try_into().map_err(|_| invalid("invalid AES key"))?;
        socket.encrypted(&key);
        // The client switches to AES-CFB8 immediately after sending its reply.
        let server_hash = signed_server_hash(&key, &self.der);
        verify_has_joined(username, &server_hash)
    }
}

/// Verify the decrypted 1.7.10 four-byte challenge for THIS connection.
/// Compare all four positions even after a mismatch; reject both shortened
/// and extended responses without revealing the expected bytes in diagnostics.
fn verify_challenge_token(challenge: &[u8; 4], returned: &[u8]) -> io::Result<()> {
    let mut mismatch = u8::from(returned.len() != challenge.len());
    for (index, expected) in challenge.iter().enumerate() {
        mismatch |= expected ^ returned.get(index).copied().unwrap_or(0);
    }
    if mismatch == 0 {
        Ok(())
    } else {
        Err(invalid("invalid encryption response"))
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
    let response_body = read_session_body(response.into_reader())?;
    parse_verified_profile(username, &response_body)
}

/// Bounded and deliberately redacted: an untrusted network/decoder error
/// might otherwise contain an HTTP query, token, or raw response excerpt.
fn read_session_body(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    reader
        .take(16_385)
        .read_to_end(&mut body)
        .map_err(|_| invalid("session response read failed"))?;
    if body.len() > 16_384 {
        return Err(invalid("oversized session response"));
    }
    Ok(body)
}

// Session-server parsing is a separate, network-free authorization boundary.
// This permits malformed replies and identity substitution to be regression-tested.
fn parse_verified_profile(username: &str, body: &[u8]) -> io::Result<VerifiedProfile> {
    let object: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| invalid("invalid session response"))?;
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
    fn encryption_response_deadline_limits_trickled_varint_prefix() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let mut connection = Connection::new(accepted);
        connection
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let writer = thread::spawn(move || {
            // Each fragment arrives well within a per-read 70 ms timeout,
            // but the whole frame takes longer than the absolute 70 ms budget.
            sender.write_all(&[0x84]).unwrap();
            thread::sleep(Duration::from_millis(40));
            sender.write_all(&[0x01]).unwrap(); // 132-byte frame length
            thread::sleep(Duration::from_millis(40));
            sender.write_all(&[0x01]).unwrap(); // Partial frame body
            thread::sleep(Duration::from_millis(40));
        });
        let started = Instant::now();
        let error = receive_encryption_response(&mut connection, Duration::from_millis(70))
            .expect_err("incomplete EncryptionResponse must expire");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "incomplete response exceeded its absolute deadline"
        );
        assert_eq!(
            connection.read_timeout().unwrap(),
            Some(Duration::from_secs(2)),
            "socket timeout must be restored even after a read error"
        );
        writer.join().unwrap();
    }

    #[test]
    fn encryption_response_deadline_accepts_complete_packet_and_restores_timeout() {
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let mut connection = Connection::new(accepted);
        connection
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut framed = Vec::new();
        legacy_protocol::write_frame(&mut framed, &[0x01, 0xa5, 0x05]).unwrap();
        sender.write_all(&framed).unwrap();

        let (id, payload) =
            receive_encryption_response(&mut connection, Duration::from_secs(1)).unwrap();
        assert_eq!(id, 0x01);
        assert_eq!(payload, [0xa5, 0x05]);
        assert_eq!(
            connection.read_timeout().unwrap(),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn encryption_challenge_rejects_stale_cross_session_and_malformed_tokens() {
        // A token is valid only for the connection that issued it. Even though
        // both sessions use the same RSA public key, replaying A's response to
        // B's independently generated challenge cannot pass verification.
        let first = [0x12, 0x34, 0x56, 0x78];
        let next = [0x87, 0x65, 0x43, 0x21];
        assert!(verify_challenge_token(&first, &first).is_ok());
        assert!(verify_challenge_token(&next, &next).is_ok());

        for (challenge, returned) in [
            (&next, &first[..]),                           // replayed response from session A
            (&first, &[][..]),                             // absent response
            (&first, &first[..3]),                         // truncated token
            (&first, &[0x12, 0x34, 0x56, 0x79][..]),       // one changed byte
            (&first, &[0x12, 0x34, 0x56, 0x78, 0x00][..]), // extra byte
        ] {
            let error = verify_challenge_token(challenge, returned).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "invalid encryption response");
            assert!(!error.to_string().contains("12345678"));
        }
    }

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
    fn failed_session_body_reader_never_reveals_underlying_secret_error() {
        struct UntrustedFailure;
        impl Read for UntrustedFailure {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other(
                    "accessToken=PRIVATE_TEST_TOKEN; rsa_private=SECRET",
                ))
            }
        }
        let error = read_session_body(UntrustedFailure).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "session response read failed");
        assert!(!error.to_string().contains("PRIVATE_TEST_TOKEN"));
    }

    #[test]
    fn session_response_limit_is_checked_without_echoing_the_body() {
        let oversized = std::io::Cursor::new(vec![b'X'; 16_385]);
        let error = read_session_body(oversized).unwrap_err();
        assert_eq!(error.to_string(), "oversized session response");
        assert!(!error.to_string().contains("XXXXX"));
        assert_eq!(
            read_session_body(std::io::Cursor::new(b"{}")).unwrap(),
            b"{}".to_vec()
        );
    }

    #[test]
    fn session_profile_accepts_only_matching_name_and_strict_uuid() {
        let bytes = br#"{"name":"Notch","id":"069a79f444e94726a5befca90e38aaf5"}"#;
        let valid = parse_verified_profile("Notch", bytes).unwrap();
        assert_eq!(valid.name, "Notch");
        assert_eq!(valid.uuid, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert!(parse_verified_profile("OtherUser", bytes).is_err());
        assert!(parse_verified_profile("Notch", br#"{"name":"Notch","id":"../../etc"}"#).is_err());
        assert!(parse_verified_profile("Notch", br#"{"name":"Notch"}"#).is_err());
        assert!(parse_verified_profile("Notch", b"not-json").is_err());
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
