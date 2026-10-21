//! Minecraft Java 1.7.10 fuel-free brewing and bounded potion effects.
//! Items are server-owned; ingredient consumption and bottle conversion commit
//! together to one world-owned JSON save. No modern blaze-powder fuel slot.
use crate::inventory::Stack;
use crate::world;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub type Position=(i32,i32,i32);
const VERSION:u8=1;
const MAX_STANDS:usize=512;
const MAX_BYTES:u64=256*1024;
const BREW_TICKS:u16=400;
fn invalid(msg:&'static str)->io::Error { io::Error::new(ErrorKind::InvalidData,msg) }
fn key((x,y,z):Position)->String { format!("{x},{y},{z}") }
fn valid_potion(item:Option<Stack>)->bool {
    item.is_none_or(|s| s.id==373 && s.count==1 && s.damage>=0)
}
fn is_reagent(item:Stack)->bool {
    matches!(item.id, 372|353|377|370|382|375|378|396|376|331|348|289)
}
/// Static 1.7.10 potion damage metadata: 0 is water, 16 awkward, 819x effects.
pub fn result(bottle:Stack, reagent:Stack)->Option<Stack> {
    if bottle.id!=373 {return None;}
    let meta=bottle.damage as u16;
    let damage=match (meta,reagent.id) {
        (0,372)=>16, // nether wart -> awkward
        (0,376)=>8200, // weakness is brewed directly from water
        (16,353)=>8194, // swiftness
        (16,377)=>8201, // strength
        (16,370)=>8193, // regeneration
        (16,382)=>8197, // healing
        (16,375)=>8196, // poison
        (16,378)=>8195, // fire resistance
        (16,396)=>8198, // night vision
        (8198,376)=>8206, // invisibility
        (8194,376)=>8202, // slowness
        (8197,376)=>8204, // harming
        (8196,376)=>8204, // harming
        (8193|8194|8195|8196|8198|8200|8201,331)=>meta+64, // extend
        (8193|8194|8196|8197|8201|8204,348)=>meta+32, // upgrade
        (8193..=8300,289)=>meta | 0x4000, // splash variant
        _=>return None
    };
    if damage==meta {return None;}
    Some(Stack { id:373,count:1,damage:damage as i16 })
}
/// Effect ID, amplifier, duration ticks, based on legacy potion damage metadata.
/// Duration modifiers are 1.7.10 style; 16384 splash flag does not extend time.
pub fn effect(potion:Stack)->Option<(u8,u8,u16)> {
    if potion.id!=373 {return None;}
    let data=potion.damage as u16;
    let base=data & 0x3f;
    let extended=data & 0x40 !=0;
    let upgraded=data & 0x20 !=0;
    let (effect,base_duration)=match base {
        1=>(10,900),2=>(1,3600),3=>(12,3600),4=>(19,900),
        5=>(6,1),6=>(16,3600),14=>(14,3600),8=>(18,1800),
        9=>(5,3600),10=>(2,1800),12=>(7,1),_=>return None
    };
    let duration=if extended {base_duration*2} else if upgraded && base_duration>1 {
        base_duration/2
    } else {base_duration};
    Some((effect,u8::from(upgraded),duration))
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Brewer {
    pub slots: [Option<Stack>;4],
    pub brew_time:u16,
}
impl Brewer {
    pub fn valid(&self)->bool {
        self.slots[..3].iter().all(|&item|valid_potion(item))
            && self.slots[3].is_none_or(|s|s.is_valid()&&is_reagent(s))
            && self.brew_time<=BREW_TICKS
    }
    fn product_possible(&self)->bool {
        self.slots[3].is_some_and(|ingredient|self.slots[..3]
            .iter().flatten().any(|&s|result(s,ingredient).is_some()))
    }
    fn tick(&mut self)->bool {
        if !self.product_possible() {
            let changed=self.brew_time!=0;
            self.brew_time=0;
            return changed;
        }
        if self.brew_time==0 { self.brew_time=BREW_TICKS; return true; }
        self.brew_time-=1;
        if self.brew_time==0 {
            let ingredient=self.slots[3].expect("available ingredient");
            for slot in &mut self.slots[..3] {
                if let Some(value)=*slot { if let Some(next)=result(value,ingredient) {
                    *slot=Some(next);
                }}
            }
            let mut remaining=ingredient;
            remaining.count-=1;
            self.slots[3]=(remaining.count!=0).then_some(remaining);
        }
        true
    }
    pub fn property(&self,window:u8)->Vec<u8> {
        let mut packet=vec![window];
        packet.extend_from_slice(&0i16.to_be_bytes());
        packet.extend_from_slice(&(self.brew_time as i16).to_be_bytes());
        packet
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brewers {
    version:u8,
    entries:BTreeMap<String,Brewer>,
    #[serde(skip)]
    revision:u64,
}
impl Default for Brewers {
    fn default()->Self { Self {version:VERSION,entries:BTreeMap::new(),revision:0} }
}
impl Brewers {
    pub fn location(world:&Path)->PathBuf {world.with_extension("brewers.json")}
    fn validate(&self)->io::Result<()> {
        if self.version!=VERSION || self.entries.len()>MAX_STANDS {return Err(invalid("brewer count/version"));}
        for (name,stand) in &self.entries {
            let parts=name.split(',').map(str::parse::<i32>).collect::<Vec<_>>();
            if parts.len()!=3 || parts.iter().any(Result::is_err) || !stand.valid() {
                return Err(invalid("bad brewing stand entry"));
            }
            let pos=(parts[0].as_ref().copied().unwrap(),parts[1].as_ref().copied().unwrap(),
                parts[2].as_ref().copied().unwrap());
            if !world::in_bounds(pos.0,pos.1,pos.2)||key(pos)!=*name {
                return Err(invalid("bad brewing stand position"));
            }
        }
        Ok(())
    }
    pub fn encoded(&self)->io::Result<Vec<u8>> {
        self.validate()?;
        let bytes=serde_json::to_vec(self).map_err(|_|invalid("encode brewing stand"))?;
        if bytes.len() as u64>MAX_BYTES {return Err(invalid("brewer file too large"));}
        Ok(bytes)
    }
    fn save(&self,path:&Path)->io::Result<()> {
        let bytes=self.encoded()?;
        if let Some(parent)=path.parent(){fs::create_dir_all(parent)?;}
        let temp=path.with_extension("brewers.tmp");
        let mut file=File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp,path)
    }
    pub fn load(path:&Path)->io::Result<Self> {
        let meta=match fs::symlink_metadata(path) {
            Ok(meta)=>meta,
            Err(err) if err.kind()==ErrorKind::NotFound=>return Ok(Self::default()),
            Err(err)=>return Err(err),
        };
        if !meta.file_type().is_file()||meta.len()>MAX_BYTES{return Err(invalid("brewer file metadata"));}
        let bytes=fs::read(path)?;
        let value:Self=serde_json::from_slice(&bytes).map_err(|_|invalid("brewer JSON"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn revision(&self)->u64 {self.revision}
    pub fn get(&self,pos:Position)->Brewer {self.entries.get(&key(pos)).copied().unwrap_or_default()}
    pub fn is_nonempty(&self,pos:Position)->bool {
        self.get(pos).slots.iter().any(Option::is_some)
    }
    pub fn staged_update(&self,pos:Position,stand:Brewer)->io::Result<Self> {
        if !world::in_bounds(pos.0,pos.1,pos.2)||!stand.valid() {
            return Err(invalid("invalid brewing stand update"));
        }
        let mut candidate=self.clone();
        if stand==Brewer::default(){candidate.entries.remove(&key(pos));}
        else {candidate.entries.insert(key(pos),stand);}
        candidate.encoded()?;
        candidate.revision=self.revision.wrapping_add(1);
        Ok(candidate)
    }
    pub fn commit_staged(&mut self,path:&Path,next:Self)->io::Result<()> {
        next.save(path)?;
        *self=next;
        Ok(())
    }
    pub fn advance(&mut self,path:&Path,steps:u32)->io::Result<()> {
        if steps==0 || self.entries.is_empty(){return Ok(());}
        let mut next=self.clone();
        let mut changed=false;
        for stand in next.entries.values_mut() {
            for _ in 0..steps.min(20) {
                if stand.tick(){changed=true;}
            }
        }
        if changed {
            next.revision=self.revision.wrapping_add(1);
            next.save(path)?;
            *self=next;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn 1710_brewing_never_uses_fuel_and_consumes_one_ingredient() {
        let water=Stack{id:373,count:1,damage:0};
        let wart=Stack{id:372,count:2,damage:0};
        assert_eq!(result(water,wart).unwrap().damage,16);
        let mut stand=Brewer{slots:[Some(water),None,None,Some(wart)],brew_time:0};
        for _ in 0..=400 {stand.tick();}
        assert_eq!(stand.slots[0].unwrap().damage,16);
        assert_eq!(stand.slots[3].unwrap().count,1);
        assert_eq!(stand.brew_time,0);
    }
    #[test]
    fn reject_unbrewable_bottle_or_non_reagent() {
        let mut stand=Brewer::default();
        stand.slots[0]=Some(Stack{id:264,count:1,damage:0});
        assert!(!stand.valid());
        stand.slots[0]=None;
        stand.slots[3]=Some(Stack{id:264,count:1,damage:0});
        assert!(!stand.valid());
    }
}
