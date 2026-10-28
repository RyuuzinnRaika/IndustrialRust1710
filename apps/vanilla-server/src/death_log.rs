//! Protocol 5 player-death announcements and diagnostic, per-world JSONL records.
//! This is observability only: scoring, drops and respawn remain session-owned.
use crate::player::PlayerPosition;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_RECORD_BYTES: usize = 2048;
// Only recent IDs are scanned: this is an audit-log retry guard, not a
// cross-file exactly-once transaction or a permanent event-id index.
const EVENT_ID_SCAN_BYTES: usize = MAX_RECORD_BYTES * 128;
static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(0);

fn generated_event_id() -> io::Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "invalid system clock"))?
        .as_nanos();
    let seq = NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed);
    Ok(format!("d2-{nanos:032x}-{:08x}-{seq:016x}", std::process::id()))
}

fn valid_event_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Detect an immediate retry, including one after reopening the file. Scan
/// at most 256 KiB rather than causing unbounded O(log_size) work on death.
fn recent_event_present(file: &mut fs::File, id: &str, record: &DeathRecord<'_>) -> io::Result<bool> {
    let len = file.metadata()?.len();
    let scan_len = len.min(EVENT_ID_SCAN_BYTES as u64) as usize;
    if scan_len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(len - scan_len as u64))?;
    let mut bytes = vec![0; scan_len];
    file.read_exact(&mut bytes)?;
    // The window may start in the middle of an older JSONL entry.
    let offset = if len > scan_len as u64 {
        bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(scan_len, |index| index + 1)
    } else {
        0
    };
    for line in bytes[offset..].split(|&byte| byte == b'\n').filter(|line| !line.is_empty()) {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value.get("event_id").and_then(Value::as_str) != Some(id) {
            continue;
        }
        if value.get("event").and_then(Value::as_str) != Some("player_death")
            || value.get("player").and_then(Value::as_str) != Some(record.player)
            || value.get("cause").and_then(Value::as_str) != Some(record.cause().code())
            || value.get("dimension").and_then(Value::as_i64) != Some(i64::from(record.dimension))
            || value.get("server_tick").and_then(Value::as_u64) != Some(record.tick)
        {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "death event ID reused for a different event",
            ));
        }
        return Ok(true);
    }
    Ok(false)
}


/// A human at negative fractional coordinates is inside the lower block,
/// not in the block obtained by truncation toward zero. Do not invent a
/// block coordinate from corrupted/non-finite player positions.
fn block_coordinate(value: f64) -> Option<i32> {
    let block = value.floor();
    if block.is_finite() && (i32::MIN as f64..=i32::MAX as f64).contains(&block) {
        Some(block as i32)
    } else {
        None
    }
}
// Protect against interleaved writes from this server process. This is not
// a cross-process lock and does not make player state + scoreboard atomic.
static DEATH_FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Repair a bounded, interrupted final JSONL entry before a new append.
/// Never truncate earlier complete lines or guess beyond our record size cap.
fn reconcile_jsonl_tail(file: &mut fs::File) -> io::Result<()> {
    let file_len = file.metadata()?.len();
    if file_len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }

    // A normal death record occupies fewer than MAX_RECORD_BYTES bytes.
    // If its start lies outside this window, do not destroy unknown data.
    let window = file_len.min(MAX_RECORD_BYTES as u64) as usize;
    file.seek(SeekFrom::Start(file_len - window as u64))?;
    let mut tail = vec![0; window];
    file.read_exact(&mut tail)?;
    let last_newline = tail.iter().rposition(|&byte| byte == b'\n');
    if last_newline.is_none() && file_len > window as u64 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "unterminated death-log tail exceeds maximum record size",
        ));
    }
    let start = last_newline.map_or(0, |index| index + 1);
    if let Ok(value) = serde_json::from_slice::<Value>(&tail[start..]) {
        // Never silently accept arbitrary complete JSON as a death record.
        if value.get("event").and_then(Value::as_str) != Some("player_death") {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "unexpected complete JSON value at death-log tail",
            ));
        }
        // A fully written death event may only have lost its final delimiter.
        file.write_all(b"\n")?;
    } else {
        let valid_len = file_len - (window - start) as u64;
        file.set_len(valid_len)?;
        eprintln!(
            "DEATH LOG RECOVERY: discarded {} bytes of incomplete final JSONL entry",
            file_len - valid_len
        );
    }
    file.sync_data()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeathCause {
    Fall,
    Starvation,
    Drowning,
    Suffocation,
    Lava,
    Fire,
    Burning,
    CombatOrExplosion,
    Mob {
        kind: &'static str,
        entity_id: i32,
    },
    Projectile {
        kind: &'static str,
        shooter_kind: Option<&'static str>,
        shooter_id: Option<i32>,
    },
    Explosion {
        kind: &'static str,
        source_id: Option<i32>,
    },
    WitherEffect,
    Mixed,
    MixedHazards,
    DispenserArrow,
    EnderPearl,
    Unknown,
}

impl DeathCause {
    pub fn code(self) -> &'static str {
        match self {
            Self::Fall => "fall",
            Self::Starvation => "starvation",
            Self::Drowning => "drowning",
            Self::Suffocation => "suffocation",
            Self::Lava => "lava",
            Self::Fire => "fire",
            Self::Burning => "burning",
            Self::CombatOrExplosion => "combat_or_explosion",
            Self::Mob { .. } => "mob",
            Self::Projectile { .. } => "projectile",
            Self::Explosion { .. } => "explosion",
            Self::WitherEffect => "wither_effect",
            Self::Mixed => "mixed_damage",
            Self::MixedHazards => "mixed_environmental",
            Self::DispenserArrow => "dispenser_arrow",
            Self::EnderPearl => "ender_pearl",
            Self::Unknown => "unknown",
        }
    }

    /// Return only server-verified provenance; never guess the nearest Mob.
    pub fn source_type(self) -> Option<&'static str> {
        match self {
            Self::Mob { kind, .. } | Self::Explosion { kind, .. } => Some(kind),
            Self::Projectile {
                shooter_kind: Some(kind),
                ..
            } => Some(kind),
            _ => None,
        }
    }

    pub fn source_entity_id(self) -> Option<i32> {
        match self {
            Self::Mob { entity_id, .. } => Some(entity_id),
            Self::Projectile { shooter_id, .. } => shooter_id,
            Self::Explosion { source_id, .. } => source_id,
            _ => None,
        }
    }

    /// The projectile kind is distinct from the Mob that fired it.
    pub fn projectile_type(self) -> Option<&'static str> {
        match self {
            Self::Projectile { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// Chat text follows the familiar Java 1.7.10 death-message vocabulary.
    /// Do not claim a specific killer when the combat event does not carry one.
    pub fn message(self, name: &str) -> String {
        match self {
            Self::Mob { kind, .. } => return format!("{name} was slain by {kind}"),
            Self::Projectile {
                shooter_kind: Some(kind),
                ..
            } => return format!("{name} was shot by {kind}"),
            Self::Projectile { kind, .. } => return format!("{name} was killed by {kind}"),
            Self::Explosion { kind, .. } => return format!("{name} was blown up by {kind}"),
            _ => {}
        }
        let description = match self {
            Self::Fall => "fell from a high place",
            Self::Starvation => "starved to death",
            Self::Drowning => "drowned",
            Self::Suffocation => "suffocated in a wall",
            Self::Lava => "tried to swim in lava",
            Self::Fire => "went up in flames",
            Self::Burning => "burned to death",
            Self::CombatOrExplosion => "was killed in combat or an explosion",
            Self::Mob { .. } | Self::Projectile { .. } | Self::Explosion { .. } => "died",
            Self::WitherEffect => "withered away",
            Self::Mixed => "was killed by several sources",
            Self::MixedHazards => "died from multiple environmental hazards",
            Self::DispenserArrow => "was shot by a dispenser",
            Self::EnderPearl => "died after using an ender pearl",
            Self::Unknown => "died",
        };
        format!("{name} {description}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FatalHit {
    pub cause: DeathCause,
    pub previous_health: f32,
    pub applied_damage: f32,
}

/// Capture only the transition from living to dead, not previous nonfatal hits.
/// This avoids attributing a fatal fall to a mob that attacked minutes earlier.
pub fn record_fatal(pending: &mut Option<FatalHit>, before: f32, after: f32, cause: DeathCause) {
    if pending.is_none() && before.is_finite() && after.is_finite() && before > 0.0 && after <= 0.0
    {
        *pending = Some(FatalHit {
            cause,
            previous_health: before,
            applied_damage: before - after,
        });
    }
}

pub struct DeathRecord<'a> {
    pub player: &'a str,
    pub hit: Option<FatalHit>,
    pub position: PlayerPosition,
    pub dimension: i32,
    pub tick: u64,
}

impl DeathRecord<'_> {
    pub fn cause(&self) -> DeathCause {
        self.hit.map_or(DeathCause::Unknown, |hit| hit.cause)
    }

    pub fn chat_message(&self) -> String {
        self.cause().message(self.player)
    }

    pub fn json(&self, timestamp_ms: u128) -> Value {
        json!({
            "event": "player_death",
            "schema_version": 1,
            "timestamp_ms": timestamp_ms,
            "player": self.player,
            "cause": self.cause().code(),
            "source_type": self.cause().source_type(),
            "source_entity_id": self.cause().source_entity_id(),
            "projectile_type": self.cause().projectile_type(),
            "death_message": self.chat_message(),
            "dimension": self.dimension,
            "position": {
                "x": self.position.x,
                "y": self.position.y,
                "z": self.position.z
            },
            "block_position": {
                "x": block_coordinate(self.position.x),
                "y": block_coordinate(self.position.y),
                "z": block_coordinate(self.position.z)
            },
            "health_before": self.hit.map(|hit| hit.previous_health),
            "applied_damage": self.hit.map(|hit| hit.applied_damage),
            "server_tick": self.tick
        })
    }

    pub fn log_path(world_path: &Path) -> PathBuf {
        world_path.with_extension("deaths.jsonl")
    }

    /// Callers without a stable ID still get a new observable ID. Retries
    /// are idempotent only when the caller explicitly reuses the same ID.
    #[allow(dead_code)] // Legacy test helper; production passes an explicit ID.
    pub fn append(&self, world_path: &Path) -> io::Result<()> {
        self.append_with_event_id(world_path, &generated_event_id()?)
            .map(|_| ())
    }

    /// Return true for a new append, false when this ID already appears in
    /// the bounded recent-record window. No changes to scoreboard or drops.
    pub fn append_with_event_id(&self, world_path: &Path, id: &str) -> io::Result<bool> {
        if !valid_event_id(id) {
            return Err(io::Error::new(ErrorKind::InvalidInput, "invalid death event ID"));
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "invalid system clock"))?
            .as_millis();
        let mut payload = self.json(timestamp_ms);
        let fields = payload.as_object_mut().ok_or_else(|| {
            io::Error::new(ErrorKind::InvalidData, "death event must be an object")
        })?;
        fields.insert("event_id".to_string(), Value::String(id.to_string()));
        let mut line = serde_json::to_vec(&payload)?;
        if line.len() >= MAX_RECORD_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "oversized death event",
            ));
        }
        line.push(b'\n');
        let _guard = DEATH_FILE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| io::Error::other("death file lock poisoned"))?;
        let path = Self::log_path(world_path);
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        reconcile_jsonl_tail(&mut file)?;
        if recent_event_present(&mut file, id, self)? {
            return Ok(false);
        }
        let previous_len = file.metadata()?.len();
        if let Err(error) = file.write_all(&line) {
            // Preserve the pre-append length if a short write is reported.
            // The process-local mutex also protects this rollback.
            if let Err(rollback_error) = file.set_len(previous_len) {
                return Err(io::Error::other(format!(
                    "death log write failed: {error}; rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
        // Sync the new line before reporting success to the player session.
        // This is not an atomic transaction with player HP, items, or scores.
        file.sync_data()?;
        Ok(true)
    }

    pub fn console_summary(&self) -> String {
        let health = self.hit.map_or_else(
            || "unknown".to_string(),
            |hit| format!("{:.1}", hit.previous_health),
        );
        let damage = self.hit.map_or_else(
            || "unknown".to_string(),
            |hit| format!("{:.1}", hit.applied_damage),
        );
        format!(
            "[death] player={} cause={} source_type={:?} source_entity_id={:?} projectile_type={:?} dimension={} x={:.2} y={:.2} z={:.2} health_before={} applied_damage={} tick={} message={:?}",
            self.player.escape_debug(),
            self.cause().code(),
            self.cause().source_type(),
            self.cause().source_entity_id(),
            self.cause().projectile_type(),
            self.dimension,
            self.position.x,
            self.position.y,
            self.position.z,
            health,
            damage,
            self.tick,
            self.chat_message()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_cause_is_recorded_only_once_and_only_at_zero_health() {
        let mut pending = None;
        record_fatal(&mut pending, 20.0, 16.0, DeathCause::CombatOrExplosion);
        assert!(pending.is_none());
        record_fatal(&mut pending, 4.0, 0.0, DeathCause::Fall);
        record_fatal(&mut pending, 0.0, 0.0, DeathCause::Unknown);
        assert_eq!(pending.unwrap().cause, DeathCause::Fall);
        assert_eq!(pending.unwrap().previous_health, 4.0);
    }

    #[test]
    fn messages_and_machine_log_keep_cause_and_escaped_player_name() {
        let record = DeathRecord {
            player: "name\\\"\nforge",
            hit: Some(FatalHit {
                cause: DeathCause::Drowning,
                previous_health: 2.0,
                applied_damage: 2.0,
            }),
            position: PlayerPosition::default(),
            dimension: -1,
            tick: 234,
        };
        assert!(record.chat_message().ends_with("drowned"));
        let data = record.json(123);
        assert_eq!(data["cause"], "drowning");
        assert_eq!(data["dimension"], -1);
        assert_eq!(data["schema_version"], 1);
        assert_eq!(data["death_message"], record.chat_message());
        assert_eq!(data["server_tick"], 234);
        // Console is a single line even when a supplied name contains controls.
        let console = record.console_summary();
        assert!(!console.contains('\n'));
        assert!(!console.contains('\r'));
        assert!(console.contains("message="));
        let wire = serde_json::to_string(&data).unwrap();
        assert!(!wire.contains('\n'));
        let decoded: Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded["player"], record.player);
    }

    #[test]
    fn jsonl_appends_one_valid_line_per_death() {
        let world = std::env::temp_dir().join(format!(
            "ir1710-deaths-{}-{:?}.irw",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = DeathRecord::log_path(&world);
        let _ = fs::remove_file(&path);
        let record = DeathRecord {
            player: "Alex",
            hit: Some(FatalHit {
                cause: DeathCause::Fall,
                previous_health: 3.0,
                applied_damage: 3.0,
            }),
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 42,
        };
        record.append(&world).unwrap();
        record.append(&world).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        let entries: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["cause"], "fall");
        assert_eq!(entries[0]["player"], "Alex");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn multiple_hazards_are_not_mislabeled_as_combat() {
        assert_eq!(DeathCause::MixedHazards.code(), "mixed_environmental");
        assert_eq!(
            DeathCause::MixedHazards.message("Alex"),
            "Alex died from multiple environmental hazards"
        );
    }

    #[test]
    fn block_positions_floor_negative_coordinates_without_fabricating_invalid_values() {
        let mut record = DeathRecord {
            player: "Alex",
            hit: Some(FatalHit {
                cause: DeathCause::Fall,
                previous_health: 4.0,
                applied_damage: 4.0,
            }),
            position: PlayerPosition {
                x: -0.01,
                y: 64.99,
                z: -12.0,
            },
            dimension: 0,
            tick: 9,
        };
        let data = record.json(321);
        assert_eq!(data["block_position"]["x"], -1);
        assert_eq!(data["block_position"]["y"], 64);
        assert_eq!(data["block_position"]["z"], -12);
        assert_eq!(data["death_message"], "Alex fell from a high place");
        assert_eq!(data["timestamp_ms"], 321);

        record.position.x = f64::NAN;
        record.position.z = f64::INFINITY;
        let invalid = record.json(322);
        assert!(invalid["block_position"]["x"].is_null());
        assert!(invalid["block_position"]["z"].is_null());
        assert_eq!(invalid["block_position"]["y"], 64);
    }

    fn sample_record() -> DeathRecord<'static> {
        DeathRecord {
            player: "Alex",
            hit: Some(FatalHit {
                cause: DeathCause::Fall,
                previous_health: 3.0,
                applied_damage: 3.0,
            }),
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 42,
        }
    }

    fn test_log_path(tag: &str) -> (PathBuf, PathBuf) {
        let world = std::env::temp_dir().join(format!(
            "ir1710-deaths-{tag}-{}-{:?}.irw",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = DeathRecord::log_path(&world);
        let _ = fs::remove_file(&path);
        (world, path)
    }

    #[test]
    fn death_log_repairs_complete_unterminated_entry_without_losing_it() {
        let (world, path) = test_log_path("missing-lf");
        let old = sample_record().json(100).to_string();
        fs::write(&path, old.as_bytes()).unwrap();
        sample_record().append(&world).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let entries: Vec<Value> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["timestamp_ms"], 100);
        assert_eq!(entries[1]["event"], "player_death");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn death_log_discards_only_an_incomplete_final_entry() {
        let (world, path) = test_log_path("short-tail");
        let old = sample_record().json(101).to_string();
        fs::write(&path, format!("{old}\n{{\"event\":\"player_death\"")).unwrap();
        sample_record().append(&world).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(serde_json::from_str::<Value>(lines[0]).unwrap()["timestamp_ms"], 101);
        assert_eq!(serde_json::from_str::<Value>(lines[1]).unwrap()["event"], "player_death");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn death_log_refuses_an_unbounded_tail_without_truncation() {
        let (world, path) = test_log_path("large-tail");
        let original = vec![b'x'; MAX_RECORD_BYTES + 1];
        fs::write(&path, &original).unwrap();
        assert!(sample_record().append(&world).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_deaths_append_separate_json_lines() {
        let (world, path) = test_log_path("concurrent");
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let world = world.clone();
                std::thread::spawn(move || {
                    for _ in 0..3 {
                        sample_record().append(&world).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let raw = fs::read_to_string(&path).unwrap();
        let entries: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries.len(), 12);
        assert!(entries.iter().all(|event| event["player"] == "Alex"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn event_id_deduplicates_retries_after_reopening_and_interleaved_records() {
        let (world, path) = test_log_path("idempotency");
        let record = sample_record();
        assert!(record.append_with_event_id(&world, "same-event-001").unwrap());
        assert!(!record.append_with_event_id(&world, "same-event-001").unwrap());
        assert!(record.append_with_event_id(&world, "another-event-002").unwrap());
        assert!(!record.append_with_event_id(&world, "same-event-001").unwrap());
        let lines: Vec<Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event_id"], "same-event-001");
        assert_eq!(lines[1]["event_id"], "another-event-002");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn event_id_collision_cannot_hide_a_different_players_death() {
        let (world, path) = test_log_path("collision");
        sample_record()
            .append_with_event_id(&world, "shared-id-001")
            .unwrap();
        let other = DeathRecord {
            player: "Steve",
            hit: None,
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 123,
        };
        assert!(other.append_with_event_id(&world, "shared-id-001").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
        assert!(other.append_with_event_id(&world, "invalid id").is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn retry_repairs_lost_newline_then_recognizes_existing_event() {
        let (world, path) = test_log_path("retry-missing-lf");
        let record = sample_record();
        assert!(record.append_with_event_id(&world, "previous-event-001").unwrap());
        let mut bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.pop(), Some(b'\n'));
        fs::write(&path, &bytes).unwrap();
        // The existing JSON is complete. Repair its newline, then deduplicate.
        assert!(!record.append_with_event_id(&world, "previous-event-001").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
        assert_eq!(fs::read(&path).unwrap().last(), Some(&b'\n'));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn complete_non_death_json_tail_is_not_silently_accepted() {
        let (world, path) = test_log_path("foreign-json");
        fs::write(&path, b"{\"other\":true}").unwrap();
        assert!(sample_record().append(&world).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{\"other\":true}");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn death_log_destination_directory_reports_error_without_panic() {
        let (world, path) = test_log_path("directory");
        fs::create_dir_all(&path).unwrap();
        assert!(sample_record().append(&world).is_err());
        fs::remove_dir(&path).unwrap();
    }

    #[test]
    fn verified_attacker_stays_consistent_across_chat_and_json() {
        let death = DeathRecord {
            player: "Alex",
            hit: Some(FatalHit {
                cause: DeathCause::Mob {
                    kind: "Zombie",
                    entity_id: 500_002,
                },
                previous_health: 2.0,
                applied_damage: 2.0,
            }),
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 123,
        };
        assert_eq!(death.chat_message(), "Alex was slain by Zombie");
        assert_eq!(death.json(0)["source_type"], "Zombie");
        assert_eq!(death.json(0)["source_entity_id"], 500_002);
        assert_eq!(death.json(0)["death_message"], death.chat_message());
        assert!(death.console_summary().contains("source_entity_id=Some(500002)"));
        assert_eq!(death.json(0)["projectile_type"], Value::Null);
        let arrow = DeathCause::Projectile {
            kind: "Arrow",
            shooter_kind: Some("Skeleton"),
            shooter_id: Some(901),
        };
        assert_eq!(arrow.projectile_type(), Some("Arrow"));
        assert_eq!(arrow.source_type(), Some("Skeleton"));
        assert_eq!(arrow.source_entity_id(), Some(901));
        assert_eq!(DeathCause::Mixed.source_entity_id(), None);
        assert_eq!(DeathCause::Mixed.code(), "mixed_damage");
        assert_eq!(DeathCause::MixedHazards.code(), "mixed_environmental");
    }

    #[test]
    fn projectile_provenance_survives_tail_repair_and_idempotent_retry() {
        let (world, path) = test_log_path("projectile-id-replay");
        let death = DeathRecord {
            player: "Alex",
            hit: Some(FatalHit {
                cause: DeathCause::Projectile {
                    kind: "Arrow",
                    shooter_kind: Some("Skeleton"),
                    shooter_id: Some(500_002),
                },
                previous_health: 2.0,
                applied_damage: 2.0,
            }),
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 123,
        };
        assert_eq!(death.chat_message(), "Alex was shot by Skeleton");
        assert!(death.append_with_event_id(&world, "projectile-death-001").unwrap());

        // Model a crash between the JSON write and the trailing delimiter.
        let mut bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.pop(), Some(b'\n'));
        fs::write(&path, bytes).unwrap();

        // The same ID must recover the delimiter, not add a second death.
        assert!(!death.append_with_event_id(&world, "projectile-death-001").unwrap());
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'));
        assert_eq!(raw.lines().count(), 1);
        let value: Value = serde_json::from_str(raw.trim_end()).unwrap();
        assert_eq!(value["event"], "player_death");
        assert_eq!(value["event_id"], "projectile-death-001");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["cause"], "projectile");
        assert_eq!(value["source_type"], "Skeleton");
        assert_eq!(value["source_entity_id"], 500_002);
        assert_eq!(value["projectile_type"], "Arrow");
        assert_eq!(value["death_message"], death.chat_message());
        assert!(value["block_position"].is_object());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_dead_player_has_safe_fallback_without_fabricated_cause() {
        let record = DeathRecord {
            player: "Alex",
            hit: None,
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 1,
        };
        assert_eq!(record.chat_message(), "Alex died");
        assert_eq!(record.json(0)["health_before"], Value::Null);
    }
}
