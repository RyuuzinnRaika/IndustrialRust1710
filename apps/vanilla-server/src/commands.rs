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
            let mut fields = message[command.len()..]
                .trim()
                .splitn(2, char::is_whitespace);
            let target = fields.next().unwrap_or("");
            let body = fields.next().unwrap_or("").trim();
            Some(
                if crate::whitelist::valid_name(target) && !body.is_empty() {
                    Ok(PublicCommand::Tell {
                        target,
                        message: body,
                    })
                } else {
                    Err("Usage: /tell <player> <message> (aliases: /msg, /w)")
                },
            )
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
        if offset.is_empty() {
            origin
        } else {
            origin
                + offset
                    .parse::<f64>()
                    .map_err(|_| invalid("invalid relative coordinate"))?
        }
    } else {
        value
            .parse::<f64>()
            .map_err(|_| invalid("invalid coordinate"))?
    };
    if !parsed.is_finite() {
        return Err(invalid("non-finite teleport coordinate"));
    }
    Ok(parsed)
}

pub fn teleport_coordinates(
    x: &str,
    y: &str,
    z: &str,
    origin: PlayerPosition,
) -> io::Result<PlayerPosition> {
    let pos = PlayerPosition {
        x: relative_coordinate(x, origin.x)?,
        y: relative_coordinate(y, origin.y)?,
        z: relative_coordinate(z, origin.z)?,
    };
    if !pos.is_valid() {
        return Err(invalid(
            "teleport destination outside supported world bounds",
        ));
    }
    Ok(pos)
}

/// Numeric IDs remain accepted in Minecraft 1.7.10. Named aliases here are
/// intentionally a documented subset until the full legacy item-name registry
/// is integrated; unknown names fail closed instead of producing the wrong item.
fn item_id(name: &str) -> io::Result<i16> {
    let unqualified = name.strip_prefix("minecraft:").unwrap_or(name);
    let id = match unqualified {
        "stone" => 1,
        "grass" => 2,
        "dirt" => 3,
        "cobblestone" => 4,
        "planks" => 5,
        "sand" => 12,
        "gravel" => 13,
        "gold_ore" => 14,
        "iron_ore" => 15,
        "coal_ore" => 16,
        "log" => 17,
        "glass" => 20,
        "wool" => 35,
        "tnt" => 46,
        "torch" => 50,
        "chest" => 54,
        "furnace" => 61,
        "redstone_wire" => 55,
        "obsidian" => 49,
        "command_block" => 137,
        "quartz_block" => 155,
        "iron_shovel" => 256,
        "iron_pickaxe" => 257,
        "iron_axe" => 258,
        "flint_and_steel" => 259,
        "apple" => 260,
        "bow" => 261,
        "arrow" => 262,
        "coal" => 263,
        "diamond" => 264,
        "iron_ingot" => 265,
        "gold_ingot" => 266,
        "iron_sword" => 267,
        "diamond_sword" => 276,
        "diamond_pickaxe" => 278,
        "stick" => 280,
        "bread" => 297,
        "string" => 287,
        "redstone" => 331,
        "bucket" => 325,
        "water_bucket" => 326,
        "lava_bucket" => 327,
        "egg" => 344,
        "bed" => 355,
        "ender_pearl" => 368,
        "blaze_rod" => 369,
        "potion" => 373,
        "emerald" => 388,
        "book" => 340,
        "enchanted_book" => 403,
        other => other
            .parse::<i16>()
            .map_err(|_| invalid("unknown 1.7.10 item name or ID"))?,
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
    let count = rest
        .first()
        .unwrap_or(&"1")
        .parse::<u8>()
        .map_err(|_| invalid("give amount must be 1..64"))?;
    if !(1..=64).contains(&count) {
        return Err(invalid("give amount must be 1..64"));
    }
    let damage = rest
        .get(1)
        .unwrap_or(&"0")
        .parse::<i16>()
        .map_err(|_| invalid("invalid item data value"))?;
    if !crate::items_1710::valid_damage(id, damage) {
        return Err(invalid("invalid 1.7.10 item data value"));
    }
    Ok((id, count, damage))
}

pub fn stack(id: i16, count: u8, damage: i16) -> Stack {
    Stack {
        id,
        count,
        damage,
        nbt: None,
    }
}

/// 1.7.10 block state is an 8-bit numeric ID plus 4-bit metadata, not 1.13
/// blockstate text. Restrict this first batch to ordinary non-TileEntity
/// terrain blocks until block-entity create/remove and drop transactions exist.
/// Block ids with NBT-backed tile data must not be overwritten by a simple
/// block snapshot: that would leave stale inventory/sign/spawner records.
pub fn is_tile_entity_block(id: u16) -> bool {
    matches!(
        id,
        23 | 25
            | 52
            | 54
            | 61
            | 62
            | 63
            | 68
            | 84
            | 116
            | 117
            | 119
            | 130
            | 137
            | 138
            | 140
            | 144
            | 146
            | 149
            | 150
            | 151
            | 154
            | 158
            | 176
            | 177
    )
}

pub fn safe_setblock_id(name: &str) -> io::Result<u8> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    let id = match bare {
        "air" => 0,
        "stone" => 1,
        "grass" => 2,
        "dirt" => 3,
        "cobblestone" => 4,
        "planks" => 5,
        "sand" => 12,
        "gravel" => 13,
        "gold_ore" => 14,
        "iron_ore" => 15,
        "coal_ore" => 16,
        "log" => 17,
        "leaves" => 18,
        "glass" => 20,
        "lapis_block" => 22,
        "sandstone" => 24,
        "wool" => 35,
        "gold_block" => 41,
        "iron_block" => 42,
        "brick_block" => 45,
        "obsidian" => 49,
        "diamond_block" => 57,
        "snow" => 80,
        "clay" => 82,
        "netherrack" => 87,
        "soul_sand" => 88,
        "glowstone" => 89,
        "stonebrick" => 98,
        "melon_block" => 103,
        "nether_brick" => 112,
        "end_stone" => 121,
        "emerald_ore" => 129,
        "emerald_block" => 133,
        "quartz_block" => 155,
        "stained_hardened_clay" => 159,
        "hay_block" => 170,
        "hardened_clay" => 172,
        "coal_block" => 173,
        other => other
            .parse::<u8>()
            .map_err(|_| invalid("unknown legacy block name"))?,
    };
    if !matches!(id, 0..=5 | 12..=18 | 20 | 22 | 24 | 35 | 41 | 42 | 45 | 49 | 57 | 80 | 82 | 87..=89 | 98 | 103 | 112 | 121 | 129 | 133 | 155 | 159 | 170 | 172 | 173)
    {
        return Err(invalid(
            "setblock only supports non-TileEntity terrain blocks",
        ));
    }
    Ok(id)
}

pub fn setblock_args(
    coords: [&str; 3],
    block: &str,
    rest: &[&str],
    origin: PlayerPosition,
) -> io::Result<(i32, i32, i32, u16, bool)> {
    if rest.len() > 2 {
        return Err(invalid(
            "Usage: /setblock <x> <y> <z> <block> [data] [replace|keep]",
        ));
    }
    let id = safe_setblock_id(block)?;
    let data = rest
        .first()
        .copied()
        .unwrap_or("0")
        .parse::<u8>()
        .map_err(|_| invalid("block data must be 0..15"))?;
    if data > 15 || (id == 0 && data != 0) {
        return Err(invalid("invalid block data"));
    }
    let keep = match rest.get(1).copied().unwrap_or("replace") {
        "replace" => false,
        "keep" => true,
        "destroy" => {
            return Err(invalid(
                "destroy mode needs atomic item drops and is not implemented",
            ))
        }
        _ => return Err(invalid("setblock mode must be replace or keep")),
    };
    let coord = |text: &str, from: f64| -> io::Result<i32> {
        let value = relative_coordinate(text, from)?.floor();
        if !(i32::MIN as f64..=i32::MAX as f64).contains(&value) {
            return Err(invalid("setblock position out of bounds"));
        }
        Ok(value as i32)
    };
    let x = coord(coords[0], origin.x)?;
    let y = coord(coords[1], origin.y)?;
    let z = coord(coords[2], origin.z)?;
    if !(0..=255).contains(&y) {
        return Err(invalid("setblock height must be 0..255"));
    }
    Ok((x, y, z, (u16::from(id) << 4) | u16::from(data), keep))
}

/// /clear [player] [item] [data=-1] [maxCount=-1]
/// Internal sentinel item=-1 means all items; data=-1 means any variant.
/// A maxCount of zero performs a read-only count, matching legacy behavior.
pub fn clear_args(
    item: Option<&str>,
    data: Option<&str>,
    max: Option<&str>,
) -> io::Result<(i16, i16, i32)> {
    if item.is_none() && (data.is_some() || max.is_some()) {
        return Err(invalid("clear data/count requires an item"));
    }
    let id = item.map(item_id).transpose()?.unwrap_or(-1);
    let data = data
        .unwrap_or("-1")
        .parse::<i16>()
        .map_err(|_| invalid("invalid clear item data"))?;
    if data < -1 {
        return Err(invalid("clear item data must be >= -1"));
    }
    let max = max
        .unwrap_or("-1")
        .parse::<i32>()
        .map_err(|_| invalid("invalid clear max count"))?;
    if !(-1..=4096).contains(&max) {
        return Err(invalid("clear max count must be -1..4096"));
    }
    Ok((id, data, max))
}

/// Only effects supported by the current authoritative survival tick are
/// accepted. Additional effect IDs need server-side movement/damage rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectRequest {
    Clear,
    Remove(u8),
    Add {
        kind: u8,
        amplifier: u8,
        seconds: u16,
    },
}
pub fn valid_effect_kind(kind: u8) -> bool {
    matches!(kind, 1 | 7 | 10 | 17 | 19)
}
pub fn effect_args(parts: &[&str]) -> io::Result<EffectRequest> {
    if parts.len() == 1 && parts[0] == "clear" {
        return Ok(EffectRequest::Clear);
    }
    if parts.is_empty() || parts.len() > 3 {
        return Err(invalid(
            "Usage: /effect <player> <id|clear> [seconds] [amplifier]",
        ));
    }
    let kind = match parts[0] {
        "speed" => 1,
        "regeneration" => 10,
        "instant_damage" | "harm" | "harming" => 7,
        "hunger" => 17,
        "poison" => 19,
        text => text
            .parse::<u8>()
            .map_err(|_| invalid("unknown legacy effect"))?,
    };
    if !valid_effect_kind(kind) {
        return Err(invalid("effect not yet implemented by the server"));
    }
    let seconds = parts
        .get(1)
        .copied()
        .unwrap_or("30")
        .parse::<u16>()
        .map_err(|_| invalid("effect duration must be 0..1200 seconds"))?;
    if seconds > 1200 {
        return Err(invalid("effect duration must be 0..1200 seconds"));
    }
    let amplifier = parts
        .get(2)
        .copied()
        .unwrap_or("0")
        .parse::<u8>()
        .map_err(|_| invalid("effect amplifier must be 0..3"))?;
    if amplifier > 3 {
        return Err(invalid("effect amplifier must be 0..3"));
    }
    if seconds == 0 {
        return Ok(EffectRequest::Remove(kind));
    }
    Ok(EffectRequest::Add {
        kind,
        amplifier,
        seconds,
    })
}

/// /summon <LegacyEntityName> [x y z]. The pre-1.8 optional SNBT compound
/// is rejected until entity NBT validation is implemented.
pub fn summon_coordinates(
    args: &[&str],
    origin: Option<PlayerPosition>,
) -> io::Result<PlayerPosition> {
    let coords = match args {
        [] => return origin.ok_or_else(|| invalid("console /summon requires x y z")),
        [x, y, z] => [*x, *y, *z],
        _ => {
            return Err(invalid(
                "Usage: /summon <entity> [x y z] (NBT not implemented)",
            ))
        }
    };
    if origin.is_none() && coords.iter().any(|v| v.starts_with('~')) {
        return Err(invalid("console summon requires absolute coordinates"));
    }
    let origin = origin.unwrap_or(PlayerPosition {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    teleport_coordinates(coords[0], coords[1], coords[2], origin)
}

/// Java 1.7.10 numeric enchantment IDs; aliases are intentionally excluded
/// until a versioned name registry is added.
pub fn enchant_args(id: &str, level: Option<&str>) -> io::Result<(i16, i16)> {
    let id = id
        .parse::<i16>()
        .map_err(|_| invalid("enchant ID must be numeric for 1.7.10"))?;
    let level = level
        .unwrap_or("1")
        .parse::<i16>()
        .map_err(|_| invalid("enchant level must be a positive number"))?;
    if !(1..=5).contains(&level) || !matches!(id, 0..=7 | 16..=21 | 32..=35 | 48..=51 | 61..=62) {
        return Err(invalid("unknown 1.7.10 enchantment ID or level"));
    }
    Ok((id, level))
}

/// Read-only legacy block query, where data=-1 tests only the numeric block ID.
/// The limited named registry can expand without ever accepting 1.13 states.
pub fn testforblock_args(
    coords: [&str; 3],
    name: &str,
    data: Option<&str>,
    origin: Option<PlayerPosition>,
) -> io::Result<(i32, i32, i32, u8, Option<u8>)> {
    if origin.is_none() && coords.iter().any(|part| part.starts_with('~')) {
        return Err(invalid("console coordinates must be absolute"));
    }
    let origin = origin.unwrap_or(PlayerPosition {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    let parse = |text: &str, base: f64| -> io::Result<i32> {
        let value = relative_coordinate(text, base)?.floor();
        if value < i32::MIN as f64 || value >= (i32::MAX as f64 + 1.0) {
            return Err(invalid("testforblock coordinate out of bounds"));
        }
        Ok(value as i32)
    };
    let x = parse(coords[0], origin.x)?;
    let y = parse(coords[1], origin.y)?;
    let z = parse(coords[2], origin.z)?;
    if !crate::world::in_bounds(x, y, z) {
        return Err(invalid("testforblock position outside world"));
    }
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    let id: u8 = match bare {
        "bedrock" => 7,
        "flowing_water" => 8,
        "water" => 9,
        "flowing_lava" => 10,
        "lava" => 11,
        "diamond_ore" => 56,
        "crafting_table" => 58,
        "furnace" => 61,
        "lit_furnace" => 62,
        "chest" => 54,
        "mob_spawner" => 52,
        "redstone_wire" => 55,
        "command_block" => 137,
        _ => safe_setblock_id(bare).or_else(|_| {
            let numeric = bare
                .parse::<u8>()
                .map_err(|_| invalid("unknown legacy block name"))?;
            if numeric > 175 || (numeric != 0 && !crate::items_1710::exists(i16::from(numeric))) {
                return Err(invalid("unknown legacy block ID"));
            }
            Ok(numeric)
        })?,
    };
    let data = match data
        .unwrap_or("-1")
        .parse::<i16>()
        .map_err(|_| invalid("block data must be -1 or 0..15"))?
    {
        -1 => None,
        0..=15 => Some(
            data.unwrap_or("0")
                .parse::<u8>()
                .map_err(|_| invalid("invalid block data"))?,
        ),
        _ => return Err(invalid("block data must be -1 or 0..15")),
    };
    Ok((x, y, z, id, data))
}

/// Bounded Java 1.7.10 /spawnpoint coordinates. Only integer block
/// locations and '~'-relative coordinates are used (never 1.13 local '^').
/// The caller selects a target online player and the target's owning session
/// validates that the actual Overworld location has space for a respawn.
pub fn spawnpoint_args(
    coordinates: &[&str],
    origin: Option<PlayerPosition>,
) -> io::Result<crate::spawnpoint::Spawnpoint> {
    if coordinates.len() != 3 {
        return Err(invalid("Usage: /spawnpoint [player] [x y z]"));
    }
    if origin.is_none() && coordinates.iter().any(|value| value.starts_with('~')) {
        return Err(invalid("console spawnpoint requires absolute coordinates"));
    }
    let base = origin.unwrap_or(PlayerPosition {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    let bases = [base.x, base.y, base.z];
    let mut values = [0i32; 3];
    for (index, source) in coordinates.iter().enumerate() {
        let value = relative_coordinate(source, bases[index])?.floor();
        if value < i32::MIN as f64 || value >= i32::MAX as f64 + 1.0 {
            return Err(invalid("spawnpoint coordinate out of range"));
        }
        values[index] = value as i32;
    }
    crate::spawnpoint::Spawnpoint::new(values[0], values[1], values[2])
}

/// Legacy 1.7.10 /setworldspawn [x y z]. The 1.16+ yaw argument,
/// 1.13+ '^' local coordinates, nonfinite and overflowing input are rejected.
pub fn setworldspawn_args(
    coordinates: &[&str],
    origin: Option<PlayerPosition>,
) -> io::Result<crate::world_spawn::WorldSpawn> {
    if coordinates.len() != 3 {
        return Err(invalid("Usage: /setworldspawn [x y z]"));
    }
    if origin.is_none() && coordinates.iter().any(|part| part.starts_with('~')) {
        return Err(invalid(
            "console setworldspawn requires absolute coordinates",
        ));
    }
    let origin = origin.unwrap_or(PlayerPosition {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    });
    let base = [origin.x, origin.y, origin.z];
    let mut values = [0i32; 3];
    for (index, text) in coordinates.iter().enumerate() {
        let value = relative_coordinate(text, base[index])?.floor();
        if value < i32::MIN as f64 || value >= i32::MAX as f64 + 1.0 {
            return Err(invalid("world spawn coordinate outside integer bounds"));
        }
        values[index] = value as i32;
    }
    crate::world_spawn::WorldSpawn::new(values[0], values[1], values[2])
}

/// 1.7.10 /seed must expose a real persisted world seed, never make up a
/// value for the legacy IRW1 flat-world format which stores no seed.
pub fn seed_text(world: &crate::world::World) -> io::Result<String> {
    world
        .natural_seed()
        .map(|seed| format!("World seed: {seed}"))
        .ok_or_else(|| invalid("this flat world has no persisted seed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_harming_effect_is_a_bounded_server_owned_request() {
        assert_eq!(
            effect_args(&["instant_damage", "1", "1"]).unwrap(),
            EffectRequest::Add {
                kind: 7,
                seconds: 1,
                amplifier: 1
            }
        );
        assert_eq!(
            effect_args(&["7", "1", "0"]).unwrap(),
            EffectRequest::Add {
                kind: 7,
                seconds: 1,
                amplifier: 0
            }
        );
        assert!(effect_args(&["7", "1", "4"]).is_err());
    }

    #[test]
    fn spawnpoint_1710_coordinates_reject_invalid_and_console_relative() {
        let origin = PlayerPosition {
            x: -2.8,
            y: 65.0,
            z: 3.3,
        };
        let parsed = spawnpoint_args(&["~", "~", "~1"], Some(origin)).unwrap();
        assert_eq!(parsed.coords(), (-3, 65, 4));
        assert_eq!(
            spawnpoint_args(&["10", "64", "-20"], None)
                .unwrap()
                .coords(),
            (10, 64, -20)
        );
        assert!(spawnpoint_args(&["~", "64", "0"], None).is_err());
        assert!(spawnpoint_args(&["0", "0", "0"], None).is_err());
        assert!(spawnpoint_args(&["0", "255", "0"], None).is_err());
        assert!(spawnpoint_args(&["NaN", "64", "0"], None).is_err());
        assert!(spawnpoint_args(&["0", "64"], None).is_err());
        assert!(spawnpoint_args(&["0", "64", "0", "extra"], None).is_err());
    }

    #[test]
    fn setworldspawn_1710_coordinate_parser_is_strict() {
        let origin = PlayerPosition {
            x: -2.4,
            y: 65.1,
            z: 3.1,
        };
        assert_eq!(
            setworldspawn_args(&["~", "~", "~1"], Some(origin))
                .unwrap()
                .coordinates(),
            (-3, 65, 4)
        );
        assert_eq!(
            setworldspawn_args(&["10", "64", "-3"], None)
                .unwrap()
                .coordinates(),
            (10, 64, -3)
        );
        assert!(setworldspawn_args(&["~", "64", "2"], None).is_err());
        assert!(setworldspawn_args(&["0", "-1", "0"], None).is_err());
        assert!(setworldspawn_args(&["0", "256", "0"], None).is_err());
        assert!(setworldspawn_args(&["^0", "65", "0"], Some(origin)).is_err());
        assert!(setworldspawn_args(&["NaN", "65", "0"], Some(origin)).is_err());
        assert!(setworldspawn_args(&["0", "65", "0", "yaw"], Some(origin)).is_err());
    }

    #[test]
    fn public_commands_are_bounded_and_distinct_from_op_commands() {
        assert_eq!(parse_public("/help"), Some(Ok(PublicCommand::Help)));
        assert_eq!(parse_public("/?"), Some(Ok(PublicCommand::Help)));
        assert_eq!(parse_public("/list"), Some(Ok(PublicCommand::List)));
        assert_eq!(
            parse_public("/me waves"),
            Some(Ok(PublicCommand::Me("waves")))
        );
        assert_eq!(
            parse_public("/w Alice hello world"),
            Some(Ok(PublicCommand::Tell {
                target: "Alice",
                message: "hello world"
            }))
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
        let origin = PlayerPosition {
            x: 10.0,
            y: 70.0,
            z: -10.0,
        };
        assert_eq!(
            teleport_coordinates("~", "~2", "0", origin).unwrap(),
            PlayerPosition {
                x: 10.0,
                y: 72.0,
                z: 0.0
            }
        );
        assert!(teleport_coordinates("NaN", "70", "0", origin).is_err());
        assert!(teleport_coordinates("~1e309", "70", "0", origin).is_err());
    }

    #[test]
    fn setblock_respects_legacy_ids_relative_positions_and_tile_safety() {
        let origin = PlayerPosition {
            x: 10.5,
            y: 70.0,
            z: -12.2,
        };
        assert_eq!(
            setblock_args(
                ["~", "~-1", "~3"],
                "minecraft:stone",
                &["2", "keep"],
                origin
            )
            .unwrap(),
            (10, 69, -10, 0x12, true)
        );
        assert!(setblock_args(["0", "64", "0"], "chest", &[], origin).is_err());
        assert!(setblock_args(["0", "64", "0"], "137", &[], origin).is_err());
        assert!(setblock_args(["0", "-1", "0"], "stone", &[], origin).is_err());
        assert!(setblock_args(["0", "64", "0"], "stone", &["0", "destroy"], origin).is_err());
        assert!(setblock_args(["NaN", "64", "0"], "stone", &[], origin).is_err());
    }

    #[test]
    fn clear_rejects_invalid_ids_and_preserves_count_only() {
        assert_eq!(clear_args(None, None, None).unwrap(), (-1, -1, -1));
        assert_eq!(
            clear_args(Some("minecraft:wool"), Some("3"), Some("0")).unwrap(),
            (35, 3, 0)
        );
        assert!(clear_args(Some("netherite_ingot"), None, None).is_err());
        assert!(clear_args(Some("diamond"), None, Some("-2")).is_err());
        assert!(clear_args(None, Some("0"), None).is_err());
    }

    #[test]
    fn effect_command_enforces_1710_syntax_and_server_implemented_ids() {
        assert_eq!(effect_args(&["clear"]).unwrap(), EffectRequest::Clear);
        assert_eq!(
            effect_args(&["10", "0"]).unwrap(),
            EffectRequest::Remove(10)
        );
        assert_eq!(
            effect_args(&["poison", "30", "1"]).unwrap(),
            EffectRequest::Add {
                kind: 19,
                amplifier: 1,
                seconds: 30
            }
        );
        assert!(effect_args(&["night_vision"]).is_err());
        assert!(effect_args(&["regeneration", "1201"]).is_err());
        assert!(effect_args(&["regeneration", "30", "4"]).is_err());
        assert!(effect_args(&["give", "Bob", "10"]).is_err());
        assert!(effect_args(&["speed", "1", "0", "true"]).is_err());
    }

    #[test]
    fn summon_arguments_reject_nbt_and_unverified_console_relative_origin() {
        let player = PlayerPosition {
            x: 7.0,
            y: 70.0,
            z: 9.0,
        };
        assert_eq!(summon_coordinates(&[], Some(player)).unwrap(), player);
        assert_eq!(
            summon_coordinates(&["~1", "~", "10"], Some(player)).unwrap(),
            PlayerPosition {
                x: 8.0,
                y: 70.0,
                z: 10.0
            }
        );
        assert!(summon_coordinates(&[], None).is_err());
        assert!(summon_coordinates(&["~", "64", "1"], None).is_err());
        assert!(summon_coordinates(&["1", "64", "2", "{CustomName:A}"], Some(player)).is_err());
    }

    #[test]
    fn enchant_and_testforblock_enforce_pre_18_syntax() {
        assert_eq!(enchant_args("16", None).unwrap(), (16, 1));
        assert_eq!(enchant_args("48", Some("5")).unwrap(), (48, 5));
        assert!(enchant_args("sharpness", None).is_err());
        assert!(enchant_args("70", None).is_err());
        assert!(enchant_args("16", Some("0")).is_err());
        let pos = PlayerPosition {
            x: 10.2,
            y: 66.0,
            z: -8.9,
        };
        assert_eq!(
            testforblock_args(["~", "~-1", "~3"], "minecraft:chest", None, Some(pos)).unwrap(),
            (10, 65, -6, 54, None)
        );
        assert_eq!(
            testforblock_args(["0", "64", "3"], "wool", Some("3"), None).unwrap(),
            (0, 64, 3, 35, Some(3))
        );
        assert!(testforblock_args(["~", "64", "3"], "stone", None, None).is_err());
        assert!(testforblock_args(["0", "256", "3"], "stone", None, None).is_err());
        assert!(testforblock_args(["0", "64", "3"], "stone", Some("16"), None).is_err());
        assert!(testforblock_args(["0", "64", "3"], "netherite_block", None, None).is_err());
    }

    #[test]
    fn seed_query_never_invents_flat_world_seed() {
        assert_eq!(
            seed_text(&crate::world::World::natural(42)).unwrap(),
            "World seed: 42"
        );
        assert!(seed_text(&crate::world::World::new()).is_err());
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
