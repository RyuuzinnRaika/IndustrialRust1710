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
    Anvil = 8,
    Hopper = 9,
    Dropper = 10,
}

pub(crate) fn encode(
    window_id: u8,
    kind: WindowKind,
    title: &str,
    slots: u8,
) -> io::Result<Vec<u8>> {
    let mut payload = vec![window_id, kind as u8];
    write_string(title, &mut payload)?;
    payload.extend_from_slice(&[slots, 1]); // Explicit title, not a translation key.
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
            (WindowKind::Anvil, 8, "Repair & Name", 0),
            (WindowKind::Hopper, 9, "Hopper", 5),
            (WindowKind::Dropper, 10, "Dropper", 9),
        ];
        for (kind, wire_type, title, slots) in cases {
            let mut expected = vec![42, wire_type, title.len() as u8];
            expected.extend_from_slice(title.as_bytes());
            expected.extend_from_slice(&[slots, 1]);
            assert_eq!(encode(42, kind, title, slots).unwrap(), expected, "{kind:?}");
        }
    }

    #[test]
    fn window_title_uses_utf8_byte_length_not_character_count() {
        assert_eq!(
            encode(1, WindowKind::Chest, "箱", 27).unwrap(),
            [1, 0, 3, 0xe7, 0xae, 0xb1, 27, 1]
        );
    }
}
