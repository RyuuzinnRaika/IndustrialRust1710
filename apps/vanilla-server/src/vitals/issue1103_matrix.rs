//! FoodStats bytecode-derived timer expectation; vanilla was not executed.
use super::Vitals;

#[test]
fn easy_floor_timer_survives_switch_to_hard() {
    // The owned 1.7.10 client jar (SHA-1 e80d9b3bf5085002218d4be59e668bac718abbc6)
    // increments the food timer while food is zero, even at Easy's HP floor.
    // On update 80 in Hard, FoodStats requests one point of starvation damage
    // and resets the timer. Final player HP in vanilla depends on the separate
    // damage path and has not been measured.
    let mut v = Vitals {
        health: 10.0,
        food: 0,
        saturation: 0.0,
        exhaustion: 0.0,
        food_timer: 0,
    };
    println!(
        "difficulty,updates,reference_timer,reference_damage_request,observed_timer,observed_hp"
    );
    println!("easy,0,0,false,{},{}", v.food_timer, v.health);
    for _ in 0..79 {
        v.tick_with_difficulty(1, 1);
    }
    println!("easy,79,79,false,{},{}", v.food_timer, v.health);
    let at_79 = v;
    v.tick_with_difficulty(1, 3);
    println!("hard,80,0,true,{},{}", v.food_timer, v.health);
    assert_eq!(at_79.health, 10.0, "no damage expected at Easy floor");
    assert_eq!(at_79.food_timer, 79, "timer should continue at Easy floor");
    assert_eq!(v.food_timer, 0, "timer should reset after update 80");
    // Rust applies starvation directly; this is an expected Rust result for
    // the test's no-other-damage scenario, not a vanilla observed HP result.
    assert_eq!(
        v.health, 9.0,
        "Hard update should apply the requested damage"
    );
}

#[test]
fn starvation_floor_timer_wraps_without_visible_change() {
    for difficulty in [0, 1, 2] {
        let health = match difficulty {
            0 => 10.0,
            1 => 10.0,
            _ => 1.0,
        };
        let mut vitals = Vitals {
            health,
            food: 0,
            saturation: 0.0,
            exhaustion: 0.0,
            food_timer: 0,
        };
        for _ in 0..2 {
            for _ in 0..3 {
                assert!(!vitals.tick_with_difficulty(20, difficulty));
            }
            assert_eq!(vitals.food_timer, 60);
            assert!(!vitals.tick_with_difficulty(20, difficulty));
            assert_eq!(vitals.food_timer, 0);
            assert_eq!(vitals.health, health);
        }
    }
}

#[test]
fn eating_or_death_stops_pending_starvation_timer() {
    let mut vitals = Vitals {
        health: 10.0,
        food: 0,
        saturation: 0.0,
        exhaustion: 0.0,
        food_timer: 79,
    };
    vitals.food = 1;
    assert!(!vitals.tick_with_difficulty(1, 1));
    assert_eq!(vitals.food_timer, 0);

    vitals.health = 1.0;
    vitals.food = 0;
    vitals.food_timer = 79;
    assert!(vitals.tick_with_difficulty(20, 3));
    assert_eq!(vitals.health, 0.0);
    assert_eq!(vitals.food_timer, 0);
    assert!(!vitals.tick_with_difficulty(20, 3));
    assert_eq!(vitals.food_timer, 0);
}
