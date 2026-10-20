//! Bounded, server-owned Creative inventory for Minecraft Java 1.7.10.
//! Window 0 has 45 slots; slots 36..44 are the nine hotbar entries.
//! Intentionally excludes NBT-equipped items, other windows, crafting, drops,
//! equipment and survival transactions until the matching game logic exists.
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const SLOT_COUNT: usize = 45;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChestDirection {
    InventoryOnly,
    IntoChest,
    OutOfChest,
}

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
    #[serde(default)]
    workbench: [Option<Stack>; 9],
}
impl Default for Inventory {
    fn default() -> Self {
        Self {
            version: VERSION,
            selected: 0,
            slots: vec![None; SLOT_COUNT],
            cursor: None,
            workbench: [None; 9],
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
            || self.slots[0].is_some()
            || self.slots[5..9].iter().any(Option::is_some)
            || self
                .workbench
                .iter()
                .flatten()
                .any(|stack| !stack.is_valid())
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

    /// Map a Protocol 5 window slot to a server-owned inventory array.
    /// Slot zero is *virtual* (the derived crafting output) and cannot be set.
    fn slot_index(window: u8, slot: i16) -> Option<(bool, usize)> {
        match (window, slot) {
            (0, 1..=4) => Some((false, slot as usize)),
            (0, 9..=44) => Some((false, slot as usize)),
            (1..=u8::MAX, 1..=9) => Some((true, (slot - 1) as usize)),
            (1..=u8::MAX, 10..=45) => Some((false, (slot - 1) as usize)),
            _ => None,
        }
    }
    fn lookup(&self, window: u8, slot: i16) -> Option<Stack> {
        let (workbench, index) = Self::slot_index(window, slot)?;
        if workbench {
            self.workbench[index]
        } else {
            self.slots[index]
        }
    }
    fn assign(&mut self, window: u8, slot: i16, stack: Option<Stack>) {
        if let Some((workbench, index)) = Self::slot_index(window, slot) {
            if workbench {
                self.workbench[index] = stack;
            } else {
                self.slots[index] = stack;
            }
        }
    }
    pub fn crafting_result(&self, window: u8) -> Option<Stack> {
        match window {
            0 => crate::crafting::result(&self.slots[1..5], 2),
            _ => crate::crafting::result(&self.workbench, 3),
        }
    }
    fn spend_ingredients(&mut self, window: u8) {
        // Called only after verifying a recipe and a destination with capacity.
        let size = if window == 0 { 4 } else { 9 };
        for index in 0..size {
            let slot = index + 1;
            if let Some(mut stack) = self.lookup(window, slot as i16) {
                stack.count -= 1;
                self.assign(window, slot as i16, (stack.count != 0).then_some(stack));
            }
        }
    }
    fn move_to_player(&mut self, window: u8, mut moving: Stack) -> Option<Stack> {
        // Used by Shift-clicking recipe output and ingredient slots.
        let begin = if window == 0 { 9 } else { 10 };
        let end = if window == 0 { 45 } else { 46 };
        for slot in begin..end {
            if let Some(mut stack) = self.lookup(window, slot) {
                if stack.id == moving.id && stack.damage == moving.damage && stack.count < 64 {
                    let count = (64 - stack.count).min(moving.count);
                    stack.count += count;
                    moving.count -= count;
                    self.assign(window, slot, Some(stack));
                    if moving.count == 0 {
                        return None;
                    }
                }
            }
        }
        for slot in begin..end {
            if self.lookup(window, slot).is_none() {
                self.assign(window, slot, Some(moving));
                return None;
            }
        }
        Some(moving)
    }
    fn take_recipe(&mut self, window: u8, shift: bool) -> bool {
        let Some(output) = self.crafting_result(window) else {
            return true;
        };
        if shift {
            // This state is cloned by the session before persistence. Requiring
            // full capacity prevents losing ingredients when output cannot fit.
            let mut candidate = self.clone();
            if candidate.move_to_player(window, output).is_some() {
                return true;
            }
            candidate.spend_ingredients(window);
            *self = candidate;
            return true;
        }
        let carried = match self.cursor {
            None => Some(output),
            Some(mut old)
                if old.id == output.id
                    && old.damage == output.damage
                    && u16::from(old.count) + u16::from(output.count) <= 64 =>
            {
                old.count += output.count;
                Some(old)
            }
            _ => return true,
        };
        self.spend_ingredients(window);
        self.cursor = carried;
        true
    }
    /// Execute only bounded, server-owned window transactions.
    ///
    /// Client-provided snapshots are never trusted; unsupported modes,
    /// unsupported armor slots and outside-drop return false.
    pub fn click_window(&mut self, window: u8, slot: i16, button: i8, mode: i8) -> bool {
        if slot == 0 {
            return match (mode, button) {
                (0, 0 | 1) => self.take_recipe(window, false),
                (1, 0 | 1) => self.take_recipe(window, true),
                _ => false,
            };
        }
        if Self::slot_index(window, slot).is_none() {
            return false;
        }
        match mode {
            0 if button == 0 || button == 1 => {
                let current = self.lookup(window, slot);
                let carried = self.cursor;
                let (new_slot, new_cursor) = match (current, carried, button) {
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
                    (Some(mut target), None, 1) => {
                        let n = target.count.div_ceil(2);
                        target.count -= n;
                        let hand = Stack { count: n, ..target };
                        ((target.count != 0).then_some(target), Some(hand))
                    }
                    (None, None, 1) => (None, None),
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
                    (target, hand, 1) => (hand, target),
                    _ => unreachable!("validated button"),
                };
                self.assign(window, slot, new_slot);
                self.cursor = new_cursor;
                true
            }
            1 if button == 0 || button == 1 => {
                // Shift ingredient slots to player; main <-> hotbar otherwise.
                let Some(mut moving) = self.lookup(window, slot) else {
                    return true;
                };
                let main_start = if window == 0 { 9 } else { 10 };
                let hotbar_start = if window == 0 { 36 } else { 37 };
                let destination = if slot < main_start {
                    main_start..hotbar_start + 9
                } else if slot >= hotbar_start {
                    main_start..hotbar_start
                } else {
                    hotbar_start..hotbar_start + 9
                };
                for target in destination.clone() {
                    if let Some(mut existing) = self.lookup(window, target) {
                        if existing.id == moving.id
                            && existing.damage == moving.damage
                            && existing.count < 64
                        {
                            let amount = (64 - existing.count).min(moving.count);
                            existing.count += amount;
                            moving.count -= amount;
                            self.assign(window, target, Some(existing));
                            if moving.count == 0 {
                                break;
                            }
                        }
                    }
                }
                if moving.count > 0 {
                    for target in destination {
                        if self.lookup(window, target).is_none() {
                            self.assign(window, target, Some(moving));
                            moving.count = 0;
                            break;
                        }
                    }
                }
                self.assign(window, slot, (moving.count > 0).then_some(moving));
                true
            }
            2 if (0..=8).contains(&button) => {
                let other = if window == 0 { 36 } else { 37 } + i16::from(button);
                let old = self.lookup(window, slot);
                let held = self.lookup(window, other);
                self.assign(window, slot, held);
                self.assign(window, other, old);
                true
            }
            _ => false,
        }
    }
    /// Compatibility wrapper for unit tests targeting the player inventory.
    #[cfg(test)]
    pub fn click(&mut self, slot: i16, button: i8, mode: i8) -> bool {
        self.click_window(0, slot, button, mode)
    }

    fn chest_stack(&self, chest: &[Option<Stack>; 27], slot: i16) -> Option<Stack> {
        if slot < 27 {
            chest[slot as usize]
        } else {
            self.slots[(slot - 18) as usize]
        }
    }
    fn set_chest_stack(
        &mut self,
        chest: &mut [Option<Stack>; 27],
        slot: i16,
        stack: Option<Stack>,
    ) {
        if slot < 27 {
            chest[slot as usize] = stack;
        } else {
            self.slots[(slot - 18) as usize] = stack;
        }
    }
    /// Single-chest 0..26 and player inventory 27..62.
    /// Rejection is None; directional writes must commit the *source* first
    /// to avoid item duplication across the non-atomic inventory/chest files.
    pub fn chest_click(
        &mut self,
        chest: &mut [Option<Stack>; 27],
        slot: i16,
        button: i8,
        mode: i8,
    ) -> Option<ChestDirection> {
        if !(0..63).contains(&slot) {
            return None;
        }
        let storage = slot < 27;
        let direction = if storage {
            if self.cursor.is_some() {
                ChestDirection::IntoChest
            } else {
                ChestDirection::OutOfChest
            }
        } else {
            ChestDirection::InventoryOnly
        };
        match mode {
            0 if button == 0 || button == 1 => {
                let existing = self.chest_stack(chest, slot);
                let carried = self.cursor;
                // Two unlike stacks must never be swapped across storage and
                // player files without a durable multi-file transaction.
                if storage
                    && existing.is_some()
                    && carried.is_some()
                    && (existing.unwrap().id != carried.unwrap().id
                        || existing.unwrap().damage != carried.unwrap().damage)
                {
                    return None;
                }
                let (new_slot, new_cursor) = match (existing, carried, button) {
                    (item, None, 0) => (None, item),
                    (None, item, 0) => (item, None),
                    (Some(mut target), Some(mut hand), 0)
                        if target.id == hand.id && target.damage == hand.damage =>
                    {
                        let count = (64 - target.count).min(hand.count);
                        target.count += count;
                        hand.count -= count;
                        (Some(target), (hand.count != 0).then_some(hand))
                    }
                    (target, hand, 0) => (hand, target),
                    (Some(mut target), None, 1) => {
                        let count = target.count.div_ceil(2);
                        target.count -= count;
                        (
                            (target.count != 0).then_some(target),
                            Some(Stack { count, ..target }),
                        )
                    }
                    (None, None, 1) => (None, None),
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
                    (target, hand, 1) => (hand, target),
                    _ => unreachable!("button validated"),
                };
                self.set_chest_stack(chest, slot, new_slot);
                self.cursor = new_cursor;
                Some(direction)
            }
            1 if button == 0 || button == 1 => {
                let Some(mut moving) = self.chest_stack(chest, slot) else {
                    return Some(direction);
                };
                let targets = if storage { 27..63 } else { 0..27 };
                for target in targets.clone() {
                    if let Some(mut existing) = self.chest_stack(chest, target) {
                        if existing.id == moving.id
                            && existing.damage == moving.damage
                            && existing.count < 64
                        {
                            let count = (64 - existing.count).min(moving.count);
                            existing.count += count;
                            moving.count -= count;
                            self.set_chest_stack(chest, target, Some(existing));
                            if moving.count == 0 {
                                break;
                            }
                        }
                    }
                }
                if moving.count > 0 {
                    for target in targets {
                        if self.chest_stack(chest, target).is_none() {
                            self.set_chest_stack(chest, target, Some(moving));
                            moving.count = 0;
                            break;
                        }
                    }
                }
                self.set_chest_stack(chest, slot, (moving.count != 0).then_some(moving));
                Some(if storage {
                    ChestDirection::OutOfChest
                } else {
                    ChestDirection::IntoChest
                })
            }
            2 if !storage && (0..=8).contains(&button) => {
                let target = 54 + i16::from(button);
                let a = self.chest_stack(chest, slot);
                let b = self.chest_stack(chest, target);
                self.set_chest_stack(chest, slot, b);
                self.set_chest_stack(chest, target, a);
                Some(ChestDirection::InventoryOnly)
            }
            _ => None,
        }
    }
    pub fn chest_window_items(&self, window: u8, chest: &[Option<Stack>; 27]) -> Vec<u8> {
        let mut out = vec![window];
        out.extend_from_slice(&63i16.to_be_bytes());
        for slot in 0..63 {
            Self::encode_slot(&mut out, self.chest_stack(chest, slot));
        }
        out
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
    pub fn window_items_for(&self, window: u8) -> Vec<u8> {
        let count: i16 = if window == 0 { 45 } else { 46 };
        let mut data = Vec::with_capacity(4 + count as usize * 7);
        data.push(window);
        data.extend_from_slice(&count.to_be_bytes());
        for slot in 0..count {
            let stack = if slot == 0 {
                self.crafting_result(window)
            } else {
                self.lookup(window, slot)
            };
            Self::encode_slot(&mut data, stack);
        }
        data
    }
    pub fn window_items(&self) -> Vec<u8> {
        self.window_items_for(0)
    }
    pub fn set_slot(&self, slot: i16) -> io::Result<Vec<u8>> {
        if !(0..SLOT_COUNT as i16).contains(&slot) {
            return Err(invalid("slot outside player window"));
        }
        let mut data = vec![0];
        data.extend_from_slice(&slot.to_be_bytes());
        Self::encode_slot(
            &mut data,
            if slot == 0 {
                self.crafting_result(0)
            } else {
                self.lookup(0, slot)
            },
        );
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chest_moves_are_bounded_and_conserve_items() {
        let mut inv = Inventory::default();
        let mut chest = [None; 27];
        inv.creative_set(
            36,
            Some(Stack {
                id: 5,
                count: 12,
                damage: 1,
            }),
        )
        .unwrap();
        let sum = |inv: &Inventory, chest: &[Option<Stack>; 27]| -> u32 {
            inv.slots
                .iter()
                .chain(chest.iter())
                .flatten()
                .map(|v| u32::from(v.count))
                .sum::<u32>()
                + inv.cursor.map_or(0, |v| u32::from(v.count))
        };
        assert_eq!(sum(&inv, &chest), 12);
        assert_eq!(
            inv.chest_click(&mut chest, 54, 0, 0),
            Some(ChestDirection::InventoryOnly)
        );
        assert_eq!(
            inv.chest_click(&mut chest, 0, 0, 0),
            Some(ChestDirection::IntoChest)
        );
        assert_eq!(chest[0].unwrap().count, 12);
        assert_eq!(sum(&inv, &chest), 12);
        assert_eq!(
            inv.chest_click(&mut chest, 0, 1, 0),
            Some(ChestDirection::OutOfChest)
        );
        assert_eq!(chest[0].unwrap().count, 6);
        assert_eq!(inv.cursor.unwrap().count, 6);
        assert_eq!(sum(&inv, &chest), 12);
        assert_eq!(
            inv.chest_click(&mut chest, 0, 0, 0),
            Some(ChestDirection::IntoChest)
        );
        assert_eq!(chest[0].unwrap().count, 12);
        assert_eq!(
            inv.chest_click(&mut chest, 0, 0, 1),
            Some(ChestDirection::OutOfChest)
        );
        assert_eq!(chest[0], None);
        assert_eq!(sum(&inv, &chest), 12);
        assert_eq!(inv.chest_click(&mut chest, 0, 1, 2), None); // cannot number swap chest
        assert_eq!(inv.chest_click(&mut chest, -999, 0, 0), None); // no outside drop
        assert_eq!(inv.chest_click(&mut chest, 63, 0, 0), None);
        assert_eq!(inv.chest_window_items(2, &chest)[..3], [2, 0, 63]);
        assert_eq!(sum(&inv, &chest), 12);
    }
    #[test]
    fn chest_rejects_cross_storage_swaps_with_unlike_items() {
        let mut inv = Inventory::default();
        let mut chest = [None; 27];
        chest[0] = Some(Stack {
            id: 1,
            count: 2,
            damage: 0,
        });
        inv.creative_set(
            36,
            Some(Stack {
                id: 3,
                count: 2,
                damage: 0,
            }),
        )
        .unwrap();
        assert!(inv.chest_click(&mut chest, 54, 0, 0).is_some());
        let before = (inv.clone(), chest);
        assert_eq!(inv.chest_click(&mut chest, 0, 0, 0), None);
        assert_eq!((inv, chest), before);
    }
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
        assert!(inventory.click(1, 0, 0)); // empty crafting grid click is a no-op
        assert!(!inventory.click(-999, 0, 0)); // outside item drops unsupported
        assert!(!inventory.click(10, 0, 5)); // drag painting unsupported
        assert_eq!(inventory, old);
        assert_eq!(&inventory.cursor_packet()[..3], &[255, 255, 255]);
    }

    #[test]
    fn player_grid_crafts_planks_and_table_without_minting_items() {
        let mut inv = Inventory::default();
        inv.creative_set(
            36,
            Some(Stack {
                id: 17,
                count: 1,
                damage: 0,
            }),
        )
        .unwrap();
        assert!(inv.click(36, 0, 0));
        assert!(inv.click(1, 0, 0)); // log into 2x2
        assert_eq!(
            inv.crafting_result(0),
            Some(Stack {
                id: 5,
                count: 4,
                damage: 0
            })
        );
        assert!(inv.click(0, 0, 0)); // obtain four planks
        assert_eq!(inv.slots[1], None);
        assert_eq!(inv.cursor.unwrap().count, 4);
        for slot in 1..=4 {
            assert!(inv.click(slot, 1, 0)); // put one plank in each crafting input
        }
        assert_eq!(
            inv.crafting_result(0),
            Some(Stack {
                id: 58,
                count: 1,
                damage: 0
            })
        );
        assert!(inv.click(0, 0, 0)); // take one crafting table
        assert_eq!(inv.cursor.unwrap().id, 58);
        assert_eq!(inv.slots[1..5], [None; 4]);
        assert_eq!(inv.crafting_result(0), None);
        assert!(inv.click(0, 0, 0)); // no recipe, cannot create another table
        assert_eq!(inv.cursor.unwrap().count, 1);
    }
    #[test]
    fn workbench_crafts_pickaxe_and_rejects_wrong_window() {
        let mut inv = Inventory::default();
        for slot in [1, 2, 3] {
            inv.workbench[slot - 1] = Some(Stack {
                id: 5,
                count: 1,
                damage: 0,
            });
        }
        for slot in [5, 8] {
            inv.workbench[slot - 1] = Some(Stack {
                id: 280,
                count: 1,
                damage: 0,
            });
        }
        assert_eq!(inv.crafting_result(1).unwrap().id, 270);
        assert!(inv.click_window(1, 0, 0, 0));
        assert_eq!(inv.cursor.unwrap().id, 270);
        assert!(inv.workbench.iter().all(Option::is_none));
        assert!(!inv.click_window(1, 46, 0, 0));
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
