//! Deterministic industrial-network core; not a Minecraft world integration.
use std::collections::VecDeque;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]pub struct NodeId(pub usize);
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum NodeKind{Generator{energy_per_tick:u64},Consumer{energy_per_tick:u64},Cable}
#[derive(Clone,Debug,PartialEq,Eq)]pub enum SimulationError{InvalidNode(NodeId),SelfConnection(NodeId),ArithmeticOverflow}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub struct TickStats{pub tick:u64,pub produced:u64,pub delivered:u64,pub network_count:usize}
#[derive(Default,Debug)]struct Component{producers:u64,consumers:Vec<(usize,u64)>,cursor:usize}
#[derive(Default,Debug)]pub struct IndustrialSimulation{nodes:Vec<NodeKind>,adj:Vec<Vec<usize>>,components:Vec<Component>,dirty:bool,last:Vec<u64>,tick:u64}
impl IndustrialSimulation{
 pub fn new()->Self{Self::default()}
 pub fn add_node(&mut self,kind:NodeKind)->NodeId{let id=NodeId(self.nodes.len());self.nodes.push(kind);self.adj.push(Vec::new());self.last.push(0);self.dirty=true;id}
 pub fn node_count(&self)->usize{self.nodes.len()}
 fn valid(&self,id:NodeId)->Result<(),SimulationError>{if id.0<self.nodes.len(){Ok(())}else{Err(SimulationError::InvalidNode(id))}}
 pub fn set_kind(&mut self,id:NodeId,kind:NodeKind)->Result<(),SimulationError>{self.valid(id)?;if self.nodes[id.0]!=kind{self.nodes[id.0]=kind;self.dirty=true;}Ok(())}
 pub fn connect(&mut self,a:NodeId,b:NodeId)->Result<(),SimulationError>{self.valid(a)?;self.valid(b)?;if a==b{return Err(SimulationError::SelfConnection(a));}
if !self.adj[a.0].contains(&b.0){self.adj[a.0].push(b.0);self.adj[b.0].push(a.0);self.dirty=true;}Ok(())}
 pub fn disconnect(&mut self,a:NodeId,b:NodeId)->Result<(),SimulationError>{self.valid(a)?;self.valid(b)?;if a==b{return Err(SimulationError::SelfConnection(a));}
if self.adj[a.0].contains(&b.0){self.adj[a.0].retain(|v|*v!=b.0);self.adj[b.0].retain(|v|*v!=a.0);self.dirty=true;}Ok(())}
 pub fn last_delivered(&self,id:NodeId)->Result<u64,SimulationError>{self.valid(id)?;Ok(self.last[id.0])}
 fn rebuild(&mut self)->Result<(),SimulationError>{let mut seen=vec![false;self.nodes.len()];let mut q=VecDeque::new();let mut comps=Vec::new();for start in 0..self.nodes.len(){if seen[start]{continue;}let mut c=Component::default();seen[start]=true;q.push_back(start);while let Some(n)=q.pop_front(){match self.nodes[n]{NodeKind::Generator{energy_per_tick}=>c.producers=c.producers.checked_add(energy_per_tick).ok_or(SimulationError::ArithmeticOverflow)?,NodeKind::Consumer{energy_per_tick}=>c.consumers.push((n,energy_per_tick)),NodeKind::Cable=>{}}for &v in &self.adj[n]{if !seen[v]{seen[v]=true;q.push_back(v);}}}c.consumers.sort_unstable_by_key(|(n,_)|*n);comps.push(c);}self.components=comps;self.dirty=false;Ok(())}
 pub fn tick(&mut self)->Result<TickStats,SimulationError>{if self.dirty{self.rebuild()?;}self.last.fill(0);let(mut produced,mut delivered)=(0u64,0u64);for c in &mut self.components{produced=produced.checked_add(c.producers).ok_or(SimulationError::ArithmeticOverflow)?;let mut remain=c.producers;let n=c.consumers.len();for j in 0..n{let(id,demand)=c.consumers[(c.cursor+j)%n];let accepted=remain.min(demand);self.last[id]=accepted;remain-=accepted;delivered=delivered.checked_add(accepted).ok_or(SimulationError::ArithmeticOverflow)?;if remain==0{break;}}if n>0{c.cursor=(c.cursor+1)%n;}}self.tick=self.tick.checked_add(1).ok_or(SimulationError::ArithmeticOverflow)?;Ok(TickStats{tick:self.tick,produced,delivered,network_count:self.components.len()})}
}
#[cfg(test)]mod tests{use super::*;#[test]fn transfers_and_disconnects(){let mut s=IndustrialSimulation::new();let a=s.add_node(NodeKind::Generator{energy_per_tick:10});let b=s.add_node(NodeKind::Consumer{energy_per_tick:5});assert_eq!(s.tick().unwrap().delivered,0);s.connect(a,b).unwrap();assert_eq!(s.tick().unwrap().delivered,5);s.disconnect(a,b).unwrap();assert_eq!(s.tick().unwrap().delivered,0);}#[test]fn fairness(){let mut s=IndustrialSimulation::new();let g=s.add_node(NodeKind::Generator{energy_per_tick:1});let a=s.add_node(NodeKind::Consumer{energy_per_tick:1});let b=s.add_node(NodeKind::Consumer{energy_per_tick:1});s.connect(g,a).unwrap();s.connect(g,b).unwrap();s.tick().unwrap();assert_eq!(s.last_delivered(a).unwrap(),1);s.tick().unwrap();assert_eq!(s.last_delivered(b).unwrap(),1);}#[test]fn invalid_nodes(){let mut s=IndustrialSimulation::new();let a=s.add_node(NodeKind::Cable);assert_eq!(s.connect(a,a),Err(SimulationError::SelfConnection(a)));}}
