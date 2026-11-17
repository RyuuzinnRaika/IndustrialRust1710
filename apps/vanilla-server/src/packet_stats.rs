//! NEXT-020 / #1003: anonymous per-packet-type traffic accounting.
//!
//! Only the direction and the 1.7.10 packet id — plus a fixed set of
//! bounded reject categories — are ever counted. Player names, IP
//! addresses and packet contents never enter these counters or the JSON
//! line they emit, so operators can collect per-type traffic profiles
//! without holding any personal information.
use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

const PACKET_ID_SLOTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    fn lane(self) -> usize {
        usize::from(self == Direction::Outbound)
    }
}

/// Fixed reject categories. They never carry client-controlled text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Login,
    Admission,
    MalformedFrame,
    Chat,
    QueueOverflow,
}

const REJECT_KINDS: [Reject; 5] = [
    Reject::Login,
    Reject::Admission,
    Reject::MalformedFrame,
    Reject::Chat,
    Reject::QueueOverflow,
];

impl Reject {
    fn index(self) -> usize {
        match self {
            Reject::Login => 0,
            Reject::Admission => 1,
            Reject::MalformedFrame => 2,
            Reject::Chat => 3,
            Reject::QueueOverflow => 4,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Reject::Login => "login",
            Reject::Admission => "admission",
            Reject::MalformedFrame => "malformed_frame",
            Reject::Chat => "chat",
            Reject::QueueOverflow => "queue_overflow",
        }
    }
}

/// Wire size of one framed packet: the VarInt length prefix plus payload.
pub fn frame_wire_len(payload_len: usize) -> u64 {
    let mut prefix = 1usize;
    let mut value = payload_len;
    while value >= 0x80 {
        value >>= 7;
        prefix += 1;
    }
    (prefix + payload_len) as u64
}

#[derive(Debug)]
pub struct PacketStats {
    frames: [[AtomicU64; PACKET_ID_SLOTS]; 2],
    bytes: [[AtomicU64; PACKET_ID_SLOTS]; 2],
    /// Traffic whose packet id did not fit the 1.7.10 range; still real
    /// bytes, so they are never silently dropped from the profile.
    unknown_frames: [AtomicU64; 2],
    unknown_bytes: [AtomicU64; 2],
    rejects: [AtomicU64; 5],
}

impl PacketStats {
    pub const fn new() -> Self {
        const fn zeroed() -> [AtomicU64; PACKET_ID_SLOTS] {
            [const { AtomicU64::new(0) }; PACKET_ID_SLOTS]
        }
        Self {
            frames: [zeroed(), zeroed()],
            bytes: [zeroed(), zeroed()],
            unknown_frames: [const { AtomicU64::new(0) }, const { AtomicU64::new(0) }],
            unknown_bytes: [const { AtomicU64::new(0) }, const { AtomicU64::new(0) }],
            rejects: [const { AtomicU64::new(0) }; 5],
        }
    }

    /// Record one framed packet. `wire_bytes` must come from
    /// [`frame_wire_len`] so the profile matches the TCP payload volume.
    pub fn record(&self, direction: Direction, packet_id: i64, wire_bytes: u64) {
        let lane = direction.lane();
        let id = usize::try_from(packet_id).unwrap_or(usize::MAX);
        if id < PACKET_ID_SLOTS {
            self.frames[lane][id].fetch_add(1, Ordering::Relaxed);
            self.bytes[lane][id].fetch_add(wire_bytes, Ordering::Relaxed);
        } else {
            self.unknown_frames[lane].fetch_add(1, Ordering::Relaxed);
            self.unknown_bytes[lane].fetch_add(wire_bytes, Ordering::Relaxed);
        }
    }

    pub fn rejected(&self, kind: Reject) {
        self.rejects[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn lane_value(&self, lane: usize, id: usize) -> Value {
        json!({
            "frames": self.frames[lane][id].load(Ordering::Relaxed),
            "bytes": self.bytes[lane][id].load(Ordering::Relaxed)
        })
    }

    /// JSON-safe profile containing only counters. Ids appear as decimal
    /// strings and only when at least one direction observed traffic.
    pub fn snapshot(&self) -> Value {
        let mut packets = Map::new();
        for id in 0..PACKET_ID_SLOTS {
            let inbound_frames = self.frames[0][id].load(Ordering::Relaxed);
            let outbound_frames = self.frames[1][id].load(Ordering::Relaxed);
            if inbound_frames == 0 && outbound_frames == 0 {
                continue;
            }
            let mut entry = Map::new();
            if inbound_frames > 0 {
                entry.insert("inbound".to_owned(), self.lane_value(0, id));
            }
            if outbound_frames > 0 {
                entry.insert("outbound".to_owned(), self.lane_value(1, id));
            }
            packets.insert(id.to_string(), Value::Object(entry));
        }
        let mut unknown = Map::new();
        for (lane, name) in [(0, "inbound"), (1, "outbound")] {
            let frames = self.unknown_frames[lane].load(Ordering::Relaxed);
            if frames > 0 {
                unknown.insert(
                    name.to_owned(),
                    json!({"frames": frames, "bytes": self.unknown_bytes[lane].load(Ordering::Relaxed)}),
                );
            }
        }
        let mut rejects = Map::new();
        for kind in REJECT_KINDS {
            rejects.insert(
                kind.label().to_owned(),
                self.rejects[kind.index()].load(Ordering::Relaxed).into(),
            );
        }
        json!({
            "schema_version": 1,
            "kind": "packet_stats",
            "process_id": std::process::id(),
            "packets": Value::Object(packets),
            "unknown": Value::Object(unknown),
            "rejects": Value::Object(rejects)
        })
    }
}

impl Default for PacketStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-wide counters fed by the shared frame choke points.
pub static GLOBAL: PacketStats = PacketStats::new();
static REPORT_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set where [`write_report`] persists the profile. First call wins; the
/// server configures it exactly once at startup.
pub fn set_report_path(path: PathBuf) {
    let _ = REPORT_PATH.set(path);
}

/// Append one cumulative anonymous snapshot line. Call after all session
/// threads have stopped so the counters are final.
pub fn write_report() -> io::Result<()> {
    let Some(path) = REPORT_PATH.get() else {
        return Ok(());
    };
    let row = GLOBAL.snapshot();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{row}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_wire_len_matches_the_varint_prefix_size() {
        assert_eq!(frame_wire_len(1), 2);
        assert_eq!(frame_wire_len(127), 128);
        assert_eq!(frame_wire_len(128), 130);
        assert_eq!(frame_wire_len(16_383), 16_385);
        assert_eq!(frame_wire_len(16_384), 16_387);
    }

    #[test]
    fn counters_are_per_direction_and_per_packet_id() {
        let stats = PacketStats::new();
        stats.record(Direction::Inbound, 3, 12);
        stats.record(Direction::Inbound, 3, 8);
        stats.record(Direction::Outbound, 3, 100);
        stats.record(Direction::Inbound, 33, 4_096);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot["packets"]["3"]["inbound"]["frames"], 2);
        assert_eq!(snapshot["packets"]["3"]["inbound"]["bytes"], 20);
        assert_eq!(snapshot["packets"]["3"]["outbound"]["frames"], 1);
        assert_eq!(snapshot["packets"]["3"]["outbound"]["bytes"], 100);
        assert_eq!(snapshot["packets"]["33"]["inbound"]["bytes"], 4_096);
        // Untouched ids never appear, so the profile stays small.
        assert!(snapshot["packets"].get("4").is_none());
        assert!(snapshot["unknown"].as_object().unwrap().is_empty());
    }

    #[test]
    fn out_of_range_ids_keep_counting_bytes_anonymously() {
        let stats = PacketStats::new();
        stats.record(Direction::Inbound, -1, 40);
        stats.record(Direction::Outbound, 999_999, 7);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot["unknown"]["inbound"]["frames"], 1);
        assert_eq!(snapshot["unknown"]["inbound"]["bytes"], 40);
        assert_eq!(snapshot["unknown"]["outbound"]["bytes"], 7);
    }

    #[test]
    fn reject_categories_count_separately_and_all_are_reported() {
        let stats = PacketStats::new();
        stats.rejected(Reject::Login);
        stats.rejected(Reject::Login);
        stats.rejected(Reject::QueueOverflow);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot["rejects"]["login"], 2);
        assert_eq!(snapshot["rejects"]["queue_overflow"], 1);
        // Every fixed category is present even when zero: the report shape
        // never depends on traffic, only the numbers change.
        assert_eq!(snapshot["rejects"]["admission"], 0);
        assert_eq!(snapshot["rejects"]["malformed_frame"], 0);
        assert_eq!(snapshot["rejects"]["chat"], 0);
    }

    #[test]
    fn snapshot_serialises_counters_only() {
        let stats = PacketStats::new();
        stats.record(Direction::Inbound, 1, 10);
        stats.rejected(Reject::Chat);
        let text = stats.snapshot().to_string();
        // The report is pure JSON numbers and fixed labels; there is no
        // place where a name, address or payload could be represented.
        for forbidden in ["name", "ip", "uuid", "player", "\"text\""] {
            assert!(
                !text.contains(forbidden),
                "unexpected personal-data-shaped key {forbidden:?} in {text}"
            );
        }
        assert!(text.contains("\"packet_stats\""));
    }
}
