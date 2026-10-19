//! Minecraft 1.7.10 Status/Ping server only; NO Login/Play or Forge Mod execution.
use legacy_protocol::{
    packet_id_and_body, read_frame, write_frame, write_string, write_varint, PROTOCOL_VERSION,
};
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
const MAX_CONNECTIONS: usize = 64;
const STATUS_JSON: &str = r#"{"version":{"name":"1.7.10","protocol":5},"players":{"max":20,"online":0},"description":{"text":"IndustrialRust1710 status-only prototype"}}"#;
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn status(s: &mut TcpStream) -> io::Result<()> {
    let request = read_frame(s)?;
    let (id, c) = packet_id_and_body(&request)?;
    if id != 0 {
        return Err(io::Error::new(ErrorKind::InvalidData, "not status request"));
    }
    c.finish()?;
    let mut reply = vec![0];
    write_string(STATUS_JSON, &mut reply)?;
    write_frame(s, &reply)?;
    match read_frame(s) {
        Ok(p) => {
            let (id, mut c) = packet_id_and_body(&p)?;
            if id != 1 {
                return Err(io::Error::new(ErrorKind::InvalidData, "not ping"));
            }
            let timestamp = c.read_i64()?;
            c.finish()?;
            let mut pong = Vec::new();
            write_varint(1, &mut pong);
            pong.extend_from_slice(&timestamp.to_be_bytes());
            write_frame(s, &pong)
        }
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::UnexpectedEof | ErrorKind::TimedOut | ErrorKind::WouldBlock
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}
fn handle(mut s: TcpStream) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    let p = read_frame(&mut s)?;
    let (id, mut c) = packet_id_and_body(&p)?;
    if id != 0 {
        return Err(io::Error::new(ErrorKind::InvalidData, "not handshake"));
    }
    let version = c.read_varint()?;
    let _host = c.read_string(255)?;
    let _port = c.read_u16()?;
    let next = c.read_varint()?;
    c.finish()?;
    if version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "wrong Minecraft protocol",
        ));
    }
    match next {
        1 => status(&mut s),
        2 => {
            let p = read_frame(&mut s)?;
            let (id, mut c) = packet_id_and_body(&p)?;
            if id != 0 {
                return Err(io::Error::new(ErrorKind::InvalidData, "not login start"));
            }
            let _name = c.read_string(16)?;
            c.finish()?;
            let mut response = vec![0];
            write_string(
                r#"{"text":"IndustrialRust1710: gameplay/login not yet implemented"}"#,
                &mut response,
            )?;
            write_frame(&mut s, &response)
        }
        _ => Err(io::Error::new(
            ErrorKind::InvalidData,
            "invalid handshake state",
        )),
    }
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--help" {
        println!("Usage: legacy-status-server [--bind IP:PORT]");
        return Ok(());
    }
    let bind: SocketAddr = match args.as_slice() {
        [_] => "127.0.0.1:25565".parse().expect("literal socket"),
        [_, flag, address] if flag == "--bind" => address
            .parse()
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid socket"))?,
        _ => return Err(io::Error::new(ErrorKind::InvalidInput, "invalid arguments")),
    };
    let listener = TcpListener::bind(bind)?;
    println!(
        "Listening on {} (Status/Ping only; Login/Play unsupported)",
        listener.local_addr()?
    );
    let active = Arc::new(AtomicUsize::new(0));
    for s in listener.incoming() {
        let s = s?;
        let n = active.fetch_add(1, Ordering::AcqRel);
        if n >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let guard = Active(Arc::clone(&active));
        thread::spawn(move || {
            let _guard = guard;
            if let Err(e) = handle(s) {
                eprintln!("connection: {e}");
            }
        });
    }
    Ok(())
}
