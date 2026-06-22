//! ui.rs — the classic right-hand command bar: minimap, money, power,
//! build buttons. Plus chat and overlays.

use crate::gfx;
use ironvein_net::Session;
use ironvein_sim::stats::{stats, Kind};
use ironvein_sim::world::World;
use ironvein_sim::{Eid, Terrain, Tp};
use macroquad::prelude::*;

pub const SIDEBAR_W: f32 = 176.0;
const MINI: f32 = 160.0;

pub const BUILD_TAB: [Kind; 27] = [
    Kind::EssenceReactor,
    Kind::HellCannon,
    Kind::SoulAltar,
    Kind::RiftAltar,
    Kind::PowerPlant,
    Kind::Reactor,
    Kind::Refinery,
    Kind::OreSilo,
    Kind::Barracks,
    Kind::Factory,
    Kind::TechCenter,
    Kind::Radar,
    Kind::GuardTower,
    Kind::Pillbox,
    Kind::CannonTower,
    Kind::MissileTurret,
    Kind::TeslaCoil,
    Kind::Obelisk,
    Kind::MissileSilo,
    Kind::RepairDepot,
    Kind::MedBay,
    Kind::House,
    Kind::Farm,
    Kind::FoodSilo,
    Kind::Wall,
    Kind::Gate,
    Kind::Road,
];
pub const UNIT_TAB: [Kind; 13] = [
    Kind::Rifleman,
    Kind::Grenadier,
    Kind::Rocketeer,
    Kind::Flamer,
    Kind::Sniper,
    Kind::Engineer,
    Kind::Harvester,
    Kind::Buggy,
    Kind::Tank,
    Kind::HeavyTank,
    Kind::Artillery,
    Kind::Champion,
    Kind::Revenant,
];

pub struct Minimap {
    img: Image,
    tex: Texture2D,
    cooldown: u32,
}

impl Minimap {
    pub fn new(w: i32, h: i32) -> Minimap {
        let img = Image::gen_image_color(w as u16, h as u16, BLACK);
        let tex = Texture2D::from_image(&img);
        tex.set_filter(FilterMode::Nearest);
        Minimap { img, tex, cooldown: 0 }
    }

    pub fn refresh(&mut self, w: &World, my_pid: u8) {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return;
        }
        self.cooldown = 12;
        for y in 0..w.map.h {
            for x in 0..w.map.w {
                let t = Tp::new(x, y);
                let f = gfx::tile_visible(w, my_pid, t);
                let c = if f == 0 {
                    Color::new(0.02, 0.02, 0.03, 1.0)
                } else {
                    // alien palette — keep the minimap in step with terrain_tint
                    let mut c = match w.map.terrain_at(t) {
                        Terrain::Grass => Color::new(0.11, 0.34, 0.28, 1.0),
                        Terrain::Dirt => Color::new(0.42, 0.27, 0.25, 1.0),
                        Terrain::Road => Color::new(0.35, 0.33, 0.36, 1.0),
                        Terrain::Water => Color::new(0.05, 0.28, 0.40, 1.0),
                        Terrain::Bridge => Color::new(0.44, 0.30, 0.28, 1.0),
                        Terrain::Rock => Color::new(0.42, 0.38, 0.50, 1.0),
                        Terrain::Tree => Color::new(0.06, 0.27, 0.24, 1.0),
                        Terrain::Sand => Color::new(0.70, 0.56, 0.54, 1.0),
                        Terrain::Snow => Color::new(0.86, 0.86, 0.95, 1.0),
                        Terrain::Ice => Color::new(0.58, 0.78, 0.90, 1.0),
                        Terrain::Marsh => Color::new(0.20, 0.30, 0.25, 1.0),
                        Terrain::Mountain => Color::new(0.30, 0.28, 0.40, 1.0),
                        Terrain::Lava => Color::new(0.90, 0.40, 0.12, 1.0),
                        Terrain::Ash => Color::new(0.20, 0.17, 0.18, 1.0),
                        Terrain::Obsidian => Color::new(0.13, 0.09, 0.16, 1.0),
                    };
                    if w.map.ore_at(t) > 0 {
                        c = Color::new(0.85, 0.72, 0.2, 1.0);
                    }
                    if f == 1 {
                        c = Color::new(c.r * 0.5, c.g * 0.5, c.b * 0.55, 1.0);
                    }
                    c
                };
                self.img.set_pixel(x as u32, y as u32, c);
            }
        }
        for e in w.ents.iter() {
            let t = e.tile();
            if gfx::tile_visible(w, my_pid, t) != 2 && e.owner != my_pid {
                continue;
            }
            let c = gfx::player_color(w, e.owner);
            let (fw, fh) = e.foot();
            for dy in 0..fh.max(1) {
                for dx in 0..fw.max(1) {
                    let (px, py) = (t.x + dx, t.y + dy);
                    if px >= 0 && py >= 0 && px < w.map.w && py < w.map.h {
                        self.img.set_pixel(px as u32, py as u32, c);
                    }
                }
            }
        }
        self.tex.update(&self.img);
    }

    pub fn draw(&self, w: &World, ox: f32, oy: f32, cam: Vec2, view: Vec2) {
        draw_rectangle(ox - 2.0, oy - 2.0, MINI + 4.0, MINI + 4.0, Color::new(0.08, 0.08, 0.09, 1.0));
        draw_texture_ex(
            &self.tex,
            ox,
            oy,
            WHITE,
            DrawTextureParams { dest_size: Some(vec2(MINI, MINI)), ..Default::default() },
        );
        // current-view marker: an axis-aligned box covering the tiles the
        // iso view currently spans (the four screen corners, inverse-projected).
        let sx = MINI / w.map.w as f32;
        let sy = MINI / w.map.h as f32;
        let corners = [cam, cam + vec2(view.x, 0.0), cam + view, cam + vec2(0.0, view.y)];
        let mut lo = vec2(f32::MAX, f32::MAX);
        let mut hi = vec2(f32::MIN, f32::MIN);
        for c in corners {
            let t = gfx::screen_to_tilef(c);
            lo = lo.min(t);
            hi = hi.max(t);
        }
        let x = ox + lo.x.max(0.0) * sx;
        let y = oy + lo.y.max(0.0) * sy;
        let rw = (hi.x.min(w.map.w as f32) - lo.x.max(0.0)) * sx;
        let rh = (hi.y.min(w.map.h as f32) - lo.y.max(0.0)) * sy;
        draw_rectangle_lines(x, y, rw, rh, 1.5, WHITE);
    }

    /// click (screen coords) -> Some(fractional tile coords) if inside the minimap
    pub fn pick(&self, w: &World, ox: f32, oy: f32, m: Vec2) -> Option<Vec2> {
        if m.x < ox || m.y < oy || m.x > ox + MINI || m.y > oy + MINI {
            return None;
        }
        let fx = (m.x - ox) / MINI;
        let fy = (m.y - oy) / MINI;
        Some(vec2(fx * w.map.w as f32, fy * w.map.h as f32))
    }
}

pub struct SidebarHit {
    pub kind: Option<Kind>,
    pub toggle_tab: bool,
    pub consumed: bool,
}

/// Find the building that would train `kind`: prefer a selected one.
pub fn producer_for(world: &World, my_pid: u8, sel: &[Eid], kind: Kind) -> Option<Eid> {
    let need = stats(kind).built_by?;
    for &id in sel {
        if let Some(e) = world.ents.get(id) {
            if e.owner == my_pid && e.kind == need && e.done {
                return Some(id);
            }
        }
    }
    world
        .ents
        .iter()
        .find(|e| e.owner == my_pid && e.kind == need && e.done)
        .map(|e| e.id)
}

const BTN_W: f32 = 80.0;
const BTN_H: f32 = 46.0;
const BTN_PITCH: f32 = 50.0;

fn button_rect(i: usize, ox: f32, oy: f32) -> Rect {
    let col = (i % 2) as f32;
    let row = (i / 2) as f32;
    Rect::new(ox + 4.0 + col * 84.0, oy + row * BTN_PITCH, BTN_W, BTN_H)
}

/// A one-line role blurb shown in the build/unit tooltip.
fn kind_blurb(k: Kind) -> &'static str {
    use Kind::*;
    match k {
        PowerPlant => "Generates power. Build before everything draws it down.",
        Reactor => "Huge power output. Needs a Tech Center first.",
        Refinery => "Refines hauled ore into credits. Drop-off for harvesters.",
        OreSilo => "Automates refining — trickles credits, expands storage.",
        Barracks => "Trains infantry.",
        Factory => "Builds vehicles and harvesters.",
        TechCenter => "Unlocks advanced tech: Reactor, Missile Turret, Obelisk…",
        Radar => "Reveals the battlefield; powers the minimap.",
        GuardTower => "Cheap automatic defense vs infantry.",
        Pillbox => "Bunker — short-range, very tough for its price.",
        CannonTower => "Long-range anti-armor defensive gun.",
        MissileTurret => "Top-tier defense: long range, heavy missiles.",
        Obelisk => "Tier-3 arcane death-ray. Costs Essence.",
        MissileSilo => "Superweapon. Charges ~3 min, then nukes anywhere (press N when ready).",
        RepairDepot => "Repairs nearby vehicles over time.",
        MedBay => "Heals nearby infantry over time.",
        House => "Raises unit cap, stores food, slowly heals troops & cooks meat.",
        Farm => "Grows food for your army. Build several to feed a big force.",
        FoodSilo => "Stores a lot of food (raises your food cap).",
        Wall => "Blocks movement. Monsters can't break it — until a boss arms them.",
        Gate => "A wall that opens only for your own troops.",
        Road => "Paved terrain — your units move 25% faster on it.",
        Rifleman => "Cheap all-purpose infantry.",
        Grenadier => "Lobs grenades — good vs clustered foes.",
        Rocketeer => "Anti-vehicle / anti-building rockets.",
        Flamer => "Short-range area burn; shreds infantry.",
        Sniper => "Very long range, one-shots most infantry. Needs Tech.",
        Engineer => "Captures enemy & neutral buildings (consumed on use).",
        Harvester => "Gathers ore, wood and stone; trucks it home.",
        Buggy => "Fast, fragile scout.",
        Tank => "The mainline battle tank.",
        HeavyTank => "Twin-cannon bruiser. Needs Tech.",
        Artillery => "Devastating range, fragile up close. Needs Tech.",
        Champion => "Tier-3 hero — a one-soldier army. Costs Essence.",
        _ => "",
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_sidebar(
    world: &World,
    _session: &Session,
    mini: &Minimap,
    my_pid: u8,
    sel: &[Eid],
    units_tab: bool,
    placing: Option<Kind>,
    cam: Vec2,
    view: Vec2,
    credits_shown: f32,
) {
    let ox = screen_width() - SIDEBAR_W;
    draw_rectangle(ox, 0.0, SIDEBAR_W, screen_height(), Color::new(0.13, 0.13, 0.15, 1.0));
    draw_rectangle(ox, 0.0, 2.0, screen_height(), Color::new(0.05, 0.05, 0.06, 1.0));

    mini.draw(world, ox + 8.0, 8.0, cam, view);
    let mut y = 8.0 + MINI + 10.0;

    let p = world.players.get(my_pid as usize);
    let credits = p.map(|p| p.credits).unwrap_or(0);
    let wood = p.map(|p| p.wood).unwrap_or(0);
    let stone = p.map(|p| p.stone).unwrap_or(0);
    let essence = p.map(|p| p.essence).unwrap_or(0);
    let food = p.map(|p| p.food).unwrap_or(0);
    let starving = p.map(|p| p.starving).unwrap_or(false);
    draw_text(&format!("$ {:>7}", (credits_shown.round() as i64).clamp(0, credits as i64 + 999)), ox + 10.0, y + 14.0, 26.0, Color::new(0.95, 0.85, 0.3, 1.0));
    y += 22.0;
    // wood / stone / essence / food — four compact columns (height fixed so the
    // tab/grid click regions in click_sidebar stay aligned)
    draw_text(&format!("Wd{:>4}", wood), ox + 5.0, y + 12.0, 13.0, Color::new(0.76, 0.58, 0.34, 1.0));
    draw_text(&format!("St{:>4}", stone), ox + 47.0, y + 12.0, 13.0, Color::new(0.74, 0.76, 0.80, 1.0));
    draw_text(&format!("Es{:>3}", essence), ox + 89.0, y + 12.0, 13.0, Color::new(0.72, 0.42, 0.88, 1.0));
    let food_col = if starving && (world.tick / 4) % 2 == 0 {
        Color::new(1.0, 0.3, 0.25, 1.0)
    } else {
        Color::new(0.5, 0.9, 0.45, 1.0)
    };
    draw_text(&format!("Fd{:>4}", food), ox + 124.0, y + 12.0, 13.0, food_col);
    y += 18.0;

    if let Some(p) = p {
        // power bar
        let total = (p.power_made.max(p.power_used)).max(1) as f32;
        let bw = SIDEBAR_W - 20.0;
        draw_text("PWR", ox + 10.0, y + 9.0, 14.0, GRAY);
        draw_rectangle(ox + 42.0, y, bw - 34.0, 9.0, Color::new(0.07, 0.07, 0.08, 1.0));
        let madew = (bw - 34.0) * (p.power_made as f32 / total);
        let usedw = (bw - 34.0) * (p.power_used as f32 / total);
        draw_rectangle(ox + 42.0, y, madew, 9.0, Color::new(0.2, 0.8, 0.3, 1.0));
        draw_rectangle(ox + 42.0, y + 5.0, usedw, 4.0, Color::new(0.9, 0.25, 0.2, 1.0));
        if p.low_power() && (world.tick / 5) % 2 == 0 {
            draw_text("LOW POWER", ox + 42.0, y - 3.0, 14.0, RED);
        }
        y += 15.0;
        draw_text(&format!("units {:>2}/{:<2}   day {}", p.unit_count, p.unit_cap, world.tick / 6000 + 1), ox + 10.0, y + 9.0, 14.0, GRAY);
        y += 18.0;
    }

    // tabs — connected to the panel below, active one lit with an accent bar
    let tabs = [("BUILD", !units_tab), ("UNITS", units_tab)];
    for (i, (label, active)) in tabs.iter().enumerate() {
        let r = Rect::new(ox + 4.0 + i as f32 * 84.0, y, 80.0, 22.0);
        let bg = if *active { Color::new(0.22, 0.24, 0.30, 1.0) } else { Color::new(0.14, 0.14, 0.17, 1.0) };
        draw_rectangle(r.x, r.y, r.w, r.h, bg);
        draw_rectangle(r.x, r.y, r.w, 2.0, if *active { Color::new(0.40, 0.66, 0.95, 1.0) } else { Color::new(0.22, 0.22, 0.26, 1.0) });
        let tw = measure_text(label, None, 17, 1.0).width;
        draw_text(label, r.x + (r.w - tw) * 0.5, r.y + 16.0, 17.0, if *active { WHITE } else { Color::new(0.55, 0.55, 0.62, 1.0) });
    }
    y += 28.0;

    // build / unit grid — icon + name + cost, with hover tooltip
    let team = gfx::player_color(world, my_pid);
    let (mx, my) = mouse_position();
    let mouse = vec2(mx, my);
    let tick = world.tick;
    let list: &[Kind] = if units_tab { &UNIT_TAB } else { &BUILD_TAB };
    let have_core = world.ents.iter().any(|e| e.owner == my_pid && e.kind.is_building() && e.done);
    let mut hover: Option<(Kind, Rect)> = None;

    let fit = |t: &str, size: u16, maxw: f32| -> String {
        if measure_text(t, None, size, 1.0).width <= maxw {
            return t.to_string();
        }
        let mut s = String::new();
        for ch in t.chars() {
            let mut trial = s.clone();
            trial.push(ch);
            if measure_text(&format!("{trial}.."), None, size, 1.0).width > maxw {
                break;
            }
            s.push(ch);
        }
        format!("{s}..")
    };

    for (i, &k) in list.iter().enumerate() {
        let r = button_rect(i, ox, y);
        let st = stats(k);
        let wc = ironvein_sim::stats::wood_cost(k);
        let sc = ironvein_sim::stats::stone_cost(k);
        let ec = ironvein_sim::stats::essence_cost(k);
        let afford = credits >= st.cost && wood >= wc && stone >= sc && essence >= ec;
        let producer = if units_tab { producer_for(world, my_pid, sel, k) } else { None };
        let req = ironvein_sim::stats::requires(k);
        let tech_ok = req
            .map(|rq| world.ents.iter().any(|e| e.owner == my_pid && e.kind == rq && e.done))
            .unwrap_or(true);
        let buildable_here = if units_tab { producer.is_some() } else { have_core };
        let enabled = buildable_here && afford && tech_ok;
        let placing_this = placing == Some(k);
        let hovered = r.contains(mouse);
        if hovered {
            hover = Some((k, r));
        }

        // beveled button body
        let body = if placing_this {
            Color::new(0.18, 0.30, 0.20, 1.0)
        } else if enabled {
            Color::new(0.21, 0.22, 0.26, 1.0)
        } else {
            Color::new(0.145, 0.145, 0.17, 1.0)
        };
        draw_rectangle(r.x, r.y, r.w, r.h, body);
        draw_rectangle(r.x, r.y, r.w, 1.0, Color::new(1.0, 1.0, 1.0, 0.07)); // top bevel
        draw_rectangle(r.x, r.y + r.h - 1.0, r.w, 1.0, Color::new(0.0, 0.0, 0.0, 0.30)); // bottom shade
        let border = if placing_this {
            Color::new(0.4, 0.95, 0.5, 1.0)
        } else if hovered {
            Color::new(0.55, 0.62, 0.78, 1.0)
        } else {
            Color::new(0.30, 0.31, 0.36, 1.0)
        };
        draw_rectangle_lines(r.x, r.y, r.w, r.h, if placing_this || hovered { 2.0 } else { 1.0 }, border);

        // name across the top, full button width
        let tcol = if enabled { WHITE } else { Color::new(0.55, 0.55, 0.6, 1.0) };
        draw_text(&fit(st.name, 12, r.w - 9.0), r.x + 5.0, r.y + 13.0, 12.0, tcol);

        // icon slot, bottom-left
        let (sx, sy, ssz) = (r.x + 3.0, r.y + 17.0, 26.0);
        draw_rectangle(sx, sy, ssz, ssz, Color::new(0.08, 0.08, 0.10, 1.0));
        gfx::build_icon(k, vec2(sx + ssz * 0.5, sy + ssz * 0.5), 10.0, team, tick);
        if !enabled {
            draw_rectangle(sx, sy, ssz, ssz, Color::new(0.10, 0.10, 0.12, 0.5)); // dim
        }

        // cost block to the right of the icon
        let tx = r.x + 33.0;
        if !tech_ok {
            // padlock over the icon
            draw_rectangle(sx + ssz * 0.5 - 6.0, sy + ssz * 0.5, 12.0, 9.0, Color::new(0.9, 0.74, 0.22, 0.95));
            draw_circle_lines(sx + ssz * 0.5, sy + ssz * 0.5, 4.0, 1.6, Color::new(0.9, 0.74, 0.22, 0.95));
            if let Some(rq) = req {
                draw_text(&fit(&format!("needs {}", stats(rq).name), 11, r.w - 35.0), tx, r.y + 31.0, 11.0, Color::new(0.86, 0.5, 0.35, 1.0));
            }
        } else {
            let costcol = if afford { Color::new(0.95, 0.82, 0.35, 1.0) } else { Color::new(0.85, 0.45, 0.35, 1.0) };
            draw_text(&format!("${}", st.cost), tx, r.y + 31.0, 13.0, costcol);
            // resource pips: wood / stone / essence, only when required
            let mut px = tx;
            for (amt, have, col) in [
                (wc, wood, Color::new(0.74, 0.56, 0.34, 1.0)),
                (sc, stone, Color::new(0.72, 0.74, 0.78, 1.0)),
                (ec, essence, Color::new(0.72, 0.42, 0.88, 1.0)),
            ] {
                if amt == 0 {
                    continue;
                }
                let c = if have >= amt { col } else { Color::new(0.85, 0.4, 0.32, 1.0) };
                draw_circle(px + 3.0, r.y + 41.0, 3.0, c);
                let label = format!("{amt}");
                draw_text(&label, px + 8.0, r.y + 44.0, 11.0, Color::new(c.r, c.g, c.b, 0.95));
                px += 8.0 + measure_text(&label, None, 11, 1.0).width + 6.0;
            }
        }

        // unit production: queue badge + progress bar
        if units_tab && tech_ok {
            if let Some(pid) = producer {
                if let Some(b) = world.ents.get(pid) {
                    let n = b.queue.iter().filter(|q| **q == k).count();
                    if n > 0 {
                        draw_text(&format!("x{n}"), r.x + r.w - 22.0, r.y + 14.0, 14.0, Color::new(0.4, 0.8, 1.0, 1.0));
                    }
                    if b.queue.first() == Some(&k) {
                        let total = stats(k).build_time * 2;
                        let frac = (b.prod_progress as f32 / total as f32).clamp(0.0, 1.0);
                        draw_rectangle(r.x + 2.0, r.y + r.h - 4.0, (r.w - 4.0) * frac, 3.0, Color::new(0.3, 0.7, 0.95, 1.0));
                    }
                }
            }
        }
    }
    let rows = list.len().div_ceil(2);
    y += rows as f32 * BTN_PITCH + 8.0;
    for line in ["LMB select · RMB order", "A atk-move · S stop · X sell", "Enter chat · F1 help · F5 save"] {
        draw_text(line, ox + 8.0, y + 11.0, 12.0, Color::new(0.5, 0.5, 0.56, 1.0));
        y += 14.0;
    }

    // hover tooltip (drawn last, to the LEFT of the sidebar so it never clips off-screen)
    if let Some((k, r)) = hover {
        draw_kind_tooltip(k, r, ox);
    }
}

/// A floating info card for a build/unit button: name, role, full cost, and key
/// stats. Anchored to the left of the sidebar so it stays on-screen.
fn draw_kind_tooltip(k: Kind, btn: Rect, sidebar_x: f32) {
    let st = stats(k);
    let wc = ironvein_sim::stats::wood_cost(k);
    let sc = ironvein_sim::stats::stone_cost(k);
    let ec = ironvein_sim::stats::essence_cost(k);
    let blurb = kind_blurb(k);

    // wrap the blurb to the panel width
    let pw = 234.0;
    let maxw = pw - 24.0;
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in blurb.split(' ') {
        let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if measure_text(&trial, None, 14, 1.0).width > maxw && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = trial;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // cost string (only nonzero parts)
    let mut cost = format!("${}", st.cost);
    if wc > 0 {
        cost += &format!("   Wood {wc}");
    }
    if sc > 0 {
        cost += &format!("   Stone {sc}");
    }
    if ec > 0 {
        cost += &format!("   Essence {ec}");
    }
    // stat line
    let secs = (st.build_time as f32 / 10.0).round() as i32;
    let stat = if st.damage > 0 {
        format!("HP {}   DMG {} (rng {})   {}s", st.max_hp, st.damage, st.range, secs)
    } else if st.power != 0 {
        format!("HP {}   PWR {:+}   {}s", st.max_hp, st.power, secs)
    } else {
        format!("HP {}   {}s", st.max_hp, secs)
    };

    let header = 24.0;
    let ph = header + lines.len() as f32 * 17.0 + 44.0;
    let px = (sidebar_x - pw - 8.0).max(6.0);
    let py = btn.y.min(screen_height() - ph - 6.0).max(6.0);

    draw_rectangle(px - 3.0, py - 3.0, pw + 6.0, ph + 6.0, Color::new(0.0, 0.0, 0.0, 0.5));
    draw_rectangle(px, py, pw, ph, Color::new(0.10, 0.11, 0.14, 0.98));
    draw_rectangle(px, py, pw, 3.0, Color::new(0.40, 0.66, 0.95, 1.0));
    draw_rectangle_lines(px, py, pw, ph, 1.5, Color::new(0.4, 0.42, 0.5, 1.0));

    draw_text(st.name, px + 12.0, py + 19.0, 19.0, WHITE);
    let mut ty = py + header + 14.0;
    for l in &lines {
        draw_text(l, px + 12.0, ty, 14.0, Color::new(0.78, 0.80, 0.86, 1.0));
        ty += 17.0;
    }
    ty += 4.0;
    draw_text(&cost, px + 12.0, ty, 14.0, Color::new(0.95, 0.82, 0.4, 1.0));
    ty += 18.0;
    draw_text(&stat, px + 12.0, ty, 13.0, Color::new(0.62, 0.78, 0.95, 1.0));
}

/// Returns what (if anything) a left-click at `m` does in the sidebar.
pub fn click_sidebar(world: &World, my_pid: u8, sel: &[Eid], units_tab: &mut bool, m: Vec2) -> SidebarHit {
    let ox = screen_width() - SIDEBAR_W;
    let mut hit = SidebarHit { kind: None, toggle_tab: false, consumed: m.x >= ox };
    if !hit.consumed {
        return hit;
    }
    // NOTE: this MUST track the header layout in draw_sidebar exactly —
    // credits (+22) then the wood/stone row (+18, always), then the power bar
    // and unit-count line (+15 +18, only when joined).
    let mut y = 8.0 + MINI + 10.0 + 22.0 + 18.0;
    if world.players.get(my_pid as usize).is_some() {
        y += 15.0 + 18.0;
    }
    for (i, _) in [0, 1].iter().enumerate() {
        let r = Rect::new(ox + 4.0 + i as f32 * 84.0, y, 80.0, 22.0);
        if r.contains(m) {
            *units_tab = i == 1;
            hit.toggle_tab = true;
            return hit;
        }
    }
    y += 28.0;
    let list: &[Kind] = if *units_tab { &UNIT_TAB } else { &BUILD_TAB };
    for (i, &k) in list.iter().enumerate() {
        if button_rect(i, ox, y).contains(m) {
            hit.kind = Some(k);
            return hit;
        }
    }
    let _ = sel;
    hit
}

// ---------------------------------------------------------------------------
// chat + overlays
// ---------------------------------------------------------------------------

pub fn draw_chat(world: &World, input: &Option<String>) {
    let base_y = screen_height() - 28.0;
    let mut shown = 0;
    for (t, pid, text) in world.chat.iter().rev() {
        if shown >= 6 {
            break;
        }
        let age = world.tick.saturating_sub(*t);
        if age > 220 && input.is_none() {
            continue;
        }
        let y = base_y - 18.0 - shown as f32 * 16.0;
        let who = if *pid == ironvein_sim::NEUTRAL {
            ("[world]".to_string(), Color::new(0.7, 0.7, 0.5, 0.9))
        } else {
            let name = world.players.get(*pid as usize).map(|p| p.name.clone()).unwrap_or_default();
            let mut c = gfx::PLAYER_COLORS[world.players.get(*pid as usize).map(|p| p.color as usize).unwrap_or(7) % 8];
            c.a = 0.95;
            (name, c)
        };
        draw_rectangle(6.0, y - 12.0, 380.0, 15.0, Color::new(0.0, 0.0, 0.0, 0.35));
        draw_text(&format!("{}: {}", who.0, text), 10.0, y, 15.0, who.1);
        shown += 1;
    }
    if let Some(buf) = input {
        draw_rectangle(6.0, base_y - 12.0, 380.0, 18.0, Color::new(0.0, 0.0, 0.0, 0.65));
        draw_text(&format!("say: {}_", buf), 10.0, base_y + 1.0, 15.0, WHITE);
        draw_text("(/give <player#> <amount> to wire credits)", 10.0, base_y + 15.0, 12.0, GRAY);
    }
}

pub fn draw_banner(session: &Session, banner_age: f32) {
    let view_w = screen_width() - SIDEBAR_W;
    if let Some(t) = session.desync_at {
        let msg = format!("DESYNC at tick {t} — simulation halted (this is a bug; the save is intact)");
        draw_rectangle(0.0, 40.0, view_w, 26.0, Color::new(0.5, 0.05, 0.05, 0.9));
        draw_text(&msg, 16.0, 58.0, 18.0, WHITE);
        return;
    }
    if !session.status.is_empty() && banner_age < 5.0 {
        let a = (1.0 - banner_age / 5.0).min(1.0);
        draw_rectangle(0.0, 40.0, view_w, 22.0, Color::new(0.05, 0.05, 0.08, 0.6 * a));
        draw_text(&session.status, 16.0, 56.0, 17.0, Color::new(0.9, 0.9, 0.95, a));
    }
}

pub fn draw_help() {
    let lines = [
        "IRONVEIN — field manual",
        "",
        "Left-drag: select your units      Right-click: move / attack / harvest / rally",
        "A then click: attack-move         S: stop      X: sell selected building",
        "Build tab: click, then place (green = legal, near your buildings)",
        "Engineers capture enemy & village buildings (the engineer is consumed)",
        "HARVESTING: a Harvester gathers gold ore (=credits), brown TREES (=wood)",
        "  and grey ROCKS / mountains (=stone). Right-click one onto the resource.",
        "  Select a harvester to light up every source on the map, colour-coded.",
        "Houses raise your unit cap and slowly heal troops nearby — build a town",
        "FOOD: soldiers eat. Farms grow it; hunt deer (right-click) & forage berries.",
        "  A House cooks raw meat into food; Food Silos store more. Starve = no training.",
        "Ore regrows near glittering nodes. Roads are fast.",
        "Ctrl+1..9 save a control group, 1..9 recall it",
        "Enter: chat. /give 2 500 wires credits. /ally 2 proposes an alliance",
        "Allies (mutual /ally) hold fire & share vision. M = the persistent world map",
        "F5: save world now (autosaves every minute) — any peer's save can re-host",
        "",
        "F1 to close",
    ];
    let w = 620.0;
    let h = lines.len() as f32 * 20.0 + 24.0;
    let x = (screen_width() - SIDEBAR_W - w) * 0.5;
    let y = (screen_height() - h) * 0.4;
    draw_rectangle(x, y, w, h, Color::new(0.05, 0.05, 0.08, 0.92));
    draw_rectangle_lines(x, y, w, h, 2.0, Color::new(0.5, 0.5, 0.6, 1.0));
    for (i, l) in lines.iter().enumerate() {
        draw_text(l, x + 16.0, y + 28.0 + i as f32 * 20.0, 16.0, if i == 0 { Color::new(0.95, 0.85, 0.3, 1.0) } else { WHITE });
    }
}

/// The "while you were away" report — a centered panel summarising what your
/// settlement earned and suffered during your absence. The dopamine of
/// returning to accrued money, the sting of a raid. This is the hook.
pub fn draw_away_report(world: &World, log: &ironvein_sim::world::AwayLog, cap_tick: u32, view: Vec2) {
    let secs = cap_tick.saturating_sub(log.from_tick) / ironvein_sim::TICK_HZ;
    let dur = if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    };

    // assemble the report lines (text, colour)
    let gold = Color::new(0.95, 0.85, 0.3, 1.0);
    let green = Color::new(0.45, 0.95, 0.45, 1.0);
    let red = Color::new(0.95, 0.4, 0.35, 1.0);
    let dim = Color::new(0.7, 0.7, 0.75, 1.0);
    let mut lines: Vec<(String, Color)> = Vec::new();
    lines.push((format!("You were away for {dur}."), dim));
    lines.push((String::new(), dim));
    if log.credits_gained > 0 {
        lines.push((format!("+{} credits earned while you slept", log.credits_gained), green));
    }
    if log.credits_lost > 0 {
        lines.push((format!("-{} credits looted by raiders", log.credits_lost), red));
    }
    if log.buildings_lost > 0 {
        lines.push((format!("{} building(s) destroyed", log.buildings_lost), red));
    }
    if log.units_lost > 0 {
        lines.push((format!("{} unit(s) lost", log.units_lost), red));
    }
    if log.attacks > 0 {
        let foe = world
            .players
            .get(log.last_foe as usize)
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "an unknown raider".into());
        lines.push((String::new(), dim));
        lines.push((format!("Your valley was raided by {foe}.", ), red));
    }
    if log.credits_lost == 0 && log.buildings_lost == 0 && log.units_lost == 0 {
        lines.push((String::new(), dim));
        lines.push(("Your base stood untouched. The valley was quiet.".into(), dim));
    }
    lines.push((String::new(), dim));
    lines.push(("press any key to continue".into(), dim));

    let w = 460.0;
    let h = lines.len() as f32 * 24.0 + 56.0;
    let x = (view.x - w) * 0.5;
    let y = (view.y - h) * 0.42;
    draw_rectangle(x - 4.0, y - 4.0, w + 8.0, h + 8.0, Color::new(0.0, 0.0, 0.0, 0.55));
    draw_rectangle(x, y, w, h, Color::new(0.06, 0.07, 0.10, 0.97));
    draw_rectangle(x, y, w, 4.0, gold);
    draw_rectangle_lines(x, y, w, h, 2.0, Color::new(0.5, 0.5, 0.6, 1.0));
    draw_text("WHILE YOU WERE AWAY", x + 20.0, y + 34.0, 26.0, gold);
    for (i, (l, c)) in lines.iter().enumerate() {
        draw_text(l, x + 20.0, y + 66.0 + i as f32 * 24.0, 19.0, *c);
    }
}

/// Survival game-over: the night overran your colony. A solemn centered panel
/// tallying how long you held out. The sim keeps running underneath (you can
/// spectate, or Esc → Quit).
pub fn draw_survival_over(tick: u32, view: Vec2) {
    let day = tick / 6000 + 1;
    let nights = tick / 6000; // full day/night cycles weathered
    let w = 520.0;
    let h = 188.0;
    let x = (view.x - w) * 0.5;
    let y = (view.y - h) * 0.40;
    draw_rectangle(0.0, 0.0, view.x, view.y, Color::new(0.0, 0.0, 0.0, 0.45));
    draw_rectangle(x - 4.0, y - 4.0, w + 8.0, h + 8.0, Color::new(0.0, 0.0, 0.0, 0.6));
    draw_rectangle(x, y, w, h, Color::new(0.08, 0.05, 0.06, 0.98));
    draw_rectangle(x, y, w, 4.0, Color::new(0.85, 0.2, 0.18, 1.0));
    draw_rectangle_lines(x, y, w, h, 2.0, Color::new(0.6, 0.25, 0.22, 1.0));
    let title = "B PROXIMA HAS FALLEN";
    let tw = measure_text(title, None, 34, 1.0).width;
    draw_text(title, x + (w - tw) * 0.5, y + 50.0, 34.0, Color::new(0.95, 0.4, 0.35, 1.0));
    let sub = format!("The dark overran your colony on day {day}.");
    let sw = measure_text(&sub, None, 20, 1.0).width;
    draw_text(&sub, x + (w - sw) * 0.5, y + 86.0, 20.0, Color::new(0.82, 0.8, 0.84, 1.0));
    let stat = format!("You held the line for {nights} night{}.", if nights == 1 { "" } else { "s" });
    let stw = measure_text(&stat, None, 22, 1.0).width;
    draw_text(&stat, x + (w - stw) * 0.5, y + 120.0, 22.0, Color::new(0.95, 0.85, 0.4, 1.0));
    let hint = "Esc → Quit Game    ·    keep watching the world burn";
    let hw = measure_text(hint, None, 16, 1.0).width;
    draw_text(hint, x + (w - hw) * 0.5, y + 158.0, 16.0, Color::new(0.6, 0.58, 0.55, 1.0));
}

/// Live alert toasts, stacked at the top-centre of the play area, fading out.
pub fn draw_toasts(toasts: &[crate::Toast], view: Vec2) {
    let mut y = 34.0;
    for t in toasts {
        let fade = (1.0 - (t.age - 4.0).max(0.0)).clamp(0.0, 1.0); // hold then fade
        let rise = (1.0 - (t.age * 4.0).min(1.0)) * 8.0; // small slide-in
        let w = measure_text(&t.text, None, 20, 1.0).width + 28.0;
        let x = (view.x - w) * 0.5;
        let yy = y - rise;
        draw_rectangle(x, yy, w, 28.0, Color::new(0.05, 0.05, 0.07, 0.82 * fade));
        draw_rectangle(x, yy, 4.0, 28.0, Color::new(t.color.r, t.color.g, t.color.b, fade));
        draw_text(&t.text, x + 14.0, yy + 19.0, 20.0, Color::new(t.color.r, t.color.g, t.color.b, fade));
        y += 34.0;
    }
}

fn parse_sector(id: &str) -> Option<(i32, i32)> {
    let col = id.chars().next()?.to_ascii_uppercase();
    if !col.is_ascii_alphabetic() {
        return None;
    }
    let c = (col as u8 - b'A') as i32;
    let r: i32 = id.get(1..)?.parse().ok()?;
    Some((c, r - 1))
}

const MAP_COLS: i32 = 8;
const MAP_ROWS: i32 = 8;
const MAP_CELL: f32 = 62.0;
const MAP_GAP: f32 = 6.0;

fn map_origin(view: Vec2) -> Vec2 {
    let gw = MAP_COLS as f32 * (MAP_CELL + MAP_GAP) - MAP_GAP;
    let gh = MAP_ROWS as f32 * (MAP_CELL + MAP_GAP) - MAP_GAP;
    vec2((view.x - gw) * 0.5, (view.y - gh) * 0.5 + 16.0)
}

/// Which region (index) is under the mouse on the world map, if any.
pub fn world_map_pick(regions: &[crate::RegionInfo], view: Vec2, m: Vec2) -> Option<usize> {
    let o = map_origin(view);
    regions.iter().position(|reg| {
        let Some((c, r)) = parse_sector(&reg.id) else { return false };
        if c < 0 || c >= MAP_COLS || r < 0 || r >= MAP_ROWS {
            return false;
        }
        let x = o.x + c as f32 * (MAP_CELL + MAP_GAP);
        let y = o.y + r as f32 * (MAP_CELL + MAP_GAP);
        m.x >= x && m.x < x + MAP_CELL && m.y >= y && m.y < y + MAP_CELL
    })
}

/// The persistent world map: a grid of sectors, each its own serverless world,
/// painted by whoever currently controls it (their base persists offline, so
/// territory is held across sessions). Yours is highlighted; hovering a live
/// rival sector offers to travel there.
pub fn draw_world_map(regions: &[crate::RegionInfo], my_key: &[u8; 32], view: Vec2, m: Vec2) {
    draw_rectangle(0.0, 0.0, view.x, view.y, Color::new(0.02, 0.03, 0.05, 0.9));
    let o = map_origin(view);
    let (ox, oy) = (o.x, o.y);
    let gh = MAP_ROWS as f32 * (MAP_CELL + MAP_GAP) - MAP_GAP;
    let (cell, gap) = (MAP_CELL, MAP_GAP);
    draw_text("WORLD MAP - THE VALLEY SECTORS", ox, oy - 26.0, 28.0, Color::new(0.95, 0.85, 0.3, 1.0));

    // dormant grid
    for r in 0..MAP_ROWS {
        for c in 0..MAP_COLS {
            let x = ox + c as f32 * (cell + gap);
            let y = oy + r as f32 * (cell + gap);
            draw_rectangle(x, y, cell, cell, Color::new(0.10, 0.11, 0.13, 1.0));
            draw_rectangle_lines(x, y, cell, cell, 1.0, Color::new(0.2, 0.2, 0.24, 1.0));
            let id = format!("{}{}", (b'A' + c as u8) as char, r + 1);
            draw_text(&id, x + 5.0, y + 15.0, 14.0, Color::new(0.32, 0.32, 0.38, 1.0));
        }
    }

    let hovered = world_map_pick(regions, view, m);

    // controlled / live sectors
    for (idx, reg) in regions.iter().enumerate() {
        let Some((c, r)) = parse_sector(&reg.id) else { continue };
        if c < 0 || c >= MAP_COLS || r < 0 || r >= MAP_ROWS {
            continue;
        }
        let x = ox + c as f32 * (cell + gap);
        let y = oy + r as f32 * (cell + gap);
        let zero = reg.ctrl_key == [0u8; 32];
        let mine = !zero && reg.ctrl_key == *my_key;
        let base = if zero {
            Color::new(0.35, 0.35, 0.4, 1.0)
        } else {
            gfx::PLAYER_COLORS[(reg.ctrl_key[0] % 8) as usize]
        };
        let hot = hovered == Some(idx) && !mine;
        draw_rectangle(x, y, cell, cell, Color::new(base.r, base.g, base.b, if hot { 0.8 } else { 0.55 }));
        let (bw, bc) = if mine {
            (3.0, Color::new(1.0, 1.0, 0.6, 1.0))
        } else if hot {
            (2.5, WHITE)
        } else {
            (1.5, base)
        };
        draw_rectangle_lines(x, y, cell, cell, bw, bc);
        let id = format!("{}{}", (b'A' + c as u8) as char, r + 1);
        draw_text(&id, x + 5.0, y + 15.0, 14.0, WHITE);
        let nm: String = reg.controller.chars().take(8).collect();
        if !nm.is_empty() {
            draw_text(&nm, x + 5.0, y + 34.0, 15.0, WHITE);
        }
        draw_text(&format!("{}p", reg.players), x + 5.0, y + cell - 8.0, 13.0, Color::new(0.8, 0.9, 0.8, 1.0));
        if mine {
            draw_text("YOU", x + cell - 32.0, y + cell - 8.0, 13.0, Color::new(1.0, 1.0, 0.6, 1.0));
        }
    }

    // travel prompt for the hovered rival sector
    if let Some(idx) = hovered {
        if let Some(reg) = regions.get(idx) {
            let mine = reg.ctrl_key != [0u8; 32] && reg.ctrl_key == *my_key;
            let txt = if mine {
                format!("Sector {} - you hold this", reg.id)
            } else {
                format!("Click to travel to Sector {}", reg.id)
            };
            draw_text(&txt, ox, oy + gh + 50.0, 18.0, Color::new(0.95, 0.9, 0.5, 1.0));
        }
    }

    draw_text(
        "M or Esc to close   |   each sector is its own persistent serverless world",
        ox,
        oy + gh + 26.0,
        16.0,
        Color::new(0.7, 0.7, 0.75, 1.0),
    );
}
