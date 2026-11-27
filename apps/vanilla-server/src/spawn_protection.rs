//! Issue #1318: vanilla `spawn-protection` enforcement semantics.
//!
//! Verification status (honesty note carried into the PR): the exact NMS
//! 1.7.10 enforcement call-site and per-dimension applicability were NOT
//! verified from decompiled primary source. The semantics implemented here
//! are pinned from the documented behavior of the `spawn-protection` key in
//! server.properties (same key and default 16 since the 1.7.10 era, per the
//! minecraft.wiki Server.properties page):
//!
//! * a square area around the world spawn, Chebyshev distance `<= radius`,
//!   boundary inclusive (`R = 1` protects exactly a 3x3 area);
//! * `R <= 0` disables protection;
//! * protection is automatically disabled while no operators are configured;
//! * operators (OP) always bypass;
//! * non-operators cannot break, place or use blocks inside the area.
//!
//! 1.7.10 `level.dat` stores exactly one spawn point and one protection
//! radius for the whole server, so this module applies the single root-world
//! spawn coordinate in every dimension. [`Policy`] deliberately accepts no
//! dimension input; the unit tests pin that global-spawn choice.
use crate::admin::Administration;
use crate::world::World;
use std::path::Path;

/// Pure decision function for the protected square.
///
/// Returns true when the block column `(x, z)` lies inside a
/// `spawn-protection` square of radius `r` centered on `spawn`.
/// The boundary is inclusive, so `r == 1` covers exactly the 3x3 cells
/// around the spawn. `r <= 0` never protects. Integer math only; the
/// subtraction widens to i64 so attacker-chosen extreme coordinates
/// (i32::MIN..i32::MAX from raw network packets) can never overflow, panic
/// or wrap into a wrong answer.
pub fn spawn_protected(r: i32, x: i32, z: i32, spawn: (i32, i32)) -> bool {
    r > 0
        && (i64::from(x) - i64::from(spawn.0)).abs() <= i64::from(r)
        && (i64::from(z) - i64::from(spawn.1)).abs() <= i64::from(r)
}

/// Per-session, periodically refreshed view of the protection state:
/// the configured radius, the single global spawn column and whether this
/// particular player is subject to protection at all (operators bypass, and
/// the whole feature is disabled while no operators are configured).
///
/// Resolution performs no per-block-action disk I/O: the packet handlers
/// call only [`Policy::blocks`], a pure integer check.
#[derive(Clone, Debug)]
pub struct Policy {
    radius: i32,
    spawn: (i32, i32),
    applies_to_this_player: bool,
}

impl Policy {
    /// Resolve the live protection state for one player.
    ///
    /// Fail-closed choices: an unreadable operators file counts as
    /// "operators configured" (protection stays active), and an unreadable
    /// operator check counts as "not an operator". A missing or invalid
    /// world-spawn sidecar falls back to the same world default the login
    /// path uses, keeping protection aligned with where players actually
    /// spawn. The spawn always comes from `root_world_path` — never a
    /// per-dimension file — because 1.7.10 has one global spawn point.
    pub fn resolve(
        configured_radius: i32,
        administration: &Administration,
        name: &str,
        verified_uuid: Option<&str>,
        root_world_path: &Path,
        world: &World,
    ) -> Self {
        let operators_configured = administration.has_operators().unwrap_or(true);
        let is_operator = administration
            .is_operator(name, verified_uuid)
            .unwrap_or(false);
        let default_spawn = (0, world.spawn_y() as i32 - 1, 0);
        let spawn = crate::world_spawn::effective(root_world_path, world)
            .map(|point| point.coordinates())
            .unwrap_or(default_spawn);
        Self {
            radius: configured_radius,
            spawn: (spawn.0, spawn.2),
            applies_to_this_player: operators_configured && !is_operator,
        }
    }

    /// True when a non-OP player must be refused for a block action in the
    /// column `(x, z)`. No disk access; safe to call per packet.
    pub fn blocks(&self, x: i32, z: i32) -> bool {
        self.applies_to_this_player && spawn_protected(self.radius, x, z, self.spawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_admin(tag: &str) -> (Administration, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "ir1710-spawnprotect-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        (Administration::new(root.clone()).unwrap(), root)
    }

    #[test]
    fn radius_zero_or_negative_disables_protection() {
        assert!(!spawn_protected(0, 0, 0, (0, 0)));
        assert!(!spawn_protected(-1, 0, 0, (0, 0)));
        assert!(!spawn_protected(i32::MIN, 0, 0, (0, 0)));
        // Even far inside the old area nothing is protected once disabled.
        for offset in [-16, -1, 0, 1, 16] {
            assert!(!spawn_protected(0, offset, offset, (0, 0)));
        }
    }

    #[test]
    fn radius_one_covers_exactly_three_by_three_including_negative_spawn() {
        // Spawn at negative coordinates: the square must follow it.
        let spawn = (-4, -9);
        for dx in -1..=1 {
            for dz in -1..=1 {
                assert!(
                    spawn_protected(1, -4 + dx, -9 + dz, spawn),
                    "({},{}) must be protected",
                    -4 + dx,
                    -9 + dz
                );
            }
        }
        // Exactly one step outside every edge and corner is editable.
        for (x, z) in [(-6, -9), (-2, -9), (-4, -11), (-4, -7), (-6, -11), (-2, -7)] {
            assert!(!spawn_protected(1, x, z, spawn), "({x},{z}) must be free");
        }
    }

    #[test]
    fn boundary_is_inclusive_and_radius_plus_one_is_editable() {
        // Vanilla default R=16 around the default spawn (0,0): the 33x33
        // square including the border is protected; R+1 in any direction is
        // not. Y never participates (square, not cube).
        assert!(spawn_protected(16, 0, 0, (0, 0)));
        assert!(spawn_protected(16, 16, 16, (0, 0)), "corner inclusive");
        assert!(spawn_protected(16, -16, -16, (0, 0)));
        assert!(spawn_protected(16, 16, -16, (0, 0)));
        assert!(!spawn_protected(16, 17, 16, (0, 0)), "R+1 outside east");
        assert!(!spawn_protected(16, -17, 16, (0, 0)), "R+1 outside west");
        assert!(!spawn_protected(16, 16, 17, (0, 0)), "R+1 outside south");
        assert!(!spawn_protected(16, 16, -17, (0, 0)), "R+1 outside north");
        assert!(!spawn_protected(16, 17, 17, (0, 0)), "R+1 corner outside");
    }

    #[test]
    fn extreme_packet_coordinates_never_overflow_the_decision() {
        // Raw packet coordinates can be any i32. The i64 widening must keep
        // the answer exact where the naive i32 subtraction would panic in
        // debug builds and wrap in release builds.
        assert!(!spawn_protected(16, i32::MIN, 0, (16, 0)));
        assert!(!spawn_protected(
            16,
            i32::MAX,
            i32::MAX,
            (i32::MIN, i32::MIN)
        ));
        assert!(spawn_protected(i32::MAX, i32::MAX, 0, (i32::MAX, 0)));
        assert!(spawn_protected(1, i32::MIN, i32::MIN, (i32::MIN, i32::MIN)));
        assert!(!spawn_protected(
            1,
            i32::MIN,
            i32::MIN + 2,
            (i32::MIN, i32::MIN)
        ));
    }

    #[test]
    fn empty_operators_list_auto_disables_protection() {
        // Fresh Administration store: ops.json exists but is empty, so the
        // whole feature is off (documented vanilla behavior).
        let (administration, root) = temp_admin("empty-ops");
        let world_path = root.join("flat.irw");
        let world = World::default();
        let policy = Policy::resolve(16, &administration, "Steve", None, &world_path, &world);
        assert!(!policy.blocks(0, 0), "no operators -> protection disabled");
        assert!(!policy.blocks(5, -5));

        // The moment any operator is configured, protection turns on for
        // everyone else at the same radius.
        administration.set_op("Operator", true).unwrap();
        let policy = Policy::resolve(16, &administration, "Steve", None, &world_path, &world);
        assert!(policy.blocks(0, 0), "one operator -> protection active");
        assert!(!policy.blocks(17, 17), "and still only inside the square");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_operator_bypasses_and_name_claims_do_not() {
        let (administration, root) = temp_admin("op-bypass");
        let world_path = root.join("flat.irw");
        let world = World::default();
        let uuid = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        administration.set_op_verified("Operator", uuid).unwrap();

        // The verified account bypasses at the spawn center...
        let policy = Policy::resolve(
            16,
            &administration,
            "Operator",
            Some(uuid),
            &world_path,
            &world,
        );
        assert!(!policy.blocks(0, 0), "OP bypasses spawn protection");

        // ...a different name claiming OP without any verified UUID does
        // not (offline usernames never grant authority in this server)...
        let claimer = Policy::resolve(16, &administration, "Operator", None, &world_path, &world);
        assert!(claimer.blocks(0, 0), "unverified name must stay protected");

        // ...and neither does an unrelated player presenting another
        // account's UUID.
        let impostor = Policy::resolve(
            16,
            &administration,
            "Operator",
            Some("d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca"),
            &world_path,
            &world,
        );
        assert!(impostor.blocks(0, 0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn single_root_world_spawn_applies_in_all_dimensions() {
        // 1.7.10 has ONE spawn point and ONE protection radius for the whole
        // server. Policy::resolve takes only the root world path and no
        // dimension input, so a per-dimension sidecar must never move the
        // protected square. This test pins that by planting a DIFFERENT
        // spawn in a nether-style sidecar next to the root world file.
        let (administration, root) = temp_admin("global-spawn");
        administration.set_op("Operator", true).unwrap();
        let world_path = root.join("flat.irw");
        let world = World::default();

        let moved = crate::world_spawn::WorldSpawn::new(40, 70, -30).unwrap();
        crate::world_spawn::save(&world_path, moved).unwrap();
        // A decoy only a dimension-aware (wrong) implementation would read.
        fs::write(
            root.join("flat.nether.worldspawn.json"),
            br#"{"version":1,"x":-400,"y":70,"z":400}"#,
        )
        .unwrap();

        let policy = Policy::resolve(16, &administration, "Steve", None, &world_path, &world);
        // The root spawn (40,-30) is protected with radius 16...
        assert!(policy.blocks(40, -30));
        assert!(policy.blocks(56, -14), "corner inclusive");
        assert!(!policy.blocks(57, -14));
        assert!(
            !policy.blocks(0, 0),
            "the default center must lose once an explicit spawn exists"
        );
        assert!(
            !policy.blocks(-400, 400),
            "a per-dimension spawn must not steer the global square"
        );
        // The same Policy value answers identically for every session and
        // therefore for every dimension (overworld, nether and end share
        // this one square in 1.7.10).
        assert_eq!(policy.blocks(40, -30), policy.blocks(40, -30));
        fs::remove_dir_all(root).unwrap();
    }
}
