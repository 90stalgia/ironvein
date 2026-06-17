//! gfx.rs — procedural isometric renderer (2:1 dimetric, 64x32 tiles).
//!
//! The world is simulated top-down on a square grid; this projects it into a
//! classic 2.5D isometric view (AoE / Tiberian-Sun era): ground is a field of
//! shaded diamonds, buildings are extruded boxes with a lit roof and two
//! shaded wall faces, units are billboards standing on their tile with a cast
//! shadow. Everything is drawn back-to-front (painter's order) so nearer
//! things occlude farther ones. Still 100% procedural, no assets.

use ironvein_sim::stats::{stats, Kind};
use ironvein_sim::world::{VisEvent, World};
use ironvein_sim::{tile_noise, Fp, Terrain, Tp, FX};
use macroquad::prelude::*;

/// Iso tile footprint on screen: 64 wide, 32 tall (2:1).
pub const TW: f32 = 64.0;
pub const TH: f32 = 32.0;
const HW: f32 = TW / 2.0;
const HH: f32 = TH / 2.0;

pub const PLAYER_COLORS: [Color; 8] = [
    Color::new(0.85, 0.18, 0.15, 1.0),
    Color::new(0.20, 0.45, 0.95, 1.0),
    Color::new(0.95, 0.75, 0.10, 1.0),
    Color::new(0.15, 0.75, 0.35, 1.0),
    Color::new(0.80, 0.30, 0.85, 1.0),
    Color::new(0.95, 0.50, 0.10, 1.0),
    Color::new(0.15, 0.80, 0.80, 1.0),
    Color::new(0.65, 0.65, 0.70, 1.0),
];

pub fn player_color(world: &World, pid: u8) -> Color {
    if pid == ironvein_sim::NEUTRAL {
        return Color::new(0.62, 0.58, 0.45, 1.0);
    }
    let idx = world.players.get(pid as usize).map(|p| p.color as usize).unwrap_or(7) % 8;
    PLAYER_COLORS[idx]
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// World fixed-point position -> isometric screen position (pre-camera).
pub fn fpx(p: Fp) -> Vec2 {
    let fx = p.x as f32 / FX as f32;
    let fy = p.y as f32 / FX as f32;
    vec2((fx - fy) * HW, (fx + fy) * HH)
}

/// Fractional tile coords -> isometric screen position (pre-camera).
pub fn tile_to_screen(tx: f32, ty: f32) -> Vec2 {
    vec2((tx - ty) * HW, (tx + ty) * HH)
}

/// Inverse projection: an iso-screen point (camera already added back) ->
/// fractional tile coordinates. Used for mouse picking and placement.
pub fn screen_to_tilef(s: Vec2) -> Vec2 {
    vec2((s.x / HW + s.y / HH) * 0.5, (s.y / HH - s.x / HW) * 0.5)
}

/// The iso-screen bounding box of a w x h tile map: (min, max).
pub fn world_bounds(w: i32, h: i32) -> (Vec2, Vec2) {
    let (wf, hf) = (w as f32, h as f32);
    let min = vec2(-hf * HW, 0.0);
    let max = vec2(wf * HW, (wf + hf) * HH);
    (min, max)
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

fn shade(c: Color, f: f32) -> Color {
    Color::new((c.r * f).min(1.0), (c.g * f).min(1.0), (c.b * f).min(1.0), c.a)
}
fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::new(a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t, a.a + (b.a - a.a) * t)
}
fn rgb(r: f32, g: f32, b: f32) -> Color {
    Color::new(r, g, b, 1.0)
}

/// The canonical colour for a harvestable resource kind (0 ore→credits, 1 wood,
/// 2 stone). Shared by tile highlights, harvester cargo bars, and the sidebar so
/// the player learns one consistent colour language.
pub fn resource_color(kind: u8) -> Color {
    match kind {
        1 => rgb(0.76, 0.58, 0.34), // wood — timber brown
        2 => rgb(0.74, 0.76, 0.80), // stone — slate grey
        _ => rgb(0.96, 0.81, 0.25), // ore → credits — gold
    }
}

/// Human label for a resource kind, for tooltips/legends.
pub fn resource_name(kind: u8) -> &'static str {
    match kind {
        1 => "Wood",
        2 => "Stone",
        _ => "Credits",
    }
}

fn diamond(c: Vec2, col: Color) {
    let n = vec2(c.x, c.y - HH);
    let e = vec2(c.x + HW, c.y);
    let s = vec2(c.x, c.y + HH);
    let w = vec2(c.x - HW, c.y);
    draw_triangle(n, e, s, col);
    draw_triangle(n, s, w, col);
}

fn quad(a: Vec2, b: Vec2, c: Vec2, d: Vec2, col: Color) {
    draw_triangle(a, b, c, col);
    draw_triangle(a, c, d, col);
}

fn smoke(x: f32, y: f32, seed: u32, tick: u32, warm: bool) {
    let period = 90u32;
    for k in 0..4u32 {
        let phase = ((tick + seed + k * 24) % period) as f32 / period as f32;
        let rise = phase * 22.0;
        let r = 1.8 + phase * 4.2;
        let a = (1.0 - phase) * 0.38;
        let sway = (tick as f32 * 0.05 + seed as f32 + k as f32).sin() * (1.5 + phase * 3.0);
        let col = if warm { Color::new(0.42, 0.40, 0.38, a) } else { Color::new(0.82, 0.84, 0.9, a) };
        draw_circle(x + sway, y - rise, r, col);
        draw_circle(x + sway - r * 0.3, y - rise - r * 0.3, r * 0.5, Color::new(col.r + 0.1, col.g + 0.1, col.b + 0.1, a * 0.7));
    }
}

/// A soft additive-ish glow halo (a few stacked translucent discs). Reads as
/// emissive light around bright things — the cheap path to a "bloom" look.
fn glow(c: Vec2, r: f32, col: Color, strength: f32) {
    draw_circle(c.x, c.y, r, Color::new(col.r, col.g, col.b, 0.10 * strength));
    draw_circle(c.x, c.y, r * 0.62, Color::new(col.r, col.g, col.b, 0.16 * strength));
    draw_circle(c.x, c.y, r * 0.32, Color::new(col.r, col.g, col.b, 0.30 * strength));
}

// ---------------------------------------------------------------------------
// Value noise for terrain variation
// ---------------------------------------------------------------------------

fn hash01(x: i32, y: i32, salt: u32) -> f32 {
    (tile_noise(x, y, salt) & 0xffff) as f32 / 65535.0
}
fn vnoise(fx: f32, fy: f32, salt: u32) -> f32 {
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let a = hash01(x0, y0, salt);
    let b = hash01(x0 + 1, y0, salt);
    let c = hash01(x0, y0 + 1, salt);
    let d = hash01(x0 + 1, y0 + 1, salt);
    let ab = a + (b - a) * sx;
    let cd = c + (d - c) * sx;
    ab + (cd - ab) * sy
}

fn terrain_tint(w: &World, t: Tp) -> Color {
    let v = vnoise(t.x as f32 * 0.28, t.y as f32 * 0.28, 1) - 0.5;
    match w.map.terrain_at(t) {
        Terrain::Grass => mix(rgb(0.18, 0.42, 0.16), rgb(0.11, 0.31, 0.12), 0.5 + v),
        Terrain::Dirt => mix(rgb(0.48, 0.38, 0.22), rgb(0.37, 0.29, 0.17), 0.5 + v),
        Terrain::Road => mix(rgb(0.41, 0.39, 0.35), rgb(0.33, 0.31, 0.28), 0.5 + v),
        Terrain::Water => rgb(0.08, 0.22, 0.42),
        Terrain::Bridge => rgb(0.46, 0.32, 0.17),
        Terrain::Rock => mix(rgb(0.48, 0.48, 0.52), rgb(0.35, 0.35, 0.39), 0.5 + v),
        Terrain::Tree => mix(rgb(0.16, 0.36, 0.15), rgb(0.11, 0.27, 0.11), 0.5 + v),
        Terrain::Sand => mix(rgb(0.80, 0.72, 0.46), rgb(0.69, 0.61, 0.37), 0.5 + v),
        Terrain::Snow => mix(rgb(0.87, 0.90, 0.95), rgb(0.75, 0.81, 0.89), 0.5 + v),
        Terrain::Ice => mix(rgb(0.66, 0.82, 0.90), rgb(0.55, 0.74, 0.86), 0.5 + v),
        Terrain::Marsh => mix(rgb(0.30, 0.35, 0.22), rgb(0.21, 0.26, 0.15), 0.5 + v),
        Terrain::Mountain => mix(rgb(0.42, 0.42, 0.47), rgb(0.28, 0.28, 0.33), 0.5 + v),
    }
}

// ---------------------------------------------------------------------------
// Terrain — a field of shaded diamonds, drawn back-to-front
// ---------------------------------------------------------------------------

pub fn draw_terrain(w: &World, cam: Vec2, view: Vec2, tick: u32) {
    // Determine the visible tile range by inverting the four view corners,
    // with generous margin for tall features (trees) poking in from off-screen.
    let corners = [cam, cam + vec2(view.x, 0.0), cam + vec2(0.0, view.y), cam + view];
    let mut lo = vec2(f32::MAX, f32::MAX);
    let mut hi = vec2(f32::MIN, f32::MIN);
    for c in corners {
        let t = screen_to_tilef(c);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    let x0 = (lo.x.floor() as i32 - 3).max(0);
    let y0 = (lo.y.floor() as i32 - 3).max(0);
    let x1 = (hi.x.ceil() as i32 + 5).min(w.map.w);
    let y1 = (hi.y.ceil() as i32 + 8).min(w.map.h);

    // row-major == valid painter's order for this projection
    for ty in y0..y1 {
        for tx in x0..x1 {
            draw_tile(w, Tp::new(tx, ty), cam, tick);
        }
    }
}

/// Draw the "being serviced" feedback: welding sparks over vehicles at a Repair
/// Depot, a green heal-cross over infantry at a Med Bay / House, and a working
/// glow on the active support buildings. Makes repair/healing unmistakable.
pub fn draw_service_fx(w: &World, my_pid: u8, cam: Vec2, tick: u32) {
    use ironvein_sim::FX;
    let (mut depots, mut bays, mut houses): (Vec<Fp>, Vec<Fp>, Vec<Fp>) = (Vec::new(), Vec::new(), Vec::new());
    for e in w.ents.iter() {
        if e.owner != my_pid || !e.done {
            continue;
        }
        match e.kind {
            Kind::RepairDepot => depots.push(e.center()),
            Kind::MedBay => bays.push(e.center()),
            Kind::House => houses.push(e.center()),
            _ => {}
        }
    }
    if depots.is_empty() && bays.is_empty() && houses.is_empty() {
        return;
    }
    let dr2 = (4 * FX as i64).pow(2);
    let br2 = (5 * FX as i64).pow(2);
    let near = |list: &[Fp], pos: Fp, r2: i64| list.iter().find(|c| pos.dist2(**c) <= r2).copied();
    let pulse = 0.5 + 0.5 * (tick as f32 * 0.22).sin();
    let mut active: Vec<Fp> = Vec::new();
    for e in w.ents.iter() {
        if e.owner != my_pid || !e.kind.is_unit() || e.hp <= 0 {
            continue;
        }
        let maxhp = stats(e.kind).max_hp;
        if e.hp >= maxhp {
            continue;
        }
        let c = fpx(e.pos) - cam - vec2(0.0, 10.0);
        if e.kind.is_infantry() {
            if let Some(b) = near(&bays, e.pos, br2).or_else(|| near(&houses, e.pos, dr2)) {
                active.push(b);
                glow(c, 9.0, rgb(0.3, 1.0, 0.4), 0.45 * pulse);
                draw_rectangle(c.x - 1.5, c.y - 5.0, 3.0, 10.0, rgb(0.45, 1.0, 0.55)); // heal cross
                draw_rectangle(c.x - 5.0, c.y - 1.5, 10.0, 3.0, rgb(0.45, 1.0, 0.55));
            }
        } else if let Some(b) = near(&depots, e.pos, dr2) {
            active.push(b);
            glow(c, 11.0, rgb(1.0, 0.6, 0.2), 0.4 + 0.4 * pulse);
            for s in 0..4 {
                let a = tick as f32 * 0.5 + s as f32 * 1.7 + e.id.idx as f32;
                let r = 3.5 + s as f32 * 1.6;
                draw_circle(c.x + a.cos() * r, c.y + a.sin() * r * 0.6, 1.2, rgb(1.0, 0.92, 0.55)); // sparks
            }
            draw_circle(c.x, c.y, 2.0, rgb(1.0, 0.8, 0.3)); // weld arc
        }
    }
    for b in active {
        let p = fpx(b) - cam;
        glow(p, 15.0, rgb(0.95, 0.75, 0.3), 0.3 * pulse);
    }
}

/// Highlight every visible, non-depleted resource tile, colour-coded by what it
/// yields (gold ore→credits, brown wood, grey stone). Called while a harvester is
/// selected so "where do I mine X?" is answered at a glance.
pub fn draw_resource_hints(w: &World, my_pid: u8, cam: Vec2, view: Vec2, tick: u32) {
    let corners = [cam, cam + vec2(view.x, 0.0), cam + vec2(0.0, view.y), cam + view];
    let mut lo = vec2(f32::MAX, f32::MAX);
    let mut hi = vec2(f32::MIN, f32::MIN);
    for c in corners {
        let t = screen_to_tilef(c);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    let x0 = (lo.x.floor() as i32 - 1).max(0);
    let y0 = (lo.y.floor() as i32 - 1).max(0);
    let x1 = (hi.x.ceil() as i32 + 2).min(w.map.w);
    let y1 = (hi.y.ceil() as i32 + 4).min(w.map.h);
    let pulse = 0.45 + 0.35 * (tick as f32 * 0.12).sin();
    for ty in y0..y1 {
        for tx in x0..x1 {
            let t = Tp::new(tx, ty);
            let Some(rk) = w.map.resource_kind(t) else { continue };
            if w.map.ore_at(t) == 0 || tile_visible(w, my_pid, t) == 0 {
                continue; // depleted or never-seen
            }
            let c = tile_to_screen(tx as f32 + 0.5, ty as f32 + 0.5) - cam;
            let base = resource_color(rk);
            let col = Color::new(base.r, base.g, base.b, 0.30 + 0.35 * pulse);
            // a diamond outline hugging the tile + a bright centre pip
            let n = vec2(c.x, c.y - HH * 0.92);
            let e = vec2(c.x + HW * 0.92, c.y);
            let s = vec2(c.x, c.y + HH * 0.92);
            let wv = vec2(c.x - HW * 0.92, c.y);
            draw_line(n.x, n.y, e.x, e.y, 1.6, col);
            draw_line(e.x, e.y, s.x, s.y, 1.6, col);
            draw_line(s.x, s.y, wv.x, wv.y, 1.6, col);
            draw_line(wv.x, wv.y, n.x, n.y, 1.6, col);
            draw_circle(c.x, c.y - 2.0, 2.2, Color::new(base.r, base.g, base.b, 0.5 + 0.4 * pulse));
        }
    }
}

fn draw_tile(w: &World, t: Tp, cam: Vec2, tick: u32) {
    let c = tile_to_screen(t.x as f32 + 0.5, t.y as f32 + 0.5) - cam;
    let base = terrain_tint(w, t);
    let terr = w.map.terrain_at(t);

    match terr {
        Terrain::Water => {
            draw_water(c, t, base, tick);
            shoreline(w, t, c);
        }
        _ => {
            // shaded ground diamond + edge bevel
            diamond(c, base);
            ground_texture(c, t, base, terr);
            tile_edges(w, t, c);
            if matches!(terr, Terrain::Tree) {
                draw_tree(c, t, base);
            } else if matches!(terr, Terrain::Rock) {
                draw_rock(c, t, base);
            } else if matches!(terr, Terrain::Mountain) {
                // a mountain reads as a taller rock with a snow cap
                draw_rock(c, t, shade(base, 0.9));
                draw_rock(c - vec2(0.0, 6.0), t, shade(base, 1.15));
                draw_triangle(c + vec2(-3.0, -8.0), c + vec2(3.0, -8.0), c + vec2(0.0, -14.0), rgb(0.9, 0.93, 0.97));
            } else if matches!(terr, Terrain::Ice) {
                // a couple of glints to read as slick ice
                if tile_noise(t.x, t.y, 7) % 3 == 0 {
                    draw_line(c.x - 5.0, c.y - 1.0, c.x + 2.0, c.y + 3.0, 1.0, Color::new(0.95, 0.98, 1.0, 0.7));
                }
            }
        }
    }
    // ore crystals only on open ground — a tree/rock's "ore" is its wood/stone,
    // shown by the tree/rock sprite itself
    let ore = w.map.ore_at(t);
    if ore > 0 && w.map.terrain_at(t).ground() {
        draw_ore(c, t, ore, tick);
    }
}

/// Scatter a little texture + the odd detail mote inside a diamond.
fn ground_texture(c: Vec2, t: Tp, base: Color, terr: Terrain) {
    for s in 0..6 {
        let h = tile_noise(t.x, t.y, 30 + s);
        // random point inside the diamond: barycentric-ish along the axes
        let u = (h & 31) as f32 / 31.0 - 0.5;
        let v = ((h >> 5) & 31) as f32 / 31.0 - 0.5;
        if u.abs() + v.abs() > 0.5 {
            continue;
        }
        let px = c.x + (u - v) * HW;
        let py = c.y + (u + v) * HH;
        let up = (h >> 11) & 1 == 0;
        draw_rectangle(px, py, 1.5, 1.5, shade(base, if up { 1.18 } else { 0.82 }));
    }
    if matches!(terr, Terrain::Grass) && tile_noise(t.x, t.y, 9) % 4 == 0 {
        let bx = c.x + ((tile_noise(t.x, t.y, 11) % 14) as f32 - 7.0);
        let by = c.y + ((tile_noise(t.x, t.y, 12) % 8) as f32 - 4.0);
        draw_line(bx, by + 3.0, bx - 1.5, by - 2.0, 1.0, rgb(0.10, 0.28, 0.11));
        draw_line(bx + 1.0, by + 3.0, bx + 1.5, by - 3.0, 1.0, rgb(0.24, 0.50, 0.18));
        draw_line(bx + 2.5, by + 3.0, bx + 3.5, by - 1.0, 1.0, rgb(0.24, 0.50, 0.18));
    }
    if matches!(terr, Terrain::Grass) && tile_noise(t.x, t.y, 21) % 26 == 0 {
        let petal = [rgb(0.95, 0.9, 0.4), rgb(0.9, 0.5, 0.7), rgb(0.85, 0.85, 0.95)][(tile_noise(t.x, t.y, 24) % 3) as usize];
        draw_circle(c.x, c.y, 1.6, petal);
        draw_circle(c.x, c.y, 0.7, rgb(0.95, 0.8, 0.2));
    }
}

/// A thin lit/dark bevel along the diamond's edges where it meets a different
/// terrain class — the iso analogue of a tile seam.
fn tile_edges(w: &World, t: Tp, c: Vec2) {
    // Only accent boundaries between *different* terrain classes (a soft
    // darkening on the seam), so open fields stay seamless — no grid.
    let here = w.map.terrain_at(t);
    let class = |k: Terrain| match k {
        Terrain::Grass | Terrain::Tree => 0,
        Terrain::Road => 1,
        Terrain::Dirt => 2,
        Terrain::Rock => 3,
        _ => 4,
    };
    let hc = class(here);
    let n = vec2(c.x, c.y - HH);
    let e = vec2(c.x + HW, c.y);
    let s = vec2(c.x, c.y + HH);
    let wv = vec2(c.x - HW, c.y);
    let seam = Color::new(0.0, 0.0, 0.0, 0.12);
    if class(w.map.terrain_at(Tp::new(t.x, t.y - 1))) != hc {
        draw_line(wv.x, wv.y, n.x, n.y, 1.5, seam);
    }
    if class(w.map.terrain_at(Tp::new(t.x + 1, t.y))) != hc {
        draw_line(n.x, n.y, e.x, e.y, 1.5, seam);
    }
    if class(w.map.terrain_at(Tp::new(t.x, t.y + 1))) != hc {
        draw_line(e.x, e.y, s.x, s.y, 1.5, seam);
    }
    if class(w.map.terrain_at(Tp::new(t.x - 1, t.y))) != hc {
        draw_line(s.x, s.y, wv.x, wv.y, 1.5, seam);
    }
}

fn draw_water(c: Vec2, t: Tp, base: Color, tick: u32) {
    diamond(c, base);
    // depth shimmer: two animated highlight scratches
    let tt = tick as f32 * 0.08;
    let w1 = ((tt + t.x as f32 * 0.6 + t.y as f32 * 0.9).sin()) * 6.0;
    draw_line(c.x - 10.0, c.y - 2.0 + w1 * 0.3, c.x + 2.0, c.y - 2.0 + w1 * 0.3, 1.2, Color::new(0.55, 0.78, 0.95, 0.4));
    let w2 = ((tt * 0.7 + t.x as f32 * 1.1).cos()) * 6.0;
    draw_line(c.x - 2.0, c.y + 4.0 + w2 * 0.3, c.x + 9.0, c.y + 4.0 + w2 * 0.3, 1.0, Color::new(0.45, 0.68, 0.88, 0.35));
    if (tile_noise(t.x, t.y, 7) + tick / 16) % 9 == 0 {
        draw_circle(c.x + ((tile_noise(t.x, t.y, 8) % 20) as f32 - 10.0), c.y + ((tile_noise(t.x, t.y, 9) % 10) as f32 - 5.0), 1.0, Color::new(0.9, 0.97, 1.0, 0.8));
    }
}

fn shoreline(w: &World, t: Tp, c: Vec2) {
    let land = |dx: i32, dy: i32| !matches!(w.map.terrain_at(Tp::new(t.x + dx, t.y + dy)), Terrain::Water);
    let foam = Color::new(0.80, 0.90, 0.95, 0.7);
    let sand = Color::new(0.74, 0.66, 0.42, 0.6);
    let n = vec2(c.x, c.y - HH);
    let e = vec2(c.x + HW, c.y);
    let s = vec2(c.x, c.y + HH);
    let wv = vec2(c.x - HW, c.y);
    // each diamond edge borders one neighbour tile
    if land(0, -1) {
        draw_line(wv.x, wv.y, n.x, n.y, 2.5, sand);
        draw_line(wv.x, wv.y, n.x, n.y, 1.0, foam);
    }
    if land(1, 0) {
        draw_line(n.x, n.y, e.x, e.y, 2.5, sand);
        draw_line(n.x, n.y, e.x, e.y, 1.0, foam);
    }
    if land(0, 1) {
        draw_line(e.x, e.y, s.x, s.y, 2.5, sand);
    }
    if land(-1, 0) {
        draw_line(s.x, s.y, wv.x, wv.y, 2.5, sand);
    }
}

fn draw_tree(c: Vec2, t: Tp, ground: Color) {
    let _ = ground;
    let jitter = (tile_noise(t.x, t.y, 3) % 5) as f32 - 2.0;
    let bx = c.x + jitter;
    // cast shadow on the ground, then trunk + a lit spherical canopy rising up
    draw_circle(bx + 6.0, c.y + 3.0, 7.0, Color::new(0.0, 0.0, 0.0, 0.16));
    let trunk_top = c.y - 10.0;
    draw_rectangle(bx - 2.0, trunk_top, 4.0, 14.0, rgb(0.30, 0.19, 0.09));
    draw_rectangle(bx - 2.0, trunk_top, 1.6, 14.0, rgb(0.40, 0.27, 0.14));
    let cy = trunk_top - 8.0;
    let r = 11.0 + (tile_noise(t.x, t.y, 4) % 3) as f32;
    draw_circle(bx, cy, r, rgb(0.06, 0.22, 0.08));
    draw_circle(bx - r * 0.2, cy - r * 0.2, r * 0.8, rgb(0.10, 0.30, 0.12));
    draw_circle(bx - r * 0.35, cy - r * 0.35, r * 0.5, rgb(0.17, 0.42, 0.18));
    draw_circle(bx - r * 0.45, cy - r * 0.45, r * 0.22, rgb(0.26, 0.52, 0.24));
    draw_circle(bx + r * 0.5, cy + r * 0.25, r * 0.4, rgb(0.08, 0.26, 0.10));
}

fn draw_rock(c: Vec2, t: Tp, base: Color) {
    for s in 0..3 {
        let h = tile_noise(t.x, t.y, 40 + s);
        let u = (h & 15) as f32 / 15.0 - 0.5;
        let v = ((h >> 4) & 15) as f32 / 15.0 - 0.5;
        let bx = c.x + (u - v) * HW * 0.7;
        let by = c.y + (u + v) * HH * 0.7;
        let bw = 7.0 + (h % 4) as f32;
        let bh = 5.0 + ((h >> 2) % 3) as f32;
        // a little boulder lump with a lit cap and dark base
        draw_circle(bx, by + bh * 0.3, bw * 0.5, Color::new(0.0, 0.0, 0.0, 0.12));
        draw_rectangle(bx - bw * 0.5, by - bh * 0.5, bw, bh, base);
        draw_rectangle(bx - bw * 0.5, by - bh * 0.5, bw, 1.6, shade(base, 1.3));
        draw_rectangle(bx - bw * 0.5, by + bh * 0.5 - 1.6, bw, 1.6, shade(base, 0.65));
    }
}

fn draw_ore(c: Vec2, t: Tp, ore: u16, tick: u32) {
    // one cheap golden glow disc over the tile, breathing slowly
    let breathe = 0.7 + 0.3 * ((tick as f32 * 0.06) + (t.x + t.y) as f32).sin();
    draw_circle(c.x, c.y, 9.0 + ore as f32 / 120.0, Color::new(0.95, 0.78, 0.25, 0.10 * breathe));
    let crystals = (2 + ore / 110).min(8) as u32;
    for s in 0..crystals {
        let h = tile_noise(t.x, t.y, 20 + s);
        let u = (h & 31) as f32 / 31.0 - 0.5;
        let v = ((h >> 5) & 31) as f32 / 31.0 - 0.5;
        if u.abs() + v.abs() > 0.5 {
            continue;
        }
        let sx = c.x + (u - v) * HW;
        let sy = c.y + (u + v) * HH;
        let g = 1.8;
        draw_triangle(vec2(sx, sy - g), vec2(sx - g, sy + g * 0.6), vec2(sx + g, sy + g * 0.6), rgb(0.55, 0.42, 0.08));
        draw_triangle(vec2(sx, sy - g), vec2(sx - g * 0.5, sy + g * 0.1), vec2(sx + g * 0.2, sy), rgb(0.95, 0.78, 0.2));
        draw_circle(sx - g * 0.2, sy - g * 0.2, 0.8, rgb(1.0, 0.96, 0.7));
        if (h + tick / 12) % 19 == 0 {
            draw_circle(sx, sy - g, 0.9, Color::new(1.0, 1.0, 0.85, 0.9));
        }
    }
}

// ---------------------------------------------------------------------------
// Entities (depth-sorted by the caller)
// ---------------------------------------------------------------------------

fn facing_vec(face: u8) -> Vec2 {
    // project a top-down facing into iso screen space
    let a = face as f32 / 16.0 * std::f32::consts::TAU;
    let (wx, wy) = (a.sin(), -a.cos());
    vec2((wx - wy) * 0.5, (wx + wy) * 0.5).normalize_or_zero()
}

pub fn draw_entity(w: &World, e: &ironvein_sim::Ent, draw_pos: Vec2, cam: Vec2, selected: bool) {
    let col = player_color(w, e.owner);
    if e.kind.is_building() {
        draw_iso_building(w, e, cam, col, selected);
    } else {
        draw_iso_unit(w, e, draw_pos - cam, col, selected);
        // a monster caught in daylight smokes and burns
        if e.kind.is_monster() && !ironvein_sim::world::is_night(w.tick) {
            let g = draw_pos - cam;
            for k in 0..3u32 {
                let fx = g.x + ((e.id.idx.wrapping_add(k * 5) % 7) as f32 - 3.0);
                let fy = g.y - 6.0 - k as f32 * 3.0;
                draw_circle(fx, fy, 2.6 - k as f32 * 0.5, Color::new(1.0, 0.45 + 0.12 * k as f32, 0.10, 0.85));
            }
            smoke(g.x, g.y - 13.0, e.id.idx, w.tick, true);
        }
    }
}

/// height in px a building of this footprint rises off the ground
pub fn bld_height(fw: i32, fh: i32) -> f32 {
    16.0 + 7.0 * fw.max(fh) as f32
}

/// Extrude a base diamond (corners n,e,s,w) up by `h`; draw the two visible
/// walls + lit roof; return the roof corners [n,e,s,w].
fn iso_box(n: Vec2, e: Vec2, s: Vec2, w: Vec2, h: f32, body: Color) -> [Vec2; 4] {
    let up = vec2(0.0, -h);
    quad(w, s, s + up, w + up, shade(body, 0.74)); // left wall (faces SW)
    quad(s, e, e + up, s + up, shade(body, 0.54)); // right wall (faces SE)
    let r = [n + up, e + up, s + up, w + up];
    quad(r[0], r[1], r[2], r[3], shade(body, 1.18)); // roof
    quad_outline(r[0], r[1], r[2], r[3], Color::new(0.08, 0.08, 0.1, 0.85));
    draw_line(s.x, s.y, s.x, s.y - h, 1.0, Color::new(0.08, 0.08, 0.1, 0.7));
    r
}

/// A panel on a wall (door/window/stripe) between base-edge points a..b,
/// spanning screen heights `lo`..`hi`.
fn wall_panel(a: Vec2, b: Vec2, lo: f32, hi: f32, col: Color) {
    quad(a + vec2(0.0, -lo), b + vec2(0.0, -lo), b + vec2(0.0, -hi), a + vec2(0.0, -hi), col);
}

/// A little team-coloured pennant on a pole at screen point `top`.
fn flag(top: Vec2, col: Color) {
    draw_line(top.x, top.y, top.x, top.y - 11.0, 1.0, rgb(0.25, 0.22, 0.2));
    draw_triangle(vec2(top.x, top.y - 11.0), vec2(top.x + 8.0, top.y - 8.5), vec2(top.x, top.y - 5.0), col);
}

/// A filled ellipse (triangle fan) — macroquad 0.4 has no primitive for it.
fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32, col: Color) {
    let n = 16;
    let mut prev = vec2(cx + rx, cy);
    for i in 1..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let p = vec2(cx + a.cos() * rx, cy + a.sin() * ry);
        draw_triangle(vec2(cx, cy), prev, p, col);
        prev = p;
    }
}

/// A vertical iso cylinder (stack / silo / tower drum) rising `h` from a base
/// centre at (cx, base_y). Returns the top-centre point (for steam etc.).
fn cylinder(cx: f32, base_y: f32, h: f32, rx: f32, ry: f32, body: Color) -> Vec2 {
    let top_y = base_y - h;
    ellipse(cx, base_y, rx, ry, shade(body, 0.65)); // bottom rim (behind)
    draw_rectangle(cx - rx, top_y, 2.0 * rx, h, body);
    draw_rectangle(cx - rx, top_y, rx * 0.55, h, shade(body, 1.18)); // lit left
    draw_rectangle(cx + rx * 0.45, top_y, rx * 0.55, h, shade(body, 0.72)); // shaded right
    ellipse(cx, top_y, rx, ry, shade(body, 1.28)); // top cap
    vec2(cx, top_y)
}

/// A tapered drum (truncated cone): radius `rb` at the base, `rt` at the top.
fn frustum(cx: f32, base_y: f32, h: f32, rb: f32, rt: f32, ry: f32, body: Color) -> Vec2 {
    let top_y = base_y - h;
    ellipse(cx, base_y, rb, ry, shade(body, 0.6)); // bottom rim (behind)
    let bl = vec2(cx - rb, base_y);
    let br = vec2(cx + rb, base_y);
    let tr = vec2(cx + rt, top_y);
    let tl = vec2(cx - rt, top_y);
    quad(tl, tr, br, bl, body);
    quad(tl, vec2(cx - rt * 0.3, top_y), vec2(cx - rb * 0.3, base_y), bl, shade(body, 1.18)); // lit left
    quad(vec2(cx + rt * 0.35, top_y), tr, br, vec2(cx + rb * 0.35, base_y), shade(body, 0.72)); // dark right
    ellipse(cx, top_y, rt, ry * rt / rb.max(0.5), shade(body, 1.25)); // top cap
    vec2(cx, top_y)
}

/// A hyperbolic cooling tower: a lower frustum narrowing to a waist, an upper
/// one flaring out, capped with a dark mouth. The iconic power-plant volume.
fn cooling_tower(cx: f32, base_y: f32, h: f32, rb: f32, body: Color) -> Vec2 {
    let rw = rb * 0.58; // waist
    let rt = rb * 0.72; // top flare
    let wy = base_y - h * 0.62;
    frustum(cx, base_y, h * 0.62, rb, rw, rb * 0.34, body);
    let top = frustum(cx, wy, h * 0.38, rw, rt, rw * 0.34, body);
    ellipse(top.x, top.y, rt, rt * 0.34, rgb(0.16, 0.16, 0.18)); // dark mouth
    top
}

fn draw_iso_building(w: &World, e: &ironvein_sim::Ent, cam: Vec2, col: Color, selected: bool) {
    let bt = e.tile();
    let (fw, fh) = e.foot();
    let (fwf, fhf) = (fw as f32, fh as f32);
    let bn = tile_to_screen(bt.x as f32, bt.y as f32) - cam;
    let be = tile_to_screen(bt.x as f32 + fwf, bt.y as f32) - cam;
    let bs = tile_to_screen(bt.x as f32 + fwf, bt.y as f32 + fhf) - cam;
    let bw = tile_to_screen(bt.x as f32, bt.y as f32 + fhf) - cam;
    let tick = w.tick;

    // cast shadow (footprint sheared to the lower-right)
    let off = vec2(HW * 0.35, HH * 0.35);
    quad(bn + off, be + off, bs + off, bw + off, Color::new(0.0, 0.0, 0.0, 0.26));

    if !e.done {
        quad(bn, be, bs, bw, Color::new(0.1, 0.1, 0.12, 0.45));
        let total = stats(e.kind).build_time * 2;
        let frac = (e.con_progress as f32 / total as f32).clamp(0.0, 1.0);
        let up = vec2(0.0, -bld_height(fw, fh) * frac);
        quad(bn + up, be + up, bs + up, bw + up, Color::new(0.4, 0.4, 0.45, 0.7));
        quad(bw, bs, bs + up, bw + up, Color::new(0.95, 0.8, 0.2, 0.35));
        draw_healthbar(e, bn + vec2(0.0, -8.0), fwf.max(fhf) * HW, true);
        return;
    }

    // each building gets its own silhouette + palette; the arm returns the
    // apex screen point used to place the floating health bar.
    let top = match e.kind {
        // -- low crop field with a little barn ----------------------------
        Kind::Farm => {
            let soil = rgb(0.37, 0.27, 0.14);
            quad(bn, be, bs, bw, soil);
            for i in 1..6 {
                let t = i as f32 / 6.0;
                let a = bn.lerp(be, t);
                let b = bw.lerp(bs, t);
                draw_line(a.x, a.y, b.x, b.y, 2.0, rgb(0.42, 0.56, 0.18));
                draw_line(a.x, a.y, b.x, b.y, 1.0, rgb(0.30, 0.43, 0.13));
            }
            quad_outline(bn, be, bs, bw, rgb(0.28, 0.20, 0.09));
            // a small barn at the back corner (anchored at bn)
            let q = |u: f32, v: f32| bn.lerp(be, u) + (bw - bn) * v;
            let r = iso_box(q(0.0, 0.0), q(0.42, 0.0), q(0.42, 0.42), q(0.0, 0.42), 13.0, rgb(0.52, 0.41, 0.28));
            let roof = rgb(0.55, 0.22, 0.18);
            let ra = r[0].lerp(r[3], 0.5) + vec2(0.0, -8.0);
            let rb = r[1].lerp(r[2], 0.5) + vec2(0.0, -8.0);
            quad(r[0], r[1], rb, ra, shade(roof, 1.1));
            quad(r[3], r[2], rb, ra, shade(roof, 0.78));
            flag(ra, col);
            ra
        }
        // -- canvas tent (triangular prism) -------------------------------
        Kind::Barracks => {
            let canvas = rgb(0.43, 0.46, 0.29);
            let ra = bn.lerp(bw, 0.5) + vec2(0.0, -23.0);
            let rb = be.lerp(bs, 0.5) + vec2(0.0, -23.0);
            quad(bn, be, rb, ra, shade(canvas, 1.12)); // back slope (lit)
            quad(bw, bs, rb, ra, shade(canvas, 0.78)); // front slope (shaded)
            draw_triangle(bn, bw, ra, shade(canvas, 0.7)); // left gable
            draw_triangle(be, bs, rb, shade(canvas, 0.55)); // right gable
            draw_line(ra.x, ra.y, rb.x, rb.y, 1.5, shade(canvas, 1.35)); // ridge
            // door flap on the front-left gable
            let d0 = bn.lerp(bw, 0.35);
            let d1 = bn.lerp(bw, 0.65);
            draw_triangle(d0, d1, bn.lerp(bw, 0.5) + vec2(0.0, -10.0), rgb(0.2, 0.22, 0.14));
            flag(ra, col);
            ra
        }
        // -- cottage: short walls + red gable roof ------------------------
        Kind::House => {
            let r = iso_box(bn, be, bs, bw, 14.0, rgb(0.66, 0.54, 0.38));
            let roof = rgb(0.58, 0.24, 0.18);
            let ra = r[0].lerp(r[3], 0.5) + vec2(0.0, -13.0);
            let rb = r[1].lerp(r[2], 0.5) + vec2(0.0, -13.0);
            quad(r[0], r[1], rb, ra, shade(roof, 1.1));
            quad(r[3], r[2], rb, ra, shade(roof, 0.78));
            draw_triangle(r[0], r[3], ra, shade(rgb(0.66, 0.54, 0.38), 0.85));
            draw_triangle(r[1], r[2], rb, shade(rgb(0.66, 0.54, 0.38), 0.7));
            draw_line(ra.x, ra.y, rb.x, rb.y, 1.5, shade(roof, 1.3));
            // door + warm glowing windows on the front (W-S) wall
            wall_panel(bw.lerp(bs, 0.42), bw.lerp(bs, 0.58), 0.0, 10.0, rgb(0.2, 0.12, 0.05));
            for wf in [0.2, 0.8] {
                let wc = bw.lerp(bs, wf) + vec2(0.0, -6.5);
                glow(wc, 6.0, rgb(1.0, 0.85, 0.45), 0.7);
            }
            wall_panel(bw.lerp(bs, 0.12), bw.lerp(bs, 0.28), 4.0, 9.0, rgb(1.0, 0.94, 0.6));
            wall_panel(bw.lerp(bs, 0.72), bw.lerp(bs, 0.88), 4.0, 9.0, rgb(1.0, 0.94, 0.6));
            draw_rectangle(rb.x - 1.5, rb.y - 6.0, 3.0, 6.0, rgb(0.3, 0.2, 0.12)); // chimney
            smoke(rb.x, rb.y - 6.0, 5, tick, true);
            ra
        }
        // -- open steel-lattice tower with a hut on top ------------------
        Kind::GuardTower => {
            let steel = rgb(0.46, 0.47, 0.5);
            let ctrg = (bn + be + bs + bw) * 0.25;
            let h = 34.0;
            let topc = |b: Vec2| b.lerp(ctrg, 0.35) + vec2(0.0, -h);
            let (tn, te, ts, tw) = (topc(bn), topc(be), topc(bs), topc(bw));
            // four tapering legs
            for (b, t) in [(bw, tw), (bs, ts), (be, te), (bn, tn)] {
                draw_line(b.x, b.y, t.x, t.y, 2.5, steel);
                draw_line(b.x, b.y, t.x, t.y, 1.0, shade(steel, 1.2));
            }
            // X cross-bracing + girders on the two camera-facing faces
            for (a0, a1, b0, b1) in [(bw, tw, bs, ts), (bs, ts, be, te)] {
                for lvl in 0..3 {
                    let f0 = lvl as f32 / 3.0;
                    let f1 = (lvl as f32 + 1.0) / 3.0;
                    draw_line(a0.lerp(a1, f0).x, a0.lerp(a1, f0).y, b0.lerp(b1, f1).x, b0.lerp(b1, f1).y, 1.2, shade(steel, 0.9));
                    draw_line(b0.lerp(b1, f0).x, b0.lerp(b1, f0).y, a0.lerp(a1, f1).x, a0.lerp(a1, f1).y, 1.2, shade(steel, 0.9));
                    draw_line(a0.lerp(a1, f1).x, a0.lerp(a1, f1).y, b0.lerp(b1, f1).x, b0.lerp(b1, f1).y, 1.0, shade(steel, 1.1));
                }
            }
            // a small hut cabin on the platform + a gable roof
            let r = iso_box(tn, te, ts, tw, 9.0, rgb(0.5, 0.42, 0.3));
            let ra = r[0].lerp(r[3], 0.5) + vec2(0.0, -6.0);
            let rb = r[1].lerp(r[2], 0.5) + vec2(0.0, -6.0);
            quad(r[0], r[1], rb, ra, rgb(0.55, 0.22, 0.18));
            quad(r[3], r[2], rb, ra, shade(rgb(0.55, 0.22, 0.18), 0.8));
            // the gun, mounted on the hut, tracking its facing
            let hc = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            let d = facing_vec(e.face);
            draw_line(hc.x, hc.y, hc.x + d.x * 15.0, hc.y + d.y * 15.0, 3.0, rgb(0.14, 0.14, 0.16));
            flag(ra, col);
            ra
        }
        // -- low stone wall ----------------------------------------------
        Kind::Wall => {
            let stone = rgb(0.5, 0.5, 0.54);
            let r = iso_box(bn, be, bs, bw, 9.0, stone);
            for i in 0..3 {
                let p = r[0].lerp(r[1], (i as f32 + 0.5) / 3.0);
                draw_rectangle(p.x - 2.0, p.y - 3.0, 4.0, 3.0, shade(stone, 1.2));
            }
            (r[0] + r[1] + r[2] + r[3]) * 0.25
        }
        // -- power plant: a big cooling-tower generator + a large stack ---
        Kind::PowerPlant => {
            let cg = (bn + be + bs + bw) * 0.25;
            // low apron the plant sits on
            quad(bn, be, bs, bw, rgb(0.40, 0.41, 0.44));
            quad_outline(bn, be, bs, bw, Color::new(0.08, 0.08, 0.1, 0.6));
            draw_line(bw.x, bw.y, bn.x, bn.y, 3.0, col); // team trim along the back rim
            // the generator IS a 3D cooling tower (back-left), venting steam
            let gb = bn.lerp(cg, 0.55).lerp(bw.lerp(cg, 0.55), 0.5);
            let gtop = cooling_tower(gb.x, gb.y + 2.0, 44.0, 14.0, rgb(0.66, 0.64, 0.6));
            smoke(gtop.x, gtop.y + 2.0, 17, tick, false);
            smoke(gtop.x - 4.0, gtop.y + 3.0, 23, tick, false);
            smoke(gtop.x + 4.0, gtop.y + 3.0, 51, tick, false);
            // a large smokestack at the front-right, warning band + smoke
            let sb = be.lerp(cg, 0.4).lerp(bs.lerp(cg, 0.4), 0.5);
            let stop = cylinder(sb.x, sb.y, 36.0, 5.5, 2.2, rgb(0.55, 0.46, 0.4));
            draw_rectangle(stop.x - 5.5, stop.y + 5.0, 11.0, 3.0, rgb(0.7, 0.25, 0.18));
            smoke(stop.x, stop.y, 41, tick, true);
            // a glowing transformer hum at the front rim
            glow(bs + vec2(0.0, -6.5), 9.0, rgb(0.95, 0.9, 0.3), 0.8);
            draw_rectangle(bs.x - 6.0, bs.y - 8.0, 12.0, 3.0, rgb(0.98, 0.93, 0.4));
            gtop
        }
        // -- ore refinery: a terraced open-pit mine + a big smokestack ----
        Kind::Refinery => {
            let ctr = (bn + be + bs + bw) * 0.25;
            let steps = 3;
            for d in 0..=steps {
                let f = d as f32 / steps as f32;
                let down = vec2(0.0, f * 9.0);
                let n = bn.lerp(ctr, f * 0.66) + down;
                let e = be.lerp(ctr, f * 0.66) + down;
                let s = bs.lerp(ctr, f * 0.66) + down;
                let wv = bw.lerp(ctr, f * 0.66) + down;
                quad(n, e, s, wv, shade(rgb(0.44, 0.34, 0.21), 1.0 - f * 0.5));
                quad_outline(n, e, s, wv, Color::new(0.0, 0.0, 0.0, 0.18));
            }
            // exposed ore seam at the pit floor
            let pit = ctr + vec2(0.0, 9.0);
            for k in 0..7 {
                let ang = k as f32 * 0.9;
                let g = pit + vec2(ang.cos() * 5.0, ang.sin() * 2.5);
                draw_triangle(g + vec2(-1.5, 1.0), g + vec2(1.5, 1.0), g + vec2(0.0, -2.0), rgb(0.9, 0.75, 0.2));
            }
            // a big smokestack at the back-left rim, warning band + smoke
            let sb = bn.lerp(ctr, 0.28);
            let top = cylinder(sb.x, sb.y, 38.0, 5.5, 2.2, rgb(0.5, 0.42, 0.36));
            draw_rectangle(top.x - 5.5, top.y + 5.0, 11.0, 3.0, rgb(0.7, 0.2, 0.15));
            smoke(top.x, top.y, 41, tick, true);
            flag(bn.lerp(ctr, 0.1) + vec2(0.0, -2.0), col); // ownership marker at the rim
            top
        }
        // -- war factory: an extruded sawtooth (north-light) roof + chimney
        Kind::Factory => {
            let body = rgb(0.36, 0.37, 0.41);
            let r = iso_box(bn, be, bs, bw, 20.0, body);
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 3.0, col);
            // bilinear roof coords: u along r[0]->r[1], v along r[0]->r[3]
            let rp = |u: f32, v: f32| r[0] + (r[1] - r[0]) * u + (r[3] - r[0]) * v;
            let teeth = 4;
            let hr = 8.0;
            let up = vec2(0.0, -hr);
            for k in 0..teeth {
                let u0 = k as f32 / teeth as f32;
                let u1 = (k as f32 + 1.0) / teeth as f32;
                let (f0a, f0b) = (rp(u0, 0.0), rp(u0, 1.0));
                let (f1a, f1b) = (rp(u1, 0.0), rp(u1, 1.0));
                // slope rising from u0 (roof level) to the ridge at u1
                quad(f0a, f0b, f1b + up, f1a + up, shade(body, 1.22));
                // glazed north-light riser dropping at u1 (faces the camera)
                quad(f1a + up, f1b + up, f1b, f1a, rgb(0.45, 0.6, 0.7));
                for m in 1..3 {
                    let lo = f1a.lerp(f1b, m as f32 / 3.0);
                    draw_line(lo.x, lo.y, lo.x, lo.y - hr, 1.0, rgb(0.25, 0.35, 0.42));
                }
                draw_line((f1a + up).x, (f1a + up).y, (f1b + up).x, (f1b + up).y, 1.0, shade(body, 1.5));
            }
            // a prominent chimney at the back corner
            let cb = r[0].lerp((r[0] + r[1] + r[2] + r[3]) * 0.25, 0.28);
            let top = cylinder(cb.x, cb.y, 24.0, 4.0, 1.7, rgb(0.32, 0.32, 0.35));
            draw_rectangle(top.x - 4.0, top.y + 3.0, 8.0, 2.0, rgb(0.7, 0.25, 0.18));
            smoke(top.x, top.y, 63, tick, true);
            // big roll-up door + team stripe on the front wall
            wall_panel(bw.lerp(bs, 0.3), bw.lerp(bs, 0.7), 0.0, 13.0, rgb(0.12, 0.12, 0.14));
            wall_panel(bw.lerp(bs, 0.3), bw.lerp(bs, 0.7), 13.0, 15.0, shade(col, 0.9));
            top
        }
        // -- construction yard: a workshop with a working tower crane -----
        Kind::ConYard => {
            // low workshop building
            let r = iso_box(bn, be, bs, bw, 16.0, rgb(0.40, 0.42, 0.47));
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 3.0, col);
            let ctr = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            wall_panel(bs.lerp(be, 0.25), bs.lerp(be, 0.65), 0.0, 12.0, shade(col, 0.7)); // bay door
            // a stack of girders on the apron (building materials)
            for i in 0..3 {
                let y = r[2].y - 2.0 - i as f32 * 2.5;
                draw_rectangle(r[2].x - 9.0, y, 18.0, 2.0, rgb(0.72, 0.62, 0.2));
                draw_rectangle(r[2].x - 9.0, y, 18.0, 0.8, rgb(0.85, 0.74, 0.28));
            }
            // --- tower crane rising from the yard ---
            let beam = rgb(0.78, 0.7, 0.22);
            let dark = rgb(0.55, 0.5, 0.16);
            let mb = r[3].lerp(ctr, 0.45); // mast base, back-left of the roof
            let mh = 42.0;
            let mw = 3.0;
            let mtop = vec2(mb.x, mb.y - mh);
            // mast lattice: two posts + rungs + X braces
            draw_line(mb.x - mw, mb.y, mtop.x - mw, mtop.y, 1.6, beam);
            draw_line(mb.x + mw, mb.y, mtop.x + mw, mtop.y, 1.6, beam);
            for i in 0..7 {
                let y0 = mb.y - i as f32 * mh / 7.0;
                let y1 = mb.y - (i as f32 + 1.0) * mh / 7.0;
                draw_line(mb.x - mw, y0, mb.x + mw, y1, 0.8, dark);
                draw_line(mb.x + mw, y0, mb.x - mw, y1, 0.8, dark);
            }
            // slewing unit + operator cab
            draw_rectangle(mtop.x - 3.5, mtop.y - 1.0, 7.0, 5.0, rgb(0.3, 0.3, 0.34));
            draw_rectangle(mtop.x - 3.5, mtop.y - 1.0, 7.0, 1.5, rgb(0.45, 0.45, 0.5));
            // jib + counter-jib, slowly slewing
            let swing = (tick as f32 * 0.02).sin();
            let jdir = vec2(1.0, 0.16 + swing * 0.1).normalize();
            let jend = mtop + jdir * 30.0;
            let cend = mtop - jdir * 12.0;
            let drop = vec2(0.0, 4.5);
            // jib truss (top + bottom chord + diagonals)
            draw_line(mtop.x, mtop.y, jend.x, jend.y, 1.6, beam);
            draw_line(mtop.x + drop.x, mtop.y + drop.y, jend.x, jend.y + drop.y * 0.4, 1.2, dark);
            for i in 1..7 {
                let t = i as f32 / 7.0;
                let a = mtop.lerp(jend, t);
                draw_line(a.x, a.y, a.x, a.y + drop.y * (1.0 - t * 0.4), 0.6, dark);
            }
            // counter-jib + counterweight block
            draw_line(mtop.x, mtop.y, cend.x, cend.y, 1.6, beam);
            draw_rectangle(cend.x - 3.0, cend.y - 1.0, 6.0, 6.0, rgb(0.32, 0.32, 0.34));
            // trolley + hoist cable + hook, bobbing
            let trolley = mtop.lerp(jend, 0.72);
            let hooky = trolley.y + 18.0 + swing * 4.0;
            draw_line(trolley.x, trolley.y + 2.0, trolley.x, hooky, 0.8, rgb(0.2, 0.2, 0.2));
            draw_rectangle(trolley.x - 1.5, hooky, 3.0, 3.0, rgb(0.45, 0.45, 0.48));
            mtop
        }
        // -- cannon tower: armoured bunker + rotating gun + big barrel -----
        Kind::CannonTower => {
            let concrete = rgb(0.5, 0.5, 0.52);
            let r = iso_box(bn, be, bs, bw, 16.0, concrete);
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 3.0, col);
            let ctr = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            // armoured turret dome
            for i in 0..4 {
                let t = i as f32 / 4.0;
                draw_circle(ctr.x - t * 1.5, ctr.y - 1.0 - t * 1.5, 7.0 * (1.0 - t * 0.6), shade(rgb(0.43, 0.44, 0.47), 0.8 + 0.5 * t));
            }
            // thick cannon barrel in the facing direction
            let d = facing_vec(e.face);
            let muzzle = vec2(ctr.x + d.x * 16.0, ctr.y - 1.0 + d.y * 16.0);
            draw_line(ctr.x, ctr.y - 1.0, muzzle.x, muzzle.y, 4.0, rgb(0.16, 0.16, 0.18));
            draw_line(ctr.x, ctr.y - 1.0, muzzle.x, muzzle.y, 1.5, rgb(0.32, 0.32, 0.34));
            draw_circle(muzzle.x, muzzle.y, 2.0, rgb(0.1, 0.1, 0.11));
            // sandbag skirt
            for k in 0..4 {
                let p = bs.lerp(be, k as f32 / 3.0);
                draw_circle(p.x, p.y, 2.5, rgb(0.45, 0.4, 0.22));
            }
            ctr + vec2(0.0, -8.0)
        }
        // -- pillbox: a low concrete drum + dome with a gun slit -----------
        Kind::Pillbox => {
            let concrete = rgb(0.52, 0.5, 0.46);
            let ctr = (bn + be + bs + bw) * 0.25;
            cylinder(ctr.x, ctr.y + 4.0, 9.0, 11.0, 5.0, concrete);
            draw_rectangle(ctr.x - 9.0, ctr.y - 1.0, 18.0, 2.0, col); // team band
            for i in 0..4 {
                let t = i as f32 / 4.0;
                draw_circle(ctr.x - t * 2.0, ctr.y - 5.0 - t * 2.0, 11.0 * (1.0 - t * 0.6), shade(concrete, 0.85 + 0.4 * t));
            }
            let d = facing_vec(e.face);
            draw_rectangle(ctr.x - 4.0, ctr.y - 6.0, 8.0, 2.5, rgb(0.1, 0.1, 0.12)); // gun slit
            draw_line(ctr.x, ctr.y - 5.0, ctr.x + d.x * 8.0, ctr.y - 5.0 + d.y * 8.0, 2.0, rgb(0.14, 0.14, 0.16));
            ctr + vec2(0.0, -12.0)
        }
        // -- radar dome: a tower with a sweeping dish + blinking beacon -----
        Kind::Radar => {
            let body = rgb(0.38, 0.4, 0.45);
            let r = iso_box(bn, be, bs, bw, 22.0, body);
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 3.0, col);
            let ctr = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            draw_rectangle(ctr.x - 1.5, ctr.y - 15.0, 3.0, 15.0, rgb(0.5, 0.5, 0.55)); // mast
            // a dish that "rotates": its width breathes as it sweeps
            let wob = (tick as f32 * 0.06).cos();
            let dish_w = 3.5 + 7.0 * wob.abs();
            let top = vec2(ctr.x + wob * 4.0, ctr.y - 17.0);
            ellipse(top.x, top.y, dish_w, 8.0, rgb(0.7, 0.72, 0.78));
            ellipse(top.x, top.y, dish_w * 0.7, 6.0, rgb(0.42, 0.44, 0.5));
            draw_circle(ctr.x, ctr.y - 15.0, 1.6, rgb(0.3, 0.3, 0.34));
            if (tick / 14) % 2 == 0 {
                glow(vec2(ctr.x, ctr.y - 2.0), 5.0, rgb(0.3, 1.0, 0.45), 0.9);
            }
            top
        }
        // -- repair depot: a hazard-striped pad + overhead welding gantry --
        Kind::RepairDepot => {
            quad(bn, be, bs, bw, rgb(0.42, 0.42, 0.45));
            quad_outline(bn, be, bs, bw, Color::new(0.08, 0.08, 0.1, 0.6));
            draw_line(bw.x, bw.y, bn.x, bn.y, 3.0, col);
            for k in 0..3 {
                let a = bn.lerp(bw, (k as f32 + 0.5) / 3.0);
                let b = be.lerp(bs, (k as f32 + 0.5) / 3.0);
                draw_line(a.x, a.y, b.x, b.y, 2.0, rgb(0.85, 0.75, 0.2)); // hazard chevrons
            }
            let h = 20.0;
            let up = vec2(0.0, -h);
            let (p1, p2) = (bn + up, be + up);
            draw_line(bn.x, bn.y, p1.x, p1.y, 2.0, rgb(0.5, 0.5, 0.54)); // posts
            draw_line(be.x, be.y, p2.x, p2.y, 2.0, rgb(0.5, 0.5, 0.54));
            draw_line(p1.x, p1.y, p2.x, p2.y, 3.0, rgb(0.7, 0.62, 0.2)); // gantry beam
            // a welder head sliding the beam, throwing sparks
            let slide = (tick as f32 * 0.05).sin() * 0.5 + 0.5;
            let head = p1.lerp(p2, slide);
            draw_rectangle(head.x - 2.0, head.y, 4.0, 4.0, rgb(0.3, 0.3, 0.34));
            draw_line(head.x, head.y + 4.0, head.x, head.y + 10.0, 1.0, rgb(0.3, 0.3, 0.34));
            if (tick / 4) % 2 == 0 {
                glow(vec2(head.x, head.y + 11.0), 4.0, rgb(0.7, 0.9, 1.0), 1.0);
                for s in 0..3 {
                    let a = tick as f32 + s as f32 * 2.1;
                    draw_circle(head.x + a.cos() * 3.0, head.y + 11.0 + (a.sin() * 2.0).abs(), 0.8, rgb(1.0, 0.9, 0.5));
                }
            }
            (bn + be + bs + bw) * 0.25 + vec2(0.0, -h)
        }
        // -- reactor: a domed containment vessel with a pulsing core -------
        Kind::Reactor => {
            quad(bn, be, bs, bw, rgb(0.38, 0.40, 0.44));
            quad_outline(bn, be, bs, bw, Color::new(0.08, 0.08, 0.1, 0.6));
            draw_line(bw.x, bw.y, bn.x, bn.y, 3.0, col);
            let cg = (bn + be + bs + bw) * 0.25;
            let dome = frustum(cg.x, cg.y + 2.0, 30.0, 15.0, 7.0, 8.0, rgb(0.60, 0.62, 0.66));
            ellipse(dome.x, dome.y, 8.0, 4.0, rgb(0.72, 0.75, 0.80));
            let pulse = 0.55 + 0.45 * (tick as f32 * 0.08).sin();
            glow(dome, 11.0, rgb(0.35, 0.95, 0.5), pulse);
            let sb = be.lerp(cg, 0.5);
            let stop = cylinder(sb.x, sb.y, 20.0, 3.5, 1.6, rgb(0.50, 0.50, 0.54));
            smoke(stop.x, stop.y, 31, tick, false);
            dome
        }
        // -- tech center: a lab block with lit windows + comms dish --------
        Kind::TechCenter => {
            let r = iso_box(bn, be, bs, bw, 30.0, rgb(0.30, 0.33, 0.40));
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 3.0, col);
            let ctr = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            for row in 0..3 {
                let h = 6.0 + row as f32 * 8.0;
                for f in [0.2, 0.4, 0.6, 0.8] {
                    wall_panel(bw.lerp(bs, f - 0.05), bw.lerp(bs, f + 0.05), h, h + 4.0, rgb(0.30, 0.85, 0.95));
                }
            }
            draw_rectangle(ctr.x - 1.0, ctr.y - 18.0, 2.0, 18.0, rgb(0.5, 0.5, 0.55));
            draw_circle(ctr.x, ctr.y - 18.0, 2.0, if (tick / 12) % 2 == 0 { rgb(1.0, 0.3, 0.3) } else { rgb(0.4, 0.1, 0.1) });
            ellipse(r[1].x, r[1].y - 6.0, 5.0, 3.0, rgb(0.70, 0.72, 0.78));
            ctr
        }
        // -- ore silo: a cluster of capped storage cylinders ---------------
        Kind::OreSilo => {
            quad(bn, be, bs, bw, rgb(0.36, 0.34, 0.30));
            quad_outline(bn, be, bs, bw, Color::new(0.08, 0.08, 0.1, 0.6));
            let cg = (bn + be + bs + bw) * 0.25;
            let mut topmost = cg;
            for (i, off) in [(0.0f32, -4.0f32), (-7.0, 3.0), (7.0, 3.0)].iter().enumerate() {
                let b = vec2(cg.x + off.0, cg.y + off.1);
                let t = cylinder(b.x, b.y, 24.0, 6.0, 3.0, rgb(0.62, 0.58, 0.50));
                ellipse(t.x, t.y, 6.0, 3.0, rgb(0.85, 0.70, 0.25));
                draw_rectangle(b.x - 6.0, b.y - 9.0, 12.0, 2.0, col);
                if i == 0 {
                    topmost = t;
                }
            }
            topmost
        }
        // -- med bay: a clean white block with a red cross -----------------
        Kind::MedBay => {
            let r = iso_box(bn, be, bs, bw, 16.0, rgb(0.78, 0.80, 0.82));
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 3.0, col);
            let ctr = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            draw_rectangle(ctr.x - 2.0, ctr.y - 8.0, 4.0, 12.0, rgb(0.85, 0.15, 0.12));
            draw_rectangle(ctr.x - 6.0, ctr.y - 4.0, 12.0, 4.0, rgb(0.85, 0.15, 0.12));
            wall_panel(bw.lerp(bs, 0.40), bw.lerp(bs, 0.60), 0.0, 9.0, rgb(0.86, 0.86, 0.90));
            glow(bw.lerp(bs, 0.5) + vec2(0.0, -5.0), 5.0, rgb(0.4, 1.0, 0.5), 0.5);
            ctr
        }
        // -- gate: two pillars with team-coloured bars (opens for friends) --
        Kind::Gate => {
            let stone = rgb(0.46, 0.46, 0.52);
            let r = iso_box(bn, be, bs, bw, 13.0, stone);
            let ctr = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            // heavier corner pillars
            for c in [r[0], r[1], r[2], r[3]] {
                draw_circle(c.x, c.y, 2.0, shade(stone, 1.2));
            }
            // team-coloured gate bars across the SW face
            for i in 0..4 {
                let p = bw.lerp(bs, 0.15 + i as f32 * 0.23);
                draw_rectangle(p.x - 1.0, p.y - 11.0, 2.0, 11.0, col);
            }
            draw_line(r[3].x, r[3].y, r[0].x, r[0].y, 2.0, shade(col, 1.2)); // lintel
            ctr
        }
        // -- missile turret: armoured base + rotating twin-tube pod --------
        Kind::MissileTurret => {
            let concrete = rgb(0.44, 0.46, 0.50);
            let ctr = (bn + be + bs + bw) * 0.25;
            cylinder(ctr.x, ctr.y + 4.0, 8.0, 10.0, 5.0, concrete);
            draw_rectangle(ctr.x - 8.0, ctr.y - 1.0, 16.0, 2.0, col);
            let d = facing_vec(e.face);
            let tc = ctr + vec2(0.0, -10.0);
            draw_rectangle(tc.x - 5.0, tc.y - 4.0, 10.0, 7.0, rgb(0.30, 0.32, 0.36));
            let perp = vec2(-d.y, d.x);
            for sgn in [-2.5, 2.5] {
                let b = tc + perp * sgn;
                draw_line(b.x, b.y, b.x + d.x * 10.0, b.y + d.y * 10.0, 2.5, rgb(0.18, 0.18, 0.2));
                draw_circle(b.x + d.x * 10.0, b.y + d.y * 10.0, 1.6, rgb(0.9, 0.4, 0.15));
            }
            tc + vec2(0.0, -6.0)
        }
        // -- obelisk: a tier-3 arcane monolith with a pulsing crystal tip -----
        Kind::Obelisk => {
            let cg = (bn + be + bs + bw) * 0.25;
            cylinder(cg.x, cg.y + 4.0, 6.0, 9.0, 4.5, rgb(0.18, 0.16, 0.22)); // base
            let tip = frustum(cg.x, cg.y - 2.0, 42.0, 7.0, 2.0, 4.0, rgb(0.17, 0.14, 0.23)); // monolith
            let pulse = 0.6 + 0.4 * (tick as f32 * 0.09).sin();
            draw_line(cg.x, cg.y - 4.0, tip.x, tip.y + 2.0, 1.5, rgb(0.55, 0.30, 0.85)); // energy seam
            glow(tip, 13.0, rgb(0.72, 0.40, 1.0), pulse);
            draw_circle(tip.x, tip.y, 3.2, rgb(0.88, 0.62, 1.0)); // crystal
            tip
        }
        // -- the Starship: a TOWERING stainless landing craft (5x5 footprint) --
        Kind::Starship => {
            let g = (bn + be + bs + bw) * 0.25;
            let rad = 24.0;
            let fin = rgb(0.58, 0.61, 0.67);
            draw_circle(g.x, g.y + 10.0, rad * 2.1, Color::new(0.09, 0.09, 0.11, 0.5)); // scorched pad
            // flared aft fins (behind the hull)
            draw_triangle(vec2(g.x - rad * 1.8, g.y + 14.0), vec2(g.x - rad * 0.7, g.y - 54.0), vec2(g.x - rad * 0.55, g.y + 14.0), fin);
            draw_triangle(vec2(g.x + rad * 1.8, g.y + 14.0), vec2(g.x + rad * 0.7, g.y - 54.0), vec2(g.x + rad * 0.55, g.y + 14.0), fin);
            // stainless hull
            let top = cylinder(g.x, g.y, 150.0, rad, rad * 0.45, rgb(0.80, 0.83, 0.88));
            draw_line(g.x - rad * 0.35, g.y, g.x - rad * 0.35, top.y, 1.5, Color::new(1.0, 1.0, 1.0, 0.32)); // seams
            draw_line(g.x + rad * 0.55, g.y, g.x + rad * 0.55, top.y, 1.5, Color::new(0.0, 0.0, 0.0, 0.16));
            for b in 0..3 {
                let by = top.y + 28.0 + b as f32 * 42.0;
                draw_rectangle(g.x - rad * 1.0, by, rad * 2.0, 4.0, rgb(0.24, 0.44, 0.60)); // window bands
            }
            // forward fins (canards)
            draw_triangle(vec2(g.x - rad, top.y + 22.0), vec2(g.x - rad * 1.9, top.y + 4.0), vec2(g.x - rad, top.y + 44.0), fin);
            draw_triangle(vec2(g.x + rad, top.y + 22.0), vec2(g.x + rad * 1.9, top.y + 4.0), vec2(g.x + rad, top.y + 44.0), fin);
            // nosecone
            let tip = frustum(g.x, top.y, 48.0, rad, 1.2, rad * 0.45, rgb(0.85, 0.88, 0.93));
            // engines idling after planetfall
            let flick = 0.45 + 0.3 * (tick as f32 * 0.11).sin();
            glow(vec2(g.x, g.y + 6.0), 26.0, rgb(1.0, 0.55, 0.2), flick);
            draw_circle(g.x, g.y + 6.0, 5.0, rgb(1.0, 0.82, 0.4));
            tip
        }
        // -- the Missile Silo: armoured bunker with a charge ring + nuke tip ----
        Kind::MissileSilo => {
            let r = iso_box(bn, be, bs, bw, 14.0, rgb(0.28, 0.30, 0.34));
            let top = (r[0] + r[1] + r[2] + r[3]) * 0.25;
            // open hatch with the missile nose poking out
            draw_circle(top.x, top.y, 9.0, rgb(0.10, 0.10, 0.12));
            draw_circle(top.x, top.y - 3.0, 4.5, rgb(0.78, 0.80, 0.84)); // missile body
            draw_triangle(top + vec2(-4.5, -3.0), top + vec2(4.5, -3.0), top + vec2(0.0, -14.0), rgb(0.88, 0.3, 0.2)); // warhead nose
            // charge ring: e.work_t / NUKE_CHARGE, green→red as it nears ready
            let frac = (e.work_t as f32 / ironvein_sim::stats::NUKE_CHARGE as f32).clamp(0.0, 1.0);
            let ready = frac >= 1.0;
            let ringc = if ready {
                let p = 0.5 + 0.5 * (tick as f32 * 0.2).sin();
                Color::new(1.0, 0.3 * p, 0.2, 1.0)
            } else {
                mix(rgb(0.3, 0.8, 0.4), rgb(0.95, 0.75, 0.2), frac)
            };
            draw_circle_lines(top.x, top.y, 13.0, 2.0, Color::new(0.0, 0.0, 0.0, 0.5));
            let seg = (frac * 24.0) as i32;
            for k in 0..seg {
                let a = k as f32 / 24.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                draw_circle(top.x + a.cos() * 13.0, top.y + a.sin() * 13.0, 1.6, ringc);
            }
            if ready {
                glow(top, 16.0, rgb(1.0, 0.4, 0.2), 0.5);
            }
            top
        }
        // -- the Food Silo: a granary — domed grain bins + a wheat sheaf -------
        Kind::FoodSilo => {
            let g = (bn + be + bs + bw) * 0.25;
            for dx in [-9.0_f32, 6.0] {
                let x = g.x + dx;
                draw_rectangle(x, g.y - 16.0, 9.0, 22.0, rgb(0.78, 0.72, 0.55)); // bin
                ellipse(x + 4.5, g.y - 16.0, 4.5, 2.2, rgb(0.86, 0.55, 0.30)); // domed top
                draw_rectangle(x, g.y - 4.0, 9.0, 3.0, rgb(0.62, 0.56, 0.40)); // band
            }
            // a wheat sheaf out front
            for k in -1..=1 {
                let sx = g.x + k as f32 * 2.2;
                draw_line(sx, g.y + 6.0, sx + k as f32 * 1.5, g.y - 4.0, 1.4, rgb(0.85, 0.72, 0.25));
                draw_circle(sx + k as f32 * 1.5, g.y - 4.5, 1.6, rgb(0.95, 0.82, 0.3));
            }
            g - vec2(0.0, 16.0)
        }
        _ => {
            let r = iso_box(bn, be, bs, bw, bld_height(fw, fh), rgb(0.4, 0.4, 0.43));
            (r[0] + r[1] + r[2] + r[3]) * 0.25
        }
    };

    if selected {
        quad_outline(bn, be, bs, bw, Color::new(0.45, 1.0, 0.5, 0.95));
    }
    draw_healthbar(e, top + vec2(0.0, -6.0), fwf.max(fhf) * HW, selected);
}

fn quad_outline(a: Vec2, b: Vec2, c: Vec2, d: Vec2, col: Color) {
    draw_line(a.x, a.y, b.x, b.y, 1.0, col);
    draw_line(b.x, b.y, c.x, c.y, 1.0, col);
    draw_line(c.x, c.y, d.x, d.y, 1.0, col);
    draw_line(d.x, d.y, a.x, a.y, 1.0, col);
}

fn draw_iso_unit(w: &World, e: &ironvein_sim::Ent, g: Vec2, col: Color, selected: bool) {
    // g is the ground point (the unit's tile position on screen)
    let d = facing_vec(e.face);
    let lit = shade(col, 1.3);
    let dim = shade(col, 0.6);
    // ground shadow
    draw_circle(g.x, g.y + 2.0, 6.0, Color::new(0.0, 0.0, 0.0, 0.26));
    let c = g - vec2(0.0, 8.0); // body sits above the ground point

    if selected {
        // selection ring on the ground
        draw_circle_lines(g.x, g.y + 1.0, 9.0, 1.5, Color::new(0.45, 1.0, 0.5, 0.9));
    }

    match e.kind {
        Kind::Rifleman
        | Kind::Rocketeer
        | Kind::Engineer
        | Kind::Grenadier
        | Kind::Flamer
        | Kind::Sniper => {
            draw_rectangle(c.x - 3.0, c.y + 2.0, 2.0, 5.0, dim);
            draw_rectangle(c.x + 1.0, c.y + 2.0, 2.0, 5.0, dim);
            draw_rectangle(c.x - 3.5, c.y - 3.0, 7.0, 7.0, col);
            draw_rectangle(c.x - 3.5, c.y - 3.0, 2.5, 7.0, lit);
            draw_rectangle(c.x - 4.5, c.y - 2.0, 2.0, 5.0, shade(col, 0.5)); // pack
            let head = match e.kind {
                Kind::Engineer => rgb(0.97, 0.86, 0.2),
                Kind::Sniper => rgb(0.45, 0.50, 0.35), // ghillie
                _ => rgb(0.88, 0.72, 0.56),
            };
            draw_circle(c.x, c.y - 5.5, 3.0, head);
            draw_rectangle(c.x - 3.0, c.y - 7.5, 6.0, 2.5, shade(col, 0.8)); // helmet
            match e.kind {
                Kind::Rifleman => {
                    draw_line(c.x, c.y, c.x + d.x * 9.0, c.y + d.y * 9.0, 2.0, rgb(0.12, 0.12, 0.14));
                }
                Kind::Rocketeer => {
                    draw_line(c.x - d.y * 3.0, c.y + d.x * 3.0, c.x + d.x * 8.0, c.y + d.y * 8.0, 3.0, rgb(0.25, 0.3, 0.25));
                    draw_circle(c.x + d.x * 8.5, c.y + d.y * 8.5, 2.0, rgb(0.95, 0.35, 0.12));
                }
                Kind::Engineer => {
                    draw_rectangle(c.x - 1.5 + d.x * 5.0, c.y - 1.5 + d.y * 5.0, 4.0, 4.0, rgb(0.72, 0.72, 0.78));
                }
                Kind::Grenadier => {
                    draw_line(c.x, c.y, c.x + d.x * 6.0, c.y + d.y * 6.0, 3.0, rgb(0.2, 0.22, 0.18));
                    draw_circle(c.x + d.x * 6.5, c.y + d.y * 6.5, 2.2, rgb(0.34, 0.5, 0.22));
                }
                Kind::Flamer => {
                    draw_line(c.x, c.y, c.x + d.x * 8.0, c.y + d.y * 8.0, 2.5, rgb(0.20, 0.20, 0.22));
                    draw_circle(c.x + d.x * 9.5, c.y + d.y * 9.5, 3.0, rgb(1.0, 0.55, 0.15));
                    draw_circle(c.x + d.x * 9.5, c.y + d.y * 9.5, 1.6, rgb(1.0, 0.86, 0.42));
                    draw_rectangle(c.x - 5.0, c.y - 3.0, 2.5, 6.0, rgb(0.70, 0.20, 0.15)); // fuel tank
                }
                Kind::Sniper => {
                    draw_line(c.x, c.y, c.x + d.x * 13.0, c.y + d.y * 13.0, 1.6, rgb(0.10, 0.10, 0.12));
                }
                _ => {}
            }
        }
        Kind::Harvester => {
            for side in [-9.0, 6.0] {
                draw_rectangle(c.x + side, c.y - 6.0, 3.0, 14.0, rgb(0.12, 0.12, 0.14));
            }
            vgrad_box(c.x - 9.0, c.y - 6.0, 18.0, 13.0, lit, dim);
            // cargo bar: tinted by what's physically in the hopper
            let frac = e.cargo as f32 / ironvein_sim::stats::HARVESTER_CAP as f32;
            draw_rectangle(c.x - 6.0, c.y - 4.0, 12.0, 6.0, rgb(0.1, 0.1, 0.12));
            draw_rectangle(c.x - 6.0, c.y - 4.0, 12.0 * frac, 6.0, resource_color(e.cargo_kind));
            draw_rectangle(c.x - 4.0 + d.x * 9.0, c.y - 2.0 + d.y * 9.0, 6.0, 5.0, shade(col, 0.5));
            // pennant: the resource it's HEADED for (its assignment) — flips the
            // instant you order it onto stone/wood, so the command reads clearly
            let target_kind = e
                .ore_tile
                .filter(|t| w.map.ore_at(*t) > 0)
                .and_then(|t| w.map.resource_kind(t))
                .unwrap_or(e.cargo_kind);
            draw_line(c.x + 6.0, c.y - 14.0, c.x + 6.0, c.y - 6.0, 1.0, rgb(0.15, 0.15, 0.17));
            draw_triangle(vec2(c.x + 6.0, c.y - 14.0), vec2(c.x + 13.0, c.y - 12.0), vec2(c.x + 6.0, c.y - 10.0), resource_color(target_kind));
        }
        Kind::Buggy => {
            for (ox, oy) in [(-6.0, -4.0), (4.0, -4.0), (-6.0, 4.0), (4.0, 4.0)] {
                draw_circle(c.x + ox + 1.0, c.y + oy + 1.0, 2.4, rgb(0.08, 0.08, 0.09));
            }
            vgrad_box(c.x - 6.5, c.y - 4.0, 13.0, 9.0, lit, dim);
            draw_rectangle(c.x - 3.0, c.y - 2.0, 6.0, 4.0, rgb(0.4, 0.55, 0.65));
            draw_line(c.x, c.y, c.x + d.x * 9.0, c.y + d.y * 9.0, 2.0, rgb(0.12, 0.12, 0.14));
        }
        Kind::Tank => {
            for side in [-10.0, 7.0] {
                draw_rectangle(c.x + side, c.y - 7.0, 3.5, 15.0, rgb(0.13, 0.13, 0.15));
            }
            vgrad_box(c.x - 7.0, c.y - 6.0, 14.0, 13.0, lit, dim);
            // domed turret
            for i in 0..4 {
                let t = i as f32 / 4.0;
                draw_circle(c.x - t * 1.5, c.y - t * 1.5, 5.0 * (1.0 - t * 0.7), shade(col, 0.85 + 0.5 * t));
            }
            draw_line(c.x, c.y, c.x + d.x * 14.0, c.y + d.y * 14.0, 3.0, rgb(0.12, 0.12, 0.14));
            draw_circle(c.x + d.x * 14.0, c.y + d.y * 14.0, 1.6, rgb(0.08, 0.08, 0.09));
        }
        Kind::HeavyTank => {
            for side in [-12.0, 9.0] {
                draw_rectangle(c.x + side, c.y - 8.0, 4.0, 18.0, rgb(0.12, 0.12, 0.14));
            }
            vgrad_box(c.x - 9.0, c.y - 7.0, 18.0, 15.0, lit, dim);
            for i in 0..4 {
                let t = i as f32 / 4.0;
                draw_circle(c.x - t * 1.5, c.y - t * 1.5, 6.5 * (1.0 - t * 0.6), shade(col, 0.85 + 0.5 * t));
            }
            let perp = vec2(-d.y, d.x);
            for sgn in [-2.0, 2.0] {
                let b = c + perp * sgn;
                draw_line(b.x, b.y, b.x + d.x * 15.0, b.y + d.y * 15.0, 2.5, rgb(0.12, 0.12, 0.14));
                draw_circle(b.x + d.x * 15.0, b.y + d.y * 15.0, 1.4, rgb(0.08, 0.08, 0.09));
            }
        }
        Kind::Artillery => {
            for side in [-9.0, 6.0] {
                draw_rectangle(c.x + side, c.y - 6.0, 3.0, 14.0, rgb(0.12, 0.12, 0.14));
            }
            vgrad_box(c.x - 7.0, c.y - 5.0, 14.0, 11.0, lit, dim);
            draw_circle(c.x, c.y - 2.0, 3.0, shade(col, 0.7)); // turret mount
            draw_line(c.x, c.y - 2.0, c.x + d.x * 20.0, c.y - 2.0 + d.y * 20.0, 2.5, rgb(0.14, 0.14, 0.16));
            draw_circle(c.x + d.x * 20.0, c.y - 2.0 + d.y * 20.0, 1.8, rgb(0.1, 0.1, 0.12));
        }
        Kind::Zombie => {
            let green = rgb(0.36, 0.52, 0.30);
            draw_rectangle(c.x - 3.0, c.y - 3.0, 6.0, 8.0, green);
            draw_rectangle(c.x - 3.0, c.y - 3.0, 2.0, 8.0, shade(green, 1.2));
            draw_line(c.x, c.y - 1.0, c.x + d.x * 7.0, c.y - 1.0 + d.y * 7.0, 2.0, shade(green, 0.85)); // reaching arm
            draw_circle(c.x, c.y - 5.5, 3.0, rgb(0.56, 0.63, 0.46)); // sickly head
            draw_circle(c.x - 0.8, c.y - 5.5, 0.7, rgb(0.85, 0.15, 0.1)); // eye
        }
        Kind::Werewolf => {
            let fur = rgb(0.42, 0.32, 0.22);
            draw_rectangle(c.x - 4.0, c.y - 1.0, 8.0, 6.0, fur); // hunched body
            draw_line(c.x - d.x * 5.0, c.y - d.y * 5.0, c.x - d.x * 9.0, c.y - 4.0 - d.y * 9.0, 2.0, shade(fur, 0.8)); // tail
            let hc = vec2(c.x + d.x * 4.0, c.y - 2.0 + d.y * 4.0);
            draw_circle(hc.x, hc.y, 3.0, shade(fur, 1.1)); // head thrust forward
            draw_triangle(hc + vec2(-2.0, -2.0), hc + vec2(-3.0, -6.0), hc + vec2(0.5, -2.5), fur); // ears
            draw_triangle(hc + vec2(2.0, -2.0), hc + vec2(3.0, -6.0), hc + vec2(-0.5, -2.5), fur);
            draw_circle(hc.x, hc.y, 0.8, rgb(0.95, 0.2, 0.1)); // red eye
        }
        Kind::Vampire => {
            let cape = rgb(0.12, 0.05, 0.10);
            draw_triangle(c + vec2(-5.5, -1.0), c + vec2(5.5, -1.0), c + vec2(0.0, 7.0), cape); // cloak
            draw_rectangle(c.x - 2.5, c.y - 3.0, 5.0, 8.0, rgb(0.18, 0.10, 0.14)); // body
            draw_triangle(c + vec2(-3.0, -3.5), c + vec2(0.5, -2.0), c + vec2(-1.0, 0.0), cape); // collar
            draw_triangle(c + vec2(3.0, -3.5), c + vec2(-0.5, -2.0), c + vec2(1.0, 0.0), cape);
            draw_circle(c.x, c.y - 5.6, 2.6, rgb(0.82, 0.80, 0.84)); // pallid head
            draw_circle(c.x - 1.0, c.y - 5.6, 0.6, rgb(0.95, 0.15, 0.15));
            draw_circle(c.x + 1.0, c.y - 5.6, 0.6, rgb(0.95, 0.15, 0.15));
        }
        Kind::Lich => {
            glow(c - vec2(0.0, 6.0), 13.0, rgb(0.4, 0.95, 0.5), 0.55);
            draw_triangle(c + vec2(-7.0, -2.0), c + vec2(7.0, -2.0), c + vec2(0.0, 12.0), rgb(0.10, 0.12, 0.16)); // robe
            draw_rectangle(c.x - 4.0, c.y - 8.0, 8.0, 8.0, rgb(0.14, 0.16, 0.20));
            draw_circle(c.x, c.y - 10.0, 4.0, rgb(0.84, 0.87, 0.80)); // skull
            draw_circle(c.x - 1.6, c.y - 10.5, 1.1, rgb(0.3, 1.0, 0.45));
            draw_circle(c.x + 1.6, c.y - 10.5, 1.1, rgb(0.3, 1.0, 0.45));
            draw_line(c.x + 6.0, c.y + 4.0, c.x + 6.0, c.y - 15.0, 1.6, rgb(0.3, 0.25, 0.2)); // staff
            draw_circle(c.x + 6.0, c.y - 16.0, 2.6, rgb(0.4, 1.0, 0.5));
        }
        Kind::Warlock => {
            glow(c - vec2(0.0, 9.0), 20.0, rgb(0.95, 0.2, 0.15), 0.75);
            draw_triangle(c + vec2(-10.0, -2.0), c + vec2(10.0, -2.0), c + vec2(0.0, 16.0), rgb(0.10, 0.04, 0.08)); // cloak
            draw_rectangle(c.x - 6.0, c.y - 13.0, 12.0, 13.0, rgb(0.16, 0.06, 0.10));
            draw_triangle(c + vec2(-6.0, -12.0), c + vec2(-9.0, -21.0), c + vec2(-2.0, -13.0), rgb(0.10, 0.04, 0.08)); // horns
            draw_triangle(c + vec2(6.0, -12.0), c + vec2(9.0, -21.0), c + vec2(2.0, -13.0), rgb(0.10, 0.04, 0.08));
            draw_circle(c.x, c.y - 15.0, 4.6, rgb(0.55, 0.45, 0.50)); // pallid face
            draw_circle(c.x - 2.0, c.y - 15.0, 1.3, rgb(1.0, 0.2, 0.15));
            draw_circle(c.x + 2.0, c.y - 15.0, 1.3, rgb(1.0, 0.2, 0.15));
        }
        Kind::HellTank => {
            // a corrupted war-hulk: blackened iron, glowing cursed seams, a heavy gun
            glow(c, 13.0, rgb(0.85, 0.25, 0.12), 0.5);
            for side in [-11.0, 8.0] {
                draw_rectangle(c.x + side, c.y - 7.0, 4.0, 16.0, rgb(0.08, 0.07, 0.07)); // treads
            }
            vgrad_box(c.x - 8.0, c.y - 6.0, 16.0, 14.0, rgb(0.20, 0.16, 0.17), rgb(0.07, 0.05, 0.06)); // hull
            draw_line(c.x - 7.0, c.y - 1.0, c.x + 7.0, c.y - 1.0, 1.0, rgb(0.85, 0.25, 0.12)); // cursed seam
            draw_line(c.x - 7.0, c.y + 3.0, c.x + 7.0, c.y + 3.0, 1.0, rgb(0.65, 0.15, 0.10));
            for i in 0..4 {
                let t = i as f32 / 4.0;
                draw_circle(c.x - t * 1.5, c.y - t * 1.5, 6.0 * (1.0 - t * 0.6), rgb(0.14 + 0.10 * t, 0.10, 0.11)); // turret
            }
            draw_circle(c.x, c.y - 2.0, 1.6, rgb(1.0, 0.55, 0.2)); // hot core
            draw_line(c.x, c.y, c.x + d.x * 16.0, c.y + d.y * 16.0, 3.0, rgb(0.10, 0.08, 0.09)); // cannon
            draw_circle(c.x + d.x * 16.0, c.y + d.y * 16.0, 1.8, rgb(0.95, 0.35, 0.12)); // muzzle glow
        }
        Kind::Champion => {
            // elite hero: gold aura, team-colored armor, plumed helm, big sword
            glow(c - vec2(0.0, 5.0), 12.0, rgb(0.95, 0.82, 0.35), 0.45);
            draw_triangle(c + vec2(-5.0, -3.0), c + vec2(5.0, -3.0), c + vec2(0.0, 8.0), rgb(0.55, 0.12, 0.14)); // cape
            draw_rectangle(c.x - 4.0, c.y - 4.0, 8.0, 10.0, lit); // armored torso
            draw_rectangle(c.x - 4.0, c.y - 4.0, 3.0, 10.0, shade(col, 1.55)); // edge highlight
            draw_rectangle(c.x - 4.5, c.y - 2.0, 1.8, 6.0, shade(col, 0.5)); // pauldron
            draw_circle(c.x, c.y - 6.3, 3.0, rgb(0.86, 0.71, 0.56)); // head
            draw_rectangle(c.x - 3.3, c.y - 8.6, 6.6, 3.0, shade(col, 0.7)); // helm
            draw_triangle(c + vec2(0.0, -12.5), c + vec2(2.0, -9.0), c + vec2(-2.0, -9.0), rgb(0.95, 0.82, 0.30)); // plume
            draw_line(c.x, c.y - 1.0, c.x + d.x * 14.0, c.y - 1.0 + d.y * 14.0, 2.6, rgb(0.86, 0.87, 0.92)); // sword
            draw_circle(c.x + d.x * 14.0, c.y - 1.0 + d.y * 14.0, 1.6, rgb(1.0, 1.0, 1.0)); // tip glint
        }
        Kind::Deer => {
            // peaceful wildlife: a little brown deer, side-on
            let fur = rgb(0.55, 0.40, 0.26);
            draw_line(c.x - 3.0, c.y + 2.0, c.x - 3.0, c.y + 7.0, 1.4, shade(fur, 0.7)); // legs
            draw_line(c.x + 2.0, c.y + 2.0, c.x + 2.0, c.y + 7.0, 1.4, shade(fur, 0.7));
            draw_rectangle(c.x - 4.0, c.y - 1.0, 8.0, 5.0, fur); // body
            draw_rectangle(c.x - 4.0, c.y - 1.0, 8.0, 2.0, shade(fur, 1.2)); // back highlight
            let hc = vec2(c.x + d.x * 4.5, c.y - 3.0); // head leans the way it faces
            draw_circle(hc.x, hc.y, 2.2, shade(fur, 1.05));
            draw_line(c.x + d.x * 4.0, c.y - 1.0, hc.x, hc.y, 1.6, fur); // neck
            // little antlers
            draw_line(hc.x, hc.y - 1.0, hc.x - 1.5, hc.y - 4.0, 1.0, rgb(0.78, 0.66, 0.48));
            draw_line(hc.x, hc.y - 1.0, hc.x + 1.5, hc.y - 4.0, 1.0, rgb(0.78, 0.66, 0.48));
            draw_line(c.x - 4.0, c.y + 0.5, c.x - 7.0, c.y - 1.0, 1.2, fur); // tail tuft
        }
        _ => {
            draw_circle(c.x, c.y, 5.0, col);
        }
    }
    draw_healthbar(e, g - vec2(0.0, 20.0), TW * 0.32, selected);
}

/// A compact, readable emblem for a `Kind`, drawn for the build/unit sidebar
/// buttons. Side-on like the in-world sprites so they're recognisable; `c` is the
/// icon centre, `s` the half-extent (the emblem spans roughly `2*s`). `team` tints
/// player kit; `tick` drives a little life (glints, pulses).
pub fn build_icon(kind: Kind, c: Vec2, s: f32, team: Color, tick: u32) {
    use Kind::*;
    let steel = rgb(0.60, 0.64, 0.72);
    let dark = rgb(0.20, 0.21, 0.26);
    let gun = rgb(0.11, 0.11, 0.13);
    let skin = rgb(0.86, 0.72, 0.56);
    let gold = rgb(0.93, 0.78, 0.28);
    let pulse = 0.5 + 0.5 * (tick as f32 * 0.09).sin();

    // a small side-view trooper facing right; weapon drawn by the caller after
    let soldier = |body: Color, helmet: Color| {
        draw_rectangle(c.x - 0.32 * s, c.y - 0.2 * s, 0.5 * s, 0.85 * s, body); // torso
        draw_rectangle(c.x - 0.32 * s, c.y - 0.2 * s, 0.18 * s, 0.85 * s, shade(body, 1.3)); // lit edge
        draw_rectangle(c.x - 0.28 * s, c.y + 0.62 * s, 0.16 * s, 0.4 * s, shade(body, 0.6)); // legs
        draw_rectangle(c.x + 0.02 * s, c.y + 0.62 * s, 0.16 * s, 0.4 * s, shade(body, 0.6));
        draw_circle(c.x - 0.06 * s, c.y - 0.45 * s, 0.3 * s, skin); // head
        draw_rectangle(c.x - 0.36 * s, c.y - 0.66 * s, 0.6 * s, 0.2 * s, helmet); // helmet
    };
    // a small side-view tank/vehicle; returns the muzzle anchor for the barrel
    let tank = |hull: Color| {
        draw_rectangle(c.x - 0.85 * s, c.y + 0.3 * s, 1.7 * s, 0.34 * s, gun); // tread
        for i in 0..4 {
            draw_circle(c.x - 0.7 * s + i as f32 * 0.45 * s, c.y + 0.47 * s, 0.12 * s, dark);
        }
        vgrad_box(c.x - 0.72 * s, c.y - 0.32 * s, 1.44 * s, 0.66 * s, shade(hull, 1.22), shade(hull, 0.72)); // hull
        draw_circle(c.x - 0.05 * s, c.y - 0.12 * s, 0.42 * s, shade(hull, 1.05)); // turret
    };

    match kind {
        // ---- power / economy ----
        PowerPlant => {
            for dx in [-0.45_f32, 0.45] {
                let x = c.x + dx * s;
                quad(vec2(x - 0.3 * s, c.y + 0.8 * s), vec2(x + 0.3 * s, c.y + 0.8 * s), vec2(x + 0.22 * s, c.y - 0.8 * s), vec2(x - 0.22 * s, c.y - 0.8 * s), steel);
                ellipse(x, c.y - 0.8 * s, 0.22 * s, 0.09 * s, shade(steel, 1.25));
            }
            draw_line(c.x - 0.18 * s, c.y - 0.1 * s, c.x + 0.05 * s, c.y - 0.1 * s, 0.16 * s, gold); // spark bolt
            draw_line(c.x + 0.05 * s, c.y - 0.1 * s, c.x - 0.05 * s, c.y + 0.25 * s, 0.16 * s, gold);
        }
        Reactor => {
            draw_circle(c.x, c.y, 0.92 * s, dark);
            draw_circle_lines(c.x, c.y, 0.92 * s, 1.5, rgb(0.3, 0.85, 0.45));
            draw_circle(c.x, c.y, 0.22 * s, mix(rgb(0.3, 1.0, 0.5), rgb(0.7, 1.0, 0.8), pulse));
            for k in 0..3 {
                let a = k as f32 * 2.094 + tick as f32 * 0.05;
                draw_circle(c.x + a.cos() * 0.6 * s, c.y + a.sin() * 0.3 * s, 0.1 * s, rgb(0.5, 1.0, 0.6));
            }
        }
        Refinery => {
            draw_rectangle(c.x - 0.8 * s, c.y - 0.2 * s, 1.0 * s, 1.0 * s, steel); // tank block
            ellipse(c.x - 0.3 * s, c.y - 0.2 * s, 0.5 * s, 0.16 * s, shade(steel, 1.2));
            for gx in [-0.55_f32, -0.3, -0.05] {
                draw_circle(c.x + gx * s, c.y + 0.6 * s, 0.1 * s, gold); // ore
            }
            draw_rectangle(c.x + 0.3 * s, c.y + 0.1 * s, 0.5 * s, 0.7 * s, dark); // pipe stack
        }
        OreSilo => {
            draw_rectangle(c.x - 0.45 * s, c.y - 0.5 * s, 0.9 * s, 1.3 * s, steel);
            ellipse(c.x, c.y - 0.5 * s, 0.45 * s, 0.18 * s, shade(steel, 1.25)); // domed top
            draw_rectangle(c.x - 0.45 * s, c.y + 0.05 * s, 0.9 * s, 0.22 * s, gold); // ore band
        }
        FoodSilo => {
            draw_rectangle(c.x - 0.5 * s, c.y - 0.5 * s, 1.0 * s, 1.3 * s, rgb(0.80, 0.72, 0.52)); // grain bin
            ellipse(c.x, c.y - 0.5 * s, 0.5 * s, 0.2 * s, rgb(0.86, 0.55, 0.30)); // domed top
            for k in -1..=1 {
                let sx = c.x + k as f32 * 0.28 * s;
                draw_line(sx, c.y + 0.7 * s, sx + k as f32 * 0.18 * s, c.y - 0.2 * s, 0.14 * s, rgb(0.85, 0.72, 0.25)); // wheat
                draw_circle(sx + k as f32 * 0.18 * s, c.y - 0.25 * s, 0.16 * s, rgb(0.95, 0.82, 0.3));
            }
        }
        // ---- production buildings ----
        Barracks => {
            draw_rectangle(c.x - 0.8 * s, c.y - 0.45 * s, 1.6 * s, 1.25 * s, shade(team, 0.85));
            draw_rectangle(c.x - 0.8 * s, c.y - 0.45 * s, 1.6 * s, 0.25 * s, shade(team, 1.2)); // eaves
            draw_rectangle(c.x - 0.22 * s, c.y + 0.05 * s, 0.44 * s, 0.75 * s, dark); // door
            draw_circle(c.x, c.y - 0.2 * s, 0.2 * s, steel); // helmet emblem
        }
        Factory => {
            draw_rectangle(c.x - 0.85 * s, c.y - 0.3 * s, 1.7 * s, 1.1 * s, shade(team, 0.8));
            for i in 0..3 {
                let x = c.x - 0.6 * s + i as f32 * 0.6 * s;
                draw_rectangle(x - 0.18 * s, c.y + 0.1 * s, 0.36 * s, 0.7 * s, dark); // roll doors
            }
            draw_circle(c.x + 0.5 * s, c.y - 0.45 * s, 0.26 * s, steel); // gear
            draw_circle(c.x + 0.5 * s, c.y - 0.45 * s, 0.12 * s, dark);
        }
        TechCenter => {
            draw_rectangle(c.x - 0.7 * s, c.y - 0.5 * s, 1.4 * s, 1.3 * s, shade(team, 0.85));
            draw_circle(c.x, c.y + 0.1 * s, 0.34 * s, rgb(0.3, 0.7, 0.95)); // dome
            draw_line(c.x, c.y - 0.5 * s, c.x, c.y - 0.95 * s, 0.12 * s, steel);
            draw_circle(c.x, c.y - 0.95 * s, 0.14 * s, mix(rgb(0.4, 0.9, 1.0), WHITE, pulse)); // antenna bead
        }
        Radar => {
            draw_rectangle(c.x - 0.2 * s, c.y + 0.0 * s, 0.4 * s, 0.85 * s, steel); // mast
            draw_circle(c.x, c.y - 0.3 * s, 0.6 * s, dark);
            draw_circle_lines(c.x, c.y - 0.3 * s, 0.6 * s, 1.2, rgb(0.4, 0.8, 0.95));
            let a = tick as f32 * 0.08;
            draw_line(c.x, c.y - 0.3 * s, c.x + a.cos() * 0.55 * s, c.y - 0.3 * s + a.sin() * 0.55 * s, 0.1 * s, rgb(0.5, 1.0, 0.7)); // sweep
        }
        // ---- defences ----
        GuardTower | Pillbox | CannonTower | MissileTurret => {
            draw_rectangle(c.x - 0.5 * s, c.y - 0.1 * s, 1.0 * s, 0.9 * s, shade(team, 0.8)); // base
            draw_circle(c.x, c.y - 0.1 * s, 0.42 * s, steel); // turret
            match kind {
                MissileTurret => {
                    for dx in [-0.18_f32, 0.18] {
                        draw_line(c.x + dx * s, c.y - 0.1 * s, c.x + dx * s + 0.7 * s, c.y - 0.6 * s, 0.16 * s, dark);
                        draw_circle(c.x + dx * s + 0.7 * s, c.y - 0.6 * s, 0.1 * s, rgb(0.95, 0.4, 0.2));
                    }
                }
                CannonTower => {
                    draw_line(c.x, c.y - 0.1 * s, c.x + 0.95 * s, c.y - 0.1 * s, 0.26 * s, gun);
                }
                Pillbox => {
                    draw_rectangle(c.x - 0.1 * s, c.y - 0.25 * s, 0.7 * s, 0.16 * s, gun); // slit + stub
                }
                _ => {
                    draw_line(c.x, c.y - 0.1 * s, c.x + 0.8 * s, c.y - 0.2 * s, 0.14 * s, gun); // MG
                }
            }
        }
        Obelisk => {
            quad(vec2(c.x - 0.22 * s, c.y + 0.85 * s), vec2(c.x + 0.22 * s, c.y + 0.85 * s), vec2(c.x + 0.1 * s, c.y - 0.85 * s), vec2(c.x - 0.1 * s, c.y - 0.85 * s), rgb(0.30, 0.18, 0.42));
            glow(vec2(c.x, c.y - 0.85 * s), 0.5 * s, rgb(0.72, 0.42, 1.0), pulse);
            draw_circle(c.x, c.y - 0.85 * s, 0.16 * s, rgb(0.88, 0.62, 1.0));
        }
        MissileSilo => {
            draw_rectangle(c.x - 0.7 * s, c.y + 0.1 * s, 1.4 * s, 0.8 * s, shade(team, 0.7)); // bunker
            draw_rectangle(c.x - 0.28 * s, c.y - 0.7 * s, 0.56 * s, 1.0 * s, steel); // missile body
            draw_triangle(vec2(c.x - 0.28 * s, c.y - 0.7 * s), vec2(c.x + 0.28 * s, c.y - 0.7 * s), vec2(c.x, c.y - 1.15 * s), rgb(0.9, 0.3, 0.2)); // warhead
            glow(vec2(c.x, c.y - 1.0 * s), 0.3 * s, rgb(1.0, 0.4, 0.2), 0.4 + 0.4 * pulse);
        }
        // ---- support / civic ----
        RepairDepot => {
            draw_rectangle(c.x - 0.7 * s, c.y - 0.2 * s, 1.4 * s, 1.0 * s, shade(team, 0.8));
            // wrench
            draw_line(c.x - 0.3 * s, c.y + 0.5 * s, c.x + 0.4 * s, c.y - 0.4 * s, 0.18 * s, steel);
            draw_circle(c.x + 0.42 * s, c.y - 0.42 * s, 0.2 * s, steel);
            draw_circle(c.x + 0.42 * s, c.y - 0.42 * s, 0.09 * s, shade(team, 0.8));
        }
        MedBay => {
            draw_rectangle(c.x - 0.7 * s, c.y - 0.5 * s, 1.4 * s, 1.3 * s, rgb(0.92, 0.92, 0.95));
            draw_rectangle(c.x - 0.12 * s, c.y - 0.25 * s, 0.24 * s, 0.8 * s, rgb(0.9, 0.2, 0.2)); // red cross
            draw_rectangle(c.x - 0.4 * s, c.y + 0.03 * s, 0.8 * s, 0.24 * s, rgb(0.9, 0.2, 0.2));
        }
        House => {
            draw_rectangle(c.x - 0.6 * s, c.y - 0.05 * s, 1.2 * s, 0.85 * s, rgb(0.80, 0.74, 0.62)); // walls
            draw_triangle(vec2(c.x - 0.72 * s, c.y - 0.05 * s), vec2(c.x + 0.72 * s, c.y - 0.05 * s), vec2(c.x, c.y - 0.7 * s), shade(team, 1.0)); // roof
            draw_rectangle(c.x - 0.13 * s, c.y + 0.3 * s, 0.26 * s, 0.5 * s, dark); // door
        }
        Farm => {
            draw_rectangle(c.x - 0.8 * s, c.y - 0.3 * s, 1.6 * s, 1.1 * s, rgb(0.36, 0.26, 0.16)); // field
            for i in 0..4 {
                let x = c.x - 0.6 * s + i as f32 * 0.4 * s;
                draw_line(x, c.y - 0.15 * s, x, c.y + 0.7 * s, 0.08 * s, rgb(0.5, 0.7, 0.25)); // crop rows
                draw_circle(x, c.y - 0.2 * s, 0.08 * s, gold);
            }
        }
        Wall => {
            for (bx, by) in [(-0.45_f32, -0.3_f32), (0.15, -0.3), (-0.15, 0.1), (0.45, 0.1), (-0.45, 0.5), (0.15, 0.5)] {
                draw_rectangle(c.x + bx * s, c.y + by * s, 0.55 * s, 0.34 * s, steel);
                draw_rectangle_lines(c.x + bx * s, c.y + by * s, 0.55 * s, 0.34 * s, 1.0, dark);
            }
        }
        Gate => {
            draw_rectangle(c.x - 0.75 * s, c.y - 0.5 * s, 0.3 * s, 1.3 * s, steel); // posts
            draw_rectangle(c.x + 0.45 * s, c.y - 0.5 * s, 0.3 * s, 1.3 * s, steel);
            draw_rectangle(c.x - 0.45 * s, c.y - 0.45 * s, 0.9 * s, 0.2 * s, gold); // top bar
        }
        Road => {
            draw_rectangle(c.x - 0.85 * s, c.y - 0.3 * s, 1.7 * s, 0.7 * s, rgb(0.34, 0.32, 0.30));
            for i in 0..3 {
                draw_rectangle(c.x - 0.55 * s + i as f32 * 0.55 * s, c.y + 0.0 * s, 0.28 * s, 0.08 * s, gold); // dashes
            }
        }
        Starship => {
            quad(vec2(c.x - 0.3 * s, c.y + 0.8 * s), vec2(c.x + 0.3 * s, c.y + 0.8 * s), vec2(c.x + 0.26 * s, c.y - 0.4 * s), vec2(c.x - 0.26 * s, c.y - 0.4 * s), steel);
            draw_triangle(vec2(c.x - 0.26 * s, c.y - 0.4 * s), vec2(c.x + 0.26 * s, c.y - 0.4 * s), vec2(c.x, c.y - 0.95 * s), shade(steel, 1.2));
            glow(vec2(c.x, c.y + 0.85 * s), 0.4 * s, rgb(1.0, 0.55, 0.2), pulse);
        }

        // ---- infantry ----
        Rifleman => {
            soldier(team, shade(team, 0.7));
            draw_line(c.x + 0.1 * s, c.y - 0.1 * s, c.x + 0.95 * s, c.y - 0.1 * s, 0.12 * s, gun);
        }
        Grenadier => {
            soldier(shade(team, 0.95), rgb(0.3, 0.42, 0.22));
            draw_circle(c.x + 0.7 * s, c.y - 0.2 * s, 0.2 * s, rgb(0.34, 0.5, 0.22));
        }
        Rocketeer => {
            soldier(team, shade(team, 0.7));
            draw_line(c.x - 0.1 * s, c.y - 0.2 * s, c.x + 0.9 * s, c.y - 0.45 * s, 0.2 * s, rgb(0.28, 0.32, 0.28));
            draw_circle(c.x + 0.92 * s, c.y - 0.45 * s, 0.16 * s, rgb(0.95, 0.4, 0.15));
        }
        Flamer => {
            soldier(rgb(0.7, 0.25, 0.15), rgb(0.5, 0.18, 0.12));
            draw_line(c.x + 0.1 * s, c.y - 0.1 * s, c.x + 0.85 * s, c.y - 0.1 * s, 0.14 * s, gun);
            draw_circle(c.x + 0.95 * s, c.y - 0.1 * s, 0.22 * s, rgb(1.0, 0.55, 0.15));
            draw_circle(c.x + 0.95 * s, c.y - 0.1 * s, 0.11 * s, rgb(1.0, 0.88, 0.45));
        }
        Sniper => {
            soldier(rgb(0.42, 0.48, 0.34), rgb(0.32, 0.36, 0.26));
            draw_line(c.x + 0.05 * s, c.y - 0.15 * s, c.x + 1.05 * s, c.y - 0.15 * s, 0.1 * s, gun);
            draw_circle(c.x + 0.45 * s, c.y - 0.28 * s, 0.08 * s, rgb(0.6, 0.9, 1.0)); // scope glint
        }
        Engineer => {
            soldier(gold, rgb(0.8, 0.6, 0.1));
            draw_rectangle(c.x + 0.5 * s, c.y - 0.05 * s, 0.35 * s, 0.35 * s, steel); // toolbox
        }
        // ---- vehicles ----
        Harvester => {
            draw_rectangle(c.x - 0.85 * s, c.y + 0.3 * s, 1.7 * s, 0.34 * s, gun); // tread
            vgrad_box(c.x - 0.75 * s, c.y - 0.4 * s, 1.2 * s, 0.8 * s, shade(team, 1.15), shade(team, 0.7)); // hopper
            draw_rectangle(c.x - 0.6 * s, c.y - 0.25 * s, 0.9 * s, 0.4 * s, gold); // ore load
            draw_rectangle(c.x + 0.45 * s, c.y - 0.2 * s, 0.4 * s, 0.5 * s, shade(team, 0.6)); // cab
        }
        Buggy => {
            for dx in [-0.5_f32, 0.5] {
                draw_circle(c.x + dx * s, c.y + 0.5 * s, 0.22 * s, dark);
            }
            vgrad_box(c.x - 0.6 * s, c.y - 0.25 * s, 1.2 * s, 0.7 * s, shade(team, 1.2), shade(team, 0.7));
            draw_rectangle(c.x - 0.2 * s, c.y - 0.1 * s, 0.5 * s, 0.3 * s, rgb(0.4, 0.55, 0.65)); // cockpit
            draw_line(c.x, c.y, c.x + 0.85 * s, c.y, 0.1 * s, gun);
        }
        Tank => {
            tank(team);
            draw_line(c.x, c.y - 0.12 * s, c.x + 0.95 * s, c.y - 0.12 * s, 0.18 * s, gun);
        }
        HeavyTank => {
            tank(shade(team, 0.92));
            for oy in [-0.26_f32, 0.02] {
                draw_line(c.x, c.y - 0.12 * s + oy * s, c.x + 0.95 * s, c.y - 0.12 * s + oy * s, 0.14 * s, gun);
            }
        }
        Artillery => {
            tank(shade(team, 0.85));
            draw_line(c.x - 0.1 * s, c.y + 0.0 * s, c.x + 0.9 * s, c.y - 0.7 * s, 0.18 * s, gun); // raised barrel
        }
        Champion => {
            glow(c, 0.85 * s, gold, 0.4 + 0.3 * pulse);
            draw_triangle(vec2(c.x - 0.4 * s, c.y - 0.1 * s), vec2(c.x + 0.4 * s, c.y - 0.1 * s), vec2(c.x, c.y + 0.6 * s), rgb(0.55, 0.12, 0.14)); // cape
            soldier(shade(team, 1.1), shade(team, 0.7));
            draw_triangle(vec2(c.x - 0.06 * s, c.y - 1.0 * s), vec2(c.x + 0.18 * s, c.y - 0.66 * s), vec2(c.x - 0.3 * s, c.y - 0.66 * s), gold); // plume
            draw_line(c.x + 0.1 * s, c.y - 0.1 * s, c.x + 1.0 * s, c.y - 0.1 * s, 0.16 * s, rgb(0.86, 0.87, 0.92)); // sword
        }
        _ => {
            draw_rectangle(c.x - 0.6 * s, c.y - 0.6 * s, 1.2 * s, 1.2 * s, steel);
        }
    }
}

fn vgrad_box(x: f32, y: f32, w: f32, h: f32, top: Color, bot: Color) {
    let n = (h / 1.5).ceil().clamp(3.0, 14.0) as i32;
    for i in 0..n {
        let t = i as f32 / (n - 1).max(1) as f32;
        draw_rectangle(x, y + h * i as f32 / n as f32, w, h / n as f32 + 0.6, mix(top, bot, t));
    }
    draw_rectangle(x, y, w, 1.5, shade(top, 1.3));
}

fn draw_healthbar(e: &ironvein_sim::Ent, at: Vec2, width: f32, selected: bool) {
    let st = stats(e.kind);
    if !(selected || (e.hp < st.max_hp && e.hp > 0)) {
        return;
    }
    let frac = (e.hp.max(0) as f32 / st.max_hp as f32).clamp(0.0, 1.0);
    let bw = width.max(14.0);
    let hb = mix(rgb(0.9, 0.15, 0.12), rgb(0.25, 0.9, 0.3), frac);
    draw_rectangle(at.x - bw * 0.5 - 1.0, at.y - 1.0, bw + 2.0, 5.0, Color::new(0.04, 0.04, 0.05, 0.92));
    draw_rectangle(at.x - bw * 0.5, at.y, bw * frac, 3.0, hb);
    draw_rectangle(at.x - bw * 0.5, at.y, bw * frac, 1.0, shade(hb, 1.3));
}

/// Diamond highlight for build placement (returns nothing; draws at tile).
pub fn draw_footprint(at: Tp, fw: i32, fh: i32, cam: Vec2, ok: bool) {
    let col = if ok { Color::new(0.2, 1.0, 0.3, 0.35) } else { Color::new(1.0, 0.2, 0.2, 0.35) };
    let line = Color::new(col.r, col.g, col.b, 0.95);
    let n = tile_to_screen(at.x as f32, at.y as f32) - cam;
    let e = tile_to_screen(at.x as f32 + fw as f32, at.y as f32) - cam;
    let s = tile_to_screen(at.x as f32 + fw as f32, at.y as f32 + fh as f32) - cam;
    let w = tile_to_screen(at.x as f32, at.y as f32 + fh as f32) - cam;
    quad(n, e, s, w, col);
    quad_outline(n, e, s, w, line);
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

pub struct Effect {
    pub ev: VisEvent,
    pub age: f32,
}

pub fn effect_ttl(ev: &VisEvent) -> f32 {
    match ev {
        VisEvent::Shot { rocket, .. } => if *rocket { 0.30 } else { 0.14 },
        VisEvent::Die { big, .. } => if *big { 0.7 } else { 0.45 },
        VisEvent::Built { .. } => 0.6,
        VisEvent::Captured { .. } => 0.8,
        VisEvent::Unload { .. } => 0.8,
        VisEvent::Pickup { .. } => 0.9,
        VisEvent::Nuke { .. } => 2.4,
    }
}

pub fn draw_effect(fx: &Effect, cam: Vec2) {
    let t = fx.age / effect_ttl(&fx.ev);
    match &fx.ev {
        VisEvent::Shot { from, to, rocket } => {
            let a = fpx(*from) - cam + vec2(0.0, -8.0);
            let b = fpx(*to) - cam + vec2(0.0, -8.0);
            if *rocket {
                let p = a.lerp(b, t.min(1.0));
                let dir = (b - a).normalize_or_zero();
                draw_line(a.x, a.y, p.x, p.y, 2.0, Color::new(0.7, 0.7, 0.72, 0.35 * (1.0 - t)));
                glow(p, 7.0, rgb(1.0, 0.6, 0.2), 1.0);
                draw_circle(p.x, p.y, 2.4, rgb(1.0, 0.55, 0.15));
                draw_circle(p.x, p.y, 1.2, rgb(1.0, 0.92, 0.7));
                draw_circle(p.x - dir.x * 5.0, p.y - dir.y * 5.0, 2.2, Color::new(1.0, 0.7, 0.3, 0.6));
                draw_circle(p.x - dir.x * 8.0, p.y - dir.y * 8.0, 1.4, Color::new(0.85, 0.8, 0.7, 0.4));
            } else {
                glow(a, 5.0, rgb(1.0, 0.85, 0.4), 1.0 - t); // muzzle flash
                draw_line(a.x, a.y, b.x, b.y, 2.4, Color::new(1.0, 0.95, 0.55, (1.0 - t) * 0.5));
                draw_line(a.x, a.y, b.x, b.y, 1.0, Color::new(1.0, 1.0, 0.85, (1.0 - t) * 0.95));
                glow(b, 4.0, rgb(1.0, 0.95, 0.7), (1.0 - t) * 0.8); // impact spark
            }
        }
        VisEvent::Die { at, big } => {
            let c = fpx(*at) - cam + vec2(0.0, -8.0);
            let r0 = if *big { 20.0 } else { 10.0 };
            glow(c, r0 * (1.2 + t * 1.5), rgb(1.0, 0.55, 0.18), (1.0 - t) * 1.2); // light burst
            draw_circle_lines(c.x, c.y, r0 * (0.4 + t * 2.0), 2.0, Color::new(1.0, 0.85, 0.5, (1.0 - t) * 0.55)); // shockwave
            // fireball: smoke shell, flame body, white-hot core (rising)
            draw_circle(c.x, c.y - t * 6.0, r0 * (0.5 + t * 1.2), Color::new(0.18, 0.16, 0.15, (1.0 - t) * 0.6));
            draw_circle(c.x, c.y - t * 4.0, r0 * (0.35 + t * 0.85), Color::new(1.0, 0.42 * (1.0 - t), 0.08, (1.0 - t) * 0.95));
            draw_circle(c.x, c.y - t * 3.0, r0 * 0.45 * (0.3 + t), Color::new(1.0, 0.92, 0.5, (1.0 - t).powf(0.5)));
            // flung embers with a touch of gravity
            for i in 0..12 {
                let ang = i as f32 / 12.0 * std::f32::consts::TAU + r0;
                let dd = r0 * (0.5 + t * 1.7);
                let ex = c.x + ang.cos() * dd;
                let ey = c.y + ang.sin() * dd * 0.6 + t * t * 8.0;
                let ec = mix(rgb(1.0, 0.8, 0.3), rgb(0.3, 0.25, 0.2), t);
                draw_circle(ex, ey, 1.6 * (1.0 - t * 0.5), Color::new(ec.r, ec.g, ec.b, 1.0 - t));
            }
        }
        VisEvent::Built { at } => {
            let c = fpx(*at) - cam;
            draw_circle_lines(c.x, c.y, 8.0 + t * 24.0, 2.0, Color::new(0.4, 1.0, 0.5, 1.0 - t));
            glow(c, 14.0, rgb(0.4, 1.0, 0.5), (1.0 - t) * 0.6);
        }
        VisEvent::Captured { at } => {
            let c = fpx(*at) - cam;
            draw_circle_lines(c.x, c.y, 7.0 + t * 28.0, 2.0, Color::new(1.0, 1.0, 1.0, 1.0 - t));
            draw_circle_lines(c.x, c.y, 3.0 + t * 18.0, 1.5, Color::new(1.0, 0.9, 0.3, 1.0 - t));
            glow(c, 16.0, rgb(1.0, 0.95, 0.6), (1.0 - t) * 0.7);
        }
        VisEvent::Unload { at, .. } => {
            let c = fpx(*at) - cam;
            let y = c.y - 10.0 - t * 18.0;
            draw_text("+$", c.x - 6.0, y, 18.0, Color::new(0.6, 1.0, 0.4, 1.0 - t));
        }
        VisEvent::Pickup { at, amount, kind } => {
            let c = fpx(*at) - cam;
            let y = c.y - 8.0 - t * 22.0;
            // essence purple, berries/meat (food) green
            let (glo, txt, pre) = if *kind == 0 {
                (rgb(0.72, 0.42, 0.9), Color::new(0.82, 0.6, 1.0, 1.0 - t), "+")
            } else {
                (rgb(0.45, 0.95, 0.4), Color::new(0.6, 1.0, 0.5, 1.0 - t), "+")
            };
            glow(vec2(c.x, c.y), 8.0 * (1.0 - t), glo, (1.0 - t) * 0.7);
            draw_text(&format!("{pre}{amount}"), c.x - 5.0, y, 17.0, txt);
        }
        VisEvent::Nuke { at } => {
            // ground zero in iso, plus the blast radius footprint
            let c = fpx(*at) - cam;
            let rpx = ironvein_sim::stats::NUKE_RADIUS as f32 * TW * 0.5;
            // expanding shockwave ring on the ground
            let ring = (t * 2.0).min(1.0);
            draw_circle_lines(c.x, c.y, rpx * ring, 3.0, Color::new(1.0, 0.8, 0.4, (1.0 - ring) * 0.8));
            // white flash, fading fast
            if t < 0.18 {
                let f = 1.0 - t / 0.18;
                draw_circle(c.x, c.y, rpx * 1.2, Color::new(1.0, 1.0, 0.95, f * 0.8));
            }
            // rising mushroom: a stem and a billowing cap
            let rise = t * 90.0;
            let stem_y = c.y - rise;
            draw_rectangle(c.x - 7.0, stem_y, 14.0, rise.max(1.0), Color::new(0.6, 0.45, 0.3, (1.0 - t) * 0.7));
            let cap_r = 14.0 + t * 34.0;
            let cap_y = stem_y - 6.0;
            glow(vec2(c.x, cap_y), cap_r * 1.3, rgb(1.0, 0.5, 0.15), (1.0 - t) * 1.2);
            draw_circle(c.x, cap_y, cap_r, Color::new(0.30, 0.20, 0.16, (1.0 - t) * 0.85));
            draw_circle(c.x, cap_y, cap_r * 0.7, Color::new(1.0, 0.45 * (1.0 - t), 0.10, (1.0 - t) * 0.9));
            draw_circle(c.x, cap_y, cap_r * 0.38, Color::new(1.0, 0.92, 0.55, (1.0 - t).powf(0.6)));
            // flung debris embers
            for i in 0..14 {
                let ang = i as f32 / 14.0 * std::f32::consts::TAU + at.x as f32;
                let dd = (10.0 + t * 60.0) * (0.6 + 0.4 * (i as f32 * 1.3).sin());
                let ex = c.x + ang.cos() * dd;
                let ey = c.y - rise * 0.4 + ang.sin() * dd * 0.6 + t * t * 30.0;
                draw_circle(ex, ey, 2.2 * (1.0 - t), Color::new(1.0, 0.6, 0.2, 1.0 - t));
            }
        }
    }
}

/// A dropped Essence mote glittering on the ground, bobbing and fading near the
/// end of its life. Purely cosmetic — the sim owns the real loot list.
pub fn draw_loot_mote(m: &ironvein_sim::world::Loot, cam: Vec2, tick: u32) {
    let c = tile_to_screen(m.tile.x as f32 + 0.5, m.tile.y as f32 + 0.5) - cam;
    let age = tick.saturating_sub(m.born);
    let ttl = ironvein_sim::world::LOOT_TTL;
    let fade = if ttl > age { (ttl - age).min(150) as f32 / 150.0 } else { 0.0 };
    let bob = (tick as f32 * 0.12 + (m.tile.x + m.tile.y) as f32).sin() * 2.0;
    let y = c.y - 5.0 + bob;
    match m.kind {
        1 => {
            // wild berries: a cluster of little red fruit on the grass
            draw_circle(c.x, c.y + 1.0, 5.0, Color::new(0.2, 0.4, 0.1, 0.22 * fade));
            glow(vec2(c.x, y), 6.0, rgb(0.9, 0.3, 0.35), 0.4 * fade);
            for (dx, dy) in [(0.0, 0.0), (-2.5, 1.0), (2.5, 1.0), (0.0, -2.5)] {
                draw_circle(c.x + dx, y + dy, 1.8, Color::new(0.9, 0.2, 0.25, fade));
            }
        }
        2 => {
            // raw meat (a hunted deer's drop): a reddish haunch
            draw_circle(c.x, c.y + 1.0, 5.0, Color::new(0.3, 0.1, 0.1, 0.22 * fade));
            glow(vec2(c.x, y), 6.0, rgb(0.85, 0.4, 0.4), 0.4 * fade);
            draw_circle(c.x, y, 3.2, Color::new(0.8, 0.35, 0.35, fade));
            draw_circle(c.x - 1.0, y - 1.0, 1.2, Color::new(0.95, 0.6, 0.6, fade)); // marbling
        }
        _ => {
            // essence crystal (purple)
            draw_circle(c.x, c.y + 1.0, 5.0, Color::new(0.45, 0.2, 0.6, 0.22 * fade));
            glow(vec2(c.x, y), 7.0, rgb(0.7, 0.4, 1.0), 0.5 * fade);
            draw_triangle(vec2(c.x, y - 5.0), vec2(c.x + 3.0, y), vec2(c.x - 3.0, y), Color::new(0.86, 0.62, 1.0, fade));
            draw_triangle(vec2(c.x, y + 5.0), vec2(c.x + 3.0, y), vec2(c.x - 3.0, y), Color::new(0.6, 0.3, 0.85, fade));
        }
    }
}

// ---------------------------------------------------------------------------
// Fog + atmosphere
// ---------------------------------------------------------------------------

pub fn draw_fog(w: &World, my_pid: u8, cam: Vec2, view: Vec2) {
    let Some(p) = w.players.get(my_pid as usize) else { return };
    if !p.joined {
        return;
    }
    let corners = [cam, cam + vec2(view.x, 0.0), cam + vec2(0.0, view.y), cam + view];
    let mut lo = vec2(f32::MAX, f32::MAX);
    let mut hi = vec2(f32::MIN, f32::MIN);
    for c in corners {
        let t = screen_to_tilef(c);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    let x0 = (lo.x.floor() as i32 - 3).max(0);
    let y0 = (lo.y.floor() as i32 - 3).max(0);
    let x1 = (hi.x.ceil() as i32 + 5).min(w.map.w);
    let y1 = (hi.y.ceil() as i32 + 5).min(w.map.h);
    let unseen = Color::new(0.02, 0.02, 0.03, 1.0);
    let explored = Color::new(0.02, 0.02, 0.05, 0.5);
    for ty in y0..y1 {
        for tx in x0..x1 {
            let f = p.fog[(ty * w.map.w + tx) as usize];
            if f == 2 {
                continue;
            }
            let c = tile_to_screen(tx as f32 + 0.5, ty as f32 + 0.5) - cam;
            diamond(c, if f == 0 { unseen } else { explored });
        }
    }
}

pub fn tile_visible(w: &World, my_pid: u8, t: Tp) -> u8 {
    match w.players.get(my_pid as usize) {
        Some(p) if p.joined && w.map.in_bounds(t) => p.fog[(t.y * w.map.w + t.x) as usize],
        _ => 2,
    }
}

pub fn draw_daylight(view: Vec2, tick: u32) {
    let phase = (tick % 6000) as f32 / 6000.0;
    let s = (phase * std::f32::consts::TAU).sin();
    let night = s.max(0.0) * 0.30;
    let dusk = (1.0 - s.abs()).clamp(0.0, 1.0) * 0.12;
    draw_rectangle(0.0, 0.0, view.x, view.y, Color::new(0.55, 0.40, 0.18, 0.05));
    if dusk > 0.005 {
        draw_rectangle(0.0, 0.0, view.x, view.y, Color::new(0.55, 0.28, 0.10, dusk));
    }
    if night > 0.01 {
        // a blood moon bathes the dark in red, otherwise it's deep blue
        if ironvein_sim::world::is_blood_moon(tick) {
            draw_rectangle(0.0, 0.0, view.x, view.y, Color::new(0.45, 0.04, 0.04, night * 2.4));
        } else {
            draw_rectangle(0.0, 0.0, view.x, view.y, Color::new(0.03, 0.05, 0.14, night));
        }
    }
    let steps = 5;
    for i in 0..steps {
        let inset = i as f32 * 8.0;
        let a = 0.05 * (1.0 - i as f32 / steps as f32);
        draw_rectangle_lines(inset, inset, view.x - inset * 2.0, view.y - inset * 2.0, 8.0, Color::new(0.0, 0.0, 0.0, a));
    }
}
