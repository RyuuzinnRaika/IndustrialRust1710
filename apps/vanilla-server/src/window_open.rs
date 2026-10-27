//! Minecraft Java 1.7.10 (Protocol 5) clientbound Open Window (0x2D).
//! This version uses a numeric window type, unlike 1.8+ namespaced IDs.
//! Wire: window ID (u8), window type (u8), title (VarInt UTF-8),
//! slot count (u8), use provided title (bool).
use legacy_protocol::write_string;
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum WindowKind {
    Chest = 0,
    CraftingTable = 1,
    Furnace = 2,
    Dispenser = 3,
    EnchantingTable = 4,
    BrewingStand = 5,
    #[allow(dead_code)] // Reserved until server-owned merchant GUI is wired.
    Villager = 6,
    #[allow(dead_code)] // Reserved until server-owned beacon GUI is wired.
    Beacon = 7,
    Anvil = 8,
    Hopper = 9,
    Dropper = 10,
    #[allow(dead_code)] // Reserved until live horse inventory is wired.
    Horse = 11,
}

pub(crate) fn encode(
    window_id: u8,
    kind: WindowKind,
    title: &str,
    slots: u8,
) -> io::Result<Vec<u8>> {
    if kind == WindowKind::Horse {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "horse windows need a trailing entity ID",
        ));
    }
    let mut payload = vec![window_id, kind as u8];
    write_string(title, &mut payload)?;
    payload.extend_from_slice(&[slots, 1]); // Explicit title, not a translation key.
    Ok(payload)
}

/// Horse inventory S2D in Protocol 5 has a trailing *big-endian* entity ID.
/// Do not use the generic encoder: omitting this field corrupts the packet.
#[allow(dead_code)] // Wire foundation; live horse inventory is tracked in #573.
pub(crate) fn encode_horse(
    window_id: u8,
    title: &str,
    slots: u8,
    entity_id: i32,
) -> io::Result<Vec<u8>> {
    if entity_id <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid horse entity ID",
        ));
    }
    let mut payload = vec![window_id, WindowKind::Horse as u8];
    write_string(title, &mut payload)?;
    payload.extend_from_slice(&[slots, 1]);
    payload.extend_from_slice(&entity_id.to_be_bytes());
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_supported_window_kinds_have_legacy_protocol5_wire_layout() {
        let cases = [
            (WindowKind::Chest, 0, "Chest", 27),
            (WindowKind::Chest, 0, "Large Chest", 54),
            (WindowKind::CraftingTable, 1, "Crafting", 0),
            (WindowKind::Furnace, 2, "Furnace", 3),
            (WindowKind::Dispenser, 3, "Dispenser", 9),
            (WindowKind::EnchantingTable, 4, "Enchant", 0),
            (WindowKind::BrewingStand, 5, "Brewing Stand", 4),
            (WindowKind::Villager, 6, "Villager", 3),
            (WindowKind::Beacon, 7, "Beacon", 1),
            (WindowKind::Anvil, 8, "Repair & Name", 0),
            (WindowKind::Hopper, 9, "Hopper", 5),
            (WindowKind::Dropper, 10, "Dropper", 9),
        ];
        for (kind, wire_type, title, slots) in cases {
            let mut expected = vec![42, wire_type, title.len() as u8];
            expected.extend_from_slice(title.as_bytes());
            expected.extend_from_slice(&[slots, 1]);
            assert_eq!(
                encode(42, kind, title, slots).unwrap(),
                expected,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn window_title_uses_utf8_byte_length_not_character_count() {
        assert_eq!(
            encode(1, WindowKind::Chest, "箱", 27).unwrap(),
            [1, 0, 3, 0xe7, 0xae, 0xb1, 27, 1]
        );
    }
    #[test]
    fn horse_open_window_requires_an_entity_id_after_the_title_and_slots() {
        assert!(encode(4, WindowKind::Horse, "Horse", 2).is_err());
        assert!(encode_horse(4, "Horse", 2, 0).is_err());
        assert_eq!(
            encode_horse(4, "Horse", 2, 0x01020304).unwrap(),
            [4, 11, 5, b'H', b'o', b'r', b's', b'e', 2, 1, 1, 2, 3, 4],
        );
    }
}
