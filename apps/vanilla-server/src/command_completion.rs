//! Vanilla 1.7.10 tab-completion suggestions. The vanilla client owns the
//! Tab key and selection UI; this server responds to C14 with an S3A list.
//! Suggestions are advisory, not authorization or command execution.
use legacy_protocol::{write_string, write_varint};
use std::io;

pub const MAX_REQUEST_CHARS: usize = 256;
pub const MAX_SUGGESTIONS: usize = 32;

const PUBLIC: &[&str] = &["/help", "/?", "/list", "/me", "/tell", "/msg", "/w"];
// Keep this list in sync with run_player_admin_command's OP level gate.
// In particular, never advertise new 1.8+ commands or unmerged features.
const LEVEL_2: &[&str] = &[
    "/time", "/weather", "/tp", "/teleport", "/gamemode", "/give",
    "/clear", "/setblock", "/effect", "/summon", "/enchant",
    "/testforblock", "/seed",
];
const LEVEL_3: &[&str] = &[
    "/kick", "/ban", "/pardon", "/pardon-uuid", "/ban-ip", "/pardon-ip",
];
const LEVEL_4: &[&str] = &["/save", "/op", "/deop", "/permission"];

const ITEMS: &[&str] = &[
    "stone", "grass", "dirt", "cobblestone", "planks", "sand", "gravel",
    "iron_ore", "gold_ore", "coal_ore", "log", "glass", "wool", "tnt",
    "torch", "chest", "furnace", "obsidian", "diamond", "iron_ingot",
    "gold_ingot", "diamond_sword", "iron_sword", "diamond_pickaxe",
    "bow", "arrow", "apple", "bread", "stick", "redstone", "emerald",
    "book", "enchanted_book", "potion", "ender_pearl",
];
const TERRAIN: &[&str] = &[
    "air", "stone", "grass", "dirt", "cobblestone", "planks",
    "sand", "gravel", "log", "leaves", "glass", "wool", "obsidian",
    "gold_block", "iron_block", "diamond_block", "quartz_block",
    "sandstone", "glowstone", "emerald_block", "netherrack",
];
const MOBS: &[&str] = &[
    "Zombie", "Skeleton", "Creeper", "Spider", "CaveSpider",
    "Enderman", "Slime", "Witch", "Cow", "Pig", "Sheep", "Chicken",
    "Horse", "Villager", "Blaze", "Ghast",
];

fn eligible(command: &str, op_level: u8) -> bool {
    PUBLIC.contains(&command)
        || (op_level >= 2 && LEVEL_2.contains(&command))
        || (op_level >= 3 && LEVEL_3.contains(&command))
        || (op_level >= 4 && LEVEL_4.contains(&command))
}

fn add(out: &mut Vec<String>, prefix: &str, options: &[&str]) {
    for &option in options {
        if option.len() <= 64
            && option.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
        {
            out.push(option.to_owned());
        }
    }
}

fn players(out: &mut Vec<String>, prefix: &str, online: &[String]) {
    for name in online {
        if name.len() <= 16
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
        {
            out.push(name.clone());
        }
    }
}

/// Server sends *replacement strings for the last token*, not full commands
/// (except when the last token itself is a command name).
pub fn complete(text: &str, op_level: u8, online: &[String]) -> Vec<String> {
    if text.chars().count() > MAX_REQUEST_CHARS
        || text.chars().any(|c| matches!(c, '\n' | '\r' | '\t'))
    {
        return Vec::new();
    }
    let trailing_space = text.ends_with(' ');
    let words: Vec<&str> = text.split_ascii_whitespace().collect();
    let mut found = Vec::new();
    if !text.starts_with('/') {
        if !trailing_space {
            players(&mut found, words.last().copied().unwrap_or(""), online);
        }
    } else if words.is_empty() {
        // Plain '/' is a request for command names.
        add(&mut found, "/", PUBLIC);
        if op_level >= 2 {
            add(&mut found, "/", LEVEL_2);
        }
        if op_level >= 3 {
            add(&mut found, "/", LEVEL_3);
        }
        if op_level >= 4 {
            add(&mut found, "/", LEVEL_4);
        }
    } else if words.len() == 1 && !trailing_space {
        let prefix = words[0];
        add(&mut found, prefix, PUBLIC);
        if op_level >= 2 {
            add(&mut found, prefix, LEVEL_2);
        }
        if op_level >= 3 {
            add(&mut found, prefix, LEVEL_3);
        }
        if op_level >= 4 {
            add(&mut found, prefix, LEVEL_4);
        }
    } else {
        let command = words[0].to_ascii_lowercase();
        if !eligible(&command, op_level) {
            return Vec::new();
        }
        let index = if trailing_space {
            words.len()
        } else {
            words.len() - 1
        };
        let prefix = if trailing_space { "" } else { words[index] };
        let previous = words
            .get(index.saturating_sub(1))
            .copied()
            .unwrap_or("");
        match (command.as_str(), index) {
            ("/tell" | "/msg" | "/w", 1)
            | (
                "/give" | "/enchant" | "/effect" | "/kick" | "/ban" | "/pardon" | "/op"
                | "/deop" | "/clear" | "/permission",
                1,
            )
            | ("/gamemode", 2) => players(&mut found, prefix, online),
            ("/tp" | "/teleport", 1) => {
                players(&mut found, prefix, online);
                add(&mut found, prefix, &["~"]);
            }
            ("/tp" | "/teleport", 2..=4)
            | ("/summon", 2..=4)
            | ("/setblock" | "/testforblock", 1..=3) => add(&mut found, prefix, &["~"]),
            ("/gamemode", 1) => {
                add(&mut found, prefix, &["survival", "creative", "0", "1"]);
            }
            ("/time", 1) => add(&mut found, prefix, &["set", "add", "query"]),
            ("/time", 2) if previous == "set" => {
                add(&mut found, prefix, &["day", "night"]);
            }
            ("/time", 2) if previous == "query" => {
                add(&mut found, prefix, &["daytime"]);
            }
            ("/weather", 1) => {
                add(&mut found, prefix, &["clear", "rain", "thunder", "set"]);
            }
            ("/weather", 2) if previous == "set" => {
                add(&mut found, prefix, &["clear", "rain", "thunder"]);
            }
            ("/give" | "/clear", 2) => add(&mut found, prefix, ITEMS),
            ("/setblock" | "/testforblock", 4) => add(&mut found, prefix, TERRAIN),
            ("/setblock", 6) => add(&mut found, prefix, &["replace", "keep"]),
            ("/effect", 2) => {
                add(
                    &mut found,
                    prefix,
                    &["clear", "1", "10", "17", "19", "speed", "regeneration", "hunger", "poison"],
                );
            }
            ("/summon", 1) => add(&mut found, prefix, MOBS),
            ("/permission", 2) => add(&mut found, prefix, &["chat", "build", "interact"]),
            ("/permission", 3) => add(&mut found, prefix, &["allow", "deny"]),
            _ => {}
        }
    }
    found.sort_by_key(|name| name.to_ascii_lowercase());
    found.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    found.truncate(MAX_SUGGESTIONS);
    found
}

/// Legacy Protocol 5 S3A Tab Complete: VarInt count followed by count
/// length-prefixed UTF-8 strings. Do not use modern Brigadier transaction IDs.
pub fn response_packet(completions: &[String]) -> io::Result<Vec<u8>> {
    if completions.len() > MAX_SUGGESTIONS
        || completions.iter().any(|value| value.chars().count() > 64)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many tab completions",
        ));
    }
    let mut body = Vec::with_capacity(1024);
    write_varint(completions.len() as u32, &mut body);
    for item in completions {
        write_string(item, &mut body)?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use legacy_protocol::read_varint;

    fn names() -> Vec<String> {
        vec!["Steve".into(), "Alex".into(), "Stefan".into()]
    }
    #[test]
    fn public_and_privileged_command_names_are_isolated() {
        let people = names();
        assert_eq!(complete("/gi", 0, &people), Vec::<String>::new());
        assert_eq!(complete("/gi", 2, &people), vec!["/give"]);
        assert_eq!(complete("/?", 0, &people), vec!["/?"]);
        assert!(complete("/", 0, &people).contains(&"/tell".to_owned()));
        assert!(!complete("/", 0, &people).contains(&"/op".to_owned()));
        assert!(!complete("/", 2, &people).contains(&"/ban".to_owned()));
        assert!(complete("/", 3, &people).contains(&"/ban".to_owned()));
        assert!(complete("/", 4, &people).contains(&"/op".to_owned()));
        assert!(!complete("/", 4, &people).contains(&"/fill".to_owned()));
    }
    #[test]
    fn completes_player_names_and_legacy_arguments() {
        let people = names();
        assert_eq!(complete("/tell st", 0, &people), vec!["Stefan", "Steve"]);
        assert_eq!(complete("/give St", 2, &people), vec!["Stefan", "Steve"]);
        assert_eq!(complete("/gamemode c", 2, &people), vec!["creative"]);
        assert_eq!(complete("/weather r", 2, &people), vec!["rain"]);
        assert_eq!(complete("/time set n", 2, &people), vec!["night"]);
        assert_eq!(complete("/summon Z", 2, &people), vec!["Zombie"]);
        assert!(complete("/tp Steve ~", 2, &people).contains(&"~".to_owned()));
        assert!(complete("/ban St", 2, &people).is_empty());
        assert_eq!(complete("Hello St", 0, &people), vec!["Stefan", "Steve"]);
    }
    #[test]
    fn rejects_oversized_and_unknown_queries_without_leaking_admins() {
        let people = names();
        assert!(complete(&"/".repeat(MAX_REQUEST_CHARS + 1), 4, &people).is_empty());
        assert!(complete("/execute ", 4, &people).is_empty());
        assert!(complete("/op St", 0, &people).is_empty());
        assert!(complete("/give St", 0, &people).is_empty());
        assert!(complete("/\n", 4, &people).is_empty());
    }
    #[test]
    fn protocol5_packet_encodes_array_not_transaction_id() {
        let expected = vec!["/give".to_owned(), "/gamemode".to_owned()];
        let packet = response_packet(&expected).unwrap();
        let mut cursor = packet.as_slice();
        assert_eq!(read_varint(&mut cursor).unwrap(), 2);
        for value in expected {
            let length = read_varint(&mut cursor).unwrap() as usize;
            assert_eq!(&cursor[..length], value.as_bytes());
            cursor = &cursor[length..];
        }
        assert!(cursor.is_empty());
        assert!(response_packet(&vec![String::from("x"); MAX_SUGGESTIONS + 1]).is_err());
    }
}
