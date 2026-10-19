use industrial_core::{IndustrialSimulation,NodeKind};
use std::time::Instant;
fn main(){let a:Vec<String>=std::env::args().collect();let count=a.get(1).and_then(|v|v.parse().ok()).unwrap_or(20_000usize);let ticks=a.get(2).and_then(|v|v.parse().ok()).unwrap_or(200usize);
let mut s=IndustrialSimulation::new();let mut previous=s.add_node(NodeKind::Generator{energy_per_tick:count as u64});for _ in 0..count{let id=s.add_node(NodeKind::Consumer{energy_per_tick:1});s.connect(previous,id).unwrap();previous=id;}s.tick().unwrap();
let start=Instant::now();let mut checksum=0u64;for _ in 0..ticks{checksum=checksum.wrapping_add(s.tick().unwrap().delivered);}let elapsed=start.elapsed().as_secs_f64();println!("nodes={} ticks={} mean_ms={:.6} checksum={}",s.node_count(),ticks,if ticks==0{0.0}else{elapsed*1000.0/ticks as f64},checksum);println!("SYNTHETIC BENCHMARK ONLY; not Minecraft server TPS.");}
