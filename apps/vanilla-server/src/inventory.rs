//! Bounded, server-owned Creative inventory for Minecraft Java 1.7.10.
//! Window 0 has 45 slots; slots 36..44 are the nine hotbar entries.
//! Intentionally excludes NBT-equipped items, other windows, crafting, drops,
//! equipment and survival transactions until the matching game logic exists.
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const SLOT_COUNT: usize = 45;
/// Protocol 5 player armor: helmet, chestplate, leggings, boots.
fn armor_accepts(slot: i16, item: Stack) -> bool {
    if item.count != 1 {
        return false;
    }
    matches!(
        (slot, item.id),
        (5, 86 | 298 | 302 | 306 | 310 | 314 | 397)
            | (6, 299 | 303 | 307 | 311 | 315)
            | (7, 300 | 304 | 308 | 312 | 316)
            | (8, 301 | 305 | 309 | 313 | 317)
    )
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Drag {
    window: u8,
    button: Option<i8>,
    slots: Vec<i16>,
}

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
    /// Legacy Java 1.7.10 stack limits. Tool, armor, potion and container
    /// items must not silently merge into a 64-stack in any window.
    pub fn max_count(self) -> u8 {
        crate::items_1710::max_count(self.id)
    }
    pub fn is_valid(self) -> bool {
        crate::items_1710::valid_stack(self.id, self.count, self.damage)
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
    #[serde(skip)]
    drag: Drag,
    #[serde(default)]
    bed_spawn: Option<[i32; 3]>,
}
impl Default for Inventory {
    fn default() -> Self {
        Self {
            version: VERSION,
            selected: 0,
            slots: vec![None; SLOT_COUNT],
            cursor: None,
            workbench: [None; 9],
            drag: Drag::default(),
            bed_spawn: None,
        }
    }
}
impl Inventory {
    /// Equipment slots are 5=helmet, 6=chest, 7=leggings, 8=boots in window zero.
    fn armor_compatible(slot: usize, stack: Stack) -> bool {
        armor_accepts(slot as i16, stack)
    }
    /// Damage limits for Java 1.7.10 armor (helmet, chest, legs, boots).
    /// Decorative pumpkins and skulls do not take armor wear.
    fn armor_durability(id: i16) -> Option<i16> {
        match id {
            298 => Some(55),
            299 => Some(80),
            300 => Some(75),
            301 => Some(65),
            302 | 306 => Some(165),
            303 | 307 => Some(240),
            304 | 308 => Some(225),
            305 | 309 => Some(195),
            310 => Some(363),
            311 => Some(528),
            312 => Some(495),
            313 => Some(429),
            314 => Some(77),
            315 => Some(112),
            316 => Some(105),
            317 => Some(91),
            _ => None,
        }
    }

    /// Wear all equipped armor on an armor-blockable attack. In 1.7.10
    /// durability loss is max(1, floor(raw damage / 4)) per equipped piece.
    /// Caller persists the staged inventory before acknowledging the hit.
    pub fn wear_armor_on_hit(&mut self, incoming_damage: u8) -> bool {
        if incoming_damage == 0 {
            return false;
        }
        let wear = (i16::from(incoming_damage) / 4).max(1);
        let mut changed = false;
        for slot in &mut self.slots[5..9] {
            let Some(mut piece) = *slot else {
                continue;
            };
            let Some(max_uses) = Self::armor_durability(piece.id) else {
                continue;
            };
            piece.damage = piece.damage.saturating_add(wear);
            *slot = (piece.damage < max_uses).then_some(piece);
            changed = true;
        }
        changed
    }

    pub fn armor_points(&self) -> u8 {
        self.slots[5..9]
            .iter()
            .flatten()
            .map(|stack| match stack.id {
                298 | 301 => 1,
                299 => 3,
                300 => 2,
                302 | 306 | 314 => 2,
                303 | 307 => 5,
                304 | 308 => 4,
                305 | 309 | 317 => 1,
                310 => 3,
                311 => 8,
                312 => 6,
                313 => 3,
                315 => 5,
                316 => 3,
                _ => 0,
            })
            .sum::<u8>()
            .min(20)
    }
    /// Consume one item by ID from Survival player slots without trusting client claims.
    pub fn consume_item(&mut self, id: i16) -> bool {
        for index in (36..45).chain(9..36) {
            if let Some(mut stack) = self.slots[index] {
                if stack.id == id {
                    stack.count -= 1;
                    self.slots[index] = (stack.count > 0).then_some(stack);
                    return true;
                }
            }
        }
        false
    }
    pub fn location(player_path: &Path) -> PathBuf {
        player_path.with_extension("inventory.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || self.selected > 8
            || self.slots.len() != SLOT_COUNT
            || self
                .bed_spawn
                .is_some_and(|[x, y, z]| !crate::world::in_bounds(x, y, z))
            || self.slots[0].is_some()
            || self.slots[5..9]
                .iter()
                .enumerate()
                .any(|(index, stack)| stack.is_some_and(|s| !Self::armor_compatible(index + 5, s)))
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
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("encode inventory"))?;
        if bytes.len() as u64 > MAX_SAVE_BYTES {
            return Err(invalid("inventory too large"));
        }
        Ok(bytes)
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
    pub fn bed_spawn(&self) -> Option<(i32, i32, i32)> {
        self.bed_spawn.map(|[x, y, z]| (x, y, z))
    }
    pub fn set_bed_spawn(&mut self, x: i32, y: i32, z: i32) -> io::Result<()> {
        if !crate::world::in_bounds(x, y, z) {
            return Err(invalid("invalid bed spawn"));
        }
        self.bed_spawn = Some([x, y, z]);
        Ok(())
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
        // Client-side destroy slot and world drops are unsupported.
        if !(9..45).contains(&slot) && !(5..=8).contains(&slot) {
            return Ok(false);
        }
        if (5..=8).contains(&slot) && stack.is_some_and(|s| !armor_accepts(slot, s)) {
            return Ok(false);
        }
        self.slots[slot as usize] = stack;
        Ok(true)
    }
    /// Accept an entire ground-item stack atomically. Return all modified
    /// window-zero slots; if none fit, leave the inventory untouched.
    pub fn receive_stack(&mut self, item: Stack) -> Option<Vec<i16>> {
        if !item.is_valid() {
            return None;
        }
        let mut next = self.clone();
        let mut remaining = item.count;
        let mut changed = Vec::new();
        for slot in (36..45).chain(9..36) {
            if let Some(current) = next.slots[slot].as_mut() {
                if current.id == item.id
                    && current.damage == item.damage
                    && current.count < current.max_count()
                {
                    let add = (current.max_count() - current.count).min(remaining);
                    current.count += add;
                    remaining -= add;
                    changed.push(slot as i16);
                    if remaining == 0 {
                        *self = next;
                        return Some(changed);
                    }
                }
            }
        }
        for slot in (36..45).chain(9..36) {
            if next.slots[slot].is_none() {
                next.slots[slot] = Some(Stack {
                    count: remaining,
                    ..item
                });
                changed.push(slot as i16);
                *self = next;
                return Some(changed);
            }
        }
        None
    }
    /// Q removes one held item; Ctrl+Q removes the entire held stack.
    /// Take an outside-window cursor drop. The caller MUST commit this debit
    /// together with the spawned ground-item snapshot before publishing state.
    /// Protocol 5: left-click drops the full cursor, right-click drops one.
    pub fn drop_cursor(&mut self, button: i8) -> Option<Stack> {
        let mut current = self.cursor?;
        match button {
            0 => {
                self.cursor = None;
                Some(current)
            }
            1 => {
                current.count -= 1;
                self.cursor = (current.count > 0).then_some(current);
                Some(Stack {
                    count: 1,
                    ..current
                })
            }
            _ => None,
        }
    }

    pub fn drop_held(&mut self, whole_stack: bool) -> Option<Stack> {
        let slot = &mut self.slots[36 + self.selected as usize];
        let held = slot.take()?;
        if !whole_stack && held.count > 1 {
            *slot = Some(Stack {
                count: held.count - 1,
                ..held
            });
            return Some(Stack { count: 1, ..held });
        }
        Some(held)
    }
    /// Snapshot all droppable player possessions before clearing them.
    /// Does not include the virtual crafting result or empty armor slots.
    pub fn possessions(&self) -> Vec<Stack> {
        // Protocol 5 window 0: slots 1..4 are the 2x2 crafting grid,
        // 5..8 are armor, and 9..44 are the backpack and hotbar. Each
        // occupied slot must be counted exactly once or death duplicates gear.
        self.slots[1..45]
            .iter()
            .chain(self.workbench.iter())
            .chain(std::slice::from_ref(&self.cursor))
            .flatten()
            .copied()
            .collect()
    }
    /// Clear precisely the same slots included by possessions(). This is
    /// committed atomically with the ground-item snapshot by the caller.
    pub fn clear_possessions(&mut self) {
        self.slots[1..45].fill(None);
        self.workbench.fill(None);
        self.cursor = None;
        self.drag = Drag::default();
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
    /// Convert one held empty bucket (325) to milk (335) only when the
    /// entire resulting inventory fits. In particular, a stacked bucket
    /// must never lose one unit when there is no free destination slot.
    pub fn fill_milk_bucket(&mut self) -> bool {
        if !self.held().is_some_and(|item| item.id == 325) {
            return false;
        }
        let mut next = self.clone();
        let count = next.held().map_or(0, |item| item.count);
        let milk = Stack {
            id: 335,
            count: 1,
            damage: 0,
        };
        if count == 1 {
            let slot = next.held_slot() as usize;
            next.slots[slot] = Some(milk);
        } else if !next.consume_held() || next.receive_stack(milk).is_none() {
            return false;
        }
        *self = next;
        true
    }
    /// Damage one server-held, unenchanted tool on a committed block break.
    /// Returns true only if a real tool was damaged or broke. Hand mining
    /// and ordinary blocks/items do not acquire arbitrary damage metadata.
    pub fn wear_held_tool(&mut self) -> bool {
        let slot = &mut self.slots[36 + self.selected as usize];
        let Some(mut value) = *slot else {
            return false;
        };
        let Some(max_uses) = crate::survival::tool_max_uses(value) else {
            return false;
        };
        value.damage += 1;
        *slot = (value.damage < max_uses).then_some(value);
        true
    }
    /// One durability point when a fishing float is reeled in.
    /// Fishing rods are distinct from mining tools in 1.7.10.
    pub fn wear_fishing_rod(&mut self) -> bool {
        let slot = &mut self.slots[36 + self.selected as usize];
        let Some(mut rod) = *slot else { return false };
        if rod.id != 346 || rod.count != 1 || !(0..64).contains(&rod.damage) {
            return false;
        }
        rod.damage += 1;
        *slot = (rod.damage < 64).then_some(rod);
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
            (0, 1..=8) => Some((false, slot as usize)),
            (0, 5..=44) => Some((false, slot as usize)),
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
        // Only after a recipe output has a durable destination.
        // In 1.7.10 crafting a cake consumes milk but leaves empty buckets
        // in the three original grid slots. No extra inventory space needed.
        let cake = self
            .crafting_result(window)
            .is_some_and(|output| output.id == 354);
        let size = if window == 0 { 4 } else { 9 };
        for index in 0..size {
            let slot = index + 1;
            if let Some(mut stack) = self.lookup(window, slot as i16) {
                stack.count -= 1;
                let next = if cake && stack.id == 335 {
                    Some(Stack {
                        id: 325,
                        count: 1,
                        damage: 0,
                    })
                } else {
                    (stack.count != 0).then_some(stack)
                };
                self.assign(window, slot as i16, next);
            }
        }
    }
    fn move_to_player(&mut self, window: u8, mut moving: Stack) -> Option<Stack> {
        // Used by Shift-clicking recipe output and ingredient slots.
        let begin = if window == 0 { 9 } else { 10 };
        let end = if window == 0 { 45 } else { 46 };
        for slot in begin..end {
            if let Some(mut stack) = self.lookup(window, slot) {
                if stack.id == moving.id
                    && stack.damage == moving.damage
                    && stack.count < moving.max_count()
                {
                    let count = (moving.max_count() - stack.count).min(moving.count);
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
            // Craft repeatedly until ingredients or destination space run out.
            // Keep each iteration atomic: a partly fitting result must never
            // consume a recipe or create an item. The caller journals the batch.
            let mut batch = self.clone();
            let mut changed = false;
            for _ in 0..64 {
                let Some(product) = batch.crafting_result(window) else {
                    break;
                };
                let mut next = batch.clone();
                if next.move_to_player(window, product).is_some() {
                    break;
                }
                next.spend_ingredients(window);
                batch = next;
                changed = true;
            }
            if changed {
                *self = batch;
            }
            return true;
        }
        let carried = match self.cursor {
            None => Some(output),
            Some(mut old)
                if old.id == output.id
                    && old.damage == output.damage
                    && u16::from(old.count) + u16::from(output.count)
                        <= u16::from(output.max_count()) =>
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
    /// Vanilla click-drag is a three-phase protocol; client-provided stack counts
    /// are never trusted.
    fn drag_click(&mut self, window: u8, slot: i16, button: i8) -> bool {
        let kind = match button {
            0..=2 => 0,
            4..=6 => 4,
            _ => return false,
        };
        match button {
            0 | 4 if slot == -999 => {
                self.drag = Drag {
                    window,
                    button: Some(kind),
                    slots: Vec::new(),
                };
                true
            }
            1 | 5 if self.drag.window == window && self.drag.button == Some(kind) => {
                if slot == 0
                    || ((5..=8).contains(&slot) && window == 0)
                    || Self::slot_index(window, slot).is_none()
                {
                    return false;
                }
                if !self.drag.slots.contains(&slot) {
                    self.drag.slots.push(slot);
                }
                true
            }
            2 | 6
                if slot == -999 && self.drag.window == window && self.drag.button == Some(kind) =>
            {
                let slots = std::mem::take(&mut self.drag.slots);
                self.drag = Drag::default();
                let Some(mut held) = self.cursor else {
                    return true;
                };
                if slots.is_empty() {
                    return true;
                }
                let each = if kind == 0 {
                    held.count / slots.len().min(64) as u8
                } else {
                    1
                };
                if each == 0 {
                    return true;
                }
                for dest in slots {
                    let existing = self.lookup(window, dest);
                    if let Some(other) = existing {
                        if other.id != held.id || other.damage != held.damage {
                            continue;
                        }
                    }
                    let current = existing.map_or(0, |s| s.count);
                    let adding = each.min(held.max_count() - current).min(held.count);
                    if adding == 0 {
                        continue;
                    }
                    self.assign(
                        window,
                        dest,
                        Some(Stack {
                            count: current + adding,
                            ..held
                        }),
                    );
                    held.count -= adding;
                    if held.count == 0 {
                        break;
                    }
                }
                self.cursor = (held.count != 0).then_some(held);
                true
            }
            _ => {
                self.drag = Drag::default();
                false
            }
        }
    }
    fn collect_matching(&mut self, window: u8) -> bool {
        let Some(mut carried) = self.cursor else {
            return true;
        };
        let end = if window == 0 { 45 } else { 46 };
        for slot in 1..end {
            if window == 0 && (5..=8).contains(&slot) {
                continue;
            }
            let Some(mut other) = self.lookup(window, slot) else {
                continue;
            };
            if other.id != carried.id || other.damage != carried.damage {
                continue;
            }
            let count = (carried.max_count() - carried.count).min(other.count);
            if count == 0 {
                break;
            }
            carried.count += count;
            other.count -= count;
            self.assign(window, slot, (other.count != 0).then_some(other));
        }
        self.cursor = Some(carried);
        true
    }
    /// Execute only bounded, server-owned window transactions.
    ///
    /// Client-provided snapshots are never trusted; unsupported modes,
    /// unsupported armor slots and outside-drop return false.
    pub fn click_window(&mut self, window: u8, slot: i16, button: i8, mode: i8) -> bool {
        if mode == 5 {
            return self.drag_click(window, slot, button);
        }
        self.drag = Drag::default();
        if mode == 6 && button == 0 && slot >= 0 {
            return self.collect_matching(window);
        }
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
        if window == 0 && (5..=8).contains(&slot) {
            let candidate = if mode == 2 {
                let other = 36 + i16::from(button);
                self.lookup(0, other)
            } else if mode == 0 {
                self.cursor
            } else {
                None
            };
            if candidate.is_some_and(|stack| !Self::armor_compatible(slot as usize, stack)) {
                return false;
            }
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
                        let n = (target.max_count() - target.count).min(hand.count);
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
                            && target.count < target.max_count() =>
                    {
                        target.count += 1;
                        hand.count -= 1;
                        (Some(target), (hand.count != 0).then_some(hand))
                    }
                    (target, hand, 1) => (hand, target),
                    _ => unreachable!("validated button"),
                };
                if window == 0
                    && (5..=8).contains(&slot)
                    && new_slot.is_some_and(|item| !armor_accepts(slot, item))
                {
                    return false;
                }
                self.assign(window, slot, new_slot);
                self.cursor = new_cursor;
                true
            }
            1 if button == 0 || button == 1 => {
                // Shift ingredient slots to player; main <-> hotbar otherwise.
                let Some(mut moving) = self.lookup(window, slot) else {
                    return true;
                };
                if window == 0 && slot >= 9 {
                    for armor_slot in 5..=8 {
                        if armor_accepts(armor_slot, moving)
                            && self.lookup(window, armor_slot).is_none()
                        {
                            self.assign(window, armor_slot, Some(moving));
                            self.assign(window, slot, None);
                            return true;
                        }
                    }
                }
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
                            && existing.count < existing.max_count()
                        {
                            let amount = (existing.max_count() - existing.count).min(moving.count);
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
            2 if (0..=8).contains(&button) && !(window == 0 && (5..=8).contains(&slot)) => {
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

    fn chest_stack(&self, chest: &[Option<Stack>], slot: i16) -> Option<Stack> {
        let n = chest.len() as i16;
        if slot < n {
            chest[slot as usize]
        } else {
            self.slots[(slot - n + 9) as usize]
        }
    }
    fn set_chest_stack(&mut self, chest: &mut [Option<Stack>], slot: i16, stack: Option<Stack>) {
        let n = chest.len() as i16;
        if slot < n {
            chest[slot as usize] = stack;
        } else {
            self.slots[(slot - n + 9) as usize] = stack;
        }
    }
    /// Paint a server-owned cursor stack into a container window using the
    /// original three-phase 1.7.10 protocol; never use client item counts.
    fn chest_drag(
        &mut self,
        window: u8,
        chest: &mut [Option<Stack>],
        slot: i16,
        button: i8,
    ) -> Option<ChestDirection> {
        let size = chest.len() as i16;
        let kind = match button {
            0..=2 => 0,
            4..=6 => 4,
            _ => return None,
        };
        match button {
            0 | 4 if slot == -999 => {
                self.drag = Drag {
                    window,
                    button: Some(kind),
                    slots: Vec::new(),
                };
                Some(ChestDirection::InventoryOnly)
            }
            1 | 5 if self.drag.window == window && self.drag.button == Some(kind) => {
                if !(0..size + 36).contains(&slot) {
                    self.drag = Drag::default();
                    return None;
                }
                if !self.drag.slots.contains(&slot) {
                    self.drag.slots.push(slot);
                }
                Some(ChestDirection::InventoryOnly)
            }
            2 | 6
                if slot == -999 && self.drag.window == window && self.drag.button == Some(kind) =>
            {
                let slots = std::mem::take(&mut self.drag.slots);
                self.drag = Drag::default();
                let Some(mut cursor) = self.cursor else {
                    return Some(ChestDirection::InventoryOnly);
                };
                if slots.is_empty() {
                    return Some(ChestDirection::InventoryOnly);
                }
                let each = if kind == 0 {
                    cursor.count / slots.len().min(64) as u8
                } else {
                    1
                };
                if each == 0 {
                    return Some(ChestDirection::InventoryOnly);
                }
                let mut changed_storage = false;
                for position in slots {
                    let existing = self.chest_stack(chest, position);
                    if size == 4
                        && position < size
                        && !crate::brewing::slot_accepts(position, cursor)
                    {
                        continue;
                    }
                    if existing
                        .is_some_and(|other| other.id != cursor.id || other.damage != cursor.damage)
                    {
                        continue;
                    }
                    let count = existing.map_or(0, |other| other.count);
                    let transfer = each.min(cursor.max_count() - count).min(cursor.count);
                    if transfer == 0 {
                        continue;
                    }
                    self.set_chest_stack(
                        chest,
                        position,
                        Some(Stack {
                            count: count + transfer,
                            ..cursor
                        }),
                    );
                    changed_storage |= position < size;
                    cursor.count -= transfer;
                    if cursor.count == 0 {
                        break;
                    }
                }
                self.cursor = (cursor.count != 0).then_some(cursor);
                Some(if changed_storage {
                    ChestDirection::IntoChest
                } else {
                    ChestDirection::InventoryOnly
                })
            }
            _ => {
                self.drag = Drag::default();
                None
            }
        }
    }

    /// Double-click collection scans both the container and the player's
    /// 36 slots. The same authoritative transaction commits both inventories.
    fn chest_collect(
        &mut self,
        chest: &mut [Option<Stack>],
        slot: i16,
        button: i8,
    ) -> Option<ChestDirection> {
        let size = chest.len() as i16;
        if button != 0 || !(0..size + 36).contains(&slot) {
            return None;
        }
        let Some(mut carried) = self.cursor else {
            return Some(ChestDirection::InventoryOnly);
        };
        let mut changed_storage = false;
        for position in 0..size + 36 {
            let Some(mut existing) = self.chest_stack(chest, position) else {
                continue;
            };
            if existing.id != carried.id || existing.damage != carried.damage {
                continue;
            }
            let amount = (carried.max_count() - carried.count).min(existing.count);
            if amount == 0 {
                break;
            }
            carried.count += amount;
            existing.count -= amount;
            self.set_chest_stack(chest, position, (existing.count != 0).then_some(existing));
            changed_storage |= position < size;
        }
        self.cursor = Some(carried);
        Some(if changed_storage {
            ChestDirection::OutOfChest
        } else {
            ChestDirection::InventoryOnly
        })
    }

    /// Single/double chests, hopper and brewing containers with 36 player slots.
    /// All mutations are staged by the caller and journaled together with the
    /// player inventory if both files are changed.
    pub fn chest_click_window(
        &mut self,
        window: u8,
        chest: &mut [Option<Stack>],
        slot: i16,
        button: i8,
        mode: i8,
    ) -> Option<ChestDirection> {
        let n = chest.len() as i16;
        if !matches!(n, 4 | 5 | 27 | 54) {
            return None;
        }
        if mode == 5 {
            return self.chest_drag(window, chest, slot, button);
        }
        self.drag = Drag::default();
        if mode == 6 {
            return self.chest_collect(chest, slot, button);
        }
        if !(0..n + 36).contains(&slot) {
            return None;
        }
        let storage = slot < n;
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
                        let count = (target.max_count() - target.count).min(hand.count);
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
                            && target.count < target.max_count() =>
                    {
                        target.count += 1;
                        hand.count -= 1;
                        (Some(target), (hand.count != 0).then_some(hand))
                    }
                    (target, hand, 1) => (hand, target),
                    _ => unreachable!("button validated"),
                };
                if n == 4
                    && storage
                    && new_slot.is_some_and(|item| !crate::brewing::slot_accepts(slot, item))
                {
                    return None;
                }
                self.set_chest_stack(chest, slot, new_slot);
                self.cursor = new_cursor;
                Some(direction)
            }
            1 if button == 0 || button == 1 => {
                let Some(mut moving) = self.chest_stack(chest, slot) else {
                    return Some(direction);
                };
                let targets = if storage { n..n + 36 } else { 0..n };
                for target in targets.clone() {
                    if n == 4 && target < n && !crate::brewing::slot_accepts(target, moving) {
                        continue;
                    }
                    if let Some(mut existing) = self.chest_stack(chest, target) {
                        if existing.id == moving.id
                            && existing.damage == moving.damage
                            && existing.count < existing.max_count()
                        {
                            let count = (existing.max_count() - existing.count).min(moving.count);
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
                        if n == 4 && target < n && !crate::brewing::slot_accepts(target, moving) {
                            continue;
                        }
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
                let target = n + 27 + i16::from(button);
                let a = self.chest_stack(chest, slot);
                let b = self.chest_stack(chest, target);
                self.set_chest_stack(chest, slot, b);
                self.set_chest_stack(chest, target, a);
                Some(ChestDirection::InventoryOnly)
            }
            _ => None,
        }
    }
    /// Compatibility helper for pre-existing inventory unit tests.
    #[cfg(test)]
    pub fn chest_click(
        &mut self,
        chest: &mut [Option<Stack>],
        slot: i16,
        button: i8,
        mode: i8,
    ) -> Option<ChestDirection> {
        self.chest_click_window(1, chest, slot, button, mode)
    }

    pub fn chest_window_items(&self, window: u8, chest: &[Option<Stack>]) -> Vec<u8> {
        let mut out = vec![window];
        let n = chest.len() as i16;
        out.extend_from_slice(&(n + 36).to_be_bytes());
        for slot in 0..n + 36 {
            Self::encode_slot(&mut out, self.chest_stack(chest, slot));
        }
        out
    }

    fn furnace_stack(&self, furnace: &crate::furnaces::Furnace, slot: i16) -> Option<Stack> {
        if slot < 3 {
            furnace.slots[slot as usize]
        } else {
            self.slots[(slot + 6) as usize]
        }
    }
    /// 1.7.10 furnace window: input 0, fuel 1, output 2,
    /// main inventory 3..29 and hotbar 30..38.
    ///
    /// Returns the storage write direction for source-first persistence.
    /// Forge/vanilla item-slot claims sent by the client are never trusted.
    pub fn furnace_click(
        &mut self,
        furnace: &mut crate::furnaces::Furnace,
        slot: i16,
        button: i8,
        mode: i8,
    ) -> Option<ChestDirection> {
        if !(0..39).contains(&slot) {
            return None;
        }
        if slot >= 3 && mode == 0 {
            return self
                .click_window(0, slot + 6, button, mode)
                .then_some(ChestDirection::InventoryOnly);
        }
        if slot >= 3 && mode == 2 && (0..=8).contains(&button) {
            let a = (slot + 6) as usize;
            let b = 36 + button as usize;
            self.slots.swap(a, b);
            return Some(ChestDirection::InventoryOnly);
        }
        if slot >= 3 && mode == 1 && (button == 0 || button == 1) {
            let idx = (slot + 6) as usize;
            let Some(stack) = self.slots[idx] else {
                return Some(ChestDirection::InventoryOnly);
            };
            let target: usize = if crate::furnaces::recipe(stack).is_some() {
                0
            } else if crate::furnaces::burn_duration(stack).is_some() {
                1
            } else {
                return Some(ChestDirection::InventoryOnly);
            };
            let existing = furnace.slots[target];
            if existing.is_some_and(|v| v.id != stack.id || v.damage != stack.damage) {
                return Some(ChestDirection::InventoryOnly);
            }
            // Non-stackable fuels (notably lava buckets) must never merge
            // beyond their own capacity through a Shift-click transaction.
            let room = stack
                .max_count()
                .saturating_sub(existing.map_or(0, |v| v.count));
            let moving = room.min(stack.count);
            if moving == 0 {
                return Some(ChestDirection::InventoryOnly);
            }
            furnace.slots[target] = Some(Stack {
                count: existing.map_or(0, |v| v.count) + moving,
                ..stack
            });
            let remaining = stack.count - moving;
            self.slots[idx] = (remaining > 0).then_some(Stack {
                count: remaining,
                ..stack
            });
            return Some(ChestDirection::IntoChest);
        }
        if slot < 3 && mode == 1 && (button == 0 || button == 1) {
            let Some(item) = furnace.slots[slot as usize] else {
                return Some(ChestDirection::OutOfChest);
            };
            let mut copy = self.clone();
            // Only commit a whole stack. Partial transfers would otherwise
            // need complex output-slot and item conservation semantics.
            if copy.move_to_player(0, item).is_some() {
                return Some(ChestDirection::OutOfChest);
            }
            furnace.slots[slot as usize] = None;
            *self = copy;
            return Some(ChestDirection::OutOfChest);
        }
        if slot < 3 && mode == 0 && (button == 0 || button == 1) {
            let old = furnace.slots[slot as usize];
            let hand = self.cursor;
            if slot == 2 {
                // Smelting output can ONLY be extracted, never inserted.
                let Some(output) = old else {
                    // Empty output cannot accept a carried item.
                    return if hand.is_none() {
                        Some(ChestDirection::OutOfChest)
                    } else {
                        None
                    };
                };
                let next = match hand {
                    None => Some(output),
                    Some(mut current)
                        if current.id == output.id
                            && current.damage == output.damage
                            && u16::from(current.count) + u16::from(output.count)
                                <= u16::from(output.max_count()) =>
                    {
                        current.count += output.count;
                        Some(current)
                    }
                    _ => return None,
                };
                self.cursor = next;
                furnace.slots[2] = None;
                return Some(ChestDirection::OutOfChest);
            }
            // Allow input recipes and recognised fuel only. Withdraw existing
            // contents freely, including the empty bucket left by lava.
            if let Some(insert) = hand {
                let allowed = if slot == 0 {
                    crate::furnaces::recipe(insert).is_some()
                } else {
                    crate::furnaces::burn_duration(insert).is_some()
                };
                if !allowed {
                    return None;
                }
            }
            if old.is_some()
                && hand.is_some()
                && (old.unwrap().id != hand.unwrap().id
                    || old.unwrap().damage != hand.unwrap().damage)
            {
                return None; // unsupported unlike-stack swap across two files
            }
            let (next_slot, next_cursor) = match (old, hand, button) {
                (item, None, 0) => (None, item),
                (None, item, 0) => (item, None),
                (Some(mut item), Some(mut cursor), 0) => {
                    let amount = (item.max_count() - item.count).min(cursor.count);
                    item.count += amount;
                    cursor.count -= amount;
                    (Some(item), (cursor.count != 0).then_some(cursor))
                }
                (Some(mut item), None, 1) => {
                    let amount = item.count.div_ceil(2);
                    item.count -= amount;
                    (
                        (item.count != 0).then_some(item),
                        Some(Stack {
                            count: amount,
                            ..item
                        }),
                    )
                }
                (None, None, 1) => (None, None),
                (None, Some(mut cursor), 1) => {
                    cursor.count -= 1;
                    (
                        Some(Stack { count: 1, ..cursor }),
                        (cursor.count != 0).then_some(cursor),
                    )
                }
                (Some(mut item), Some(mut cursor), 1) => {
                    let amount = u8::from(item.count < item.max_count());
                    item.count += amount;
                    cursor.count -= amount;
                    (Some(item), (cursor.count != 0).then_some(cursor))
                }
                _ => unreachable!("validated furnace button"),
            };
            furnace.slots[slot as usize] = next_slot;
            self.cursor = next_cursor;
            return Some(if hand.is_some() {
                ChestDirection::IntoChest
            } else {
                ChestDirection::OutOfChest
            });
        }
        None
    }
    pub fn furnace_window_items(&self, window: u8, furnace: &crate::furnaces::Furnace) -> Vec<u8> {
        let mut packet = vec![window];
        packet.extend_from_slice(&39i16.to_be_bytes());
        for slot in 0..39 {
            Self::encode_slot(&mut packet, self.furnace_stack(furnace, slot));
        }
        packet
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
    #[test]
    fn outside_drop_cursor_only_debits_requested_quantity() {
        let original = Stack {
            id: 332,
            count: 3,
            damage: 0,
        };
        let mut inv = Inventory {
            cursor: Some(original),
            ..Inventory::default()
        };
        assert_eq!(inv.drop_cursor(9), None);
        assert_eq!(inv.cursor, Some(original));
        assert_eq!(
            inv.drop_cursor(1),
            Some(Stack {
                count: 1,
                ..original
            })
        );
        assert_eq!(
            inv.cursor,
            Some(Stack {
                count: 2,
                ..original
            })
        );
        assert_eq!(
            inv.drop_cursor(0),
            Some(Stack {
                count: 2,
                ..original
            })
        );
        assert_eq!(inv.cursor, None);
        assert_eq!(inv.drop_cursor(0), None);
        assert!(inv.encoded().is_ok());
    }

    #[test]
    fn new_sign_recipe_shift_crafting_is_atomic_when_inventory_is_full() {
        let sign = Stack {
            id: 323,
            count: 16,
            damage: 0,
        };
        let mut inv = Inventory::default();
        for slot in 9..45 {
            inv.slots[slot] = Some(sign);
        }
        // A stack with only one free slot cannot accept the recipe's three
        // signs, so the entire transaction must be rejected without debit.
        inv.slots[9] = Some(Stack { count: 15, ..sign });
        for index in [0, 1, 2, 3, 4, 5] {
            inv.workbench[index] = Some(Stack {
                id: 5,
                count: 2,
                damage: 0,
            });
        }
        inv.workbench[7] = Some(Stack {
            id: 280,
            count: 2,
            damage: 0,
        });
        assert_eq!(
            inv.crafting_result(1),
            Some(Stack {
                id: 323,
                count: 3,
                damage: 0
            })
        );
        let blocked = inv.clone();
        assert!(inv.click_window(1, 0, 0, 1));
        assert_eq!(inv, blocked);
        // Free another slot. Both craft iterations must debit each ingredient
        // twice and yield exactly six signs, not one partial/ghost batch.
        inv.slots[10] = None;
        assert!(inv.click_window(1, 0, 0, 1));
        assert!(inv.workbench.iter().all(Option::is_none));
        assert_eq!(inv.slots[9], Some(sign));
        assert_eq!(inv.slots[10], Some(Stack { count: 5, ..sign }));
        assert!(inv.crafting_result(1).is_none());
        assert!(inv.encoded().is_ok());
    }

    #[test]
    fn brewing_gui_rejects_invalid_bottle_reagent_and_shift_moves() {
        let mut inv = Inventory::default();
        let mut slots = [None; 4];
        let diamond = Stack {
            id: 264,
            count: 1,
            damage: 0,
        };
        let water = Stack {
            id: 373,
            count: 1,
            damage: 0,
        };
        inv.cursor = Some(diamond);
        assert_eq!(inv.chest_click_window(1, &mut slots, 0, 0, 0), None);
        assert_eq!(inv.cursor, Some(diamond));
        assert_eq!(slots, [None; 4]);

        inv.cursor = Some(water);
        assert_eq!(
            inv.chest_click_window(1, &mut slots, 0, 0, 0),
            Some(ChestDirection::IntoChest)
        );
        assert_eq!(slots[0], Some(water));

        inv.creative_set(
            9,
            Some(Stack {
                id: 4,
                count: 32,
                damage: 0,
            }),
        )
        .unwrap();
        assert!(inv.chest_click_window(1, &mut slots, 4, 0, 1).is_some());
        assert_eq!(inv.slots[9].unwrap().count, 32);
        assert!(slots[1..].iter().all(Option::is_none));

        inv.cursor = Some(Stack {
            id: 4,
            count: 3,
            damage: 0,
        });
        assert!(inv.chest_click_window(1, &mut slots, -999, 0, 5).is_some());
        assert!(inv.chest_click_window(1, &mut slots, 1, 1, 5).is_some());
        assert!(inv.chest_click_window(1, &mut slots, -999, 2, 5).is_some());
        assert!(slots[1..].iter().all(Option::is_none));
        assert_eq!(inv.cursor.unwrap().count, 3);
    }

    #[test]
    fn shift_click_never_stacks_lava_buckets_in_furnace() {
        let mut inv = Inventory::default();
        let bucket = Stack {
            id: 327,
            count: 1,
            damage: 0,
        };
        let mut furnace = crate::furnaces::Furnace::default();
        furnace.slots[1] = Some(bucket);
        inv.creative_set(36, Some(bucket)).unwrap();
        assert!(inv.furnace_click(&mut furnace, 30, 0, 1).is_some());
        assert_eq!(furnace.slots[1], Some(bucket));
        assert_eq!(inv.held(), Some(bucket));
    }

    #[test]
    fn legacy_item_caps_apply_in_player_chest_and_hopper_windows() {
        let potion = Stack {
            id: 373,
            count: 1,
            damage: 8194,
        };
        assert_eq!(potion.max_count(), 1);
        assert!(!(Stack { count: 2, ..potion }).is_valid());
        assert_eq!(
            Stack {
                id: 332,
                count: 16,
                damage: 0
            }
            .max_count(),
            16
        );
        assert!(!(Stack {
            id: 332,
            count: 17,
            damage: 0
        })
        .is_valid());

        let mut inv = Inventory::default();
        inv.creative_set(36, Some(potion)).unwrap();
        inv.creative_set(37, Some(potion)).unwrap();
        assert!(inv.click_window(0, 36, 0, 0));
        assert!(inv.click_window(0, 37, 0, 0));
        assert_eq!(inv.slots[37], Some(potion));
        assert_eq!(inv.cursor, Some(potion));
        assert!(inv.click_window(0, 38, 0, 0));
        assert_eq!(inv.slots[38], Some(potion));

        let mut chest = [None; 27];
        chest[0] = Some(potion);
        inv.cursor = Some(potion);
        assert!(inv.chest_click(&mut chest, 0, 0, 0).is_some());
        assert_eq!(chest[0], Some(potion));
        assert_eq!(inv.cursor, Some(potion));
    }

    #[test]
    fn shift_crafting_consumes_each_iteration_and_stops_at_capacity() {
        let mut inv = Inventory::default();
        // 2x2 player grid ingredients are only set through server-owned clicks.
        inv.creative_set(
            36,
            Some(Stack {
                id: 17,
                count: 3,
                damage: 0,
            }),
        )
        .unwrap();
        assert!(inv.click_window(0, 36, 0, 0));
        assert!(inv.click_window(0, 1, 0, 0));
        assert!(inv.click_window(0, 0, 0, 1));
        assert!(inv.crafting_result(0).is_none());
        assert_eq!(inv.slots[9].unwrap().count, 12);
        assert_eq!(inv.slots[1], None);
    }
    #[test]
    fn armor_equipping_reduces_damage_and_is_included_in_death_drops() {
        let mut inv = super::Inventory::default();
        let helm = super::Stack {
            id: 306,
            count: 1,
            damage: 0,
        };
        assert!(inv.receive_stack(helm).is_some());
        assert!(inv.click_window(0, 36, 0, 0));
        assert!(inv.click_window(0, 5, 0, 0));
        assert_eq!(inv.armor_points(), 2);
        assert!(inv.possessions().contains(&helm));
        let stone = super::Stack {
            id: 1,
            count: 1,
            damage: 0,
        };
        assert!(inv.receive_stack(stone).is_some());
        assert!(inv.click_window(0, 36, 0, 0));
        assert!(!inv.click_window(0, 6, 0, 0)); // helmet cannot be an arbitrary chest piece
        inv.clear_possessions();
        assert_eq!(inv.armor_points(), 0);
        assert!(!inv.possessions().contains(&helm));
    }

    #[test]
    fn stew_consumption_replaces_one_bowl_without_creating_extra_items() {
        let mut inventory = Inventory::default();
        let stew = Stack {
            id: 282,
            count: 1,
            damage: 0,
        };
        let bowl = Stack {
            id: 281,
            count: 1,
            damage: 0,
        };
        inventory.creative_set(36, Some(stew)).unwrap();
        assert!(inventory.consume_held());
        assert_eq!(inventory.held(), None);
        assert!(inventory.receive_stack(bowl).is_some());
        assert_eq!(inventory.held(), Some(bowl));
        assert!(inventory.encoded().is_ok());
        assert!(!inventory.consume_item(282));
    }

    #[test]
    fn armor_uses_legacy_durability_and_breaks_on_combat_damage() {
        let mut inv = Inventory::default();
        let helmet = Stack {
            id: 298,
            count: 1,
            damage: 54,
        };
        let chest = Stack {
            id: 307,
            count: 1,
            damage: 100,
        };
        let pumpkin = Stack {
            id: 86,
            count: 1,
            damage: 0,
        };
        inv.creative_set(5, Some(helmet)).unwrap();
        inv.creative_set(6, Some(chest)).unwrap();
        assert!(!inv.wear_armor_on_hit(0));
        assert!(inv.wear_armor_on_hit(7));
        assert_eq!(inv.slots[5], None); // leather helmet broke
        assert_eq!(inv.slots[6].unwrap().damage, 101); // 7 / 4 => 1
        assert!(inv.wear_armor_on_hit(12));
        assert_eq!(inv.slots[6].unwrap().damage, 104); // 12 / 4 => 3
        inv.creative_set(5, Some(pumpkin)).unwrap();
        assert!(inv.wear_armor_on_hit(4));
        assert_eq!(inv.slots[5], Some(pumpkin)); // decorative item is unaffected
        assert!(inv.encoded().is_ok());
    }

    #[test]
    fn death_drops_armor_and_crafting_slots_exactly_once() {
        let mut inventory = Inventory::default();
        let helmet = Stack {
            id: 306,
            count: 1,
            damage: 12,
        };
        let chestplate = Stack {
            id: 307,
            count: 1,
            damage: 7,
        };
        let boots = Stack {
            id: 309,
            count: 1,
            damage: 3,
        };
        let logs = Stack {
            id: 17,
            count: 4,
            damage: 2,
        };
        let diamonds = Stack {
            id: 264,
            count: 5,
            damage: 0,
        };
        let sticks = Stack {
            id: 280,
            count: 2,
            damage: 0,
        };
        inventory.creative_set(5, Some(helmet)).unwrap();
        inventory.creative_set(6, Some(chestplate)).unwrap();
        inventory.creative_set(8, Some(boots)).unwrap();
        inventory.slots[1] = Some(logs);
        inventory.slots[9] = Some(diamonds);
        inventory.cursor = Some(sticks);
        inventory.workbench[0] = Some(sticks);

        let before = inventory.possessions();
        assert_eq!(before.len(), 7);
        assert_eq!(before.iter().filter(|&&s| s == helmet).count(), 1);
        assert_eq!(before.iter().filter(|&&s| s == chestplate).count(), 1);
        assert_eq!(before.iter().filter(|&&s| s == boots).count(), 1);
        let total: u32 = before.iter().map(|s| u32::from(s.count)).sum();
        assert_eq!(total, 16);

        inventory.clear_possessions();
        assert!(inventory.possessions().is_empty());
        assert_eq!(inventory.armor_points(), 0);
        assert!(inventory.encoded().is_ok());
    }

    #[test]
    fn death_collects_hotbar_backpack_cursor_and_crafting_without_duplicate() {
        let mut inventory = super::Inventory::default();
        let stone = super::Stack {
            id: 1,
            count: 9,
            damage: 0,
        };
        let apple = super::Stack {
            id: 260,
            count: 2,
            damage: 0,
        };
        inventory.creative_set(36, Some(stone)).unwrap();
        inventory.creative_set(9, Some(apple)).unwrap();
        inventory.cursor = Some(apple);
        inventory.workbench[0] = Some(stone);
        let possessions = inventory.possessions();
        assert_eq!(possessions.len(), 4);
        inventory.clear_possessions();
        assert!(inventory.possessions().is_empty());
        assert!(inventory.held().is_none());
        assert_eq!(inventory.selected(), 0);
    }

    use super::*;

    #[test]
    fn dropping_and_receiving_stacks_preserves_counts_and_rejects_partial_pickup() {
        let mut inv = Inventory::default();
        let stack = Stack {
            id: 3,
            count: 12,
            damage: 0,
        };
        inv.creative_set(36, Some(stack)).unwrap();
        assert_eq!(inv.drop_held(false), Some(Stack { count: 1, ..stack }));
        assert_eq!(inv.held().unwrap().count, 11);
        assert_eq!(inv.drop_held(true), Some(Stack { count: 11, ..stack }));
        assert_eq!(inv.held(), None);
        assert_eq!(inv.drop_held(false), None);
        inv.creative_set(36, Some(Stack { count: 60, ..stack }))
            .unwrap();
        assert_eq!(inv.receive_stack(stack), Some(vec![36, 37]));
        assert_eq!(inv.held().unwrap().count, 64);
        assert_eq!(inv.slots[37].unwrap().count, 8);
        for slot in 9..45 {
            inv.slots[slot] = Some(Stack {
                id: 1,
                count: 64,
                damage: 0,
            });
        }
        let before = inv.clone();
        assert_eq!(inv.receive_stack(stack), None);
        assert_eq!(inv, before);
    }

    #[test]
    fn furnace_inventory_moves_and_output_are_server_owned() {
        let mut inv = Inventory::default();
        let mut furnace = crate::furnaces::Furnace::default();
        inv.creative_set(
            36,
            Some(Stack {
                id: 15,
                count: 3,
                damage: 0,
            }),
        )
        .unwrap();
        // Hotbar 36 maps to furnace window 30; Shift inserts input into slot 0.
        assert_eq!(
            inv.furnace_click(&mut furnace, 30, 0, 1),
            Some(ChestDirection::IntoChest)
        );
        assert_eq!(furnace.slots[0].unwrap().count, 3);
        assert!(inv.held().is_none());
        inv.creative_set(
            36,
            Some(Stack {
                id: 263,
                count: 2,
                damage: 0,
            }),
        )
        .unwrap();
        assert!(inv.furnace_click(&mut furnace, 30, 0, 1).is_some());
        assert_eq!(furnace.slots[1].unwrap().count, 2);
        // Never allow the client to move an arbitrary item into output.
        inv.creative_set(
            37,
            Some(Stack {
                id: 57,
                count: 10,
                damage: 0,
            }),
        )
        .unwrap();
        inv.furnace_click(&mut furnace, 31, 0, 0);
        let before = (inv.clone(), furnace);
        assert_eq!(inv.furnace_click(&mut furnace, 2, 0, 0), None);
        assert_eq!((inv.clone(), furnace), before);
        inv.furnace_click(&mut furnace, 31, 0, 0);
        furnace.slots[2] = Some(Stack {
            id: 265,
            count: 1,
            damage: 0,
        });
        assert_eq!(
            inv.furnace_click(&mut furnace, 2, 0, 0),
            Some(ChestDirection::OutOfChest)
        );
        assert_eq!(inv.cursor.unwrap().id, 265);
        assert_eq!(furnace.slots[2], None);
        assert!(!inv.furnace_click(&mut furnace, 39, 0, 0).is_some());
        assert_eq!(&inv.furnace_window_items(7, &furnace)[..3], &[7, 0, 39]);
    }
    #[test]
    fn tool_wear_increases_damage_and_breaks_at_material_limit() {
        let mut inv = Inventory::default();
        inv.creative_set(
            36,
            Some(Stack {
                id: 270,
                count: 1,
                damage: 57,
            }),
        )
        .unwrap();
        assert!(inv.wear_held_tool());
        assert_eq!(inv.held().unwrap().damage, 58);
        assert!(inv.wear_held_tool());
        assert_eq!(inv.held(), None);
        assert!(!inv.wear_held_tool());
        inv.creative_set(
            36,
            Some(Stack {
                id: 4,
                count: 12,
                damage: 0,
            }),
        )
        .unwrap();
        assert!(!inv.wear_held_tool());
        assert_eq!(inv.held().unwrap().count, 12);
        inv.creative_set(
            36,
            Some(Stack {
                id: 274,
                count: 1,
                damage: 130,
            }),
        )
        .unwrap();
        assert!(inv.wear_held_tool());
        assert_eq!(inv.held(), None);
    }
    #[test]
    fn shared_container_drag_collect_conserves_items_and_resets_invalid_drag() {
        let mut player = Inventory::default();
        let mut chest = [None; 54];
        chest[0] = Some(Stack {
            id: 4,
            count: 10,
            damage: 0,
        });
        player.cursor = Some(Stack {
            id: 4,
            count: 15,
            damage: 0,
        });
        assert!(player
            .chest_click_window(3, &mut chest, -999, 0, 5)
            .is_some());
        assert!(player.chest_click_window(3, &mut chest, 0, 1, 5).is_some());
        assert!(player.chest_click_window(3, &mut chest, 1, 1, 5).is_some());
        assert_eq!(
            player.chest_click_window(3, &mut chest, -999, 2, 5),
            Some(ChestDirection::IntoChest)
        );
        assert_eq!(chest[0].unwrap().count, 17);
        assert_eq!(chest[1].unwrap().count, 7);
        assert_eq!(player.cursor.unwrap().count, 1);

        assert_eq!(
            player.chest_click_window(3, &mut chest, 0, 0, 6),
            Some(ChestDirection::OutOfChest)
        );
        assert!(chest[0].is_none());
        assert!(chest[1].is_none());
        assert_eq!(player.cursor.unwrap().count, 25);
        assert_eq!(
            player.chest_click_window(3, &mut chest, -999, 4, 5),
            Some(ChestDirection::InventoryOnly)
        );
        // Cannot finish a drag on a different container window.
        assert_eq!(player.chest_click_window(4, &mut chest, -999, 6, 5), None);
        assert_eq!(player.cursor.unwrap().count, 25);

        // Malformed or out-of-range dragged slots cannot create items.
        assert!(player
            .chest_click_window(4, &mut chest, -999, 0, 5)
            .is_some());
        assert_eq!(player.chest_click_window(4, &mut chest, 90, 1, 5), None);
        assert_eq!(player.chest_click_window(4, &mut chest, -999, 2, 5), None);
        assert_eq!(player.cursor.unwrap().count, 25);
    }

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
        assert!(inv.creative_set(5, None).unwrap()); // armor slot is now valid
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
        assert_eq!(inv.receive_stack(item), Some(vec![36]));
        assert_eq!(inv.receive_stack(item), Some(vec![36]));
        assert_eq!(inv.held().unwrap().count, 2);
        assert!(inv.consume_held());
        assert_eq!(inv.held().unwrap().count, 1);
        assert!(inv.consume_held());
        assert!(inv.held().is_none());
        assert!(!inv.consume_held());
        assert!(inv
            .receive_stack(Stack {
                id: 7,
                count: 65,
                damage: 0
            })
            .is_none());
    }
    #[test]
    fn milking_cow_is_atomic_with_stacked_buckets_and_full_inventory() {
        let mut inventory = Inventory::default();
        inventory
            .creative_set(
                36,
                Some(Stack {
                    id: 325,
                    count: 1,
                    damage: 0,
                }),
            )
            .unwrap();
        assert!(inventory.fill_milk_bucket());
        assert_eq!(inventory.held().unwrap().id, 335);
        assert!(!inventory.fill_milk_bucket());

        for slot in 9..45 {
            inventory
                .creative_set(
                    slot,
                    Some(Stack {
                        id: 1,
                        count: 64,
                        damage: 0,
                    }),
                )
                .unwrap();
        }
        inventory
            .creative_set(
                36,
                Some(Stack {
                    id: 325,
                    count: 2,
                    damage: 0,
                }),
            )
            .unwrap();
        let full = inventory.clone();
        assert!(!inventory.fill_milk_bucket());
        assert_eq!(inventory, full);
        inventory.creative_set(9, None).unwrap();
        assert!(inventory.fill_milk_bucket());
        assert_eq!(inventory.held().unwrap().count, 1);
        assert_eq!(inventory.slots[9].unwrap().id, 335);
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
