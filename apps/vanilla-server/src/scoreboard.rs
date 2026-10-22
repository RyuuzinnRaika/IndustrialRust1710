//! Bounded server-authoritative scoreboard model for Protocol 5.
//! Client packet serialization and connection lifecycle hooks are not yet wired.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_OBJECTIVES: usize = 64;
pub const MAX_TEAMS: usize = 64;
pub const MAX_SCORES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Objective {
    pub display_name: String,
    pub criterion: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub display_name: String,
    pub prefix: String,
    pub suffix: String,
    pub members: BTreeSet<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scoreboard {
    pub objectives: BTreeMap<String, Objective>,
    pub scores: BTreeMap<(String, String), i32>,
    pub teams: BTreeMap<String, Team>,
    pub displays: BTreeMap<u8, String>,
}
fn valid_name(name: &str, max: usize) -> bool {
    !name.is_empty() && name.len() <= max && !name.chars().any(char::is_control)
}
impl Scoreboard {
    pub fn add_objective(
        &mut self,
        name: &str,
        display_name: &str,
        criterion: &str,
    ) -> Result<(), &'static str> {
        if !valid_name(name, 16) || !valid_name(display_name, 32) || !valid_name(criterion, 32) {
            return Err("invalid objective");
        }
        if self.objectives.contains_key(name) || self.objectives.len() >= MAX_OBJECTIVES {
            return Err("duplicate or full objective registry");
        }
        self.objectives.insert(
            name.into(),
            Objective {
                display_name: display_name.into(),
                criterion: criterion.into(),
            },
        );
        Ok(())
    }
    pub fn remove_objective(&mut self, name: &str) -> bool {
        if self.objectives.remove(name).is_none() {
            return false;
        }
        self.scores.retain(|(_, obj), _| obj != name);
        self.displays.retain(|_, obj| obj != name);
        true
    }
    pub fn set_score(
        &mut self,
        player: &str,
        objective: &str,
        score: i32,
    ) -> Result<(), &'static str> {
        if !valid_name(player, 40) || !self.objectives.contains_key(objective) {
            return Err("invalid score target");
        }
        let key = (player.to_owned(), objective.to_owned());
        if !self.scores.contains_key(&key) && self.scores.len() >= MAX_SCORES {
            return Err("score limit");
        }
        self.scores.insert(key, score);
        Ok(())
    }
    pub fn set_display(&mut self, slot: u8, objective: Option<&str>) -> Result<(), &'static str> {
        if slot > 18 {
            return Err("invalid display slot");
        }
        if let Some(objective) = objective {
            if !self.objectives.contains_key(objective) {
                return Err("unknown objective");
            }
            self.displays.insert(slot, objective.into());
        } else {
            self.displays.remove(&slot);
        }
        Ok(())
    }
    pub fn add_team(&mut self, name: &str, display_name: &str) -> Result<(), &'static str> {
        if !valid_name(name, 16) || !valid_name(display_name, 32) {
            return Err("invalid team");
        }
        if self.teams.contains_key(name) || self.teams.len() >= MAX_TEAMS {
            return Err("duplicate or full team registry");
        }
        self.teams.insert(
            name.into(),
            Team {
                display_name: display_name.into(),
                ..Team::default()
            },
        );
        Ok(())
    }
    pub fn join_team(&mut self, team: &str, player: &str) -> Result<(), &'static str> {
        if !valid_name(player, 40) || !self.teams.contains_key(team) {
            return Err("invalid team member");
        }
        // Vanilla scoreboard membership is exclusive across teams.
        for group in self.teams.values_mut() {
            group.members.remove(player);
        }
        let group = self.teams.get_mut(team).ok_or("unknown team")?;
        if group.members.len() >= 1024 {
            return Err("team full");
        }
        group.members.insert(player.into());
        Ok(())
    }
    pub fn remove_team(&mut self, name: &str) -> bool {
        self.teams.remove(name).is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn objectives_scores_displays() {
        let mut board = Scoreboard::default();
        board.add_objective("kills", "Kills", "dummy").unwrap();
        board.set_score("Alice", "kills", 3).unwrap();
        board.set_display(1, Some("kills")).unwrap();
        assert_eq!(
            board.scores.get(&("Alice".into(), "kills".into())),
            Some(&3)
        );
        assert!(board.set_display(19, Some("kills")).is_err());
        assert!(board.remove_objective("kills"));
        assert!(board.scores.is_empty() && board.displays.is_empty());
    }
    #[test]
    fn exclusive_team_membership() {
        let mut board = Scoreboard::default();
        board.add_team("red", "Red").unwrap();
        board.add_team("blue", "Blue").unwrap();
        board.join_team("red", "Alice").unwrap();
        board.join_team("blue", "Alice").unwrap();
        assert!(!board.teams["red"].members.contains("Alice"));
        assert!(board.teams["blue"].members.contains("Alice"));
    }
}
