//! Protocol 5 scoreboard state, persistence and packet encoding.
//! Console code is the only mutation authority while online accounts are unauthenticated.
use crate::multiplayer::Event;
use legacy_protocol::write_string;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const MAX_SIZE: u64 = 128 * 1024;
fn invalid(m: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, m)
}
fn token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn event(id: u32, body: Vec<u8>) -> Event {
    Event { id, body }
}
fn string(value: &str, body: &mut Vec<u8>) -> io::Result<()> {
    write_string(value, body)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Objective {
    display: String,
    criterion: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Team {
    display: String,
    prefix: String,
    suffix: String,
    friendly_fire: u8,
    players: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scoreboard {
    version: u8,
    objectives: BTreeMap<String, Objective>,
    // BTreeMap objective -> (holder -> signed score)
    scores: BTreeMap<String, BTreeMap<String, i32>>,
    teams: BTreeMap<String, Team>,
    displays: [Option<String>; 3],
}
impl Default for Scoreboard {
    fn default() -> Self {
        Self {
            version: 1,
            objectives: BTreeMap::new(),
            scores: BTreeMap::new(),
            teams: BTreeMap::new(),
            displays: [None, None, None],
        }
    }
}
impl Scoreboard {
    pub fn location(world: &Path) -> PathBuf {
        world.with_extension("scoreboard.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != 1 || self.objectives.len() > 128 || self.teams.len() > 128 {
            return Err(invalid("invalid scoreboard version or size"));
        }
        for (name, obj) in &self.objectives {
            if !token(name, 16)
                || !matches!(obj.criterion.as_str(), "dummy" | "deathCount")
                || obj.display.chars().count() > 32
            {
                return Err(invalid("invalid scoreboard objective"));
            }
        }
        let mut membership = BTreeSet::new();
        for (name, team) in &self.teams {
            if !token(name, 16)
                || team.display.chars().count() > 32
                || team.prefix.chars().count() > 16
                || team.suffix.chars().count() > 16
                || team.friendly_fire > 3
                || team.players.len() > 256
            {
                return Err(invalid("invalid scoreboard team"));
            }
            for player in &team.players {
                if !token(player, 40) || !membership.insert(player.to_ascii_lowercase()) {
                    return Err(invalid("duplicate or invalid scoreboard team membership"));
                }
            }
        }
        for (name, holders) in &self.scores {
            if !self.objectives.contains_key(name)
                || holders.len() > 4096
                || holders.keys().any(|key| !token(key, 40))
            {
                return Err(invalid("invalid scoreboard score"));
            }
        }
        for entry in self.displays.iter().flatten() {
            if !self.objectives.contains_key(entry) {
                return Err(invalid("dangling scoreboard display"));
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let md = match fs::symlink_metadata(path) {
            Ok(v) => v,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if !md.file_type().is_file() || md.len() > MAX_SIZE {
            return Err(invalid("invalid scoreboard file"));
        }
        let state: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
        state.validate()?;
        Ok(state)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() as u64 > MAX_SIZE {
            return Err(invalid("scoreboard too large"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("scoreboard.tmp");
        let mut out = File::create(&tmp)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        drop(out);
        fs::rename(tmp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
    fn objective_packet(name: &str, display: &str, action: u8) -> io::Result<Event> {
        let mut body = Vec::new();
        string(name, &mut body)?;
        string(display, &mut body)?;
        body.push(action);
        Ok(event(0x3b, body))
    }
    fn score_packet(name: &str, objective: &str, value: i32) -> io::Result<Event> {
        let mut body = Vec::new();
        string(name, &mut body)?;
        body.push(0);
        string(objective, &mut body)?;
        body.extend_from_slice(&value.to_be_bytes());
        Ok(event(0x3c, body))
    }
    /// Protocol 5 score removal omits objective and value; it removes all
    /// scores held by this name across every objective on the client.
    fn reset_score_packet(holder: &str) -> io::Result<Event> {
        let mut body = Vec::new();
        string(holder, &mut body)?;
        body.push(1);
        Ok(event(0x3c, body))
    }
    fn display_packet(slot: usize, objective: &str) -> io::Result<Event> {
        let mut body = vec![slot as u8];
        string(objective, &mut body)?;
        Ok(event(0x3d, body))
    }
    fn team_packet(name: &str, team: &Team) -> io::Result<Event> {
        let mut body = Vec::new();
        string(name, &mut body)?;
        body.push(0);
        string(&team.display, &mut body)?;
        string(&team.prefix, &mut body)?;
        string(&team.suffix, &mut body)?;
        body.push(team.friendly_fire);
        body.extend_from_slice(&(team.players.len() as i16).to_be_bytes());
        for player in &team.players {
            string(player, &mut body)?;
        }
        Ok(event(0x3e, body))
    }
    pub fn snapshot(&self) -> io::Result<Vec<Event>> {
        self.validate()?;
        let mut packets = Vec::new();
        for (name, obj) in &self.objectives {
            packets.push(Self::objective_packet(name, &obj.display, 0)?);
        }
        for (name, holders) in &self.scores {
            for (holder, value) in holders {
                packets.push(Self::score_packet(holder, name, *value)?);
            }
        }
        for (slot, entry) in self.displays.iter().enumerate() {
            if let Some(name) = entry {
                packets.push(Self::display_packet(slot, name)?);
            }
        }
        for (name, team) in &self.teams {
            packets.push(Self::team_packet(name, team)?);
        }
        Ok(packets)
    }
    pub fn add_objective(
        &mut self,
        name: &str,
        display: &str,
        criterion: &str,
    ) -> io::Result<Vec<Event>> {
        if !token(name, 16)
            || display.chars().count() > 32
            || !matches!(criterion, "dummy" | "deathCount")
            || self.objectives.len() >= 128
            || self.objectives.contains_key(name)
        {
            return Err(invalid("invalid or existing objective"));
        }
        let packet = Self::objective_packet(name, display, 0)?;
        self.objectives.insert(
            name.into(),
            Objective {
                display: display.into(),
                criterion: criterion.into(),
            },
        );
        Ok(vec![packet])
    }
    pub fn remove_objective(&mut self, name: &str) -> io::Result<Vec<Event>> {
        let objective = self
            .objectives
            .get(name)
            .ok_or_else(|| invalid("unknown scoreboard objective"))?;
        let remove = Self::objective_packet(name, &objective.display, 1)?;
        let mut events = Vec::new();
        for slot in 0..self.displays.len() {
            if self.displays[slot].as_deref() == Some(name) {
                events.push(Self::display_packet(slot, "")?);
            }
        }
        events.push(remove);
        self.objectives.remove(name);
        self.scores.remove(name);
        for slot in &mut self.displays {
            if slot.as_deref() == Some(name) {
                *slot = None;
            }
        }
        Ok(events)
    }
    pub fn reset_player_scores(&mut self, holder: &str) -> io::Result<Vec<Event>> {
        if !token(holder, 40) {
            return Err(invalid("invalid scoreboard score holder"));
        }
        if !self.scores.values().any(|scores| scores.contains_key(holder)) {
            return Err(invalid("score holder has no scores"));
        }
        let packet = Self::reset_score_packet(holder)?;
        for scores in self.scores.values_mut() {
            scores.remove(holder);
        }
        Ok(vec![packet])
    }
    pub fn set_score(&mut self, holder: &str, name: &str, value: i32) -> io::Result<Vec<Event>> {
        if !token(holder, 40) || !self.objectives.contains_key(name) {
            return Err(invalid("unknown objective or player"));
        }
        let packet = Self::score_packet(holder, name, value)?;
        self.scores
            .entry(name.into())
            .or_default()
            .insert(holder.into(), value);
        Ok(vec![packet])
    }
    pub fn set_display(&mut self, slot: usize, name: Option<&str>) -> io::Result<Vec<Event>> {
        if slot >= 3 || name.is_some_and(|n| !self.objectives.contains_key(n)) {
            return Err(invalid("invalid objective display"));
        }
        let packet = Self::display_packet(slot, name.unwrap_or(""))?;
        self.displays[slot] = name.map(str::to_owned);
        Ok(vec![packet])
    }
    pub fn add_team(&mut self, name: &str, display: &str) -> io::Result<Vec<Event>> {
        if !token(name, 16)
            || display.chars().count() > 32
            || self.teams.contains_key(name)
            || self.teams.len() >= 128
        {
            return Err(invalid("invalid or existing team"));
        }
        let team = Team {
            display: display.into(),
            prefix: String::new(),
            suffix: String::new(),
            friendly_fire: 3,
            players: BTreeSet::new(),
        };
        let packet = Self::team_packet(name, &team)?;
        self.teams.insert(name.into(), team);
        Ok(vec![packet])
    }
    pub fn remove_team(&mut self, name: &str) -> io::Result<Vec<Event>> {
        if !self.teams.contains_key(name) {
            return Err(invalid("unknown scoreboard team"));
        }
        let mut body = Vec::new();
        string(name, &mut body)?;
        body.push(1); // Remove team
        self.teams.remove(name);
        Ok(vec![event(0x3e, body)])
    }
    pub fn remove_player_from_team(&mut self, name: &str, holder: &str) -> io::Result<Vec<Event>> {
        if !token(holder, 40) || !self.teams.get(name).is_some_and(|t| t.players.contains(holder)) {
            return Err(invalid("unknown member of scoreboard team"));
        }
        let mut body = Vec::new();
        string(name, &mut body)?;
        body.push(4); // Remove players
        body.extend_from_slice(&1i16.to_be_bytes());
        string(holder, &mut body)?;
        self.teams.get_mut(name).unwrap().players.remove(holder);
        Ok(vec![event(0x3e, body)])
    }
    pub fn add_player_to_team(&mut self, name: &str, holder: &str) -> io::Result<Vec<Event>> {
        if !token(holder, 40) || self.teams.values().any(|t| t.players.contains(holder)) {
            return Err(invalid("invalid or already assigned player"));
        }
        let team = self
            .teams
            .get_mut(name)
            .ok_or_else(|| invalid("unknown team"))?;
        team.players.insert(holder.into());
        let mut body = Vec::new();
        string(name, &mut body)?;
        body.push(3);
        body.extend_from_slice(&1i16.to_be_bytes());
        string(holder, &mut body)?;
        Ok(vec![event(0x3e, body)])
    }
    pub fn record_death(&mut self, player: &str) -> io::Result<Vec<Event>> {
        let objectives: Vec<String> = self
            .objectives
            .iter()
            .filter(|(_, o)| o.criterion == "deathCount")
            .map(|(name, _)| name.clone())
            .collect();
        let mut out = Vec::new();
        for objective in objectives {
            let value = self
                .scores
                .get(&objective)
                .and_then(|v| v.get(player))
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            out.extend(self.set_score(player, &objective, value)?);
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoreboards_sync_and_survive_restart() {
        let mut board = Scoreboard::default();
        assert_eq!(
            board
                .add_objective("death", "Deaths", "deathCount")
                .unwrap()[0]
                .id,
            0x3b
        );
        assert!(board.add_objective("death", "Duplicate", "dummy").is_err());
        assert_eq!(board.record_death("Alice").unwrap()[0].id, 0x3c);
        assert_eq!(
            board.record_death("Alice").unwrap()[0].body.last(),
            Some(&2)
        );
        board.set_display(1, Some("death")).unwrap();
        board.add_team("blue", "Blue").unwrap();
        board.add_player_to_team("blue", "Alice").unwrap();
        assert!(board.add_player_to_team("blue", "Alice").is_err());
        assert!(board.snapshot().unwrap().iter().any(|e| e.id == 0x3e));
        assert_eq!(board.remove_player_from_team("blue", "Alice").unwrap()[0].id, 0x3e);
        assert!(board.remove_player_from_team("blue", "Alice").is_err());
        board.add_player_to_team("blue", "Alice").unwrap();
        assert_eq!(board.remove_team("blue").unwrap()[0].id, 0x3e);
        assert!(board.remove_team("blue").is_err());
        let path = std::env::temp_dir().join(format!(
            "ir1710-scoreboard-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        board.save(&path).unwrap();
        assert_eq!(
            Scoreboard::load(&path).unwrap().snapshot().unwrap().len(),
            board.snapshot().unwrap().len()
        );
        let reset = board.reset_player_scores("Alice").unwrap();
        assert_eq!(reset.len(), 1);
        assert_eq!(reset[0].id, 0x3c);
        // Removal is the last byte of the Protocol 5 reset-score packet.
        assert_eq!(reset[0].body.last(), Some(&1));
        assert!(board.reset_player_scores("Alice").is_err());
        let removed = board.remove_objective("death").unwrap();
        assert_eq!(removed.len(), 2); // clear sidebar before removing objective
        assert_eq!(removed[0].id, 0x3d);
        assert_eq!(removed[1].id, 0x3b);
        assert!(board.remove_objective("death").is_err());
        assert!(board.snapshot().unwrap().is_empty());
        fs::remove_file(path).unwrap();
    }
}
