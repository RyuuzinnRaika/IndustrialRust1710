#![no_main]
use libfuzzer_sys::fuzz_target;

// Reuse the actual vanilla-server decoder and validators, not a similar model.
#[path = "../../apps/vanilla-server/src/legacy_inventory_nbt.rs"]
mod legacy_inventory_nbt;
#[path = "../../apps/vanilla-server/src/legacy_nbt.rs"]
mod legacy_nbt;

fuzz_target!(|bytes: &[u8]| {
    // NBT is untrusted; production decoder enforces 2 MiB before parsing.
    let _ = legacy_nbt::parse_chunk(bytes, 0, 0);
});
