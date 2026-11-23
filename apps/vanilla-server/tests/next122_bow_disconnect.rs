//! NEXT-122 / #1105: disconnect while drawing a bow, then reload inventory.
//!
//! A second, synthetic client observes the server; no real world or account
//! is used. Vanilla client and save-failure behavior remain untested.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{read_varint, write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("ir1710-next122-{}-{stamp}", std::process::id()));
        // Only a newly created directory belongs to this test.
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Observer"},{"name":"Archer"}]"#,
        )
        .unwrap();
        Self(root)
    }
}

struct Server {
    child: Child,
    log: PathBuf,
    address: SocketAddr,
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            eprintln!(
                "owned server {} did not exit after kill; log {}",
                self.child.id(),
                self.log.display()
            );
        }
    }
}

impl Server {
    fn start(root: &Path, run: usize) -> Self {
        let log = root.join(format!("server-{run}.log"));
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log)
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(root)
            .args(["--bind", "127.0.0.1:0"])
            .arg("--world")
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap();
        // Guard owns the child before waiting for readiness.
        let mut server = Self {
            child,
            log,
            address: "127.0.0.1:0".parse().unwrap(),
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            assert!(
                Instant::now() < deadline,
                "startup timeout; log {}",
                server.log.display()
            );
            if let Some(status) = server.child.try_wait().unwrap() {
                panic!("server exited {status}; log {}", server.log.display());
            }
            let output = fs::read_to_string(&server.log).unwrap_or_default();
            if let Some(addr) = output.lines().find_map(|line| {
                line.strip_prefix("IndustrialRust1710: listening on ")
                    .and_then(|s| s.split_whitespace().next())
            }) {
                let address: SocketAddr = addr.parse().expect("server bind address");
                assert!(address.ip().is_loopback() && address.port() != 0);
                server.address = address;
                return server;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn console(&mut self, command: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
    }

    fn join(&mut self, name: &str) -> (BufferedClient, i32, Vec<u8>) {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < until, "{name} join timed out");
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before join"
            );
            if let Some(value) = self.try_join(name) {
                return value;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn try_join(&self, name: &str) -> Option<(BufferedClient, i32, Vec<u8>)> {
        let stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(3)).unwrap();
        let mut client = BufferedClient::new(stream);
        client.set_nodelay(true).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("localhost", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.address.port().to_be_bytes());
        write_varint(2, &mut handshake);
        client.write_all(&packet(0, &handshake)).unwrap();
        let mut login = Vec::new();
        write_string(name, &mut login).unwrap();
        client.write_all(&packet(0, &login)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut entity = None;
        let mut selected = None;
        let mut initial_inventory = None;
        loop {
            assert!(Instant::now() < deadline, "{name} login timeout");
            let Some((id, body)) = pump(&mut client) else {
                continue;
            };
            match id {
                0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
                0x01 if body.len() >= 4 => {
                    entity = Some(i32::from_be_bytes(body[..4].try_into().unwrap()))
                }
                0x09 if body.len() == 1 => selected = Some(body[0]),
                0x30 if body.first() == Some(&0) => initial_inventory = Some(body),
                0x08 if body.len() == 33 => {
                    assert_eq!(selected, Some(0), "selected hotbar slot at join");
                    return Some((
                        client,
                        entity.expect("join entity"),
                        initial_inventory.expect("initial player inventory"),
                    ));
                }
                0x40 => return None,
                _ => {}
            }
        }
    }

    fn stop(&mut self) {
        self.console("/stop");
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "server exit {status}; log {}",
                    self.log.display()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "stop timeout; log {}",
                self.log.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &payload).unwrap();
    framed
}

fn pump(client: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    client.poll_packet().expect("Protocol 5 read")
}

fn keepalive(client: &mut BufferedClient, id: u32, body: &[u8]) {
    if id == 0 && body.len() == 4 {
        client.write_all(&packet(0, body)).unwrap();
    }
    assert_ne!(id, 0x40, "unexpected disconnect: {body:?}");
}

fn wait_for(
    client: &mut BufferedClient,
    wanted: u32,
    predicate: impl Fn(&[u8]) -> bool,
    label: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "timeout waiting for {label}");
        let Some((id, body)) = pump(client) else {
            continue;
        };
        keepalive(client, id, &body);
        if id == wanted && predicate(&body) {
            return body;
        }
    }
}

fn entity(body: &[u8]) -> Option<i32> {
    let mut rest = body;
    read_varint(&mut rest).ok().map(|value| value as i32)
}

fn arrow(body: &[u8]) -> Option<(i32, i32)> {
    let mut rest = body;
    let id = read_varint(&mut rest).ok()? as i32;
    if rest.len() < 19 || rest[0] != 60 {
        return None;
    }
    Some((id, i32::from_be_bytes(rest[15..19].try_into().ok()?)))
}

fn destroyed(body: &[u8], id: i32) -> bool {
    body.len() == 5 && body[0] == 1 && i32::from_be_bytes(body[1..5].try_into().unwrap()) == id
}

fn send_chat(client: &mut BufferedClient, marker: &str) {
    let mut body = Vec::new();
    write_string(marker, &mut body).unwrap();
    client.write_all(&packet(1, &body)).unwrap();
}

fn use_bow(client: &mut BufferedClient) {
    let mut body = Vec::new();
    body.extend_from_slice(&(-1i32).to_be_bytes());
    body.push(255);
    body.extend_from_slice(&(-1i32).to_be_bytes());
    body.push(255);
    body.extend_from_slice(&261i16.to_be_bytes());
    body.push(1);
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&[0, 0, 0]);
    client.write_all(&packet(8, &body)).unwrap();
}

fn release_bow(client: &mut BufferedClient) {
    let mut body = vec![5];
    body.extend_from_slice(&10i32.to_be_bytes());
    body.push(64);
    body.extend_from_slice(&0i32.to_be_bytes());
    body.push(255);
    client.write_all(&packet(7, &body)).unwrap();
}

/// Parse Window Items, rejecting NBT rather than treating byte patterns as slots.
fn slots(body: &[u8]) -> io::Result<Vec<Option<(i16, u8, i16)>>> {
    let mut input = body;
    let take = |input: &mut &[u8], n| -> io::Result<Vec<u8>> {
        if input.len() < n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (head, tail) = input.split_at(n);
        *input = tail;
        Ok(head.to_vec())
    };
    if take(&mut input, 1)?[0] != 0 {
        return Err(io::Error::other("unexpected window"));
    }
    let count = u16::from_be_bytes(take(&mut input, 2)?.try_into().unwrap());
    let mut result = Vec::new();
    for _ in 0..count {
        let id = i16::from_be_bytes(take(&mut input, 2)?.try_into().unwrap());
        if id == -1 {
            result.push(None);
            continue;
        }
        let amount = take(&mut input, 1)?[0];
        let damage = i16::from_be_bytes(take(&mut input, 2)?.try_into().unwrap());
        let nbt_len = i16::from_be_bytes(take(&mut input, 2)?.try_into().unwrap());
        if nbt_len != -1 {
            return Err(io::Error::other("unexpected NBT"));
        }
        result.push(Some((id, amount, damage)));
    }
    if !input.is_empty() {
        return Err(io::Error::other("trailing window bytes"));
    }
    Ok(result)
}

fn assert_inventory(body: &[u8], arrows: u32) {
    let entries = slots(body).expect("parse window slots");
    assert_eq!(
        entries.get(36),
        Some(&Some((261, 1, 0))),
        "selected slot 0 must contain the undamaged bow"
    );
    let bows: Vec<_> = entries
        .iter()
        .flatten()
        .filter(|slot| slot.0 == 261)
        .collect();
    assert_eq!(bows, vec![&(261, 1, 0)], "bow and durability");
    assert_eq!(
        entries
            .iter()
            .flatten()
            .filter(|slot| slot.0 == 262)
            .map(|slot| u32::from(slot.1))
            .sum::<u32>(),
        arrows
    );
}

fn assert_arrow_count(body: &[u8], arrows: u32) {
    let entries = slots(body).expect("parse window slots");
    assert_eq!(
        entries
            .iter()
            .flatten()
            .filter(|slot| slot.0 == 262)
            .map(|slot| u32::from(slot.1))
            .sum::<u32>(),
        arrows,
        "arrow quantity"
    );
}

fn no_arrow_until(
    client: &mut BufferedClient,
    predicate: impl Fn(u32, &[u8]) -> bool,
    label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "timeout waiting for {label}");
        let Some((id, body)) = pump(client) else {
            continue;
        };
        keepalive(client, id, &body);
        assert!(
            !(id == 0x0e && arrow(&body).is_some()),
            "unexpected arrow before {label}"
        );
        if predicate(id, &body) {
            return;
        }
    }
}

fn no_arrow_for(client: &mut BufferedClient, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if let Some((id, body)) = pump(client) {
            keepalive(client, id, &body);
            assert!(
                !(id == 0x0e && arrow(&body).is_some()),
                "unexpected delayed arrow"
            );
        }
    }
}

#[test]
fn next122_unreleased_bow_preserves_inventory_after_disconnect_and_restart() {
    let fixture = Fixture::new();
    let mut server = Server::start(&fixture.0, 1);
    let (mut observer, _, _) = server.join("Observer");
    let (mut archer, archer_id, _) = server.join("Archer");
    wait_for(
        &mut observer,
        0x0c,
        |body| entity(body) == Some(archer_id),
        "archer spawn",
    );
    server.console("/gamemode survival Archer");
    wait_for(
        &mut archer,
        0x39,
        |body| body.first() == Some(&0),
        "survival",
    );
    server.console("/give Archer 261 1");
    wait_for(
        &mut archer,
        0x30,
        |body| slots(body).is_ok_and(|s| s.iter().flatten().any(|x| x.0 == 261)),
        "bow",
    );
    server.console("/give Archer 262 5");
    let before = wait_for(
        &mut archer,
        0x30,
        |body| slots(body).is_ok_and(|s| s.iter().flatten().any(|x| x.0 == 262)),
        "arrows",
    );
    assert_inventory(&before, 5);
    let began = Instant::now();
    use_bow(&mut archer);
    // Ordered chat receipt proves the server processed the preceding C08.
    send_chat(&mut archer, "next122-draw-processed");
    no_arrow_until(
        &mut observer,
        |id, body| id == 0x02 && body.windows(22).any(|s| s == b"next122-draw-processed"),
        "draw marker",
    );
    assert!(
        began.elapsed() < Duration::from_millis(950),
        "draw was no longer partial"
    );
    drop(archer); // no Release Use Item packet
    no_arrow_until(
        &mut observer,
        |id, body| id == 0x13 && destroyed(body, archer_id),
        "archer disconnect",
    );
    no_arrow_for(&mut observer, Duration::from_millis(500));
    server.stop();
    drop(server);

    let mut server = Server::start(&fixture.0, 2);
    let (mut archer, archer_id, after) = server.join("Archer");
    // The server sends the authoritative inventory before the join position.
    assert_inventory(&after, 5);
    let (mut observer, _, _) = server.join("Observer");
    use_bow(&mut archer);
    let until = Instant::now() + Duration::from_millis(1250);
    while Instant::now() < until {
        if let Some((id, body)) = pump(&mut archer) {
            keepalive(&mut archer, id, &body);
        }
    }
    release_bow(&mut archer);
    let shot = wait_for(
        &mut observer,
        0x0e,
        |body| arrow(body).is_some_and(|(_, shooter)| shooter == archer_id),
        "positive-control arrow",
    );
    assert_eq!(arrow(&shot).unwrap().1, archer_id);
    let debited = wait_for(
        &mut archer,
        0x30,
        |body| {
            slots(body).is_ok_and(|s| {
                s.iter()
                    .flatten()
                    .filter(|x| x.0 == 262)
                    .map(|x| u32::from(x.1))
                    .sum::<u32>()
                    == 4
            })
        },
        "positive-control inventory",
    );
    // Bow durability after a real shot is outside the no-release contract.
    assert_arrow_count(&debited, 4);
    server.stop();
    drop(archer);
    drop(observer);
    drop(server);
    fs::remove_dir_all(&fixture.0).expect("remove owned fixture after server exit");
}
