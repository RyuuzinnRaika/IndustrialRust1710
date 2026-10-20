//! Bounded, server-owned Creative inventory for Minecraft Java 1.7.10.
//! Window 0 has 45 slots; slots 36..44 are the nine hotbar entries.
//! Intentionally excludes NBT-equipped items, other windows, crafting, drops,
//! equipment and survival transactions until the matching game logic exists.
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const SLOT_COUNT: usize = 45;
const MAX_SAVE_BYTES: u64 = 16 * 1024;
const VERSION: u8 = 1;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    pub id: i16,
    pub count: u8,
    pub damage: i16,
}
impl Stack {
    pub fn is_valid(self) -> bool {
        (1..=4095).contains(&self.id) && (1..=64).contains(&self.count) && self.damage >= 0
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    version: u8,
    selected: u8,
    slots: Vec<Option<Stack>>,
    // Carried items must survive disconnects: otherwise a click could erase them.
    // serde(default) also loads older version-1 saves without the new field.
    #[serde(default)]
    cursor: Option<Stack>,
}
impl Default for Inventory {
    fn default() -> Self {
        Self {
            version: VERSION,
            selected: 0,
            slots: vec![None; SLOT_COUNT],
            cursor: None,
        }
    }
}
impl Inventory {
    pub fn location(player_path: &Path) -> PathBuf {
        player_path.with_extension("inventory.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || self.selected > 8
            || self.slots.len() != SLOT_COUNT
            || self.slots[..9].iter().any(Option::is_some)
            || self.slots.iter().flatten().any(|stack| !stack.is_valid())
            || self.cursor.is_some_and(|stack| !stack.is_valid())
        {
            return Err(invalid("invalid Creative inventory save"));
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(value) => value,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_SAVE_BYTES {
            return Err(invalid("inventory save size or type is invalid"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_SAVE_BYTES {
            return Err(invalid("inventory save grew beyond size cap"));
        }
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| invalid("inventory save is malformed"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("could not encode inventory"))?;
        if bytes.len() as u64 > MAX_SAVE_BYTES {
            return Err(invalid("inventory save exceeds limit"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("inventory.tmp");
        let mut output = File::create(&temp)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        drop(output);
        // Atomic on supported filesystems; never delete original before renaming.
        fs::rename(&temp, path)
    }
    pub fn selected(&self) -> u8 {
        self.selected
    }
    pub fn select(&mut self, slot: i16) -> io::Result<()> {
        if !(0..=8).contains(&slot) {
            return Err(invalid("invalid selected hotbar index"));
        }
        self.selected = slot as u8;
        Ok(())
    }
    pub fn creative_set(&mut self, slot: i16, stack: Option<Stack>) -> io::Result<bool> {
        if stack.is_some_and(|item| !item.is_valid()) {
            return Err(invalid("invalid Creative item"));
        }
        // No crafting/armor/dragged item windows implemented. Client-side
        // destroy slot and world drops are intentionally unsupported.
        if !(9..45).contains(&slot) {
            return Ok(false);
        }
        self.slots[slot as usize] = stack;
        Ok(true)
    }
    /// Add one mined item directly to inventory (temporary until entity pickup
    /// is implemented). Prefer the hotbar, then main inventory; never overflow.
    pub fn receive_one(&mut self, item: Stack) -> Option<i16> {
        if !item.is_valid() || item.count != 1 {
            return None;
        }
        let slots = (36..45).chain(9..36);
        for slot in slots.clone() {
            if let Some(stack) = self.slots[slot].as_mut() {
                if stack.id == item.id && stack.damage == item.damage && stack.count < 64 {
                    stack.count += 1;
                    return Some(slot as i16);
                }
            }
        }
        for slot in slots {
            if self.slots[slot].is_none() {
                self.slots[slot] = Some(item);
                return Some(slot as i16);
            }
        }
        None
    }
    /// Commit one server-owned held item consumption (Survival only).
    pub fn consume_held(&mut self) -> bool {
        let slot = &mut self.slots[36 + self.selected as usize];
        let Some(stack) = slot.as_mut() else {
            return false;
        };
        if stack.count > 1 {
            stack.count -= 1;
        } else {
            *slot = None;
        }
        true
    }
    pub fn held_slot(&self) -> i16 {
        36 + i16::from(self.selected)
    }
    pub fn held(&self) -> Option<Stack> {
        self.slots[36 + self.selected as usize]
    }
    pub fn can_place(&self, claimed: Option<Stack>) -> bool {
        match (self.held(), claimed) {
            (Some(authoritative), Some(request)) => {
                authoritative.id == request.id
                    && authoritative.damage == request.damage
                    && request.count >= 1
            }
            _ => false,
        }
    }

    /// Protocol 5 window 0: normal clicks, shift transfers, and number keys.
    ///
    /// The client-supplied clicked stack is NEVER used to mutate authoritative
    /// items. Unsupported actions return false so the caller can reject them
    /// with Confirm Transaction and resynchronize the window.
    pub fn click(&mut self, slot: i16, button: i8, mode: i8) -> bool {
        if !(9..SLOT_COUNT as i16).contains(&slot) {
            return false; // Crafting, armor, and outside-drop are not implemented.
        }
        let index = slot as usize;
        match mode {
            0 if button == 0 || button == 1 => {
                let current = self.slots[index];
                let carried = self.cursor;
                let (new_slot, new_cursor) = match (current, carried, button) {
                    // Left click takes the whole stack, deposits it or swaps.
                    (item, None, 0) => (None, item),
                    (None, item, 0) => (item, None),
                    (Some(mut target), Some(mut hand), 0)
                        if target.id == hand.id && target.damage == hand.damage =>
                    {
                        let n = (64 - target.count).min(hand.count);
                        target.count += n;
                        hand.count -= n;
                        (Some(target), (hand.count != 0).then_some(hand))
                    }
                    (target, hand, 0) => (hand, target),
                    // Right click takes the upper half, rounded up.
                    (Some(mut target), None, 1) => {
                        let n = target.count.div_ceil(2);
                        target.count -= n;
                        let hand = Stack { count: n, ..target };
                        ((target.count != 0).then_some(target), Some(hand))
                    }
                    (None, None, 1) => (None, None),
                    // Right click places a single item into an empty slot.
                    (None, Some(mut hand), 1) => {
                        hand.count -= 1;
                        (
                            Some(Stack { count: 1, ..hand }),
                            (hand.count != 0).then_some(hand),
                        )
                    }
                    (Some(mut target), Some(mut hand), 1)
                        if target.id == hand.id
                            && target.damage == hand.damage
                            && target.count < 64 =>
                    {
                        target.count += 1;
                        hand.count -= 1;
                        (Some(target), (hand.count != 0).then_some(hand))
                    }
                    // An incompatible stack swaps, including on right click.
                    (target, hand, 1) => (hand, target),
                    _ => unreachable!("button was validated"),
                };
                self.slots[index] = new_slot;
                self.cursor = new_cursor;
                true
            }
            1 if button == 0 || button == 1 => {
                // Shift click transfers hotbar <-> main inventory, merging
                // stacks first. No cursor changes and no item duplication.
                let Some(mut moving) = self.slots[index].take() else {
                    return true;
                };
                let destination = if index >= 36 { 9..36 } else { 36..45 };
                for target in destination.clone() {
                    if let Some(stack) = self.slots[target].as_mut() {
                        if stack.id == moving.id
                            && stack.damage == moving.damage
                            && stack.count < 64
                        {
                            let count = (64 - stack.count).min(moving.count);
                            stack.count += count;
                            moving.count -= count;
                            if moving.count == 0 {
                                break;
                            }
                        }
                    }
                }
                if moving.count > 0 {
                    for target in destination {
                        if self.slots[target].is_none() {
                            self.slots[target] = Some(moving);
                            moving.count = 0;
                            break;
                        }
                    }
                }
                self.slots[index] = (moving.count > 0).then_some(moving);
                true
            }
            2 if (0..=8).contains(&button) => {
                // Hotbar number-key swap, restricted to real inventory slots.
                self.slots.swap(index, 36 + button as usize);
                true
            }
            _ => false,
        }
    }
    /// Clientbound Set Slot (0x2F): window -1, slot -1 represents the cursor.
    pub fn cursor_packet(&self) -> Vec<u8> {
        let mut out = vec![255, 255, 255];
        Self::encode_slot(&mut out, self.cursor);
        out
    }
    fn encode_slot(out: &mut Vec<u8>, slot: Option<Stack>) {
        match slot {
            None => out.extend_from_slice(&(-1i16).to_be_bytes()),
            Some(value) => {
                out.extend_from_slice(&value.id.to_be_bytes());
                out.push(value.count);
                out.extend_from_slice(&value.damage.to_be_bytes());
                out.extend_from_slice(&(-1i16).to_be_bytes()); // no NBT payload
            }
        }
    }
    pub fn window_items(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(4 + self.slots.len() * 7);
        data.push(0); // player inventory window
        data.extend_from_slice(&(SLOT_COUNT as i16).to_be_bytes());
        for &slot in &self.slots {
            Self::encode_slot(&mut data, slot);
        }
        data
    }
    pub fn set_slot(&self, slot: i16) -> io::Result<Vec<u8>> {
        if !(0..SLOT_COUNT as i16).contains(&slot) {
            return Err(invalid("slot outside player window"));
        }
        let mut data = vec![0]; // window 0
        data.extend_from_slice(&slot.to_be_bytes());
        Self::encode_slot(&mut data, self.slots[slot as usize]);
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_zero_moves_stacks_without_creating_or_losing_items() {
        let mut inventory = Inventory::default();
        let a = Stack {
            id: 3,
            count: 17,
            damage: 0,
        };
        let b = Stack {
            id: 3,
            count: 60,
            damage: 0,
        };
        inventory.creative_set(36, Some(a)).unwrap();
        inventory.creative_set(10, Some(b)).unwrap();
        let total = |state: &Inventory| -> u32 {
            state
                .slots
                .iter()
                .flatten()
                .map(|item| u32::from(item.count))
                .sum::<u32>()
                + state.cursor.map_or(0, |item| u32::from(item.count))
        };
        assert_eq!(total(&inventory), 77);
        assert!(inventory.click(36, 0, 0)); // take all 17
        assert_eq!(inventory.cursor.unwrap().count, 17);
        assert!(inventory.click(10, 0, 0)); // merge 4 into the 60 stack
        assert_eq!(inventory.slots[10].unwrap().count, 64);
        assert_eq!(inventory.cursor.unwrap().count, 13);
        assert!(inventory.click(11, 1, 0)); // place one by right click
        assert_eq!(inventory.slots[11].unwrap().count, 1);
        assert_eq!(inventory.cursor.unwrap().count, 12);
        assert!(inventory.click(12, 0, 0)); // place the rest
        assert_eq!(inventory.cursor, None);
        assert_eq!(total(&inventory), 77);
        assert!(inventory.click(12, 0, 1)); // shift back to hotbar
        assert!(inventory.click(11, 2, 2)); // hotbar key 3 swap
        assert_eq!(total(&inventory), 77);
        let old = inventory.clone();
        assert!(!inventory.click(1, 0, 0)); // crafting grid unsupported
        assert!(!inventory.click(-999, 0, 0)); // outside item drops unsupported
        assert!(!inventory.click(10, 0, 5)); // drag painting unsupported
        assert_eq!(inventory, old);
        assert_eq!(&inventory.cursor_packet()[..3], &[255, 255, 255]);
    }

    #[test]
    fn cursor_roundtrip_and_legacy_save_are_safe() {
        let mut inventory = Inventory::default();
        inventory
            .creative_set(
                36,
                Some(Stack {
                    id: 4,
                    count: 3,
                    damage: 0,
                }),
            )
            .unwrap();
        inventory.click(36, 1, 0);
        assert_eq!(inventory.cursor.unwrap().count, 2);
        let save = std::env::temp_dir().join(format!(
            "ir1710-cursor-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        inventory.save(&save).unwrap();
        assert_eq!(Inventory::load(&save).unwrap(), inventory);
        let raw = std::fs::read_to_string(&save).unwrap();
        let legacy = raw.replace(",\"cursor\":{\"id\":4,\"count\":2,\"damage\":0}", "");
        std::fs::write(&save, legacy).unwrap();
        assert!(
            Inventory::load(&save).is_ok(),
            "older inventory save remains readable"
        );
        let _ = std::fs::remove_file(save);
    }
    #[test]
    fn creative_hotbar_boundaries() {
        let mut inv = Inventory::default();
        assert_eq!(inv.window_items().len(), 93); // 3 + 45*2
        assert_eq!(&inv.set_slot(36).unwrap(), &[0, 0, 36, 255, 255]);
        assert!(!inv.creative_set(-1, None).unwrap());
        assert!(!inv.creative_set(45, None).unwrap());
        assert!(!inv.creative_set(5, None).unwrap());
        assert!(inv.select(9).is_err());
        assert!(inv
            .creative_set(
                36,
                Some(Stack {
                    id: 4,
                    count: 64,
                    damage: 0
                })
            )
            .unwrap());
        assert!(inv.can_place(Some(Stack {
            id: 4,
            count: 1,
            damage: 0
        })));
        assert!(!inv.can_place(Some(Stack {
            id: 5,
            count: 64,
            damage: 0
        })));
        assert_eq!(
            inv.set_slot(36).unwrap(),
            vec![0, 0, 36, 0, 4, 64, 0, 0, 255, 255]
        );
        inv.select(1).unwrap();
        assert!(inv.held().is_none());
        assert!(!inv.can_place(Some(Stack {
            id: 4,
            count: 64,
            damage: 0
        })));
        assert!(inv
            .creative_set(
                37,
                Some(Stack {
                    id: 5,
                    count: 1,
                    damage: 2
                })
            )
            .unwrap());
        assert!(inv.can_place(Some(Stack {
            id: 5,
            count: 1,
            damage: 2
        })));
    }
    #[test]
    fn survival_items_stack_then_consume_without_duplication() {
        let mut inv = Inventory::default();
        let item = Stack {
            id: 3,
            count: 1,
            damage: 0,
        };
        assert_eq!(inv.receive_one(item), Some(36));
        assert_eq!(inv.receive_one(item), Some(36));
        assert_eq!(inv.held().unwrap().count, 2);
        assert!(inv.consume_held());
        assert_eq!(inv.held().unwrap().count, 1);
        assert!(inv.consume_held());
        assert!(inv.held().is_none());
        assert!(!inv.consume_held());
        assert!(inv
            .receive_one(Stack {
                id: 7,
                count: 65,
                damage: 0
            })
            .is_none());
    }
    #[test]
    fn saves_and_fails_closed_on_corruption() {
        let dir = std::env::temp_dir().join(format!("ir1710-inventory-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("positioner.inventory.json");
        let mut inv = Inventory::default();
        inv.creative_set(
            36,
            Some(Stack {
                id: 4,
                count: 64,
                damage: 0,
            }),
        )
        .unwrap();
        inv.select(0).unwrap();
        inv.save(&path).unwrap();
        assert_eq!(Inventory::load(&path).unwrap(), inv);
        fs::write(&path, b"{broken").unwrap();
        assert!(Inventory::load(&path).is_err());
        fs::write(&path, b"[]").unwrap();
        assert!(Inventory::load(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
