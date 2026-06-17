//! The stability contract, enforced by CI:
//!   1. Same seed + same commands => bit-identical world hash, every time.
//!   2. save -> load -> continue == never-saved continue (snapshots are perfect).
//!   3. A full bot-vs-bot war actually plays out (economy, production, combat all fire).

use ironvein_sim::bot::Bot;
use ironvein_sim::mapgen;
use ironvein_sim::{Command, Mode, Pid, World};

/// Drive a world with two bots for `ticks`, collecting hashes every 100 ticks.
fn run_botwar(seed: u64, ticks: u32) -> (World, Vec<u64>) {
    let mut w = mapgen::verdant_divide(seed, Mode::Skirmish);
    let mut bots = vec![Bot::new(0, "Crimson".into()), Bot::new(1, "Cobalt".into())];
    let mut hashes = Vec::new();
    for _ in 0..ticks {
        let mut cmds: Vec<(Pid, Command)> = Vec::new();
        for b in bots.iter_mut() {
            for c in b.think(&w) {
                cmds.push((b.pid, c));
            }
        }
        w.step(&cmds);
        if w.tick % 100 == 0 {
            hashes.push(w.hash());
        }
    }
    (w, hashes)
}

#[test]
fn determinism_two_runs_identical() {
    let (wa, ha) = run_botwar(42, 1500);
    let (wb, hb) = run_botwar(42, 1500);
    assert_eq!(ha, hb, "hash trail diverged between identical runs");
    assert_eq!(wa.hash(), wb.hash());
    assert_eq!(wa.save_bytes(), wb.save_bytes(), "snapshots are not byte-identical");
}

#[test]
fn different_seeds_differ() {
    let (wa, _) = run_botwar(42, 400);
    let (wb, _) = run_botwar(43, 400);
    assert_ne!(wa.hash(), wb.hash());
}

#[test]
fn save_load_continue_is_seamless() {
    // run 600 ticks, snapshot, then run both the original and the loaded copy
    // for another 600 ticks with the same commands: they must stay identical.
    let mut w = mapgen::verdant_divide(7, Mode::Persistent);
    let mut bots = vec![Bot::new(0, "Crimson".into()), Bot::new(1, "Cobalt".into())];
    for _ in 0..600 {
        let mut cmds: Vec<(Pid, Command)> = Vec::new();
        for b in bots.iter_mut() {
            for c in b.think(&w) {
                cmds.push((b.pid, c));
            }
        }
        w.step(&cmds);
    }
    let snap = w.save_bytes();
    let mut loaded = World::load_bytes(&snap).expect("snapshot failed to load");
    assert_eq!(w.hash(), loaded.hash(), "load does not reproduce the saved state");

    // bots are stateful; clone the strategy by re-deriving from identical worlds:
    // feed BOTH worlds the same command stream computed from the original.
    let mut bots2 = vec![Bot::new(0, "Crimson".into()), Bot::new(1, "Cobalt".into())];
    // fast-forward the fresh bots' internal clocks against the loaded world once
    // (Bot is deliberately ~stateless apart from attack timing; sync that field)
    for (a, b) in bots.iter().zip(bots2.iter_mut()) {
        b.clone_timing_from(a);
    }
    for _ in 0..600 {
        let mut cmds: Vec<(Pid, Command)> = Vec::new();
        for b in bots.iter_mut() {
            for c in b.think(&w) {
                cmds.push((b.pid, c));
            }
        }
        let cmds2 = cmds.clone();
        w.step(&cmds);
        loaded.step(&cmds2);
        assert_eq!(w.hash(), loaded.hash(), "loaded world diverged at tick {}", w.tick);
    }
    let _ = bots2;
}

#[test]
fn the_war_actually_happens() {
    let (w, _) = run_botwar(123, 4000);
    // both settled
    assert!(w.players.len() >= 2);
    assert!(w.players[0].joined && w.players[1].joined);
    // economy ran: someone earned beyond starting credits at some point
    // (credits get spent, so check infrastructure instead)
    let buildings = w.ents.iter().filter(|e| e.kind.is_building()).count();
    let units = w.ents.iter().filter(|e| e.kind.is_unit()).count();
    assert!(buildings >= 6, "bots failed to build bases (got {buildings})");
    assert!(units >= 4, "bots failed to raise units (got {units})");
    // chat log captured world events
    assert!(!w.chat.is_empty());
}

#[test]
fn join_grants_starter_kit_and_spawns_are_exclusive() {
    let mut w = mapgen::verdant_divide(5, Mode::Persistent);
    w.step(&[(0, Command::Join { name: "Ada".into(), color: 0, key: [1; 32] })]);
    w.step(&[(1, Command::Join { name: "Lin".into(), color: 1, key: [2; 32] })]);
    let p0_ents = w.ents.iter().filter(|e| e.owner == 0).count();
    let p1_ents = w.ents.iter().filter(|e| e.owner == 1).count();
    // planetfall: a Starship + its 55-unit landing contingent
    assert_eq!(p0_ents, 56, "starter kit should be the Starship + full landing party");
    assert_eq!(p1_ents, 56);
    // exactly one Starship (the landing craft), and it's a building
    assert_eq!(
        w.ents.iter().filter(|e| e.owner == 0 && e.kind == ironvein_sim::stats::Kind::Starship).count(),
        1,
        "one landing craft per colony"
    );
    assert_ne!(
        w.map.spawn_used[0], w.map.spawn_used[1],
        "two players on one spawn site"
    );
    // double-join is a no-op
    let before = w.hash();
    let mut w2 = World::load_bytes(&w.save_bytes()).unwrap();
    w.step(&[(0, Command::Join { name: "Ada".into(), color: 0, key: [1; 32] })]);
    w2.step(&[]);
    let _ = before;
    assert_eq!(w.hash(), w2.hash(), "rejoin must not change the world");
}

#[test]
fn dominant_tracks_who_controls_the_region() {
    let mut w = mapgen::verdant_divide(7, Mode::Persistent);
    assert_eq!(w.dominant(), None, "nobody has built anything yet");
    w.step(&[(0, Command::Join { name: "Ada".into(), color: 0, key: [1; 32] })]);
    assert_eq!(w.dominant(), Some(0), "Ada's landed Starship makes her the controller");
    assert!(w.territory(0) > 0);
    w.step(&[(1, Command::Join { name: "Lin".into(), color: 1, key: [2; 32] })]);
    // equal starter bases -> tie broken to the lowest pid
    assert_eq!(w.dominant(), Some(0));
}

#[test]
fn alliances_are_mutual_and_persist() {
    let mut w = mapgen::verdant_divide(11, Mode::Persistent);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [1; 32] })]);
    w.step(&[(1, Command::Join { name: "B".into(), color: 1, key: [2; 32] })]);
    assert!(!w.allied(0, 1));
    w.step(&[(0, Command::Ally { with: 1 })]); // one-sided offer
    assert!(!w.allied(0, 1), "a one-sided offer is not an alliance");
    w.step(&[(1, Command::Ally { with: 0 })]); // reciprocated
    assert!(w.allied(0, 1) && w.allied(1, 0), "mutual offers form an alliance");
    // survives a save/load round-trip (ally mask is serialised + hashed)
    let w2 = World::load_bytes(&w.save_bytes()).unwrap();
    assert_eq!(w.hash(), w2.hash());
    assert!(w2.allied(0, 1));
    // withdrawing breaks it
    w.step(&[(0, Command::Ally { with: 1 })]);
    assert!(!w.allied(0, 1));
}

#[test]
fn allies_do_not_shoot_each_other() {
    use ironvein_sim::stats::Kind;
    use ironvein_sim::{Fp, FX};
    let mut w = mapgen::verdant_divide(12, Mode::Persistent);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [1; 32] })]);
    w.step(&[(1, Command::Join { name: "B".into(), color: 1, key: [2; 32] })]);
    // two riflemen toe-to-toe on open ground
    let spot = Fp { x: 60 * FX, y: 60 * FX };
    let a = w.ents.spawn(0, Kind::Rifleman, spot);
    let b = w.ents.spawn(1, Kind::Rifleman, Fp { x: spot.x + FX, y: spot.y });
    // ally before they can fight
    w.step(&[(0, Command::Ally { with: 1 }), (1, Command::Ally { with: 0 })]);
    for _ in 0..40 {
        w.step(&[]);
    }
    let ahp = w.ents.get(a).map(|e| e.hp).unwrap_or(0);
    let bhp = w.ents.get(b).map(|e| e.hp).unwrap_or(0);
    assert_eq!(ahp, 100, "allied rifleman A took damage");
    assert_eq!(bhp, 100, "allied rifleman B took damage");
}

#[test]
fn monsters_spawn_at_night_and_burn_at_dawn() {
    use ironvein_sim::mapgen;
    use ironvein_sim::world::{Mode, is_night};
    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    // give the world a couple of joined players so monsters have an anchor
    w.step(&[(0, ironvein_sim::command::Command::Join { name: "A".into(), color: 0, key: [0;32] })]);
    w.step(&[(1, ironvein_sim::command::Command::Join { name: "B".into(), color: 1, key: [1;32] })]);
    // run through a night
    let mut peak = 0usize;
    while w.tick < 2700 {
        w.step(&[]);
        let m = w.ents.iter().filter(|e| e.kind.is_monster()).count();
        peak = peak.max(m);
    }
    assert!(is_night(1500), "tick 1500 should be night");
    assert!(peak > 0, "monsters should have spawned during the night (peak={peak})");
    // run into daylight; the horde should burn down
    while w.tick < 3300 { w.step(&[]); }
    let day = w.ents.iter().filter(|e| e.kind.is_monster()).count();
    assert!(day < peak, "monsters should burn off by day (day={day}, peak={peak})");
}

/// Food economy (save v11): you land with food and a storage cap, the army eats
/// it (run dry → starving), wildlife roams, and it all round-trips through a save.
#[test]
fn food_economy_starves_and_persists_v11() {
    use ironvein_sim::stats::{Kind, FOOD_PERIOD};
    use ironvein_sim::world::Mode;
    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    assert!(w.players[0].food > 0, "you land with a larder");
    assert!(w.players[0].food_cap >= ironvein_sim::stats::BASE_FOOD_CAP, "food cap is set");
    assert!(w.ents.iter().any(|e| e.kind == Kind::Deer), "deer roam the meadow");

    // v11 round-trip (food + food_cap + starving + loot kinds are hashed)
    let w2 = World::load_bytes(&w.save_bytes()).unwrap();
    assert_eq!(w2.players[0].food, w.players[0].food);
    assert_eq!(w2.hash(), w.hash(), "v11 save is bit-identical");

    // empty the larder; the big landing army goes hungry
    w.players[0].food = 0;
    for _ in 0..(FOOD_PERIOD * 2) {
        w.step(&[]);
    }
    assert!(w.players[0].starving, "no food + an army to feed = starving");
}

/// Regression: a harvester already carrying ORE, ordered onto STONE, must end up
/// mining stone (it dumps the ore first, then commits to the commanded tile —
/// it must NOT silently revert to ore). The bug: `seek_more_or_unload` resumed
/// the old cargo_kind after the forced dump, ignoring the player's order.
#[test]
fn ore_laden_harvester_obeys_a_stone_order() {
    use ironvein_sim::map::Terrain;
    use ironvein_sim::stats::Kind;
    use ironvein_sim::world::Mode;
    use ironvein_sim::{Tp, FX};
    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let harv = w
        .ents
        .iter()
        .find(|e| e.owner == 0 && e.kind == Kind::Harvester)
        .map(|e| e.id)
        .expect("a starter harvester");
    let hpos = w.ents.get(harv).unwrap().tile();

    // plant a finished refinery next to it so it has somewhere to offload
    let rt = Tp::new(hpos.x + 4, hpos.y);
    for dy in -1..4 {
        for dx in -1..4 {
            let t = Tp::new(rt.x + dx, rt.y + dy);
            w.map.set_ore(t, 0);
            w.map.set_terrain(t, Terrain::Dirt);
        }
    }
    let r = w.ents.spawn(0, Kind::Refinery, ironvein_sim::Fp { x: rt.x * FX, y: rt.y * FX });
    w.map.stamp_block(rt, ironvein_sim::stats::stats(Kind::Refinery).footprint, r.idx + 1);

    // the harvester is hauling a load of ORE when the player redirects it
    if let Some(e) = w.ents.get_mut(harv) {
        e.cargo = 300;
        e.cargo_kind = 0;
    }
    // nearest rock/mountain (stone) to aim at
    let mut stone = None;
    'outer: for rr in 1i32..80 {
        for dy in -rr..=rr {
            for dx in -rr..=rr {
                if dx.abs() != rr && dy.abs() != rr {
                    continue;
                }
                let t = Tp::new(hpos.x + dx, hpos.y + dy);
                if w.map.resource_kind(t) == Some(2) {
                    stone = Some(t);
                    break 'outer;
                }
            }
        }
    }
    let stone = stone.expect("a rock somewhere on the map");

    let stone0 = w.players[0].stone;
    w.step(&[(0, Command::Harvest { units: vec![harv], tile: stone })]);
    for _ in 0..4000 {
        w.step(&[]);
        if w.players[0].stone > stone0 {
            break;
        }
    }
    assert!(w.players[0].stone > stone0, "the harvester must deliver stone, not revert to ore");
}

/// On landing, the harvesters should be spread across all three resources (some
/// for wood, some for stone, the rest for ore) rather than every truck piling
/// onto the gold.
#[test]
fn landing_harvesters_fan_out_across_resources() {
    use ironvein_sim::stats::Kind;
    use ironvein_sim::world::Mode;
    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let mut kinds = std::collections::BTreeSet::new();
    for e in w.ents.iter().filter(|e| e.owner == 0 && e.kind == Kind::Harvester) {
        if let Some(rk) = e.ore_tile.and_then(|t| w.map.resource_kind(t)) {
            kinds.insert(rk);
        }
    }
    assert!(kinds.contains(&1), "some harvesters head for wood");
    assert!(kinds.contains(&2), "some harvesters head for stone");
    assert!(kinds.contains(&0), "some harvesters head for ore");
}

#[test]
fn harvesters_chop_trees_into_wood_and_open_ground() {
    use ironvein_sim::command::Command;
    use ironvein_sim::map::Terrain;
    use ironvein_sim::stats::Kind;
    use ironvein_sim::world::Mode;
    use ironvein_sim::{mapgen, Tp};
    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let harv = w
        .ents
        .iter()
        .find(|e| e.owner == 0 && e.kind == Kind::Harvester)
        .map(|e| e.id)
        .expect("starter harvester");
    let hpos = w.ents.get(harv).unwrap().tile();
    // nearest tree to the harvester
    let mut tree = None;
    'outer: for r in 1i32..70 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let t = Tp::new(hpos.x + dx, hpos.y + dy);
                if w.map.resource_kind(t) == Some(1) {
                    tree = Some(t);
                    break 'outer;
                }
            }
        }
    }
    let tree = tree.expect("a tree somewhere on the map");
    assert_eq!(w.map.terrain_at(tree), Terrain::Tree);
    w.step(&[(0, Command::Harvest { units: vec![harv], tile: tree })]);
    for _ in 0..3000 {
        w.step(&[]);
        if w.map.terrain_at(tree) != Terrain::Tree {
            break;
        }
    }
    assert_ne!(w.map.terrain_at(tree), Terrain::Tree, "the tree should be chopped down to open ground");
}

/// Essence (the tier-3 currency, save format v7) must be covered by hash() and
/// survive a save/reload round-trip — a missed field is a silent desync.
#[test]
fn essence_is_hashed_and_saved_v7() {
    use ironvein_sim::stats::{essence_cost, Kind};
    use ironvein_sim::world::Mode;
    assert_eq!(essence_cost(Kind::Obelisk), 200);
    assert_eq!(essence_cost(Kind::Champion), 150);
    assert_eq!(essence_cost(Kind::Rifleman), 0);

    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let h0 = w.hash();
    w.players[0].essence = 321;
    assert_ne!(w.hash(), h0, "hash() must cover essence or peers silently desync");

    let bytes = w.save_bytes();
    let w2 = World::load_bytes(&bytes).expect("reload v7 save");
    assert_eq!(w2.players[0].essence, 321, "essence survives save/reload");
    assert_eq!(w2.hash(), w.hash(), "reloaded world is bit-identical");
}

/// Difficulty (save v10): persists through a save, and Hard breeds a denser night
/// than Easy at the same seed/tick.
#[test]
fn difficulty_persists_and_scales_the_horde() {
    use ironvein_sim::world::Mode;
    // round-trip the difficulty byte
    let mut w = mapgen::verdant_divide(5, Mode::Survival);
    w.difficulty = 2;
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let w2 = World::load_bytes(&w.save_bytes()).unwrap();
    assert_eq!(w2.difficulty, 2);
    assert_eq!(w2.hash(), w.hash(), "difficulty is hashed; save is bit-identical");

    // measure the raw night horde at each difficulty (army removed so the count
    // reflects spawn pressure, not how fast a big force mows them down)
    let horde = |d: u8| -> usize {
        let mut w = mapgen::verdant_divide(9, Mode::Survival);
        w.difficulty = d;
        w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
        let units: Vec<_> = w.ents.iter().filter(|e| e.owner == 0 && e.kind.is_unit()).map(|e| e.id).collect();
        for u in units {
            w.ents.despawn(u);
        }
        for _ in 0..1400 {
            w.step(&[]);
        }
        w.ents.iter().filter(|e| e.kind.is_monster()).count()
    };
    let easy = horde(0);
    let hard = horde(2);
    assert!(easy > 0, "even easy spawns a night horde");
    assert!(hard > easy, "hard is denser than easy (hard {hard} vs easy {easy})");
}

/// Survival mode (save byte 2): round-trips through a save, the solo night still
/// swarms, and losing your whole colony eliminates you.
#[test]
fn survival_mode_persists_defeats_and_swarms() {
    use ironvein_sim::world::Mode;
    let mut w = mapgen::verdant_divide(5, Mode::Survival);
    assert_eq!(w.mode, Mode::Survival);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);

    // the new mode survives a save/reload (mode byte = 2) and is hashed
    let w2 = World::load_bytes(&w.save_bytes()).unwrap();
    assert_eq!(w2.mode, Mode::Survival);
    assert_eq!(w2.hash(), w.hash(), "survival save is bit-identical");

    // the night still comes for a lone holdout
    for _ in 0..1300 {
        w.step(&[]);
    }
    assert!(w.ents.iter().any(|e| e.kind.is_monster()), "the survival night spawns a horde");

    // defeat: strip the colony, and one more tick ends the run
    let ids: Vec<_> = w.ents.iter().filter(|e| e.owner == 0).map(|e| e.id).collect();
    for id in ids {
        w.ents.despawn(id);
    }
    w.step(&[]);
    assert!(w.players[0].defeated, "losing everything in survival eliminates you");
}

/// The nuke superweapon: a fully-charged Missile Silo flattens everything inside
/// the blast radius and spares what's outside it, then discharges.
#[test]
fn a_charged_nuke_flattens_the_blast_zone() {
    use ironvein_sim::stats::{Kind, NUKE_CHARGE, NUKE_RADIUS};
    use ironvein_sim::world::Mode;
    use ironvein_sim::{Tp, NEUTRAL};
    let mut w = mapgen::verdant_divide(5, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);

    // a finished, fully-charged silo well away from the target
    let silo = w.ents.spawn(0, Kind::MissileSilo, Tp::new(4, 4).center());
    if let Some(e) = w.ents.get_mut(silo) {
        e.work_t = NUKE_CHARGE;
    }
    // targets: two at ground zero, one safely outside the radius
    let tgt = Tp::new(w.map.w / 2, w.map.h / 2);
    let z0 = w.ents.spawn(NEUTRAL, Kind::Zombie, tgt.center());
    let z1 = w.ents.spawn(NEUTRAL, Kind::Zombie, Tp::new(tgt.x + 1, tgt.y).center());
    let far = w.ents.spawn(NEUTRAL, Kind::Zombie, Tp::new(tgt.x + NUKE_RADIUS + 6, tgt.y).center());

    w.step(&[(0, Command::FireNuke { silo, at: tgt })]);

    assert!(w.ents.get(z0).is_none(), "ground-zero target is vaporized");
    assert!(w.ents.get(z1).is_none(), "an adjacent target is vaporized");
    assert!(w.ents.get(far).is_some(), "a target outside the radius survives");
    assert!(w.ents.get(silo).map(|e| e.work_t).unwrap_or(NUKE_CHARGE) < NUKE_CHARGE, "the silo discharged");
}

/// The night-horde rule: an unarmed grunt can savage units but cannot scratch a
/// building — until a boss walks and arms the dark, after which it batters
/// structures too. (Lore: "only local damage, till they take up the machinery of war".)
#[test]
fn night_grunts_spare_buildings_until_a_boss_arms_them() {
    use ironvein_sim::stats::Kind;
    use ironvein_sim::world::Mode;
    use ironvein_sim::{Tp, NEUTRAL};
    let mut w = mapgen::verdant_divide(5, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    // isolate the Starship so a monster's only candidate target is the building
    let (ship_id, ship_t) = w
        .ents
        .iter()
        .find(|e| e.owner == 0 && e.kind == Kind::Starship)
        .map(|e| (e.id, e.tile()))
        .unwrap();
    let units: Vec<_> = w.ents.iter().filter(|e| e.owner == 0 && e.kind.is_unit()).map(|e| e.id).collect();
    for u in units {
        w.ents.despawn(u);
    }
    // jump to night so the grunt neither burns nor waits for dusk
    w.tick = 1000;
    let hp0 = w.ents.get(ship_id).unwrap().hp;

    // an UNARMED zombie at the ship's doorstep (no boss anywhere)
    let z = w.ents.spawn(NEUTRAL, Kind::Zombie, Tp::new(ship_t.x + 3, ship_t.y + 1).center());
    if let Some(e) = w.ents.get_mut(z) {
        e.aggressive = true;
    }
    for _ in 0..80 {
        w.step(&[]);
    }
    assert_eq!(w.ents.get(ship_id).unwrap().hp, hp0, "an unarmed grunt cannot scratch a building");

    // now a Lich walks: the dark is armed, and the horde turns on the structure
    w.ents.spawn(NEUTRAL, Kind::Lich, Tp::new(ship_t.x + 7, ship_t.y + 7).center());
    for _ in 0..200 {
        w.step(&[]);
    }
    let hp1 = w.ents.get(ship_id).map(|e| e.hp).unwrap_or(0);
    assert!(hp1 < hp0, "once a boss arms them, grunts batter the building (hp {hp1} < {hp0})");
}

/// The capstone (save v8): the animated war-hulk is classified right, and the
/// reckoning's peace window is hashed + survives save/reload.
#[test]
fn reckoning_capstone_is_deterministic_v8() {
    use ironvein_sim::stats::Kind;
    use ironvein_sim::world::Mode;
    // a war-hulk is a monster (hostile to all, drops essence) but only smoulders
    // by day like a boss — it must not flash to ash with the grunts
    assert!(Kind::HellTank.is_monster());
    assert!(Kind::HellTank.smoulders());
    assert!(Kind::Warlock.smoulders());
    assert!(!Kind::Zombie.smoulders());

    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let h0 = w.hash();
    w.peace_until = 99_000;
    assert_ne!(w.hash(), h0, "hash() must cover peace_until or peers silently desync");

    let w2 = World::load_bytes(&w.save_bytes()).expect("reload v8 save");
    assert_eq!(w2.peace_until, 99_000, "peace_until survives save/reload");
    assert_eq!(w2.hash(), w.hash(), "reloaded world is bit-identical");
}

/// Loot motes (save v9): a mote on top of a unit gets vacuumed (essence to the
/// collector), an out-of-reach mote lingers and survives a hashed save/reload.
#[test]
fn essence_motes_are_collected_and_persist_v9() {
    use ironvein_sim::world::{Loot, Mode};
    use ironvein_sim::Tp;
    let mut w = mapgen::verdant_divide(mapgen::POC_SEED, Mode::Skirmish);
    w.step(&[(0, Command::Join { name: "A".into(), color: 0, key: [0; 32] })]);
    let upos = w
        .ents
        .iter()
        .find(|e| e.owner == 0 && e.kind.is_unit() && e.hp > 0)
        .map(|e| e.tile())
        .expect("a starting unit to collect with");
    let before = w.players[0].essence;

    // drop an essence mote on the unit → next tick it's vacuumed to that player
    // (clear any wild berries first so we measure only our mote)
    w.loot.clear();
    w.loot.push(Loot { tile: upos, amount: 40, kind: ironvein_sim::world::LOOT_ESSENCE, born: w.tick });
    w.step(&[]);
    assert_eq!(w.players[0].essence, before + 40, "the collector banks the essence");

    // a mote off in the far corner lingers, and round-trips through a save
    w.loot.clear();
    w.loot.push(Loot { tile: Tp::new(1, 1), amount: 7, kind: ironvein_sim::world::LOOT_ESSENCE, born: w.tick });
    let h0 = w.hash();
    let w2 = World::load_bytes(&w.save_bytes()).expect("reload save");
    assert_eq!(w2.loot.len(), 1, "the uncollected mote persists");
    assert_eq!(w2.loot[0].amount, 7);
    assert_eq!(w2.hash(), h0, "hash() covers loot; the reload is bit-identical");
}
