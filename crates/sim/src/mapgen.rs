//! mapgen.rs — the proof-of-concept level: VERDANT DIVIDE.
//!
//! 128x128 tiles. A wandering river splits the valley north-to-south, crossed by
//! two road bridges. Six ore fields (each with a regenerating node) anchor the
//! economy. A neutral village sits mid-east — capture the huts with engineers
//! and they're yours to live in. Four settlement sites, one per newcomer.

use crate::map::{Map, Terrain};
use crate::rng::Pcg32;
use crate::stats::{stats, Kind, ROCK_STONE, TREE_WOOD};
use crate::world::{Mode, World};
use crate::{Fp, Tp, FX, NEUTRAL};

pub const POC_SEED: u64 = 0x1B0_4E51;

pub fn verdant_divide(seed: u64, mode: Mode) -> World {
    let size = 128;
    let mut m = Map::new(size, size);
    let mut rng = Pcg32::new(seed ^ 0xC0FFEE);

    // --- dirt patches for texture ---
    for _ in 0..50 {
        blob(&mut m, &mut rng, Terrain::Dirt, 2, 5);
    }

    // --- biome regions: broad snowfields and deserts recolour the ground ---
    for _ in 0..5 {
        biome_blob(&mut m, &mut rng, Terrain::Snow, 9, 16);
    }
    for _ in 0..5 {
        biome_blob(&mut m, &mut rng, Terrain::Sand, 9, 16);
    }

    // --- rocky highlands hugging the corners (some peaks are tall mountains) ---
    for &(cx, cy) in &[(6, 6), (size - 7, 6), (6, size - 7), (size - 7, size - 7)] {
        for _ in 0..3 {
            let mut x = cx + rng.range_i32(-4, 4);
            let mut y = cy + rng.range_i32(-4, 4);
            for _ in 0..30 {
                let ter = if rng.chance(4, 10) { Terrain::Mountain } else { Terrain::Rock };
                stamp(&mut m, Tp::new(x, y), 1, ter);
                x += rng.range_i32(-1, 1);
                y += rng.range_i32(-1, 1);
            }
        }
    }

    // --- a few short impassable mountain ridges for mid-field cover (never
    //     paved over roads/bridges/water, and short enough not to wall a base) ---
    for _ in 0..3 {
        let mut x = rng.range_i32(30, size - 30);
        let mut y = rng.range_i32(30, size - 30);
        let len = rng.range_i32(6, 12);
        for _ in 0..len {
            ridge(&mut m, Tp::new(x, y));
            if rng.chance(4, 10) {
                ridge(&mut m, Tp::new(x + 1, y));
            }
            x += rng.range_i32(-1, 1);
            y += rng.range_i32(0, 1);
        }
    }

    // --- forests ---
    for _ in 0..34 {
        let cx = rng.range_i32(4, size - 5);
        let cy = rng.range_i32(4, size - 5);
        let r = rng.range_i32(2, 4);
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r && rng.chance(6, 10) {
                    let t = Tp::new(cx + dx, cy + dy);
                    if m.terrain_at(t) == Terrain::Grass || m.terrain_at(t) == Terrain::Dirt {
                        m.set_terrain(t, Terrain::Tree);
                    }
                }
            }
        }
    }

    // --- the river (wandering vertical band) + remember its course ---
    let mut course = vec![0i32; size as usize];
    let mut cx = size / 2;
    for y in 0..size {
        cx += rng.range_i32(-1, 1);
        cx = cx.clamp(size / 2 - 12, size / 2 + 12);
        course[y as usize] = cx;
        for dx in -2..=2 {
            m.set_terrain(Tp::new(cx + dx, y), Terrain::Water);
        }
        // soft banks: clear trees near water
        for dx in -3..=3 {
            let t = Tp::new(cx + dx, y);
            if m.terrain_at(t) == Terrain::Tree && dx.abs() == 3 {
                m.set_terrain(t, Terrain::Grass);
            }
        }
    }

    // --- two bridges with road approaches ---
    for &by in &[34i32, 94] {
        let bx = course[by as usize];
        for y in by..(by + 2) {
            for dx in -8..=8 {
                let t = Tp::new(bx + dx, y);
                let ter = if m.terrain_at(t) == Terrain::Water { Terrain::Bridge } else { Terrain::Road };
                m.set_terrain(t, ter);
            }
        }
        // roads running outward from each bridgehead
        for dx in 9..26 {
            for &sx in &[-1i32, 1] {
                let t = Tp::new(bx + sx * dx, by);
                if m.terrain_at(t) != Terrain::Water {
                    m.set_terrain(t, Terrain::Road);
                }
            }
        }
    }

    // --- marshy fringes hugging the river (slow, unbuildable mud) ---
    for y in 0..size {
        let bx = course[y as usize];
        for dx in -5..=5 {
            let t = Tp::new(bx + dx, y);
            if (dx.abs() == 3 || dx.abs() == 4) && m.terrain_at(t) == Terrain::Grass && rng.chance(4, 10) {
                m.set_terrain(t, Terrain::Marsh);
            }
        }
    }

    // --- where the river runs through snow it freezes into crossable ice ---
    for y in 0..size {
        let bx = course[y as usize];
        let snowy = m.terrain_at(Tp::new(bx - 5, y)) == Terrain::Snow || m.terrain_at(Tp::new(bx + 5, y)) == Terrain::Snow;
        if snowy {
            for dx in -2..=2 {
                let t = Tp::new(bx + dx, y);
                if m.terrain_at(t) == Terrain::Water {
                    m.set_terrain(t, Terrain::Ice);
                }
            }
        }
    }

    // --- ore fields: 6 fields, each with a regenerating node at its heart ---
    let field_anchors = [
        (24, 24),
        (size - 25, 24),
        (24, size - 25),
        (size - 25, size - 25),
        (size / 2 - 18, size / 2),
        (size / 2 + 18, size / 2),
    ];
    for &(ax, ay) in &field_anchors {
        let cx = ax + rng.range_i32(-3, 3);
        let cy = ay + rng.range_i32(-3, 3);
        let c = Tp::new(cx, cy);
        for dy in -4..=4 {
            for dx in -4..=4 {
                let d2 = dx * dx + dy * dy;
                if d2 > 16 {
                    continue;
                }
                let t = Tp::new(cx + dx, cy + dy);
                if m.terrain_at(t).ground() {
                    let amt = (520 - d2 * 28 + rng.range_i32(-60, 60)).clamp(80, 650) as u16;
                    m.set_ore(t, amt);
                }
            }
        }
        if m.terrain_at(c).ground() {
            m.nodes.push(c);
        }
    }

    // --- settlement sites: clear ground, guaranteed room for a 3x3 yard ---
    let sites = [
        Tp::new(14, 14),
        Tp::new(size - 18, 14),
        Tp::new(14, size - 18),
        Tp::new(size - 18, size - 18),
    ];
    for s in sites.iter() {
        for dy in -3..7 {
            for dx in -3..7 {
                let t = Tp::new(s.x + dx, s.y + dy);
                // clear anything you can't build on (mountains, marsh, water…)
                // but keep snow/sand so a base can sit in its biome
                if !m.terrain_at(t).buildable() {
                    m.set_terrain(t, Terrain::Grass);
                }
                m.set_ore(t, 0);
            }
        }
        m.spawns.push(*s);
        m.spawn_used.push(NEUTRAL);
    }

    // --- timber & stone: a tree holds wood, rock/mountain holds stone (the
    //     resource AMOUNT rides the ore layer; its TYPE is read from terrain) ---
    for y in 0..size {
        for x in 0..size {
            let t = Tp::new(x, y);
            match m.terrain_at(t) {
                Terrain::Tree => m.set_ore(t, TREE_WOOD),
                Terrain::Rock | Terrain::Mountain => m.set_ore(t, ROCK_STONE),
                _ => {}
            }
        }
    }

    let mut world = World::new(m, seed, mode);

    // --- the neutral village (capture the huts; move in) ---
    let vx = size / 2 + 22;
    let vy = size / 2 - 4;
    let village: [(i32, i32, Kind); 6] = [
        (0, 0, Kind::House),
        (3, 1, Kind::House),
        (0, 4, Kind::House),
        (4, 4, Kind::House),
        (7, 2, Kind::Farm),
        (-3, 2, Kind::Farm),
    ];
    for &(dx, dy, kind) in village.iter() {
        let at = Tp::new(vx + dx, vy + dy);
        let (fw, fh) = stats(kind).footprint;
        // flatten the lot
        let mut ok = true;
        for yy in 0..fh {
            for xx in 0..fw {
                let t = Tp::new(at.x + xx, at.y + yy);
                if !world.map.in_bounds(t) || world.map.terrain_at(t) == Terrain::Water {
                    ok = false;
                }
            }
        }
        if !ok {
            continue;
        }
        for yy in 0..fh {
            for xx in 0..fw {
                let t = Tp::new(at.x + xx, at.y + yy);
                world.map.set_terrain(t, Terrain::Dirt);
                world.map.set_ore(t, 0);
            }
        }
        let id = world.ents.spawn(NEUTRAL, kind, Fp { x: at.x * FX, y: at.y * FX });
        world.map.stamp_block(at, (fw, fh), id.idx + 1);
    }

    world
}

/// THE NETHEREALM — the map you descend into when you march a force through the
/// Warlock's rift. Returns just the `Map` (the descent keeps the existing World,
/// players and surviving army, and only swaps the ground beneath them). Built
/// from the passed world RNG, so every peer descends into an identical hell.
/// Ash plains cut by lava, obsidian spires for stone, charred groves for wood,
/// ember-ore for credits, four landing sites — and no kind sun overhead.
pub fn nether_realm(rng: &mut Pcg32) -> Map {
    let size = 128;
    let mut m = Map::new(size, size);
    // base: the whole realm is ash
    for y in 0..size {
        for x in 0..size {
            m.set_terrain(Tp::new(x, y), Terrain::Ash);
        }
    }
    // charred dirt for texture
    for _ in 0..60 {
        let (cx, cy, r) = (rng.range_i32(2, size - 3), rng.range_i32(2, size - 3), rng.range_i32(2, 5));
        for dy in -r..=r {
            for dx in -r..=r {
                let t = Tp::new(cx + dx, cy + dy);
                if dx * dx + dy * dy <= r * r && m.terrain_at(t) == Terrain::Ash {
                    m.set_terrain(t, Terrain::Dirt);
                }
            }
        }
    }
    // lava lakes
    for _ in 0..11 {
        let (cx, cy, r) = (rng.range_i32(8, size - 9), rng.range_i32(8, size - 9), rng.range_i32(4, 9));
        for dy in -r..=r {
            for dx in -r..=r {
                let t = Tp::new(cx + dx, cy + dy);
                if dx * dx + dy * dy <= r * r && matches!(m.terrain_at(t), Terrain::Ash | Terrain::Dirt) {
                    m.set_terrain(t, Terrain::Lava);
                }
            }
        }
    }
    // a winding lava river
    let mut cx = size / 2;
    for y in 0..size {
        cx = (cx + rng.range_i32(-1, 1)).clamp(size / 2 - 14, size / 2 + 14);
        for dx in -2..=2 {
            let t = Tp::new(cx + dx, y);
            if matches!(m.terrain_at(t), Terrain::Ash | Terrain::Dirt) {
                m.set_terrain(t, Terrain::Lava);
            }
        }
    }
    // obsidian spires (the stone source), clustered like rocky highlands
    for _ in 0..14 {
        let (mut x, mut y) = (rng.range_i32(6, size - 7), rng.range_i32(6, size - 7));
        for _ in 0..rng.range_i32(8, 18) {
            let t = Tp::new(x, y);
            if m.terrain_at(t) == Terrain::Ash {
                m.set_terrain(t, Terrain::Obsidian);
            }
            x += rng.range_i32(-1, 1);
            y += rng.range_i32(-1, 1);
        }
    }
    // charred groves (wood) — petrified forests on the ash
    for _ in 0..16 {
        let (cx, cy, r) = (rng.range_i32(4, size - 5), rng.range_i32(4, size - 5), rng.range_i32(2, 4));
        for dy in -r..=r {
            for dx in -r..=r {
                let t = Tp::new(cx + dx, cy + dy);
                if dx * dx + dy * dy <= r * r && rng.chance(6, 10) && m.terrain_at(t) == Terrain::Ash {
                    m.set_terrain(t, Terrain::Tree);
                }
            }
        }
    }
    // ember-ore fields (credits) + regrowth nodes
    let anchors = [(28, 28), (size - 29, 28), (28, size - 29), (size - 29, size - 29), (size / 2, size / 2 - 20), (size / 2, size / 2 + 20)];
    for &(ax, ay) in &anchors {
        let (cx, cy) = (ax + rng.range_i32(-3, 3), ay + rng.range_i32(-3, 3));
        let c = Tp::new(cx, cy);
        for dy in -4..=4 {
            for dx in -4..=4 {
                let d2 = dx * dx + dy * dy;
                let t = Tp::new(cx + dx, cy + dy);
                if d2 <= 16 && m.terrain_at(t).ground() {
                    m.set_ore(t, (520 - d2 * 28 + rng.range_i32(-60, 60)).clamp(80, 650) as u16);
                }
            }
        }
        if m.terrain_at(c).ground() {
            m.nodes.push(c);
        }
    }
    // landing sites: clear ash, guaranteed room for a 3x3 yard
    for s in [Tp::new(16, 16), Tp::new(size - 20, 16), Tp::new(16, size - 20), Tp::new(size - 20, size - 20)].iter() {
        for dy in -3..7 {
            for dx in -3..7 {
                let t = Tp::new(s.x + dx, s.y + dy);
                if !m.terrain_at(t).buildable() {
                    m.set_terrain(t, Terrain::Ash);
                }
                m.set_ore(t, 0);
            }
        }
        m.spawns.push(*s);
        m.spawn_used.push(NEUTRAL);
    }
    // resource AMOUNT rides the ore layer; TYPE is read from terrain (obsidian → stone,
    // charred tree → wood), same convention as the overworld.
    for y in 0..size {
        for x in 0..size {
            let t = Tp::new(x, y);
            match m.terrain_at(t) {
                Terrain::Tree => m.set_ore(t, TREE_WOOD),
                Terrain::Obsidian => m.set_ore(t, ROCK_STONE),
                _ => {}
            }
        }
    }
    m
}

fn blob(m: &mut Map, rng: &mut Pcg32, ter: Terrain, rmin: i32, rmax: i32) {
    let cx = rng.range_i32(2, m.w - 3);
    let cy = rng.range_i32(2, m.h - 3);
    let r = rng.range_i32(rmin, rmax);
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                let t = Tp::new(cx + dx, cy + dy);
                if m.terrain_at(t) == Terrain::Grass {
                    m.set_terrain(t, ter);
                }
            }
        }
    }
}

/// A large soft-edged region that recolours open ground (grass/dirt) into a
/// biome terrain — leaves water/roads/forest alone. Edges fade via a chance
/// falloff so biomes blend rather than draw hard circles.
fn biome_blob(m: &mut Map, rng: &mut Pcg32, ter: Terrain, rmin: i32, rmax: i32) {
    let cx = rng.range_i32(6, m.w - 7);
    let cy = rng.range_i32(6, m.h - 7);
    let r = rng.range_i32(rmin, rmax);
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r * r {
                continue;
            }
            // soft edge: tiles near the rim only sometimes convert
            let edge = d2 * 10 / (r * r + 1);
            if edge >= 7 && !rng.chance((7 - (edge - 7).min(6)) as u32, 8) {
                continue;
            }
            let t = Tp::new(cx + dx, cy + dy);
            if matches!(m.terrain_at(t), Terrain::Grass | Terrain::Dirt) {
                m.set_terrain(t, ter);
            }
        }
    }
}

/// Stamp an impassable mountain tile, but never pave over a road, bridge, or
/// the river — so crossings and arteries stay intact.
fn ridge(m: &mut Map, c: Tp) {
    if !m.in_bounds(c) {
        return;
    }
    if matches!(m.terrain_at(c), Terrain::Road | Terrain::Bridge | Terrain::Water | Terrain::Ice) {
        return;
    }
    m.set_terrain(c, Terrain::Mountain);
}

fn stamp(m: &mut Map, c: Tp, r: i32, ter: Terrain) {
    for dy in -r..=r {
        for dx in -r..=r {
            m.set_terrain(Tp::new(c.x + dx, c.y + dy), ter);
        }
    }
}
