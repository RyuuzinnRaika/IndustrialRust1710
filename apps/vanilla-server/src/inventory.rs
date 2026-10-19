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
}
impl Default for Inventory {
    fn default() -> Self {
        Self {
            version: VERSION,
            selected: 0,
            slots: vec![None; SLOT_COUNT],
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
