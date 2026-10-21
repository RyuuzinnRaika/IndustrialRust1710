//! Bounded Protocol 5 natural-survival entities. No external entity registry.
use crate::inventory::Stack;
use crate::multiplayer::{self,Event};
use crate::navigation;
use crate::physics;
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const BASE_ID:i32=500_000;
const MAX_STEPS:u64=4;
const ATTACK_INTERVAL:u64=20;
const HOSTILE_CAP:usize=32;
const PASSIVE_CAP:usize=24;
const ARROW_CAP:usize=128;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Kind { Zombie,Cow,Pig,Sheep,Chicken,Skeleton,Creeper,Spider }
impl Kind {
    fn protocol(self)->u8 { match self {
        Self::Zombie=>54,Self::Cow=>92,Self::Pig=>90,Self::Sheep=>91,
        Self::Chicken=>93,Self::Skeleton=>51,Self::Creeper=>50,Self::Spider=>52
    }}
    fn passive(self)->bool { matches!(self,Self::Cow|Self::Pig|Self::Sheep|Self::Chicken) }
    fn health(self)->i16 { match self {Self::Chicken=>4,Self::Sheep|Self::Pig|Self::Cow=>10,Self::Spider=>16,Self::Creeper=>20,Self::Skeleton=>20,Self::Zombie=>20} }
    fn drop(self)->Stack {
        let id=match self { Self::Zombie=>367,Self::Cow=>363,Self::Pig=>319,Self::Sheep=>35,
          Self::Chicken=>365,Self::Skeleton=>352,Self::Creeper=>289,Self::Spider=>287 };
        Stack{id,count:1,damage:0}
    }
    fn breeding_food(self,held:i16)->bool {
        match self {Self::Cow|Self::Sheep=>held==296,Self::Pig=>held==391,
           Self::Chicken=>matches!(held,295|361|362),_=>false}
    }
}
#[derive(Clone,Copy,Debug)]
struct Mob {
    id:i32, kind:Kind, position:PlayerPosition, health:i16, hit_tick:u64,
    bite_tick:u64, fuse:u8, breed_tick:u64, baby:bool
}
impl Mob {
    fn spawn(self)->Event {
        let mut body=Vec::new();
        write_varint(self.id as u32,&mut body);
        body.push(self.kind.protocol());
        for p in [self.position.x,self.position.y,self.position.z] {
            body.extend_from_slice(&((p*32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0,0,0]);
        for _ in 0..3 {body.extend_from_slice(&0i16.to_be_bytes());}
        body.extend_from_slice(&[0,0]); // flags byte
        if self.baby {
            body.push((12<<3)|2); // metadata index 12, type 2 (int)
            body.extend_from_slice(&(-24000i32).to_be_bytes());
        }
        body.push(0x7f);
        Event{id:0x0f,body}
    }
    fn status(self,value:u8)->Event {
        let mut body=self.id.to_be_bytes().to_vec();
        body.push(value);
        Event{id:0x1a,body}
    }
    fn nearby(self,position:PlayerPosition,radius:f64)->bool {
        let dx=self.position.x-position.x; let dz=self.position.z-position.z;
        let dy=self.position.y-position.y;
        dx*dx+dy*dy+dz*dz<=radius*radius
    }
}
#[derive(Clone,Copy,Debug)]
struct Arrow {
    id:i32, position:PlayerPosition, velocity:[f64;3],
    shooter:Option<i32>, age:u16, damage:u8
}
impl Arrow {
    fn spawn(self)->Event {
        let mut body=Vec::new();
        write_varint(self.id as u32,&mut body);
        body.push(60); // Protocol 5 arrow object
        for p in [self.position.x,self.position.y,self.position.z] {
            body.extend_from_slice(&((p*32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0,0]);
        body.extend_from_slice(&self.shooter.unwrap_or(0).to_be_bytes());
        for speed in self.velocity {
            body.extend_from_slice(&((speed*8000.0).clamp(-32768.0,32767.0) as i16).to_be_bytes());
        }
        Event{id:0x0e,body}
    }
}
#[derive(Debug)]
pub struct Attack {
    pub events:Vec<Event>,
    pub drop:Option<(Stack,(i32,i32,i32))>,
    pub experience:u32,
}
#[derive(Debug)]
pub struct Explosion {
    pub position:PlayerPosition,
    pub radius:f64,
}
impl Explosion {
    pub fn packet(&self)->Event {
        let mut body=Vec::new();
        for v in [self.position.x,self.position.y,self.position.z] {body.extend_from_slice(&(v as f32).to_be_bytes());}
        body.extend_from_slice(&(self.radius as f32).to_be_bytes());
        body.extend_from_slice(&0i32.to_be_bytes()); // block records arrive as S23 updates
        for _ in 0..3 {body.extend_from_slice(&0.0f32.to_be_bytes());}
        Event{id:0x27,body}
    }
}
#[derive(Default,Debug)]
pub struct Mobs {
    entities:BTreeMap<i32,Mob>,
    arrows:BTreeMap<i32,Arrow>,
    pending_hits:Vec<(PlayerPosition,u8,u64)>,
    pending_drops:Vec<(Stack,(i32,i32,i32))>,
    explosions:Vec<Explosion>,
    last_tick:u64,
    next_id:i32,
    enabled:bool,
}
fn hash(mut x:u64)->u64 {
    x^=x>>30;x=x.wrapping_mul(0xbf58476d1ce4e5b9);
    x^=x>>27;x=x.wrapping_mul(0x94d049bb133111eb);
    x^(x>>31)
}
fn distance(a:PlayerPosition,b:PlayerPosition)->f64 {
    (a.x-b.x).hypot(a.z-b.z)
}
fn sky_exposed(world:&World,p:PlayerPosition)->bool {
    let x=p.x.floor() as i32;let z=p.z.floor() as i32;
    ((p.y as i32+2)..=255).all(|y|world.get(x,y,z)==0)
}
fn damage_held(held:Option<Stack>)->i16 {
    match held.map(|s|s.id) {
        Some(268)|Some(283)=>4,Some(272)=>5,Some(267)=>6,Some(276)=>7,_=>1
    }
}
impl Mobs {
    fn insert(&mut self,kind:Kind,position:PlayerPosition,baby:bool)->Event {
        let id=self.next_id;
        self.next_id+=1;
        let mob=Mob{id,kind,position,health:kind.health(),hit_tick:0,bite_tick:0,
            fuse:0,breed_tick:0,baby};
        let event=mob.spawn();
        self.entities.insert(id,mob);
        event
    }
    /// Natural Survival only, preserving flat E2E world behaviour.
    pub fn new(world:&World,enabled:bool)->Self {
        let mut state=Self{next_id:BASE_ID,enabled:enabled&&world.is_natural(),..Self::default()};
        if state.enabled {
            for (n,(x,z)) in [(8,9),(-8,7),(7,-8),(-9,-7)].into_iter().enumerate() {
                let p=PlayerPosition{x:x as f64+0.5,
                    y:world.natural_seed().map_or(65.0,|seed|crate::terrain::surface(seed,x,z) as f64+1.0),
                    z:z as f64+0.5};
                if !physics::collides(world,&p)&&physics::supported(world,&p) {
                    state.insert(if n==0 {Kind::Zombie}else if n==1 {Kind::Skeleton}
                        else if n==2 {Kind::Cow}else {Kind::Creeper},p,false);
                }
            }
        }
        state
    }
    pub fn spawn_events(&self)->impl Iterator<Item=Event>+'_ {
        self.entities.values().copied().map(Mob::spawn)
            .chain(self.arrows.values().copied().map(Arrow::spawn))
    }
    pub fn take_drops(&mut self)->Vec<(Stack,(i32,i32,i32))> {
        std::mem::take(&mut self.pending_drops)
    }
    pub fn take_explosions(&mut self)->Vec<Explosion> {
        std::mem::take(&mut self.explosions)
    }
    /// Attack a mob with authoritative held equipment, reach and cooldown.
    pub fn player_attack(&mut self,id:i32,at:PlayerPosition,held:Option<Stack>,tick:u64)->Option<Attack> {
        let mob=self.entities.get_mut(&id)?;
        if !mob.nearby(at,3.0)||(mob.hit_tick!=0&&tick.saturating_sub(mob.hit_tick)<10) {return None;}
        mob.hit_tick=tick;
        mob.health-=damage_held(held);
        let killed=mob.health<=0;
        let result=Attack{events:if killed {vec![mob.status(3),multiplayer::destroy(id)]}
                else {vec![mob.status(2)]},
            drop:killed.then_some((mob.kind.drop(),(mob.position.x.floor() as i32,
                mob.position.y.floor() as i32,mob.position.z.floor() as i32))),
            experience:if killed {if mob.kind.passive(){2}else{5}}else{0}};
        if killed {self.entities.remove(&id);}
        Some(result)
    }
    /// Animal breeding consumes feed only if a nearby same-kind partner exists.
    pub fn feed(&mut self,id:i32,held:Option<Stack>,tick:u64)->Option<Event> {
        let first=*self.entities.get(&id)?;
        let food=held?.id;
        if !first.kind.passive()||first.baby||!first.kind.breeding_food(food)
            ||tick.saturating_sub(first.breed_tick)<600 {return None;}
        let mate=self.entities.values().copied().find(|m|m.id!=id && m.kind==first.kind &&
            !m.baby && tick.saturating_sub(m.breed_tick)>=600 && distance(m.position,first.position)<7.0)?;
        self.entities.get_mut(&id)?.breed_tick=tick;
        self.entities.get_mut(&mate.id)?.breed_tick=tick;
        Some(self.insert(first.kind,first.position,true))
    }
    /// Charging and ammunition are managed by the player session before this call.
    pub fn fire_arrow(&mut self,shooter:Option<i32>,at:PlayerPosition,yaw:f32,pitch:f32,
        speed:f64,damage:u8)->Option<Event> {
        if self.arrows.len()>=ARROW_CAP||!at.is_valid()||!yaw.is_finite()||!pitch.is_finite(){return None;}
        let y=(yaw as f64).to_radians();let p=(pitch as f64).to_radians();
        let vx=-y.sin()*p.cos()*speed;let vz=y.cos()*p.cos()*speed;
        let vy=-p.sin()*speed;
        let id=self.next_id;self.next_id+=1;
        let arrow=Arrow{id,position:PlayerPosition{y:at.y+1.3,..at},velocity:[vx,vy,vz],
            shooter,age:0,damage};
        let event=arrow.spawn();
        self.arrows.insert(id,arrow);
        Some(event)
    }
    fn spawn_natural(&mut self,now:u64,world:&World,players:&[PlayerPosition],events:&mut Vec<Event>) {
        if !self.enabled||players.is_empty()||now%80!=0 {return;}
        let passive=self.entities.values().filter(|m|m.kind.passive()).count();
        let hostile=self.entities.len()-passive;
        for index in 0..4u64 {
            let seed=hash(now^index.wrapping_mul(0x9e3779b97f4a7c15)
                ^world.natural_seed().unwrap_or(0));
            let player=players[(seed as usize)%players.len()];
            let distance=24+(seed%25) as i32;
            let x=player.x.floor() as i32+if seed&1==0 {distance}else{-distance};
            let z=player.z.floor() as i32+(((seed>>8)%49) as i32-24);
            let Some(terrain)=world.natural_seed() else {return;};
            let p=PlayerPosition{x:x as f64+0.5,
                y:(crate::terrain::surface(terrain,x,z)+1) as f64,z:z as f64+0.5};
            if !p.is_valid()||distance_to_players(p,players)<24.0 ||
                !physics::supported(world,&p)||physics::collides(world,&p) {continue;}
            let open=sky_exposed(world,p);
            let is_night=(now%24000)>=13000&&(now%24000)<=23000;
            let grass=world.get(x,p.y as i32-1,z)>>4==2;
            let kind=if open && !is_night && grass {
                if passive>=PASSIVE_CAP {continue;}
                match seed%4 {0=>Kind::Cow,1=>Kind::Pig,2=>Kind::Sheep,_=>Kind::Chicken}
            } else {
                if hostile>=HOSTILE_CAP || (open && !is_night) {continue;}
                match seed%4 {0=>Kind::Zombie,1=>Kind::Skeleton,2=>Kind::Creeper,_=>Kind::Spider}
            };
            events.push(self.insert(kind,p,false));
            break; // One per spawn cycle bounds packet and CPU cost.
        }
    }
    /// Tick at most four elapsed 20Hz steps; caller owns broadcast fan-out.
    pub fn tick(&mut self,now:u64,world:&World,players:&[PlayerPosition])->Vec<Event> {
        let steps=now.saturating_sub(self.last_tick).min(MAX_STEPS);
        self.last_tick=now;
        let mut events=Vec::new();
        self.pending_hits.retain(|(_,_,t)|now.saturating_sub(*t)<30);
        self.spawn_natural(now,world,players,&mut events);
        for _ in 0..steps {
            let mut shots=Vec::new();
            let mut dead=Vec::new();
            for mob in self.entities.values_mut() {
                if !players.is_empty() && distance_to_players(mob.position,players)>128.0 {
                    dead.push(mob.id);continue;
                }
                let nearest=players.iter().copied()
                    .filter(|p|mob.nearby(*p,if mob.kind.passive(){12.0}else{24.0}))
                    .min_by(|a,b|distance(*a,mob.position).total_cmp(&distance(*b,mob.position)));
                let Some(target)=nearest else {continue;};
                let d=distance(target,mob.position);
                if mob.kind==Kind::Creeper {
                    if d<3.0 {mob.fuse=mob.fuse.saturating_add(1);} else {mob.fuse=mob.fuse.saturating_sub(1);}
                    if mob.fuse>=30 {
                        self.explosions.push(Explosion{position:mob.position,radius:3.0});
                        dead.push(mob.id);continue;
                    }
                }
                if mob.kind==Kind::Skeleton && d<16.0 &&
                    now.saturating_sub(mob.bite_tick)>=40 {
                    mob.bite_tick=now;
                    let yaw=(-(target.x-mob.position.x)).atan2(target.z-mob.position.z).to_degrees() as f32;
                    let pitch=-(target.y+1.0-mob.position.y).atan2(d).to_degrees() as f32;
                    shots.push((mob.id,mob.position,yaw,pitch));
                }
                let destination=if mob.kind.passive() {
                    if (now/80+mob.id as u64)%4!=0 {continue;}
                    PlayerPosition{x:mob.position.x+if mob.id%2==0 {1.0}else{-1.0},
                        z:mob.position.z+if mob.id%3==0 {1.0}else{-1.0},..mob.position}
                }else{target};
                if d<if mob.kind.passive(){1.0}else{1.5} {continue;}
                let waypoint=if now%20==0 {
                    navigation::next_step(world,mob.position,destination,64).unwrap_or(destination)
                }else{destination};
                let dx=waypoint.x-mob.position.x;
                let dz=waypoint.z-mob.position.z;
                let len=dx.hypot(dz);
                if len<1e-6 {continue;}
                let speed=if mob.kind.passive(){0.065}else{0.11};
                let candidate=PlayerPosition{x:mob.position.x+dx/len*speed,
                    z:mob.position.z+dz/len*speed,y:mob.position.y};
                if physics::valid_movement(world,&mob.position,&candidate)&&physics::supported(world,&candidate) {
                    mob.position=candidate;
                    events.push(multiplayer::teleport(mob.id,&candidate,0.0,0.0));
                }
            }
            for id in dead {
                if self.entities.remove(&id).is_some() { events.push(multiplayer::destroy(id)); }
            }
            for (id,at,yaw,pitch) in shots {
                if let Some(packet)=self.fire_arrow(Some(id),at,yaw,pitch,1.5,3) {events.push(packet);}
            }
            self.tick_arrows(world,players,now,&mut events);
        }
        if self.explosions.len()>32 {self.explosions.truncate(32);}
        events
    }
    fn tick_arrows(&mut self,world:&World,players:&[PlayerPosition],now:u64,events:&mut Vec<Event>) {
        let mut removed=Vec::new();
        let mut arrow_hits=Vec::new();
        for arrow in self.arrows.values_mut() {
            arrow.age+=1;
            let next=PlayerPosition{x:arrow.position.x+arrow.velocity[0],
                y:arrow.position.y+arrow.velocity[1],z:arrow.position.z+arrow.velocity[2]};
            arrow.velocity[0]*=0.99; arrow.velocity[1]=arrow.velocity[1]*0.99-0.05;
            arrow.velocity[2]*=0.99;
            if arrow.age>=100 || !next.is_valid() || physics::collides(world,&next) {
                removed.push(arrow.id);continue;
            }
            arrow.position=next;
            if arrow.shooter.is_some_and(|id|self.entities.contains_key(&id)) {
                if let Some(hit)=players.iter().find(|p| {
                    (p.x-next.x).hypot(p.z-next.z)<0.85 && (p.y+0.9-next.y).abs()<1.3
                }) {
                    self.pending_hits.push((*hit,arrow.damage,now));
                    removed.push(arrow.id);continue;
                }
            } else {
                if let Some(mob)=self.entities.values().find(|m|m.nearby(next,0.9)) {
                    arrow_hits.push((mob.id,arrow.damage));
                    removed.push(arrow.id);continue;
                }
            }
            events.push(multiplayer::teleport(arrow.id,&next,0.0,0.0));
        }
        for (id,damage) in arrow_hits {
            if let Some(mob)=self.entities.get_mut(&id) {
                mob.health-=i16::from(damage);
                if mob.health<=0 {
                    self.pending_drops.push((mob.kind.drop(),(mob.position.x.floor() as i32,
                        mob.position.y.floor() as i32,mob.position.z.floor() as i32)));
                    events.push(mob.status(3));events.push(multiplayer::destroy(id));
                    self.entities.remove(&id);
                }else{events.push(mob.status(2));}
            }
        }
        for id in removed {
            self.arrows.remove(&id);
            events.push(multiplayer::destroy(id));
        }
    }
    pub fn hurt_player(&mut self,player:PlayerPosition,tick:u64)->u8 {
        let mut amount=0u8;
        self.pending_hits.retain(|(p,d,t)| {
            if tick.saturating_sub(*t)<30 && (p.x-player.x).hypot(p.z-player.z)<0.75 {
                amount=amount.saturating_add(*d);false
            }else{tick.saturating_sub(*t)<30}
        });
        for mob in self.entities.values_mut() {
            if !mob.kind.passive() && mob.kind!=Kind::Skeleton && mob.kind!=Kind::Creeper
                && mob.nearby(player,1.6) &&
                (mob.bite_tick==0||tick.saturating_sub(mob.bite_tick)>=ATTACK_INTERVAL) {
                mob.bite_tick=tick;
                amount=amount.saturating_add(if mob.kind==Kind::Spider {2}else{3});
                break;
            }
        }
        amount
    }
}
fn distance_to_players(p:PlayerPosition,players:&[PlayerPosition])->f64 {
    players.iter().map(|other|distance(p,*other)).fold(f64::INFINITY,f64::min)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_mobs_spawn_in_protocol5() {
        let world=World::natural(42);
        let mut mobs=Mobs::new(&world,true);
        assert!(!mobs.entities.is_empty());
        let zombie=*mobs.entities.values().next().unwrap();
        assert_eq!(zombie.kind,Kind::Zombie);
        let packet=zombie.spawn();
        assert_eq!(packet.id,0x0f);
        assert_eq!(packet.body[3],54);
        assert!(packet.body.ends_with(&[0,0,0x7f]));
        let position=zombie.position;
        for tick in [100,111] {
            assert!(mobs.player_attack(zombie.id,position,Some(Stack{id:276,count:1,damage:0}),tick).unwrap().drop.is_none());
        }
        assert_eq!(mobs.player_attack(zombie.id,position,Some(Stack{id:276,count:1,damage:0}),122).unwrap().drop.unwrap().0.id,367);
    }
    #[test]
    fn flat_world_has_no_mobs_and_bow_shot_is_bounded() {
        assert!(Mobs::new(&World::new(),true).entities.is_empty());
        let mut mobs=Mobs::default();
        assert!(mobs.fire_arrow(None,PlayerPosition::default(),0.0,0.0,3.0,4).is_some());
        assert_eq!(mobs.arrows.len(),1);
        assert_eq!(mobs.arrows.values().next().unwrap().spawn().id,0x0e);
    }
}
