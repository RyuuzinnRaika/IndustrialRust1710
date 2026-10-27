//! Protocol 5 player-death announcements and diagnostic, per-world JSONL records.
//! This is observability only: scoring, drops and respawn remain session-owned.
use crate::player::PlayerPosition;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_RECORD_BYTES: usize = 2048;
const MAX_OUTBOX_BYTES: u64 = 8 * 1024;
const MAX_LOG_SCAN_BYTES: u64 = 64 * 1024 * 1024;
// A single write_all remains serialized across all concurrent player sessions.
static DEATH_FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
            Self::DispenserArrow => "dispenser_arrow",
            Self::EnderPearl => "ender_pearl",
            Self::Unknown => "unknown",
        }
    }

    /// Chat text follows the familiar Java 1.7.10 death-message vocabulary.
    /// Do not claim a specific killer when the combat event does not carry one.
    pub fn message(self, name: &str) -> String {
        let description = match self {
            Self::Fall => "fell from a high place",
            Self::Starvation => "starved to death",
            Self::Drowning => "drowned",
            Self::Suffocation => "suffocated in a wall",
            Self::Lava => "tried to swim in lava",
            Self::Fire => "went up in flames",
            Self::Burning => "burned to death",
            Self::CombatOrExplosion => "was killed in combat or an explosion",
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
pub fn record_fatal(
    pending: &mut Option<FatalHit>,
    before: f32,
    after: f32,
    cause: DeathCause,
) {
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
            "timestamp_ms": timestamp_ms,
            "player": self.player,
            "cause": self.cause().code(),
            "dimension": self.dimension,
            "position": {
                "x": self.position.x,
                "y": self.position.y,
                "z": self.position.z
            },
            "health_before": self.hit.map(|hit| hit.previous_health),
            "applied_damage": self.hit.map(|hit| hit.applied_damage),
            "server_tick": self.tick
        })
    }

    pub fn log_path(world_path: &Path) -> PathBuf {
        world_path.with_extension("deaths.jsonl")
    }

    /// One append per death; failures are reported by the caller but never
    /// prevent a dead player's normal drops/respawn path.
    pub fn append(&self, world_path: &Path) -> io::Result<()> {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "invalid system clock"))?
            .as_millis();
        let mut line = serde_json::to_vec(&self.json(timestamp_ms))?;
        if line.len() >= MAX_RECORD_BYTES {
            return Err(io::Error::new(ErrorKind::InvalidData, "oversized death event"));
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
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        let previous_len = file.metadata()?.len();
        if let Err(error) = file.write_all(&line) {
            // A short write must not leave an invalid JSON fragment in the
            // middle of the next player's event. Keep the original error.
            let _ = file.set_len(previous_len);
            return Err(error);
        }
        // Flush the append before returning to the owning player session.
        // Not an atomic transaction with player NBT or the death scoreboard.
        file.sync_data()
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
            "[death] player={} cause={} dimension={} x={:.2} y={:.2} z={:.2} health_before={} applied_damage={} tick={}",
            self.player,
            self.cause().code(),
            self.dimension,
            self.position.x,
            self.position.y,
            self.position.z,
            health,
            damage,
            self.tick
        )
    }
}


/// A crash-recoverable, per-player *last event* outbox. This does not claim
/// atomicity with the NBT player save or deathCount scoreboard. A single
/// fully-written outbox event is replayable after a process crash.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDeath {
    version: u8,
    event_id: String,
    world_file: String,
    message: String,
    entry: Value,
}

fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, detail)
}

fn outbox_path(player_path: &Path) -> PathBuf {
    player_path.with_extension("death-outbox.json")
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn regular_file(path: &Path, limit: u64) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() && meta.len() <= limit => Ok(true),
        Ok(_) => Err(invalid("unsafe or oversized death log file")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn valid_world_relative(path: &Path) -> bool {
    path.components().count() <= 3
        && path.components().count() > 0
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.extension().is_some_and(|extension| extension == "irw")
}

impl StoredDeath {
    fn new(
        record: &DeathRecord<'_>,
        root_world_path: &Path,
        death_world_path: &Path,
    ) -> io::Result<Self> {
        let base = root_world_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let relative = death_world_path
            .strip_prefix(base)
            .map_err(|_| invalid("death world path outside root world"))?;
        if !valid_world_relative(relative) {
            return Err(invalid("unsafe relative death world path"));
        }
        let world_file = relative
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 death world path"))?
            .to_string();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("invalid system clock"))?
            .as_millis();
        let event_id = format!("{:032x}", rand::random::<u128>());
        let mut entry = record.json(timestamp);
        entry["event_id"] = Value::String(event_id.clone());
        let created = Self {
            version: 1,
            event_id,
            world_file,
            message: record.chat_message(),
            entry,
        };
        created.validate()?;
        Ok(created)
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != 1
            || self.event_id.len() != 32
            || !self.event_id.bytes().all(|c| c.is_ascii_hexdigit())
            || self.entry.get("event_id").and_then(Value::as_str)
                != Some(self.event_id.as_str())
            || self.entry.get("event").and_then(Value::as_str) != Some("player_death")
            || self.message.len() > 512
            || !valid_world_relative(Path::new(&self.world_file))
        {
            return Err(invalid("invalid persisted death event"));
        }
        Ok(())
    }

    fn read(path: &Path) -> io::Result<Option<Self>> {
        if !regular_file(path, MAX_OUTBOX_BYTES)? {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_OUTBOX_BYTES {
            return Err(invalid("oversized death outbox"));
        }
        let stored: Self =
            serde_json::from_slice(&bytes).map_err(|_| invalid("corrupt death outbox"))?;
        stored.validate()?;
        Ok(Some(stored))
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let payload = serde_json::to_vec(self)?;
        if payload.len() as u64 > MAX_OUTBOX_BYTES {
            return Err(invalid("oversized death event"));
        }
        if regular_file(path, MAX_OUTBOX_BYTES)? {
            // Existing outbox must already have been replayed before replacement.
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = path.with_extension("death-outbox.tmp");
        // Do not follow arbitrary symlink placeholders during atomic rename.
        if regular_file(&temp, MAX_OUTBOX_BYTES)? {
            fs::remove_file(&temp)?;
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        out.write_all(&payload)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&temp, path)?;
        sync_dir(parent)
    }

    fn append_once(&self, root_world_path: &Path) -> io::Result<bool> {
        self.validate()?;
        let base = root_world_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let log_path = base.join(&self.world_file).with_extension("deaths.jsonl");
        let _guard = DEATH_FILE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| invalid("death file lock poisoned"))?;
        if !regular_file(&log_path, MAX_LOG_SCAN_BYTES)? {
            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&log_path)?;
        let size = file.metadata()?.len();
        if size > MAX_LOG_SCAN_BYTES {
            return Err(invalid("death log exceeds bounded replay scan"));
        }
        let mut offset = 0u64;
        let mut torn_tail = false;
        {
            let mut reader = BufReader::new(&file);
            loop {
                let mut line = Vec::new();
                let read = reader.read_until(b'\n', &mut line)?;
                if read == 0 {
                    break;
                }
                if line.len() > MAX_RECORD_BYTES {
                    return Err(invalid("invalid oversized death log entry"));
                }
                if !line.ends_with(b"\n") {
                    torn_tail = true;
                    break;
                }
                let value: Value = serde_json::from_slice(&line)
                    .map_err(|_| invalid("corrupt existing death log entry"))?;
                if value.get("event_id").and_then(Value::as_str)
                    == Some(self.event_id.as_str())
                {
                    return Ok(false);
                }
                offset += read as u64;
            }
        }
        if torn_tail {
            // A crash may interrupt the last append. Only discard the
            // incomplete trailing fragment; never silently alter valid rows.
            file.set_len(offset)?;
            file.sync_data()?;
        }
        let mut encoded = serde_json::to_vec(&self.entry)?;
        if encoded.len() >= MAX_RECORD_BYTES {
            return Err(invalid("oversized JSONL death event"));
        }
        encoded.push(b'\n');
        if offset.saturating_add(encoded.len() as u64) > MAX_LOG_SCAN_BYTES {
            return Err(invalid("death log rollover required"));
        }
        file.seek(SeekFrom::End(0))?;
        let old_len = file.metadata()?.len();
        if let Err(error) = file.write_all(&encoded) {
            let _ = file.set_len(old_len);
            return Err(error);
        }
        file.sync_data()?;
        Ok(true)
    }
}

/// Durable outbox before JSONL publication. The outbox is saved first so a
/// later logging failure or process crash can be recovered on server startup.
/// The gameplay caller handles failures without suppressing ordinary respawn.
pub fn append_recoverable(
    record: &DeathRecord<'_>,
    root_world_path: &Path,
    death_world_path: &Path,
    player_path: &Path,
) -> io::Result<()> {
    let path = outbox_path(player_path);
    if let Some(previous) = StoredDeath::read(&path)? {
        previous.append_once(root_world_path)?;
    }
    let event = StoredDeath::new(record, root_world_path, death_world_path)?;
    event.save(&path)?;
    event.append_once(root_world_path)?;
    Ok(())
}

/// Before admitting any player, replay every prepared per-player event. An
/// event_id de-duplicates a write that completed but crashed before cleanup.
/// This is log-only replay; it does not touch inventories or scoreboards.
pub fn recover_pending(root_world_path: &Path) -> io::Result<usize> {
    let base = root_world_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let players = base.join("players");
    let mut replayed = 0usize;
    for directory in [players.clone(), players.join("verified")] {
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(meta) if meta.file_type().is_dir() => {}
            _ => return Err(invalid("unsafe players directory for death recovery")),
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file = entry.file_name();
            let Some(filename) = file.to_str() else {
                continue;
            };
            if !filename.ends_with(".death-outbox.json") {
                continue;
            }
            let path = entry.path();
            let saved = StoredDeath::read(&path)?
                .ok_or_else(|| invalid("disappeared death outbox"))?;
            if saved.append_once(root_world_path)? {
                replayed += 1;
            }
        }
    }
    Ok(replayed)
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
        assert_eq!(data["server_tick"], 234);
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
    fn a_broken_diagnostic_destination_returns_error_not_panic() {
        let world = std::env::temp_dir().join(format!(
            "ir1710-death-failed-{}-{:?}.irw",
            std::process::id(),
            std::thread::current().id()
        ));
        let log_dir = DeathRecord::log_path(&world);
        let _ = fs::remove_file(&log_dir);
        fs::create_dir_all(&log_dir).unwrap();
        let record = DeathRecord {
            player: "Alex",
            hit: None,
            position: PlayerPosition::default(),
            dimension: 0,
            tick: 3,
        };
        assert!(record.append(&world).is_err());
        fs::remove_dir(&log_dir).unwrap();
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
