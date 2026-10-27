//! Bounded Minecraft Java 1.7.10 command syntax shared by client and console.
//! Deliberately does not accept commands added in 1.8 or later.
use crate::inventory::Stack;
use crate::player::PlayerPosition;
use std::io::{self, ErrorKind};

#[derive(Debug, PartialEq, Eq)]
pub enum PublicCommand<'a> {
    Help,
    List,
    Me(&'a str),
    Tell { target: &'a str, message: &'a str },
}

/// Recognized unprivileged commands, including invalid usage. Unknown commands
/// return None and continue through the *verified* administrator permission gate.
pub fn parse_public(message: &str) -> Option<Result<PublicCommand<'_>, &'static str>> {
    let command = message.split_whitespace().next()?;
    match command {
        "/help" | "/?" => Some(if message.split_whitespace().count() == 1 {
            Ok(PublicCommand::Help)
        } else {
            Err("Usage: /help")
        }),
        "/list" => Some(if message.split_whitespace().count() == 1 {
            Ok(PublicCommand::List)
        } else {
            Err("Usage: /list")
        }),
        "/me" => Some(match message.strip_prefix("/me").map(str::trim) {
            Some(body) if !body.is_empty() => Ok(PublicCommand::Me(body)),
            _ => Err("Usage: /me <action>"),
        }),
        "/tell" | "/msg" | "/w" => {
            let mut fields = message[command.len()..].trim().splitn(2, char::is_whitespace);
            let target = fields.next().unwrap_or("");
            let body = fields.next().unwrap_or("").trim();
            Some(if crate::whitelist::valid_name(target) && !body.is_empty() {
                Ok(PublicCommand::Tell { target, message: body })
            } else {
                Err("Usage: /tell <player> <message> (aliases: /msg, /w)")
            })
        }
        _ => None,
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

/// Java 1.7.10 understands day/night aliases; newer noon/midnight aliases
/// are intentionally not advertised.
pub fn time_value(value: &str) -> io::Result<u64> {
    match value {
        "day" => Ok(1000),
        "night" => Ok(13000),
        _ => value
            .parse::<u64>()
            .map(|n| n % 24_000)
            .map_err(|_| invalid("time must be day, night or a non-negative tick count")),
    }
}

pub fn relative_coordinate(value: &str, origin: f64) -> io::Result<f64> {
    let parsed = if let Some(offset) = value.strip_prefix('~') {
        if offset.is_empty() { origin } else {
            origin + offset.parse::<f64>().map_err(|_| invalid("invalid relative coordinate"))?
        }
    } else {
        value.parse::<f64>().map_err(|_| invalid("invalid coordinate"))?
    };
    if !parsed.is_finite() {
        return Err(invalid("non-finite teleport coordinate"));
    }
    Ok(parsed)
}

pub fn teleport_coordinates(
    x: &str, y: &str, z: &str, origin: PlayerPosition,
) -> io::Result<PlayerPosition> {
    let pos = PlayerPosition {
        x: relative_coordinate(x, origin.x)?,
        y: relative_coordinate(y, origin.y)?,
        z: relative_coordinate(z, origin.z)?,
    };
    if !pos.is_valid() {
        return Err(invalid("teleport destination outside supported world bounds"));
    }
    Ok(pos)
}

/// Numeric IDs remain accepted in Minecraft 1.7.10. Named aliases here are
/// intentionally a documented subset until the full legacy item-name registry
/// is integrated; unknown names fail closed instead of producing the wrong item.
fn item_id(name: &str) -> io::Result<i16> {
    let unqualified = name.strip_prefix("minecraft:").unwrap_or(name);
    let id = match unqualified {
        "stone" => 1, "grass" => 2, "dirt" => 3, "cobblestone" => 4,
        "planks" => 5, "sand" => 12, "gravel" => 13, "gold_ore" => 14,
        "iron_ore" => 15, "coal_ore" => 16, "log" => 17, "glass" => 20,
        "wool" => 35, "tnt" => 46, "torch" => 50, "chest" => 54,
        "furnace" => 61, "redstone_wire" => 55, "obsidian" => 49,
        "command_block" => 137, "quartz_block" => 155,
        "iron_shovel" => 256, "iron_pickaxe" => 257, "iron_axe" => 258,
        "flint_and_steel" => 259, "apple" => 260, "bow" => 261,
        "arrow" => 262, "coal" => 263, "diamond" => 264,
        "iron_ingot" => 265, "gold_ingot" => 266, "iron_sword" => 267,
        "diamond_sword" => 276, "diamond_pickaxe" => 278,
        "stick" => 280, "bread" => 297, "string" => 287,
        "redstone" => 331, "bucket" => 325, "water_bucket" => 326,
        "lava_bucket" => 327, "egg" => 344, "bed" => 355,
        "ender_pearl" => 368, "blaze_rod" => 369, "potion" => 373,
        "emerald" => 388, "book" => 340, "enchanted_book" => 403,
        other => other.parse::<i16>().map_err(|_| invalid("unknown 1.7.10 item name or ID"))?,
    };
    if !crate::items_1710::exists(id) {
        return Err(invalid("item ID does not exist in Minecraft 1.7.10"));
    }
    Ok(id)
}

/// /give <player> <item> [amount] [data], with a maximum of 64 items per
/// invocation. The session owner splits unstackable items before saving.
pub fn give_args(item: &str, rest: &[&str]) -> io::Result<(i16, u8, i16)> {
    if rest.len() > 2 {
        return Err(invalid("Usage: /give <player> <item> [amount] [data]"));
    }
    let id = item_id(item)?;
    let count = rest.first().unwrap_or(&"1")
        .parse::<u8>().map_err(|_| invalid("give amount must be 1..64"))?;
    if !(1..=64).contains(&count) {
        return Err(invalid("give amount must be 1..64"));
    }
    let damage = rest.get(1).unwrap_or(&"0")
        .parse::<i16>().map_err(|_| invalid("invalid item data value"))?;
    if !crate::items_1710::valid_damage(id, damage) {
        return Err(invalid("invalid 1.7.10 item data value"));
    }
    Ok((id, count, damage))
}

pub fn stack(id: i16, count: u8, damage: i16) -> Stack {
    Stack { id, count, damage, nbt: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_commands_are_bounded_and_distinct_from_op_commands() {
        assert_eq!(parse_public("/help"), Some(Ok(PublicCommand::Help)));
        assert_eq!(parse_public("/?"), Some(Ok(PublicCommand::Help)));
        assert_eq!(parse_public("/list"), Some(Ok(PublicCommand::List)));
        assert_eq!(parse_public("/me waves"), Some(Ok(PublicCommand::Me("waves"))));
        assert_eq!(
            parse_public("/w Alice hello world"),
            Some(Ok(PublicCommand::Tell { target: "Alice", message: "hello world" }))
        );
        assert!(parse_public("/tell Alice").unwrap().is_err());
        assert!(parse_public("/me ").unwrap().is_err());
        assert_eq!(parse_public("/op Alice"), None);
        assert_eq!(parse_public("/give Alice diamond"), None);
        assert_eq!(parse_public("hello"), None);
    }

    #[test]
    fn versioned_time_and_safe_teleport() {
        assert_eq!(time_value("day").unwrap(), 1000);
        assert_eq!(time_value("night").unwrap(), 13000);
        assert_eq!(time_value("24000").unwrap(), 0);
        assert!(time_value("-1").is_err());
        assert!(time_value("noon").is_err());
        let origin = PlayerPosition { x: 10.0, y: 70.0, z: -10.0 };
        assert_eq!(
            teleport_coordinates("~", "~2", "0", origin).unwrap(),
            PlayerPosition { x: 10.0, y: 72.0, z: 0.0 }
        );
        assert!(teleport_coordinates("NaN", "70", "0", origin).is_err());
        assert!(teleport_coordinates("~1e309", "70", "0", origin).is_err());
    }

    #[test]
    fn give_is_legacy_bounded_and_validated() {
        assert_eq!(give_args("minecraft:diamond", &[]).unwrap(), (264, 1, 0));
        assert_eq!(give_args("264", &["64"]).unwrap(), (264, 64, 0));
        assert!(give_args("diamond", &["0"]).is_err());
        assert!(give_args("diamond", &["65"]).is_err());
        assert!(give_args("netherite_ingot", &[]).is_err());
        assert!(give_args("-1", &[]).is_err());
        assert!(give_args("diamond", &["1", "0", "ignored"]).is_err());
    }
}
