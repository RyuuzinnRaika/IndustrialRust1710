//! Server-owned Minecraft 1.7.10 hopper inventory and bounded 8-tick transfers.
//! Hopper inventories, chests and furnaces are kept coherent by a redo journal.
use crate::chests::Chests;
use crate::furnaces::Furnaces;
use crate::inventory::Stack;
use crate::journal;
use crate::world::{self, World};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub type Position = (i32, i32, i32);
pub const HOPPER_SLOTS: usize = 5;
const MAX_HOPPERS: usize = 512;
const MAX_BYTES: u64 = 128 * 1024;
const VERSION: u8 = 1;
fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
fn key((x, y, z): Position) -> String { format!("{x},{y},{z}") }
fn parse_key(k: &str) -> io::Result<Position> {
    let parts = k.split(',').map(str::parse::<i32>).collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(Result::is_err) {
        return Err(invalid("bad hopper coordinates"));
    }
    let pos = (parts[0].as_ref().copied().unwrap(),
               parts[1].as_ref().copied().unwrap(),
               parts[2].as_ref().copied().unwrap());
    if key(pos) != k || !world::in_bounds(pos.0,pos.1,pos.2) {
        return Err(invalid("noncanonical hopper position"));
    }
    Ok(pos)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hoppers {
    version: u8,
    entries: BTreeMap<String, [Option<Stack>; HOPPER_SLOTS]>,
    #[serde(skip)]
    revision: u64,
}
impl Default for Hoppers {
    fn default() -> Self {
        Self { version: VERSION, entries: BTreeMap::new(), revision: 0 }
    }
}
fn move_one(src: &mut [Option<Stack>], dst: &mut [Option<Stack>]) -> bool {
    for value in src.iter_mut() {
        let Some(mut stack) = *value else { continue };
        let target_index = dst.iter().position(|target| target.is_some_and(|s|
            s.id == stack.id && s.damage == stack.damage && s.count < 64))
            .or_else(|| dst.iter().position(Option::is_none));
        let Some(index) = target_index else { continue };
        let target = &mut dst[index];
        if let Some(dest) = target { dest.count += 1; }
        else { *target = Some(Stack { count:1, ..stack }); }
        stack.count -= 1;
        *value = (stack.count != 0).then_some(stack);
        return true;
    }
    false
}
impl Hoppers {
    pub fn location(world: &Path) -> PathBuf { world.with_extension("hoppers.json") }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.entries.len() > MAX_HOPPERS {
            return Err(invalid("invalid hopper count or version"));
        }
        for (key, slots) in &self.entries {
            parse_key(key)?;
            if slots.iter().flatten().any(|stack| !stack.is_valid()) {
                return Err(invalid("invalid hopper item"));
            }
        }
        Ok(())
    }
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("hopper encoding failed"))?;
        if bytes.len() as u64 > MAX_BYTES { return Err(invalid("hopper save too large")); }
        Ok(bytes)
    }
    fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = self.encoded()?;
        if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
        let temp = path.with_extension("hoppers.tmp");
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        if !meta.file_type().is_file() || meta.len() > MAX_BYTES {
            return Err(invalid("invalid hopper save metadata"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_BYTES { return Err(invalid("hopper save grew too large")); }
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("malformed hopper data"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn revision(&self) -> u64 { self.revision }
    pub fn slots(&self, pos: Position) -> [Option<Stack>;HOPPER_SLOTS] {
        self.entries.get(&key(pos)).copied().unwrap_or([None;HOPPER_SLOTS])
    }
    pub fn has_contents(&self,pos:Position)->bool {
        self.slots(pos).iter().any(Option::is_some)
    }
    pub fn staged_update(&self,pos:Position,slots:[Option<Stack>;HOPPER_SLOTS])->io::Result<Self>{
        if !world::in_bounds(pos.0,pos.1,pos.2)
            || slots.iter().flatten().any(|s| !s.is_valid()) {
            return Err(invalid("invalid hopper update"));
        }
        let mut candidate=self.clone();
        candidate.entries.insert(key(pos),slots);
        candidate.revision=self.revision.wrapping_add(1);
        candidate.encoded()?;
        Ok(candidate)
    }
    pub fn commit_staged(&mut self,path:&Path,candidate:Self)->io::Result<()> {
        candidate.save(path)?;
        *self=candidate;
        Ok(())
    }
    pub fn register(&mut self,path:&Path,pos:Position)->io::Result<()> {
        if self.entries.contains_key(&key(pos)) {return Ok(());}
        let next=self.staged_update(pos,[None;HOPPER_SLOTS])?;
        self.commit_staged(path,next)
    }
    pub fn advance(&mut self, world:&World, chests:&mut Chests, chest_path:&Path,
                   furnaces:&mut Furnaces, furnace_path:&Path, hopper_path:&Path,
                   journal_path:&Path)->io::Result<usize> {
        let mut transferred=0;
        // Each hopper may attempt one push and one pull every eight ticks.
        // Bound the work to 64 configured hoppers per cycle.
        let positions: Vec<Position> = self.entries.keys().take(64)
            .map(|name| parse_key(name)).collect::<io::Result<_>>()?;
        for (x,y,z) in positions {
            if world.get(x,y,z)>>4 != 154 { continue; }
            let facing=world.get(x,y,z)&7;
            let target=match facing {
                2=>(x,y,z-1),3=>(x,y,z+1),4=>(x-1,y,z),5=>(x+1,y,z),
                _=>(x,y-1,z)
            };
            let below=if world::in_bounds(target.0,target.1,target.2) {
                world.get(target.0,target.1,target.2)>>4
            } else {0};
            if below==54 {
                let mut source=self.slots((x,y,z));
                let mut sink=chests.slots(target);
                if move_one(&mut source,&mut sink) {
                    let new_self=self.staged_update((x,y,z),source)?;
                    let new_chest=chests.staged_update(target,sink)?;
                    journal::commit(journal_path, &[(hopper_path,new_self.encoded()?),
                                                     (chest_path,new_chest.encoded()?)])?;
                    *self=new_self; *chests=new_chest;
                    transferred+=1;
                }
            } else if below==61 || below==62 {
                let mut source=self.slots((x,y,z));
                let mut furnace=furnaces.get(target);
                // Input hopper enters the furnace's smelting input, not its output.
                if move_one(&mut source,&mut furnace.slots[..1]) {
                    let new_self=self.staged_update((x,y,z),source)?;
                    let new_furnace=furnaces.staged_update(target,furnace)?;
                    journal::commit(journal_path, &[(hopper_path,new_self.encoded()?),
                                                     (furnace_path,new_furnace.encoded()?)])?;
                    *self=new_self; *furnaces=new_furnace;
                    transferred+=1;
                }
            }
            let above=(x,y+1,z);
            if !world::in_bounds(above.0,above.1,above.2) {continue;}
            let source_block=world.get(above.0,above.1,above.2)>>4;
            if source_block==54 {
                let mut source=chests.slots(above);
                let mut dest=self.slots((x,y,z));
                if move_one(&mut source,&mut dest) {
                    let new_chest=chests.staged_update(above,source)?;
                    let new_self=self.staged_update((x,y,z),dest)?;
                    journal::commit(journal_path, &[(chest_path,new_chest.encoded()?),
                                                     (hopper_path,new_self.encoded()?)])?;
                    *chests=new_chest; *self=new_self;
                    transferred+=1;
                }
            } else if source_block==61 || source_block==62 {
                let mut furnace=furnaces.get(above);
                let mut dest=self.slots((x,y,z));
                if move_one(&mut furnace.slots[2..3],&mut dest) {
                    let new_furnace=furnaces.staged_update(above,furnace)?;
                    let new_self=self.staged_update((x,y,z),dest)?;
                    journal::commit(journal_path, &[(furnace_path,new_furnace.encoded()?),
                                                     (hopper_path,new_self.encoded()?)])?;
                    *furnaces=new_furnace; *self=new_self;
                    transferred+=1;
                }
            }
        }
        Ok(transferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_one_respects_stack_boundaries_and_item_conservation() {
        let mut src=[Some(Stack{id:4,count:3,damage:0});5];
        let mut target=[Some(Stack{id:4,count:63,damage:0});27];
        assert!(move_one(&mut src,&mut target));
        assert_eq!(src[0].unwrap().count,2);
        assert_eq!(target[0].unwrap().count,64);
        assert_eq!(src.iter().flatten().map(|s|u32::from(s.count)).sum::<u32>()
            + target.iter().flatten().map(|s|u32::from(s.count)).sum::<u32>(), 15+27*63);
    }
}
