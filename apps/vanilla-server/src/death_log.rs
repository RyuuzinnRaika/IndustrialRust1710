//! Protocol 5 player-death announcements and diagnostic, per-world JSONL records.
//! This is observability only: scoring, drops and respawn remain session-owned.
use crate::player::PlayerPosition;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_RECORD_BYTES: usize = 2048;
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
    Mob { kind: &'static str, entity_id: i32 },
    Projectile {
        kind: &'static str,
        shooter_kind: Option<&'static str>,
        shooter_id: Option<i32>,
    },
    Explosion { kind: &'static str, source_id: Option<i32> },
    WitherEffect,
    Mixed,
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
            Self::DispenserArrow => "dispenser_arrow",
            Self::EnderPearl => "ender_pearl",
            Self::Unknown => "unknown",
        }
    }

    /// Return only sources confirmed by the server; never infer a name from
    /// the death position or a nearby entity.
    pub fn source_type(self) -> Option<&'static str> {
        match self {
            Self::Mob { kind, .. } | Self::Explosion { kind, .. } => Some(kind),
            Self::Projectile { shooter_kind: Some(kind), .. } => Some(kind),
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

    /// Chat text follows the familiar Java 1.7.10 death-message vocabulary.
    /// Do not claim a specific killer when the combat event does not carry one.
    pub fn message(self, name: &str) -> String {
        match self {
            Self::Mob { kind, .. } => return format!("{name} was slain by {kind}"),
            Self::Projectile { shooter_kind: Some(kind), .. } => {
                return format!("{name} was shot by {kind}");
            }
            Self::Projectile { kind, .. } => {
                return format!("{name} was killed by {kind}");
            }
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
            "source_type": self.cause().source_type(),
            "source_entity_id": self.cause().source_entity_id(),
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
        file.write_all(&line)
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
    fn server_confirmed_attacker_is_present_in_chat_and_structured_log() {
        let death = DeathRecord {
            player: "Alex",
            hit: Some(FatalHit {
                cause: DeathCause::Mob { kind: "Zombie", entity_id: 500_002 },
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
        let mixed = DeathCause::Mixed;
        assert_eq!(mixed.source_entity_id(), None);
        assert_eq!(mixed.code(), "mixed_damage");
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
