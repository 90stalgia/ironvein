//! ironvein — the game client.
//!
//!   ironvein                              live in your own valley (1 bot neighbor)
//!   ironvein --bots 3 --map skirmish      classic deathmatch vs bots
//!   ironvein --host 47777 --name Ada      host your world for friends
//!   ironvein --join 192.168.1.20:47777    settle in a friend's world
//!   ironvein --load saves/world.iv --host 47777    resurrect a saved world

mod audio;
mod gfx;
mod menu;
mod music;
mod synth;
mod ui;

/// A fading click marker (move/attack/harvest order, or a rally point). Stored
/// in iso-world space so it stays anchored as the camera pans. Pure feedback.
struct CmdMarker {
    at: Vec2,
    color: Color,
    age: f32,
}

use ironvein_net::{Session, SessionKind};
use ironvein_sim::bot::Bot;
use ironvein_sim::command::Command;
use ironvein_sim::stats::{stats, Kind};
use ironvein_sim::world::Mode;
use ironvein_sim::{mapgen, Eid, Fp, Tp, World};
use macroquad::prelude::*;
use std::collections::HashMap;

const AUTOSAVE_EVERY: u32 = 600;

/// A transient on-screen alert ("under attack!", "building lost").
pub struct Toast {
    pub text: String,
    pub color: Color,
    pub age: f32,
}

/// One sector on the persistent world map (assembled from Nostr beacons in the
/// browser, or the local region natively).
pub struct RegionInfo {
    pub id: String,
    pub controller: String,
    pub ctrl_key: [u8; 32],
    pub host: [u8; 32],
    pub tick: u32,
    pub players: u32,
}

/// Fire an OS desktop notification — the real "pull the player back" channel,
/// since it pops even when the game window is backgrounded.
#[cfg(not(target_arch = "wasm32"))]
fn notify_desktop(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .arg("-u").arg("critical")
        .arg("-a").arg("IRONVEIN")
        .arg(title)
        .arg(body)
        .spawn();
}

#[cfg(target_arch = "wasm32")]
extern "C" {
    fn ivn_notify(tptr: *const u8, tlen: usize, bptr: *const u8, blen: usize);
    fn ivn_url_param(kptr: *const u8, klen: usize, out: *mut u8, cap: usize) -> i32;
}

/// Read a `?key=value` URL query parameter (browser only). Returns None if the
/// param is absent, empty, or doesn't fit our small scratch buffer.
#[cfg(target_arch = "wasm32")]
fn url_param(key: &str) -> Option<String> {
    let mut buf = [0u8; 128];
    let n = unsafe { ivn_url_param(key.as_ptr(), key.len(), buf.as_mut_ptr(), buf.len()) };
    if n <= 0 {
        return None;
    }
    core::str::from_utf8(&buf[..n as usize]).ok().map(str::to_string)
}

/// A fresh, well-spread map seed from the wall clock — so each new game lands on
/// a different planet. Chosen ONCE in `main`, outside the sim (the sim itself
/// stays float/clock-free); it's then baked into the world and shared to joiners.
fn fresh_seed() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    let raw = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xC0FFEE_D00D);
    #[cfg(target_arch = "wasm32")]
    let raw = (macroquad::miniquad::date::now() * 1_000_000.0) as u64;
    // splitmix64 scramble: a near-sequential clock still yields a spread-out seed
    let mut z = raw.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Derive a map seed from a region name — same name yields the same world on
/// every peer (so a sector is genuinely a shared place), different names give
/// different terrain. FNV-1a, matching the sim's hashing style.
#[cfg(target_arch = "wasm32")]
fn region_seed(region: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in region.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
#[cfg(target_arch = "wasm32")]
fn notify_desktop(title: &str, body: &str) {
    unsafe { ivn_notify(title.as_ptr(), title.len(), body.as_ptr(), body.len()) };
}

struct Args {
    name: String,
    color: u8,
    /// true if color was given explicitly (--color / ?color); else the Settings
    /// colour pick is used.
    color_set: bool,
    host: Option<u16>,
    join: Option<String>,
    listen: u16,
    bots: usize,
    skirmish: bool,
    seed: u64,
    /// true if the seed was pinned explicitly (--seed / ?seed / a named region);
    /// otherwise a fresh game rolls a new map each run.
    seed_set: bool,
    load: Option<String>,
    save_dir: String,
    demo: bool,
    /// browser only: which world region to host/advertise on Nostr
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    region: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        name: std::env::var("USER").unwrap_or_else(|_| "settler".into()),
        color: 0,
        color_set: false,
        host: None,
        join: None,
        listen: 0,
        bots: 1,
        skirmish: false,
        seed: mapgen::POC_SEED,
        seed_set: false,
        load: None,
        save_dir: "saves".into(),
        demo: false,
        region: "A1".into(),
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--name" => { a.name = argv[i + 1].clone(); i += 1; }
            "--color" => { a.color = argv[i + 1].parse().unwrap_or(0); a.color_set = true; i += 1; }
            "--host" => { a.host = Some(argv[i + 1].parse().expect("--host PORT")); i += 1; }
            "--join" => { a.join = Some(argv[i + 1].clone()); i += 1; }
            "--listen" => { a.listen = argv[i + 1].parse().unwrap_or(0); i += 1; }
            "--bots" => { a.bots = argv[i + 1].parse().unwrap_or(1); i += 1; }
            "--map" => { a.skirmish = argv[i + 1] == "skirmish"; i += 1; }
            "--seed" => { a.seed = argv[i + 1].parse().unwrap_or(a.seed); a.seed_set = true; i += 1; }
            "--load" => { a.load = Some(argv[i + 1].clone()); i += 1; }
            "--save-dir" => { a.save_dir = argv[i + 1].clone(); i += 1; }
            "--demo" => { a.demo = true; }
            #[cfg(not(target_arch = "wasm32"))]
            "--render-music" => {
                let path = argv.get(i + 1).cloned().unwrap_or_else(|| "ironvein_music.wav".into());
                match music::render_wav(&path) {
                    Ok(()) => println!("rendered procedural soundtrack loop -> {path}"),
                    Err(e) => eprintln!("render-music failed: {e}"),
                }
                std::process::exit(0);
            }
            #[cfg(not(target_arch = "wasm32"))]
            "--render-stems" => {
                let dir = argv.get(i + 1).cloned().unwrap_or_else(|| "stems_rust".into());
                match music::render_stems_to_dir(&dir) {
                    Ok(paths) => {
                        println!("rendered {} stems to {dir}/ (22050 Hz, 20s loop):", paths.len());
                        for p in paths {
                            println!("  {p}");
                        }
                    }
                    Err(e) => eprintln!("render-stems failed: {e}"),
                }
                std::process::exit(0);
            }
            #[cfg(not(target_arch = "wasm32"))]
            "--render-title" => {
                let path = argv.get(i + 1).cloned().unwrap_or_else(|| "ironvein_title.wav".into());
                match music::render_title_wav(&path) {
                    Ok(()) => println!("rendered title-screen theme -> {path}"),
                    Err(e) => eprintln!("render-title failed: {e}"),
                }
                std::process::exit(0);
            }
            #[cfg(not(target_arch = "wasm32"))]
            "--render-nether" => {
                let path = argv.get(i + 1).cloned().unwrap_or_else(|| "ironvein_nether.wav".into());
                match music::render_nether_wav(&path) {
                    Ok(()) => println!("rendered netherealm bed -> {path}"),
                    Err(e) => eprintln!("render-nether failed: {e}"),
                }
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("ironvein [--name N] [--color 0-7] [--bots N] [--map valley|skirmish] [--seed N]");
                println!("         [--host PORT] [--join IP:PORT] [--listen PORT] [--load FILE] [--save-dir DIR]");
                std::process::exit(0);
            }
            o => eprintln!("(ignoring unknown arg {o})"),
        }
        i += 1;
    }

    // Browser has no argv, so identity/region come from the page URL:
    //   index.html?region=B2&name=Ada&color=3&bots=1&seed=42
    // Open two tabs on different regions and they discover each other on the
    // Nostr map; click a foreign sector to travel (WebRTC-join its host).
    #[cfg(target_arch = "wasm32")]
    {
        let region_param = url_param("region");
        if let Some(r) = &region_param {
            a.region = r.trim().to_uppercase();
        }
        if let Some(n) = url_param("name") {
            a.name = n;
        }
        if let Some(c) = url_param("color").and_then(|s| s.parse::<u8>().ok()) {
            a.color = c % 8;
            a.color_set = true;
        }
        if let Some(b) = url_param("bots").and_then(|s| s.parse::<usize>().ok()) {
            a.bots = b.min(6);
        }
        // explicit ?seed wins; otherwise a named region picks its own terrain.
        // Either way the browser world is pinned (a sector is a stable world).
        match url_param("seed").and_then(|s| s.parse::<u64>().ok()) {
            Some(s) => { a.seed = s; a.seed_set = true; }
            None if region_param.is_some() => { a.seed = region_seed(&a.region); a.seed_set = true; }
            None => {}
        }
    }

    a
}

struct App {
    session: Session,
    bots: Vec<Bot>,
    bot_thought_at: u32,
    cam: Vec2,
    sel: Vec<Eid>,
    drag: Option<Vec2>,
    placing: Option<Kind>,
    /// a charged Missile Silo armed for launch; the next map click is the target
    nuke_arm: Option<Eid>,
    amove: bool,
    chat: Option<String>,
    groups: Vec<Vec<Eid>>,
    /// render interpolation anchors, keyed by (slot index, generation) so a
    /// reused arena slot can't inherit the dead occupant's position
    lerp: HashMap<(u32, u32), (Fp, Fp)>,
    /// last-seen realm (in the nether?) — a realm change replaces the whole entity
    /// arena, so we reset interpolation anchors + selection when it flips.
    nether_seen: bool,
    fx: Vec<gfx::Effect>,
    help: bool,
    units_tab: bool,
    mini: ui::Minimap,
    credits_shown: f32,
    last_status: String,
    banner_age: f32,
    // native persistence + demo-reel screenshotting; unused in the browser
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    save_path: std::path::PathBuf,
    last_saved: u32,
    centered: bool,
    demo: bool,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    demo_shots: Vec<u32>,
    shot_idx: usize,
    /// offscreen scene target for 2x supersampling (anti-aliasing)
    scene: Option<macroquad::prelude::RenderTarget>,
    /// the (screen_w, screen_h) the scene target was sized for
    scene_for: (f32, f32),
    /// "while you were away" report captured on (re)join: (log, capture tick)
    away_report: Option<(ironvein_sim::world::AwayLog, u32)>,
    away_age: f32,
    // ---- live alerts / notifications ----
    toasts: Vec<Toast>,
    /// last-seen hp + is_building for our own entities (to detect damage/loss)
    watch: HashMap<(u32, u32), (i32, bool)>,
    prev_low: bool,
    prev_defeated: bool,
    prev_starving: bool,
    prev_night: bool, // for night-fall / blood-moon / dawn announcements
    atk_cd: f32,   // in-game "under attack" toast cooldown
    notif_cd: f32, // desktop notification cooldown
    idle: f32,     // seconds since the player last gave input
    last_mouse: Vec2,
    // ---- persistent world map ----
    regions: Vec<RegionInfo>,
    show_map: bool,
    my_key: [u8; 32],
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    my_region: String,
    // ---- cross-region travel (browser) ----
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    identity: ironvein_net::Identity,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    my_name: String,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    my_color: u8,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    travel: Option<ironvein_net::Joiner>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    traveling_to: Option<String>,
    // ---- front-end shell + feedback ----
    settings: menu::Settings,
    overlay: menu::Overlay,
    markers: Vec<CmdMarker>,
    /// camera trauma 0..1 → screen shake; decays each frame. Pure presentation.
    shake: f32,
    /// full-screen white-out 0..1 from a near nuke detonation; decays each frame.
    flash: f32,
}

impl App {
    fn world(&self) -> &World {
        &self.session.world
    }
    fn my_pid(&self) -> u8 {
        self.session.my_pid
    }

    fn view(&self) -> Vec2 {
        vec2(screen_width() - ui::SIDEBAR_W, screen_height())
    }

    /// Drop a fading order marker at a tile centre (move/attack/harvest/rally
    /// feedback) and play a soft confirm blip. Pure client-side presentation.
    fn mark(&mut self, t: Tp, color: Color) {
        self.markers.push(CmdMarker {
            at: gfx::tile_to_screen(t.x as f32 + 0.5, t.y as f32 + 0.5),
            color,
            age: 0.0,
        });
        if self.markers.len() > 24 {
            self.markers.remove(0);
        }
        audio::sfx("radar", 0.32);
    }

    /// (Re)allocate the offscreen supersample target to match the window.
    fn ensure_scene(&mut self, sw: f32, sh: f32) {
        const SS: f32 = 1.5; // supersampling factor (2.25x pixels) for anti-aliasing
        if self.scene.is_none() || self.scene_for != (sw, sh) {
            let t = render_target((sw * SS).max(1.0) as u32, (sh * SS).max(1.0) as u32);
            t.texture.set_filter(FilterMode::Linear); // linear downscale = AA
            self.scene = Some(t);
            self.scene_for = (sw, sh);
        }
    }

    fn clamp_cam(&mut self) {
        let v = self.view();
        let (min, max) = gfx::world_bounds(self.world().map.w, self.world().map.h);
        // keep a little of the map on screen at each edge
        self.cam.x = self.cam.x.clamp(min.x, (max.x - v.x).max(min.x));
        self.cam.y = self.cam.y.clamp(min.y - v.y * 0.3, (max.y - v.y * 0.7).max(min.y));
    }

    fn center_on_base(&mut self) {
        let me = self.my_pid();
        let target = self
            .world()
            .ents
            .iter()
            .find(|e| e.owner == me && e.kind == Kind::ConYard)
            .or_else(|| self.world().ents.iter().find(|e| e.owner == me))
            .map(|e| gfx::fpx(e.center()));
        if let Some(t) = target {
            let v = self.view();
            self.cam = t - v * 0.5;
            self.clamp_cam();
            self.centered = true;
        }
    }

    /// world-pixel position of an entity, interpolated between sim ticks
    fn draw_pos(&self, e: &ironvein_sim::Ent) -> Vec2 {
        let a = self.session.alpha();
        match self.lerp.get(&(e.id.idx, e.id.gen)) {
            Some((prev, cur)) if *cur == e.pos => {
                let p = gfx::fpx(*prev);
                let c = gfx::fpx(*cur);
                p.lerp(c, a)
            }
            _ => gfx::fpx(e.pos),
        }
    }

    fn after_ticks(&mut self) {
        // Crossing realms (the descent, or the Rift Altar home) replaces the entire
        // entity arena in one tick, so reused slots would inherit the dead occupants'
        // interpolation anchors (units flashing across the map). On any realm change,
        // reset anchors + the now-dangling selection before the anchors are rebuilt.
        let in_nether = matches!(self.session.world.realm, ironvein_sim::world::Realm::Nether);
        if in_nether != self.nether_seen {
            self.nether_seen = in_nether;
            self.lerp.clear();
            self.sel.clear();
            self.fx.clear();
        }
        // shift interpolation anchors
        let mut seen: Vec<(u32, u32)> = Vec::with_capacity(64);
        for e in self.session.world.ents.iter() {
            if !e.kind.is_unit() {
                continue;
            }
            let key = (e.id.idx, e.id.gen);
            seen.push(key);
            let entry = self.lerp.entry(key).or_insert((e.pos, e.pos));
            if entry.1 != e.pos {
                entry.0 = entry.1;
                entry.1 = e.pos;
            }
        }
        if self.lerp.len() > seen.len() + 32 {
            self.lerp.retain(|k, _| seen.contains(k));
        }
        // collect visual events
        // Distance attenuation: full volume across the visible area, fading to
        // silence ~2 screens out, so an off-camera battle is a faint rumble
        // rather than full-blast noise. Pure presentation — never feeds the sim.
        let view = self.view();
        let center = self.cam + view * 0.5;
        let full_r = view.length() * 0.5;
        let fade_r = full_r * 2.2;
        let atten = |pos: Fp| -> f32 {
            let d = gfx::fpx(pos).distance(center);
            1.0 - ((d - full_r) / (fade_r - full_r)).clamp(0.0, 1.0)
        };
        for ev in self.session.world.events.clone() {
            // presentation-only: map each sim event to a one-shot at a volume
            // scaled by how close it is to the camera. Throttled/deduped in JS.
            use ironvein_sim::world::VisEvent;
            let pos = match &ev {
                VisEvent::Shot { from, .. } => *from,
                VisEvent::Die { at, .. }
                | VisEvent::Built { at }
                | VisEvent::Captured { at }
                | VisEvent::Unload { at, .. }
                | VisEvent::Pickup { at, .. }
                | VisEvent::Nuke { at } => *at,
            };
            let g = atten(pos);
            if g > 0.02 {
                match &ev {
                    VisEvent::Shot { rocket: true, .. } => audio::sfx("rocket", g),
                    VisEvent::Shot { rocket: false, .. } => audio::sfx_either("tank", "rifle", g),
                    VisEvent::Die { big, .. } => audio::sfx("explosion", g * if *big { 1.0 } else { 0.55 }),
                    VisEvent::Built { .. } => audio::sfx("build", g),
                    VisEvent::Captured { .. } => audio::sfx("radar", g),
                    VisEvent::Unload { .. } => audio::sfx_either("mine", "harvest", g),
                    VisEvent::Pickup { .. } => audio::sfx("harvest", g * 0.55),
                    VisEvent::Nuke { .. } => audio::sfx("explosion", (g * 2.0).min(1.0)),
                }
            }
            // camera trauma: weighted by how close the blast is to the viewport, so
            // a far-off skirmish is a faint tremor and a nuke next door rattles hard.
            match &ev {
                VisEvent::Nuke { .. } => {
                    self.shake = (self.shake + 0.9 * g).min(1.0);
                    self.flash = (self.flash + g).min(1.0);
                }
                VisEvent::Die { big: true, .. } => self.shake = (self.shake + 0.30 * g).min(1.0),
                VisEvent::Die { big: false, .. } => self.shake = (self.shake + 0.10 * g).min(0.6),
                VisEvent::Shot { rocket: true, .. } => self.shake = (self.shake + 0.06 * g).min(0.5),
                _ => {}
            }
            self.fx.push(gfx::Effect { ev, age: 0.0 });
        }
        self.check_alerts();
        // autosave: the persistent world is kept by any peer (bytes are
        // identical, any save can re-host); a Survival/Skirmish run is kept by
        // its own player so it can be resumed from the menu's Continue.
        let t = self.session.world.tick;
        let m = self.session.world.mode;
        let keep = m == Mode::Persistent
            || (!matches!(self.session.kind, SessionKind::Client) && (m == Mode::Survival || m == Mode::Skirmish));
        if keep && t / AUTOSAVE_EVERY != self.last_saved / AUTOSAVE_EVERY {
            self.last_saved = t;
            self.save_now(false);
        }
    }

    fn toast(&mut self, text: impl Into<String>, color: Color) {
        self.toasts.push(Toast { text: text.into(), color, age: 0.0 });
        if self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
    }

    /// Native has no cross-region Nostr, so the map shows just our own sector,
    /// with whoever currently controls it (which persists offline).
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_regions(&mut self) {
        let w = &self.session.world;
        let (name, key) = w
            .dominant()
            .and_then(|pid| w.players.get(pid as usize))
            .map(|p| (p.name.clone(), p.key))
            .unwrap_or((String::new(), [0u8; 32]));
        let players = w.players.iter().filter(|p| p.joined).count() as u32;
        let tick = w.tick;
        let host = self.my_key;
        self.regions = vec![RegionInfo { id: self.my_region.clone(), controller: name, ctrl_key: key, host, tick, players }];
    }

    /// Browser: paint the map from the live Nostr region beacons.
    #[cfg(target_arch = "wasm32")]
    fn set_regions(&mut self, beacons: Vec<ironvein_net::nostr::Beacon>) {
        self.regions = beacons
            .into_iter()
            .map(|b| RegionInfo {
                id: b.region,
                controller: b.controller_name,
                ctrl_key: b.controller,
                host: b.host,
                tick: b.tick,
                players: b.players,
            })
            .collect();
    }

    /// Begin travelling to another region (browser only — cross-region travel
    /// rides WebRTC + Nostr). Spins up a `Joiner` to that region's host; the
    /// loop polls it and swaps the session in once the data channel is up.
    #[cfg(target_arch = "wasm32")]
    fn travel_to(&mut self, idx: usize) {
        let Some(reg) = self.regions.get(idx) else { return };
        if reg.host == [0u8; 32] || reg.host == self.my_key {
            return; // nowhere to dial, or you're already here
        }
        let (host, id) = (reg.host, reg.id.clone());
        if let Some(j) = ironvein_net::browser::join(&host, self.identity.clone(), &self.my_name, self.my_color) {
            self.travel = Some(j);
            self.traveling_to = Some(id.clone());
            self.show_map = false;
            self.toast(format!("Traveling to Sector {id}..."), Color::new(0.5, 0.8, 1.0, 1.0));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn travel_to(&mut self, _idx: usize) {
        self.toast("Cross-region travel is a browser feature", Color::new(0.8, 0.8, 0.85, 1.0));
    }

    /// We've arrived in a new region: adopt the session and reset view state.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn arrive(&mut self, session: Session) {
        self.session = session;
        self.sel.clear();
        self.drag = None;
        self.placing = None;
        self.amove = false;
        self.lerp.clear();
        self.fx.clear();
        self.watch.clear();
        self.bots.clear();
        self.groups = vec![Vec::new(); 10];
        self.centered = false;
        self.my_region = self.traveling_to.take().unwrap_or_else(|| self.my_region.clone());
        // surface a "while you were away" for the base waiting in this region
        let w = &self.session.world;
        let me = self.session.my_pid;
        self.away_report = w.players.get(me as usize).and_then(|p| {
            let dur = w.tick.saturating_sub(p.away.from_tick);
            if dur > 300 && p.away.eventful() {
                Some((p.away.clone(), w.tick))
            } else {
                None
            }
        });
        self.away_age = 0.0;
        self.toast(format!("Arrived in Sector {}", self.my_region), Color::new(0.5, 1.0, 0.5, 1.0));
    }

    /// Watch our own faction for damage / losses / power / defeat and raise
    /// alerts. Client-side only (per-player, observational) — no sim state, so
    /// it never affects determinism. High-priority alerts also fire an OS
    /// desktop notification when the player has been idle (i.e. is away).
    fn check_alerts(&mut self) {
        let me = self.session.my_pid;
        // -- gather everything from the (immutably borrowed) world first --
        let (took_damage, lost_bld, lost_unit, new_watch, low, defeated, starving) = {
            let w = &self.session.world;
            let mut new_watch: HashMap<(u32, u32), (i32, bool)> = HashMap::with_capacity(64);
            let mut took_damage = false;
            for e in w.ents.iter() {
                if e.owner != me || e.hp <= 0 {
                    continue;
                }
                let key = (e.id.idx, e.id.gen);
                new_watch.insert(key, (e.hp, e.kind.is_building()));
                if let Some(&(old_hp, _)) = self.watch.get(&key) {
                    if e.hp < old_hp {
                        took_damage = true;
                    }
                }
            }
            let mut lost_bld = 0u32;
            let mut lost_unit = 0u32;
            for (key, &(old_hp, was_bld)) in &self.watch {
                if old_hp > 0 && !new_watch.contains_key(key) {
                    if was_bld {
                        lost_bld += 1;
                    } else {
                        lost_unit += 1;
                    }
                }
            }
            let (low, defeated, starving) = w
                .players
                .get(me as usize)
                .map(|p| (p.low_power(), p.defeated, p.starving))
                .unwrap_or((false, false, false));
            (took_damage, lost_bld, lost_unit, new_watch, low, defeated, starving)
        };
        self.watch = new_watch;

        // -- now emit alerts (mutating self) --
        if took_damage && self.atk_cd <= 0.0 {
            self.atk_cd = 7.0;
            self.toast("Your base is under attack!", Color::new(1.0, 0.35, 0.3, 1.0));
            if !self.demo && self.idle > 45.0 && self.notif_cd <= 0.0 {
                self.notif_cd = 60.0;
                notify_desktop("IRONVEIN — under attack", "Your settlement is being raided. Get back in there.");
            }
        }
        if lost_bld > 0 {
            self.toast(format!("{lost_bld} building(s) destroyed!"), Color::new(1.0, 0.4, 0.35, 1.0));
        }
        if lost_unit >= 3 {
            self.toast(format!("{lost_unit} units lost in battle"), Color::new(1.0, 0.6, 0.3, 1.0));
        }
        if low && !self.prev_low {
            self.toast("Low power - production slowed", Color::new(0.95, 0.85, 0.3, 1.0));
        }
        self.prev_low = low;
        if starving && !self.prev_starving {
            self.toast("Your army is starving! Build Farms or hunt deer", Color::new(1.0, 0.4, 0.3, 1.0));
        }
        self.prev_starving = starving;
        if defeated && !self.prev_defeated {
            self.toast("You have been eliminated", Color::new(1.0, 0.3, 0.3, 1.0));
            if !self.demo {
                notify_desktop("IRONVEIN — eliminated", "Your settlement has fallen.");
            }
        }
        self.prev_defeated = defeated;
    }

    /// How dangerous the moment feels (0 calm .. 1 frantic), for the adaptive
    /// soundtrack. Presentation only — reads the world, never mutates the sim.
    fn music_intensity(&self) -> f32 {
        let w = &self.session.world;
        let me = self.my_pid();
        let mut x: f32 = 0.0;
        if ironvein_sim::world::is_night(w.tick) {
            x += 0.22;
        }
        if ironvein_sim::world::is_blood_moon(w.tick) {
            x += 0.13;
        }
        if self.atk_cd > 0.0 {
            x += 0.28; // base recently took fire
        }
        if w.ents.iter().any(|e| e.kind.is_boss() && e.hp > 0) {
            x += 0.45; // a boss on the field cranks the dread
        }
        if let Some(p) = w.players.get(me as usize) {
            if p.starving {
                x += 0.06;
            }
            if p.low_power() {
                x += 0.04;
            }
        }
        // monsters massing near my territory
        let (mut sx, mut sy, mut n) = (0i64, 0i64, 0i64);
        for e in w.ents.iter() {
            if e.owner == me && e.kind.is_building() {
                let t = e.tile();
                sx += t.x as i64;
                sy += t.y as i64;
                n += 1;
            }
        }
        if n > 0 {
            let (cx, cy) = ((sx / n) as i32, (sy / n) as i32);
            let near = w
                .ents
                .iter()
                .filter(|e| e.kind.is_monster() && e.hp > 0 && (e.tile().x - cx).abs() <= 34 && (e.tile().y - cy).abs() <= 34)
                .count();
            x += (near as f32 / 8.0).min(1.0) * 0.30;
        }
        x.clamp(0.0, 1.0)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_now(&self, announce: bool) {
        let bytes = self.session.world.save_bytes();
        if let Some(dir) = self.save_path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let tmp = self.save_path.with_extension("iv.tmp");
        let ok = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &self.save_path)).is_ok();
        if announce {
            println!("{} -> {}", if ok { "saved" } else { "SAVE FAILED" }, self.save_path.display());
        }
    }

    /// Write the live world to manual save slot `n` (+ a human label sidecar the
    /// load screen reads). Triggered from the in-game pause → SAVE GAME.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_to_slot(&mut self, n: usize) {
        let w = &self.session.world;
        let bytes = w.save_bytes();
        std::fs::create_dir_all("saves").ok();
        let path = menu::slot_iv_path(n);
        let tmp = format!("{path}.tmp");
        let ok = std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &path)).is_ok();
        if ok {
            let mode = match w.mode {
                Mode::Survival => "Survival",
                Mode::Skirmish => "Skirmish",
                Mode::Persistent => "Persistent",
            };
            let day = w.tick / 6000 + 1;
            let name: String = w.players.get(self.session.my_pid as usize).map(|p| p.name.clone()).unwrap_or_default();
            menu::write_slot_label(n, &format!("{mode} · Day {day} · {name}"));
            self.toast(format!("Saved to slot {n}"), Color::new(0.6, 1.0, 0.5, 1.0));
        } else {
            self.toast("Save failed", Color::new(1.0, 0.4, 0.3, 1.0));
        }
    }

    // In the browser there is no local filesystem; persistence rides on the
    // peers and seed nodes (every peer's bytes are identical), and the JS
    // shim can mirror to localStorage if desired.
    #[cfg(target_arch = "wasm32")]
    fn save_now(&self, _announce: bool) {}
    #[cfg(target_arch = "wasm32")]
    fn save_to_slot(&mut self, _n: usize) {}

    fn validate_selection(&mut self) {
        let me = self.my_pid();
        let w = &self.session.world;
        self.sel.retain(|id| w.ents.get(*id).map(|e| e.owner == me && e.hp > 0).unwrap_or(false));
        // drop a nuke arming if its silo died or already fired (discharged)
        if let Some(silo) = self.nuke_arm {
            let ok = w.ents.get(silo).map(|e| e.owner == me && e.kind == Kind::MissileSilo && e.work_t >= ironvein_sim::stats::NUKE_CHARGE).unwrap_or(false);
            if !ok {
                self.nuke_arm = None;
            }
        }
    }

    fn selected_units(&self) -> Vec<Eid> {
        let w = self.world();
        self.sel.iter().copied().filter(|id| w.ents.get(*id).map(|e| e.kind.is_unit()).unwrap_or(false)).collect()
    }
}

fn window_conf() -> Conf {
    Conf {
        window_title: "IRONVEIN".to_string(),
        window_width: 1280,
        window_height: 800,
        high_dpi: false,
        ..Default::default()
    }
}

/// Dial `host` over WebRTC and pump the handshake (~15s) until it yields a live
/// session, the host refuses, or we give up. Returns None to bounce back to the
/// lobby. Shared by the public list and the private room-code path.
#[cfg(target_arch = "wasm32")]
async fn try_join(
    matchmaker: &mut ironvein_net::browser::Matchmaker,
    host: &[u8; 32],
    identity: &ironvein_net::Identity,
    name: &str,
    color: u8,
) -> Option<Session> {
    let mut joiner = ironvein_net::browser::join(host, identity.clone(), name, color)?;
    let deadline = get_time() + 15.0;
    while get_time() < deadline {
        matchmaker.pump();
        match joiner.poll() {
            Some(Ok(session)) => return Some(session),
            Some(Err(_)) => return None, // refused (full / declined)
            None => {}
        }
        splash("Joining colony..");
        next_frame().await;
    }
    None
}

/// Browser startup rendezvous: browse public colonies, join one, found a new
/// (public or invite-only) colony, or enter a friend's room code to find their
/// private world. Renders the lobby each frame while it listens for beacons.
#[cfg(target_arch = "wasm32")]
async fn discover_and_connect(
    matchmaker: &mut ironvein_net::browser::Matchmaker,
    world: World,
    identity: ironvein_net::Identity,
    name: &str,
    color: u8,
    bots: &[String],
) -> Session {
    let my_key = matchmaker.my_key();
    let my_code = matchmaker.room_code(); // our own shareable invite code
    let mut world = Some(world); // moved out only when we choose to host
    let mut code_input = String::new();
    // when set, we're waiting for an invite-only host's beacon: (derived region, give-up time)
    let mut pending: Option<(String, f64)> = None;

    // Interactive lobby: browse public colonies, JOIN one, FOUND a new (public or
    // invite-only) one, or enter a friend's room code to find their private world.
    loop {
        matchmaker.pump();

        // room-code text entry
        while let Some(ch) = get_char_pressed() {
            if ch.is_ascii_alphanumeric() && code_input.len() < 8 {
                code_input.push(ch.to_ascii_uppercase());
            }
        }
        if is_key_pressed(KeyCode::Backspace) {
            code_input.pop();
        }
        if is_key_pressed(KeyCode::Escape) {
            pending = None;
        }

        // waiting on a private host: watch for the beacon under its derived region
        if let Some((region, deadline)) = pending.clone() {
            if get_time() > deadline {
                pending = None;
            } else if let Some(b) =
                matchmaker.regions().into_iter().find(|b| b.region == region && b.host != my_key && b.host != [0u8; 32])
            {
                if let Some(session) = try_join(matchmaker, &b.host, &identity, name, color).await {
                    return session;
                }
                pending = None; // refused / timed out → back to the lobby
            } else {
                splash("Looking for private colony.. (Esc to cancel)");
                next_frame().await;
                continue;
            }
        }

        // public colony list: not us, not invite-only, not the zero placeholder
        let mut seen = std::collections::HashSet::new();
        let beacons: Vec<_> = matchmaker
            .regions()
            .into_iter()
            .filter(|b| !b.private && b.host != my_key && b.host != [0u8; 32])
            .filter(|b| seen.insert(b.host))
            .collect();
        let rows: Vec<menu::LobbyRow> = beacons
            .iter()
            .map(|b| {
                let who = if b.controller_name.is_empty() { "a colony".to_string() } else { b.controller_name.clone() };
                menu::LobbyRow {
                    title: format!("{who}  ·  Sector {}", b.region),
                    sub: format!("{} player{}  ·  day {}", b.players, if b.players == 1 { "" } else { "s" }, b.tick / 6000 + 1),
                }
            })
            .collect();

        let m = vec2(mouse_position().0, mouse_position().1);
        let click = is_mouse_button_pressed(MouseButton::Left);
        match menu::lobby_screen(name, color, &rows, &my_code, &code_input, m, click) {
            menu::LobbyAction::Join(i) => {
                if let Some(session) = try_join(matchmaker, &beacons[i].host, &identity, name, color).await {
                    return session;
                }
            }
            menu::LobbyAction::JoinCode => {
                // the code is the meeting place: derive its private topic, start
                // listening, and wait for that host's beacon to arrive.
                let region = ironvein_net::nostr::room_code_region(&code_input);
                matchmaker.watch_code(&code_input);
                pending = Some((region, get_time() + 25.0));
            }
            menu::LobbyAction::Host => {
                splash("Founding your colony..");
                next_frame().await;
                return ironvein_net::browser::host(world.take().unwrap(), identity, name, color, bots);
            }
            menu::LobbyAction::HostPrivate => {
                // beacon only on the code-derived topic; dwell on the splash so the
                // host can read/share the code before the world spins up.
                matchmaker.go_private();
                let until = get_time() + 4.0;
                while get_time() < until {
                    matchmaker.pump();
                    splash(&format!("PRIVATE COLONY  -  share code  {my_code}"));
                    next_frame().await;
                }
                return ironvein_net::browser::host(world.take().unwrap(), identity, name, color, bots);
            }
            menu::LobbyAction::None => {}
        }
        next_frame().await;
    }
}

/// A centered loading message on the dark backdrop (browser rendezvous splash).
#[cfg(target_arch = "wasm32")]
fn splash(msg: &str) {
    clear_background(Color::new(0.05, 0.06, 0.07, 1.0));
    let title = "IRONVEIN";
    let td = measure_text(title, None, 52, 1.0);
    draw_text(
        title,
        screen_width() / 2.0 - td.width / 2.0,
        screen_height() / 2.0 - 50.0,
        52.0,
        Color::new(0.85, 0.42, 0.22, 1.0),
    );
    let d = measure_text(msg, None, 26, 1.0);
    draw_text(
        msg,
        screen_width() / 2.0 - d.width / 2.0,
        screen_height() / 2.0 + 10.0,
        26.0,
        Color::new(0.78, 0.72, 0.55, 1.0),
    );
}

#[macroquad::main(window_conf)]
async fn main() {
    let args = parse_args();

    // Front-end: load settings, bring audio up (so the menu has music once a
    // click unlocks it), then show the main menu before anything connects.
    let mut settings = menu::Settings::load();
    audio::preload();
    settings.apply_audio();
    audio::start_ambience();

    // The music engine (the ported synth3.py) takes ~2s to render its loop, so
    // show a frame first — neither native nor web should stare at a black screen
    // while it synthesises — then build the soundtrack.
    {
        clear_background(Color::new(0.05, 0.06, 0.07, 1.0));
        let t = "IRONVEIN";
        let td = measure_text(t, None, 52, 1.0);
        draw_text(t, screen_width() / 2.0 - td.width / 2.0, screen_height() / 2.0 - 40.0, 52.0, Color::new(0.85, 0.42, 0.22, 1.0));
        let m = "composing the soundtrack..";
        let md = measure_text(m, None, 24, 1.0);
        draw_text(m, screen_width() / 2.0 - md.width / 2.0, screen_height() / 2.0 + 14.0, 24.0, Color::new(0.78, 0.72, 0.55, 1.0));
        next_frame().await;
    }
    audio::prepare_music();
    // ...then a softer, bedded title theme for the menu (the gameplay stems stay
    // silent on the menu — they only ride up with battle intensity in-game).
    audio::prepare_title();
    // ...and the netherealm bed (silent until the player descends), composed now
    // so the descent never hitches mid-game.
    audio::prepare_nether();

    // Per-mode save slots so Survival/Skirmish runs each resume independently.
    let save_dir = std::path::PathBuf::from(&args.save_dir);
    let surv_path = save_dir.join("survival.iv");
    let skir_path = save_dir.join("skirmish.iv");
    #[cfg(not(target_arch = "wasm32"))]
    let (surv_save, skir_save) = (surv_path.exists(), skir_path.exists());
    #[cfg(target_arch = "wasm32")]
    let (surv_save, skir_save) = (false, false); // no disk in the browser

    // Menu picks mode + difficulty (or Continue); demo skips it for CLI defaults.
    let mut chosen: Option<menu::StartChoice> = None;
    if !args.demo {
        match menu::start_screen(&mut settings, &args.region, surv_save, skir_save).await {
            menu::StartChoice::Quit => {
                #[cfg(not(target_arch = "wasm32"))]
                std::process::exit(0);
            }
            other => chosen = Some(other),
        }
        settings.apply_audio();
    }
    // Leaving the title screen: stop the menu theme so the in-game soundtrack (the
    // adaptive stems, ridden by battle intensity) takes over cleanly.
    audio::stop_title();

    // Skirmish bot count rises with difficulty; survival has no rivals.
    let bots_for = |d: u8| -> usize { match d { 0 => 1, 2 => 3, _ => 2 } };
    // a manual save slot picked from the menu's LOAD GAME screen
    let load_slot: Option<usize> = match &chosen {
        Some(menu::StartChoice::LoadSlot(n)) => Some(*n),
        _ => None,
    };
    let (mode, difficulty, mut bot_count, mut save_path, continue_run) = match chosen {
        Some(menu::StartChoice::Start { survival: true, difficulty, continue_run }) => {
            (Mode::Survival, difficulty, 0, surv_path.clone(), continue_run)
        }
        Some(menu::StartChoice::Start { survival: false, difficulty, continue_run }) => {
            (Mode::Skirmish, difficulty, bots_for(difficulty), skir_path.clone(), continue_run)
        }
        // LoadSlot: mode/bots/save_path are taken from the loaded world below
        Some(menu::StartChoice::LoadSlot(..)) => (Mode::Persistent, 1, 0, save_dir.join("world.iv"), false),
        // demo / headless: honor the CLI flags, persistent world.iv
        _ => (if args.skirmish { Mode::Skirmish } else { Mode::Persistent }, 1, args.bots, save_dir.join("world.iv"), false),
    };

    // World: a manual load slot, a resumed run (Continue), an explicit --load, or fresh.
    let world = if let Some(n) = load_slot {
        let bytes = std::fs::read(menu::slot_iv_path(n)).expect("read save slot");
        World::load_bytes(&bytes).expect("parse save slot")
    } else if continue_run {
        let bytes = std::fs::read(&save_path).expect("read saved run");
        World::load_bytes(&bytes).expect("parse saved run")
    } else if let Some(p) = &args.load {
        let bytes = std::fs::read(p).expect("read save file");
        World::load_bytes(&bytes).expect("parse save file")
    } else {
        // fresh game: roll a new map each run unless the seed was pinned
        // (--seed, ?seed, or a named region). The seed is baked into the world
        // at creation, saved, and shared to joiners via the snapshot, so this
        // stays deterministic for the session.
        let seed = if args.seed_set { args.seed } else { fresh_seed() };
        let mut w = mapgen::verdant_divide(seed, mode);
        w.difficulty = difficulty;
        w
    };
    // On Continue or a slot-load, the loaded world's own difficulty governs the
    // bot count so the same rivals re-bind to their saved bases.
    if continue_run || load_slot.is_some() {
        bot_count = if world.mode == Mode::Skirmish { bots_for(world.difficulty) } else { 0 };
    }
    // a slot-load then autosaves to its own per-mode file (so Continue finds it)
    if load_slot.is_some() {
        save_path = match world.mode {
            Mode::Survival => surv_path.clone(),
            Mode::Skirmish => skir_path.clone(),
            Mode::Persistent => save_dir.join("world.iv"),
        };
    }

    let bot_roster: Vec<String> = (0..bot_count)
        .map(|i| format!("Bot {}", ["Gravel", "Sable", "Rust", "Moss", "Flint", "Ash"][i % 6]))
        .collect();

    // Your faction colour: an explicit --color/?color wins; otherwise the colour
    // you picked in Settings. (The sim still bumps it if it clashes with someone
    // already in the region — see cmd_join.)
    let color = if args.color_set { args.color } else { settings.color };

    // one keypair per callsign, persisted: your base can only be reclaimed
    // by this key, on any host, forever. (On wasm the browser localStorage is
    // owned by the JS shim; here we mint a fresh in-memory key per session.)
    #[cfg(not(target_arch = "wasm32"))]
    let identity = {
        let id_file: String = args.name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        ironvein_net::Identity::load_or_create(std::path::Path::new(&format!("saves/id-{id_file}.key")))
    };
    #[cfg(target_arch = "wasm32")]
    let identity = ironvein_net::Identity::generate();

    // keep a copy of the identity for cross-region travel (re-joining elsewhere)
    let identity_for_app = identity.clone();

    // Native: TCP host/join/solo from the CLI flags.
    #[cfg(not(target_arch = "wasm32"))]
    let session = if let Some(addr) = &args.join {
        println!("dialing {addr} …");
        Session::join(addr, identity, args.listen, &args.name, color).expect("join failed")
    } else if let Some(port) = args.host {
        Session::host(world, identity, port, &args.name, color, &bot_roster).expect("bind failed")
    } else {
        Session::solo(world, identity, &args.name, color, &bot_roster)
    };

    // Browser: host a region over WebRTC and advertise it on Nostr relays.
    // (Joining a discovered region is wired through the in-game region browser;
    // see ARCHITECTURE.md §browser bring-up.)
    #[cfg(target_arch = "wasm32")]
    let mut matchmaker = ironvein_net::browser::Matchmaker::new(
        identity.clone(),
        // Several open relays: two peers only meet if they share a connected
        // one, so more (and more reliable) relays cut the odds of a split-brain
        // where each is talking to a different subset. All standard relays
        // forward our ephemeral kinds (29000/29001) to live subscribers.
        &[
            // Permissive, open-write relays that forward our ephemeral kinds.
            // (Dropped offchain.pub — web-of-trust gated, rejected our writes;
            // damus rate-limits hard, so it's last-resort backup only.)
            "wss://nos.lol",
            "wss://relay.primal.net",
            "wss://nostr.mom",
            "wss://relay.damus.io",
        ],
        &args.region,
    );
    #[cfg(target_arch = "wasm32")]
    let session = discover_and_connect(&mut matchmaker, world, identity, &args.name, color, &bot_roster).await;

    let my_key = session.my_key();
    let app_identity = identity_for_app;

    // "while you were away": if our reclaimed settlement accrued income or
    // took raids during a real absence, surface a report on login.
    let away_report = {
        let w = &session.world;
        let me = session.my_pid;
        w.players.get(me as usize).and_then(|p| {
            let dur = w.tick.saturating_sub(p.away.from_tick);
            if dur > 300 && p.away.eventful() {
                Some((p.away.clone(), w.tick))
            } else {
                None
            }
        })
    };

    let mut bots: Vec<Bot> = session
        .bot_pids()
        .into_iter()
        .map(|pid| {
            let name = session.roster.get(&pid).map(|p| p.name.clone()).unwrap_or_else(|| format!("Bot {pid}"));
            Bot::new(pid, name)
        })
        .collect();
    if args.demo {
        // in the demo reel, a bot pilots the local faction too
        bots.push(Bot::new(session.my_pid, args.name.clone()));
    }

    let mini = ui::Minimap::new(session.world.map.w, session.world.map.h);
    let mut app = App {
        session,
        bots,
        bot_thought_at: u32::MAX,
        cam: vec2(0.0, 0.0),
        sel: Vec::new(),
        drag: None,
        placing: None,
        nuke_arm: None,
        amove: false,
        chat: None,
        groups: vec![Vec::new(); 10],
        lerp: HashMap::new(),
        nether_seen: false,
        fx: Vec::new(),
        help: false,
        units_tab: false,
        mini,
        credits_shown: 0.0,
        last_status: String::new(),
        banner_age: 99.0,
        save_path,
        last_saved: 0,
        centered: false,
        demo: args.demo,
        demo_shots: vec![700, 2800, 5200],
        shot_idx: 0,
        scene: None,
        scene_for: (0.0, 0.0),
        away_report,
        away_age: 0.0,
        toasts: Vec::new(),
        watch: HashMap::new(),
        prev_low: false,
        prev_defeated: false,
        prev_starving: false,
        prev_night: false,
        atk_cd: 0.0,
        notif_cd: 0.0,
        idle: 0.0,
        last_mouse: vec2(0.0, 0.0),
        regions: Vec::new(),
        show_map: false,
        my_key,
        my_region: args.region.clone(),
        identity: app_identity,
        my_name: args.name.clone(),
        my_color: color,
        travel: None,
        traveling_to: None,
        settings,
        overlay: menu::Overlay::None,
        markers: Vec::new(),
        shake: 0.0,
        flash: 0.0,
    };
    app.last_saved = app.session.world.tick;

    loop {
        let dt = (get_frame_time() as f64).min(0.25);

        // keep native looping music/ambience topped up (no-op in the browser)
        audio::update();
        // browser: once the stems finish downloading, upgrade master → adaptive
        audio::poll_adaptive();
        // in the netherealm a dedicated haunting bed takes over (stems duck out) —
        // and it starts swelling DURING the descent build-up, before the world flips
        // (the descent's stale-anchor reset is handled in `after_ticks`).
        audio::set_nether(
            matches!(app.session.world.realm, ironvein_sim::world::Realm::Nether) || app.session.world.descent_at != 0,
        );
        // ride the adaptive soundtrack on how dangerous things feel right now
        if audio::adaptive_music() {
            audio::set_music_intensity(app.music_intensity());
        }
        audio::pump_nether();

        // age out the "while you were away" report (any key or ~20s dismisses)
        if app.away_report.is_some() {
            app.away_age += dt as f32;
            if app.away_age > 20.0 || get_last_key_pressed().is_some() {
                app.away_report = None;
            }
        }

        // tick alert cooldowns, toast lifetimes, and idle (away) detection
        let dt32 = dt as f32;
        app.atk_cd = (app.atk_cd - dt32).max(0.0);
        app.notif_cd = (app.notif_cd - dt32).max(0.0);
        for t in app.toasts.iter_mut() {
            t.age += dt32;
        }
        app.toasts.retain(|t| t.age < 5.0);
        let m = vec2(mouse_position().0, mouse_position().1);
        let active = m.distance(app.last_mouse) > 1.0
            || is_mouse_button_down(MouseButton::Left)
            || is_mouse_button_down(MouseButton::Right);
        app.last_mouse = m;
        if active {
            app.idle = 0.0;
        } else {
            app.idle += dt32;
        }

        // world map: toggle with M, refresh its sectors while open
        let map_was_open = app.show_map;
        if app.chat.is_none() && (is_key_pressed(KeyCode::M) || (app.show_map && is_key_pressed(KeyCode::Escape))) {
            app.show_map = !app.show_map;
        }

        // pause overlay: ESC opens it when nothing else is consuming ESC (chat,
        // map close, building placement, attack-move are handled elsewhere).
        let esc = is_key_pressed(KeyCode::Escape);
        let mut esc_opened_overlay = false;
        if !app.demo
            && app.overlay == menu::Overlay::None
            && app.chat.is_none()
            && !map_was_open
            && !app.show_map
            && app.placing.is_none()
            && !app.amove
            && esc
        {
            app.overlay = menu::Overlay::Pause;
            audio::sfx("radar", 0.4);
            esc_opened_overlay = true;
        }

        if app.show_map {
            #[cfg(not(target_arch = "wasm32"))]
            app.refresh_regions();
            #[cfg(target_arch = "wasm32")]
            {
                let r = matchmaker.regions();
                app.set_regions(r);
            }
        }

        // Browser: shuttle WebRTC SDP/ICE across Nostr each frame, and keep
        // our region beacon fresh so newcomers can find this world.
        #[cfg(target_arch = "wasm32")]
        {
            matchmaker.pump();
            // drive an in-flight cross-region travel to completion
            if let Some(j) = app.travel.as_mut() {
                if let Some(result) = j.poll() {
                    app.travel = None;
                    match result {
                        Ok(new_session) => app.arrive(new_session),
                        Err(e) => app.toast(format!("Travel failed: {e}"), Color::new(1.0, 0.4, 0.35, 1.0)),
                    }
                }
            }
            // Only the region's host beacons it; a joined client stays silent
            // so it doesn't advertise a duplicate world. If this client later
            // inherits the region through host migration, kind flips to Host
            // and the beacon resumes automatically.
            if app.session.kind == SessionKind::Host {
                let w = &app.session.world;
                let players = w.players.iter().filter(|p| p.joined).count() as u32;
                let (ckey, cname) = w
                    .dominant()
                    .and_then(|pid| w.players.get(pid as usize))
                    .map(|p| (p.key, p.name.clone()))
                    .unwrap_or(([0u8; 32], String::new()));
                matchmaker.advertise(w.tick, players, args.seed, ckey, &cname);
            }
        }

        // After a host migration this peer may have inherited the dead host's
        // bots; spawn brains for any bot pid we now drive but don't yet steer.
        for pid in app.session.bot_pids() {
            if !app.bots.iter().any(|b| b.pid == pid) {
                let name = app.session.roster.get(&pid).map(|p| p.name.clone()).unwrap_or_else(|| format!("Bot {pid}"));
                app.bots.push(Bot::new(pid, name));
            }
        }

        // bots think once per sim tick, on whichever peer owns them
        if app.session.world.tick != app.bot_thought_at && !app.bots.is_empty() {
            app.bot_thought_at = app.session.world.tick;
            let mut queued: Vec<(u8, Command)> = Vec::new();
            for b in app.bots.iter_mut() {
                for c in b.think(&app.session.world) {
                    queued.push((b.pid, c));
                }
            }
            for (pid, c) in queued {
                app.session.queue_as(pid, c);
            }
        }

        let steps = if app.demo {
            let mut n = 0;
            for _ in 0..24 {
                if app.session.world.tick != app.bot_thought_at {
                    app.bot_thought_at = app.session.world.tick;
                    let mut queued: Vec<(u8, Command)> = Vec::new();
                    for b in app.bots.iter_mut() {
                        for c in b.think(&app.session.world) {
                            queued.push((b.pid, c));
                        }
                    }
                    for (pid, c) in queued {
                        app.session.queue_as(pid, c);
                    }
                }
                n += app.session.update(ironvein_net::TICK_DT);
            }
            n
        } else {
            app.session.update(dt)
        };
        if steps > 0 {
            app.after_ticks();
        }
        // announce nightfall / blood moons / dawn — the heartbeat of the survival loop
        {
            let tick = app.session.world.tick;
            let night = ironvein_sim::world::is_night(tick);
            if night != app.prev_night {
                app.prev_night = night;
                if night {
                    let n = ironvein_sim::world::night_count(tick);
                    if ironvein_sim::world::is_blood_moon(tick) {
                        app.toast(format!("BLOOD MOON  —  Night {n}.  The horde comes from every side."), Color::new(1.0, 0.25, 0.2, 1.0));
                        audio::sfx("explosion", 0.5);
                    } else {
                        app.toast(format!("Night {n} falls  —  the dark stirs."), Color::new(0.55, 0.6, 0.85, 1.0));
                        audio::sfx("radar", 0.6);
                    }
                } else {
                    app.toast("Dawn breaks  —  the dark recedes.", Color::new(0.95, 0.82, 0.42, 1.0));
                }
            }
        }
        if !app.centered && app.world().players.get(app.my_pid() as usize).map(|p| p.joined).unwrap_or(false) {
            app.center_on_base();
        }
        app.validate_selection();

        if app.session.status != app.last_status {
            app.last_status = app.session.status.clone();
            app.banner_age = 0.0;
        }
        app.banner_age += dt as f32;

        // money odometer
        let target = app.world().players.get(app.my_pid() as usize).map(|p| p.credits as f32).unwrap_or(0.0);
        app.credits_shown += (target - app.credits_shown) * (dt as f32 * 8.0).min(1.0);
        if (app.credits_shown - target).abs() < 1.0 {
            app.credits_shown = target;
        }

        if app.demo {
            demo_camera(&mut app);
        } else if app.overlay == menu::Overlay::None {
            handle_input(&mut app, dt as f32);
        }

        for f in app.fx.iter_mut() {
            f.age += dt as f32;
        }
        app.fx.retain(|f| f.age < gfx::effect_ttl(&f.ev));
        for mk in app.markers.iter_mut() {
            mk.age += dt as f32;
        }
        app.markers.retain(|mk| mk.age < 0.6);
        // screen shake / flash decay (frame-rate independent)
        app.shake = (app.shake - dt as f32 * 1.7).max(0.0);
        app.flash = (app.flash - dt as f32 * 2.4).max(0.0);
        // the descent build-up: a tremor that rises as the rift takes hold
        if app.session.world.descent_at != 0 {
            let togo = app.session.world.descent_at.saturating_sub(app.session.world.tick) as f32;
            app.shake = app.shake.max((1.0 - togo / 36.0).clamp(0.0, 1.0) * 0.85);
        }

        draw(&mut app);

        // pause overlay draws on top of the live game; the sim/network kept
        // pumping above, so this is safe in multiplayer (a local view, not a
        // real pause). esc that just opened it must not also close it.
        if app.overlay != menu::Overlay::None {
            let (mx, my) = mouse_position();
            let m = vec2(mx, my);
            let click = is_mouse_button_pressed(MouseButton::Left);
            let down = is_mouse_button_down(MouseButton::Left);
            match menu::overlay_frame(&mut app.overlay, &mut app.settings, m, click, down, esc && !esc_opened_overlay) {
                menu::OverlayResult::Quit =>
                {
                    #[cfg(not(target_arch = "wasm32"))]
                    std::process::exit(0)
                }
                menu::OverlayResult::SaveSlot(n) => app.save_to_slot(n),
                menu::OverlayResult::None => {}
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        if app.demo {
            if let Some(&at) = app.demo_shots.get(app.shot_idx) {
                if app.session.world.tick >= at {
                    screenshot(&format!("shots/ironvein_{}.png", app.shot_idx + 1));
                    app.shot_idx += 1;
                }
            } else {
                std::process::exit(0);
            }
        }
        next_frame().await
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn mouse_world(app: &App) -> Vec2 {
    let (mx, my) = mouse_position();
    vec2(mx, my) + app.cam
}

/// Pick the entity under an iso-screen point `wp` (= mouse + camera). Units
/// are hit by proximity to their billboard; buildings by the ground tile
/// under the cursor falling inside their footprint. The top-most (greatest
/// iso depth) match wins, with units preferred over buildings on a tie.
fn ent_hit(world: &World, wp: Vec2, my_pid: u8) -> Option<Eid> {
    let tf = gfx::screen_to_tilef(wp);
    let tile = Tp::new(tf.x.floor() as i32, tf.y.floor() as i32);
    let mut best: Option<(i64, i32, Eid)> = None;
    for e in world.ents.iter() {
        if e.hp <= 0 {
            continue;
        }
        let hit = if e.kind.is_building() {
            let bt = e.tile();
            let (fw, fh) = e.foot();
            let inside = |t: Tp| t.x >= bt.x && t.x < bt.x + fw && t.y >= bt.y && t.y < bt.y + fh;
            // test the ground tile under the cursor, and the tile under the
            // cursor projected down by the building's height (so clicking the
            // raised roof/walls selects it too)
            let roof = gfx::screen_to_tilef(wp + vec2(0.0, gfx::bld_height(fw, fh)));
            inside(tile) || inside(Tp::new(roof.x.floor() as i32, roof.y.floor() as i32))
        } else {
            let body = gfx::fpx(e.center()) - vec2(0.0, 8.0);
            (body - wp).length() < gfx::TW * 0.28
        };
        if hit {
            let c = e.center();
            let depth = (c.x + c.y) as i64;
            let score = if e.kind.is_unit() { 2 } else { 0 } + if e.owner == my_pid { 1 } else { 0 };
            if best.map(|(d, s, _)| (depth, score) >= (d, s)).unwrap_or(true) {
                best = Some((depth, score, e.id));
            }
        }
    }
    best.map(|(_, _, id)| id)
}

/// The tile under an iso-screen point (mouse + camera).
fn tile_at(wp: Vec2) -> Tp {
    let tf = gfx::screen_to_tilef(wp);
    Tp::new(tf.x.floor() as i32, tf.y.floor() as i32)
}

fn handle_input(app: &mut App, dt: f32) {
    let (mx, my) = mouse_position();
    let m = vec2(mx, my);
    let in_view = m.x < screen_width() - ui::SIDEBAR_W;

    // ---- world map is a modal overlay: click a sector to travel ----
    if app.show_map {
        if is_mouse_button_pressed(MouseButton::Left) {
            if let Some(idx) = ui::world_map_pick(&app.regions, app.view(), m) {
                app.travel_to(idx);
            }
        }
        return; // swallow game input while the map is open
    }

    // ---- chat capture eats the keyboard ----
    if let Some(buf) = &mut app.chat {
        while let Some(ch) = get_char_pressed() {
            if !ch.is_control() && buf.len() < 110 {
                buf.push(ch);
            }
        }
        if is_key_pressed(KeyCode::Backspace) {
            buf.pop();
        }
        if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
            let text = app.chat.take().unwrap();
            let trimmed = text.trim().to_string();
            if let Some(rest) = trimmed.strip_prefix("/give ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() == 2 {
                    if let (Ok(to), Ok(amount)) = (parts[0].parse::<u8>(), parts[1].parse::<u32>()) {
                        app.session.queue(Command::GiveCredits { to, amount });
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix("/ally ") {
                if let Ok(with) = rest.trim().parse::<u8>() {
                    app.session.queue(Command::Ally { with });
                }
            } else if !trimmed.is_empty() {
                app.session.queue(Command::Chat { text: trimmed });
            }
        }
        if is_key_pressed(KeyCode::Escape) {
            app.chat = None;
        }
        return;
    }
    if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
        app.chat = Some(String::new());
        return;
    }

    // ---- camera ----
    let mut scroll = vec2(0.0, 0.0);
    let spd = 520.0 * dt;
    if is_key_down(KeyCode::Left) { scroll.x -= spd; }
    if is_key_down(KeyCode::Right) { scroll.x += spd; }
    if is_key_down(KeyCode::Up) { scroll.y -= spd; }
    if is_key_down(KeyCode::Down) { scroll.y += spd; }
    if in_view || m.x >= screen_width() - 4.0 {
        if m.x < 6.0 { scroll.x -= spd; }
        if m.x > screen_width() - 6.0 { scroll.x += spd; }
        if m.y < 6.0 { scroll.y -= spd; }
        if m.y > screen_height() - 6.0 { scroll.y += spd; }
    }
    app.cam += scroll;
    app.clamp_cam();

    // ---- hotkeys ----
    if is_key_pressed(KeyCode::F1) { app.help = !app.help; }
    if is_key_pressed(KeyCode::F5) { app.save_now(true); }
    // CHEAT (solo testing only — bypasses lockstep): Ctrl+Shift+N drops you straight
    // into the netherealm with a war-chest of Essence to try the tier-3 toys.
    if is_key_pressed(KeyCode::N) && is_key_down(KeyCode::LeftControl) && is_key_down(KeyCode::LeftShift) {
        app.session.world.cheat_descend();
    }
    if is_key_pressed(KeyCode::Escape) {
        if app.nuke_arm.is_some() { app.nuke_arm = None; }
        else if app.placing.is_some() { app.placing = None; }
        else if app.amove { app.amove = false; }
        else { app.sel.clear(); }
    }
    if is_key_pressed(KeyCode::A) && !app.selected_units().is_empty() {
        app.amove = true;
    }
    if is_key_pressed(KeyCode::S) {
        let units = app.selected_units();
        if !units.is_empty() {
            app.session.queue(Command::Stop { units });
        }
    }
    if is_key_pressed(KeyCode::X) {
        let me = app.my_pid();
        let target = app.sel.iter().copied().find(|id| {
            app.world().ents.get(*id).map(|e| e.kind.is_building() && e.owner == me && e.done).unwrap_or(false)
        });
        if let Some(b) = target {
            app.session.queue(Command::Sell { building: b });
            app.sel.retain(|id| *id != b);
        }
    }
    if is_key_pressed(KeyCode::H) {
        app.center_on_base();
    }
    if is_key_pressed(KeyCode::N) {
        // arm a charged Missile Silo: the next map click picks the target
        let me = app.my_pid();
        let ready = app.sel.iter().copied().find(|id| {
            app.world()
                .ents
                .get(*id)
                .map(|e| e.owner == me && e.kind == Kind::MissileSilo && e.done && e.work_t >= ironvein_sim::stats::NUKE_CHARGE)
                .unwrap_or(false)
        });
        if let Some(silo) = ready {
            app.nuke_arm = Some(silo);
            app.toast("SELECT NUKE TARGET (right-click to cancel)", Color::new(1.0, 0.4, 0.3, 1.0));
        } else if app.sel.iter().any(|id| app.world().ents.get(*id).map(|e| e.kind == Kind::MissileSilo).unwrap_or(false)) {
            app.toast("silo still charging", Color::new(0.9, 0.7, 0.3, 1.0));
        }
    }
    // control groups
    let digits = [KeyCode::Key1, KeyCode::Key2, KeyCode::Key3, KeyCode::Key4, KeyCode::Key5, KeyCode::Key6, KeyCode::Key7, KeyCode::Key8, KeyCode::Key9];
    for (i, k) in digits.iter().enumerate() {
        if is_key_pressed(*k) {
            if is_key_down(KeyCode::LeftControl) || is_key_down(KeyCode::RightControl) {
                app.groups[i + 1] = app.sel.clone();
            } else if !app.groups[i + 1].is_empty() {
                app.sel = app.groups[i + 1].clone();
                app.validate_selection();
            }
        }
    }

    // ---- mouse: left ----
    if is_mouse_button_pressed(MouseButton::Left) {
        // minimap?
        let ox = screen_width() - ui::SIDEBAR_W;
        if let Some(target) = app.mini.pick(app.world(), ox + 8.0, 8.0, m) {
            app.cam = gfx::tile_to_screen(target.x, target.y) - app.view() * 0.5;
            app.clamp_cam();
        } else if !in_view {
            let mut tab = app.units_tab;
            let hit = ui::click_sidebar(app.world(), app.my_pid(), &app.sel, &mut tab, m);
            app.units_tab = tab;
            if let Some(k) = hit.kind {
                if k.is_building() {
                    app.placing = if app.placing == Some(k) { None } else { Some(k) };
                } else if let Some(b) = ui::producer_for(app.world(), app.my_pid(), &app.sel, k) {
                    app.session.queue(Command::Train { building: b, kind: k });
                }
            }
        } else if let Some(silo) = app.nuke_arm {
            let at = tile_at(mouse_world(app));
            if app.world().map.in_bounds(at) {
                app.session.queue(Command::FireNuke { silo, at });
                app.mark(at, RED);
                app.nuke_arm = None;
            }
        } else if let Some(k) = app.placing {
            let at = tile_at(mouse_world(app));
            if app.world().can_place(app.my_pid(), k, at) {
                app.session.queue(Command::Build { kind: k, at });
                if !is_key_down(KeyCode::LeftShift) {
                    app.placing = None;
                }
            }
        } else if app.amove {
            let to = tile_at(mouse_world(app));
            let units = app.selected_units();
            if !units.is_empty() {
                app.session.queue(Command::AttackMove { units, to });
                app.mark(to, ORANGE);
            }
            app.amove = false;
        } else {
            app.drag = Some(m);
        }
    }
    if is_mouse_button_released(MouseButton::Left) {
        if let Some(start) = app.drag.take() {
            let end = m;
            let me = app.my_pid();
            let shift = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
            if !shift {
                app.sel.clear();
            }
            let sel_n0 = app.sel.len();
            if start.distance(end) < 5.0 {
                let wp = end + app.cam;
                if let Some(id) = ent_hit(app.world(), wp, me) {
                    let own = app.world().ents.get(id).map(|e| e.owner == me).unwrap_or(false);
                    if own {
                        if shift && app.sel.contains(&id) {
                            app.sel.retain(|s| *s != id);
                        } else if !app.sel.contains(&id) {
                            app.sel.push(id);
                        }
                    }
                }
            } else {
                let r = Rect::new(start.x.min(end.x) + app.cam.x, start.y.min(end.y) + app.cam.y, (start.x - end.x).abs(), (start.y - end.y).abs());
                let picks: Vec<Eid> = app
                    .world()
                    .ents
                    .iter()
                    .filter(|e| e.owner == me && e.kind.is_unit() && e.hp > 0)
                    .filter(|e| {
                        let p = gfx::fpx(e.pos);
                        r.contains(p)
                    })
                    .map(|e| e.id)
                    .collect();
                for id in picks {
                    if !app.sel.contains(&id) {
                        app.sel.push(id);
                    }
                }
            }
            if app.sel.len() > sel_n0 {
                audio::sfx("radar", 0.30); // selection confirm blip
            }
        }
    }

    // ---- mouse: right (context order) ----
    if is_mouse_button_pressed(MouseButton::Right) {
        if app.nuke_arm.is_some() {
            app.nuke_arm = None;
        } else if app.placing.is_some() {
            app.placing = None;
        } else if app.amove {
            app.amove = false;
        } else if in_view {
            issue_context_order(app);
        }
    }
}

fn issue_context_order(app: &mut App) {
    let me = app.my_pid();
    let wp = mouse_world(app);
    let t = tile_at(wp);
    let units = app.selected_units();
    let hit = ent_hit(app.world(), wp, me);

    if let Some(id) = hit {
        let (owner, kind) = {
            let e = app.world().ents.get(id).unwrap();
            (e.owner, e.kind)
        };
        if owner != me {
            // engineer capture has priority when exactly engineers are selected
            let engineers: Vec<Eid> = units
                .iter()
                .copied()
                .filter(|u| app.world().ents.get(*u).map(|e| e.kind == Kind::Engineer).unwrap_or(false))
                .collect();
            if kind.is_building() && !engineers.is_empty() {
                for eng in engineers {
                    app.session.queue(Command::Capture { unit: eng, target: id });
                }
                app.mark(t, SKYBLUE);
                return;
            }
            let armed: Vec<Eid> = units
                .iter()
                .copied()
                .filter(|u| app.world().ents.get(*u).map(|e| stats(e.kind).damage > 0).unwrap_or(false))
                .collect();
            if !armed.is_empty() {
                app.session.queue(Command::Attack { units: armed, target: id });
                app.mark(t, RED);
                return;
            }
        } else if matches!(kind, Kind::RepairDepot | Kind::MedBay | Kind::House) {
            // send the matching units to be serviced (vehicles→depot, infantry→bay/house)
            let vehicles = kind == Kind::RepairDepot;
            let send: Vec<Eid> = units
                .iter()
                .copied()
                .filter(|u| {
                    app.world()
                        .ents
                        .get(*u)
                        .map(|e| if vehicles { e.kind.is_unit() && !e.kind.is_infantry() } else { e.kind.is_infantry() })
                        .unwrap_or(false)
                })
                .collect();
            if !send.is_empty() {
                app.session.queue(Command::Move { units: send, to: t });
                let (col, label) = if vehicles {
                    (Color::new(1.0, 0.7, 0.3, 1.0), "to repairs")
                } else {
                    (Color::new(0.45, 1.0, 0.55, 1.0), "to the med bay")
                };
                app.mark(t, col);
                app.toast(label, col);
                return;
            }
        }
    }
    // harvesters to a resource: ore (ground), wood (tree), or stone (rock/mtn)
    if let Some(rk) = app.world().map.resource_kind(t) {
        let harv: Vec<Eid> = units
            .iter()
            .copied()
            .filter(|u| app.world().ents.get(*u).map(|e| e.kind == Kind::Harvester).unwrap_or(false))
            .collect();
        if !harv.is_empty() {
            let rest: Vec<Eid> = units.iter().copied().filter(|u| !harv.contains(u)).collect();
            app.session.queue(Command::Harvest { units: harv, tile: t });
            if !rest.is_empty() {
                app.session.queue(Command::Move { units: rest, to: t });
            }
            // colour the marker by resource: gold=ore, green=wood, grey=stone
            let col = match rk {
                1 => Color::new(0.45, 0.75, 0.3, 1.0),
                2 => Color::new(0.7, 0.72, 0.76, 1.0),
                _ => GOLD,
            };
            app.mark(t, col);
            return;
        }
    }
    if !units.is_empty() {
        app.session.queue(Command::Move { units, to: t });
        app.mark(t, GREEN);
        return;
    }
    // a lone selected building: set rally
    if let Some(&b) = app.sel.first() {
        if app.world().ents.get(b).map(|e| e.kind.is_building() && e.owner == me).unwrap_or(false) {
            app.session.queue(Command::SetRally { building: b, at: t });
            app.mark(t, ORANGE);
        }
    }
}

fn demo_camera(app: &mut App) {
    // glide between the action for screenshots
    let w = app.world();
    let focus = match app.shot_idx {
        0 => w.ents.iter().find(|e| e.owner == app.session.my_pid && e.kind == Kind::ConYard).map(|e| gfx::fpx(e.center())),
        1 => {
            // most-built enemy base
            let mut best: Option<(usize, Vec2)> = None;
            for pid in 0..8u8 {
                if pid == app.session.my_pid { continue; }
                let n = w.ents.iter().filter(|e| e.owner == pid && e.kind.is_building()).count();
                if n > 0 && best.map(|(bn, _)| n > bn).unwrap_or(true) {
                    let c = w.ents.iter().find(|e| e.owner == pid && e.kind == Kind::ConYard)
                        .or_else(|| w.ents.iter().find(|e| e.owner == pid && e.kind.is_building()))
                        .map(|e| gfx::fpx(e.center())).unwrap_or(vec2(0.0, 0.0));
                    best = Some((n, c));
                }
            }
            best.map(|(_, c)| c)
        }
        _ => app.fx.iter().find_map(|f| match &f.ev {
            ironvein_sim::world::VisEvent::Shot { from, .. } => Some(gfx::fpx(*from)),
            ironvein_sim::world::VisEvent::Die { at, .. } => Some(gfx::fpx(*at)),
            _ => None,
        }).or_else(|| w.ents.iter().find(|e| e.owner != ironvein_sim::NEUTRAL && e.kind.is_unit()).map(|e| gfx::fpx(e.center()))),
    };
    if let Some(f) = focus {
        app.cam = f - app.view() * 0.5;
        app.clamp_cam();
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn draw(app: &mut App) {
    let (sw, sh) = (screen_width(), screen_height());
    let view = app.view();
    let me = app.session.my_pid;
    let fog_pid = if app.demo { ironvein_sim::NEUTRAL } else { me }; // demo = observer vision

    // ---- render the world into a 2x supersample target ----
    app.ensure_scene(sw, sh);
    let scene = app.scene.clone().unwrap();
    let mut scam = Camera2D::from_display_rect(Rect::new(0.0, 0.0, sw, sh));
    scam.render_target = Some(scene.clone());
    set_camera(&scam);
    clear_background(Color::new(0.04, 0.04, 0.05, 1.0));

    // screen shake: jitter the world camera for this render only (restored before
    // the HUD). Trauma is squared so small tremors barely move, big ones slam.
    let true_cam = app.cam;
    if app.shake > 0.0 {
        let trauma = app.shake * app.shake;
        let ti = get_time() as f32;
        let amp = trauma * 13.0;
        app.cam += vec2((ti * 46.3).sin() * amp, (ti * 39.7 + 1.7).sin() * amp);
    }

    let w = &app.session.world;
    let tick = w.tick;

    gfx::draw_terrain(w, app.cam, view, tick);

    // pick a harvester → every harvestable tile lights up, colour-coded by yield
    let harvester_selected = app
        .sel
        .iter()
        .any(|id| w.ents.get(*id).map(|e| e.kind == Kind::Harvester && e.owner == me).unwrap_or(false));
    if harvester_selected {
        gfx::draw_resource_hints(w, fog_pid, app.cam, view, tick);
    }

    // rally flag for a selected building
    for &id in &app.sel {
        if let Some(e) = w.ents.get(id) {
            if let Some(r) = e.rally {
                let p = gfx::fpx(r.center()) - app.cam;
                draw_line(p.x, p.y, p.x, p.y - 10.0, 2.0, WHITE);
                draw_triangle(vec2(p.x, p.y - 10.0), vec2(p.x + 8.0, p.y - 7.0), vec2(p.x, p.y - 4.0), gfx::player_color(w, me));
            }
        }
    }

    // loot: essence motes glitter on the ground where you can currently see
    for m in &w.loot {
        if gfx::tile_visible(w, fog_pid, m.tile) == 2 {
            gfx::draw_loot_mote(m, app.cam, tick);
        }
    }

    // entities: buildings first, then units on top; respect fog
    let mut draw_list: Vec<&ironvein_sim::Ent> = Vec::new();
    for e in w.ents.iter() {
        let vis = gfx::tile_visible(w, fog_pid, e.tile());
        let show = e.owner == me || vis == 2 || (vis == 1 && e.kind.is_building());
        if show {
            draw_list.push(e);
        }
    }
    // isometric painter's order: back-to-front by tile depth (x+y of the
    // entity's centre). Buildings draw before units at the same depth.
    draw_list.sort_by_key(|e| {
        let c = e.center();
        ((c.x + c.y) as i64, e.kind.is_unit() as u8)
    });
    let positions: Vec<Vec2> = draw_list.iter().map(|e| app.draw_pos(e)).collect();
    for (e, pos) in draw_list.iter().zip(positions.iter()) {
        gfx::draw_entity(w, e, *pos, app.cam, app.sel.contains(&e.id));
    }

    // welding sparks / heal crosses over units being serviced at a depot/bay/house
    gfx::draw_service_fx(w, me, app.cam, tick);

    for f in &app.fx {
        gfx::draw_effect(f, app.cam);
    }

    // order markers: a quick expanding double-ring that fades (move/attack/etc.)
    for mk in &app.markers {
        let t = (mk.age / 0.6).clamp(0.0, 1.0);
        let p = mk.at - app.cam;
        let a = (1.0 - t) * 0.9;
        let c = Color::new(mk.color.r, mk.color.g, mk.color.b, a);
        draw_circle_lines(p.x, p.y, 4.0 + t * 16.0, 2.0, c);
        draw_circle_lines(p.x, p.y, 2.0 + t * 8.0, 2.0, Color::new(mk.color.r, mk.color.g, mk.color.b, a * 0.6));
    }

    gfx::draw_fog(w, fog_pid, app.cam, view);
    gfx::draw_daylight(view, tick);

    // placement ghost
    if let Some(k) = app.placing {
        let (mx, my) = mouse_position();
        if mx < view.x {
            let tf = gfx::screen_to_tilef(vec2(mx, my) + app.cam);
            let at = Tp::new(tf.x.floor() as i32, tf.y.floor() as i32);
            let ok = w.can_place(me, k, at);
            let (fw, fh) = stats(k).footprint;
            gfx::draw_footprint(at, fw, fh, app.cam, ok);
        }
    }

    // ---- composite the supersampled scene onto the screen (downscale = AA) ----
    set_default_camera();
    draw_texture_ex(
        &scene.texture,
        0.0,
        0.0,
        WHITE,
        DrawTextureParams { dest_size: Some(vec2(sw, sh)), flip_y: true, ..Default::default() },
    );
    app.cam = true_cam; // restore the true camera so HUD overlays don't shake

    // nuke detonation white-out, fading fast over the whole viewport
    if app.flash > 0.0 {
        draw_rectangle(0.0, 0.0, sw, sh, Color::new(1.0, 0.97, 0.9, (app.flash * 0.55).min(0.7)));
    }

    // THE DESCENT: as the rift drags the world under, a violet dark swells and
    // closes in — accelerating to near-black at the moment it tears away.
    {
        let wld = &app.session.world;
        if wld.descent_at != 0 && matches!(wld.realm, ironvein_sim::world::Realm::Overworld) {
            let togo = wld.descent_at.saturating_sub(wld.tick) as f32;
            let p = (1.0 - togo / 36.0).clamp(0.0, 1.0);
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.10, 0.0, 0.14, 0.78 * p * p)); // violet wash
            // a closing vignette: dark bands creeping in from the edges
            let band = (sh * 0.5) * p * p;
            draw_rectangle(0.0, 0.0, sw, band, Color::new(0.02, 0.0, 0.04, 0.9 * p));
            draw_rectangle(0.0, sh - band, sw, band, Color::new(0.02, 0.0, 0.04, 0.9 * p));
        }
    }

    // ---- UI overlays, drawn crisp at native resolution ----
    let w = &app.session.world;
    let tick = w.tick;
    // drag box
    if let Some(start) = app.drag {
        let (mx, my) = mouse_position();
        let r = Rect::new(start.x.min(mx), start.y.min(my), (start.x - mx).abs(), (start.y - my).abs());
        draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5, Color::new(0.4, 1.0, 0.4, 0.9));
    }

    if app.amove {
        let (mx, my) = mouse_position();
        draw_circle_lines(mx, my, 9.0, 1.5, RED);
        draw_text("attack-move", mx + 12.0, my + 4.0, 14.0, RED);
    }

    // hover a resource tile → a label naming what it yields + how to harvest it
    if app.placing.is_none() && app.nuke_arm.is_none() && !app.amove {
        let (mx, my) = mouse_position();
        if mx < view.x {
            let tf = gfx::screen_to_tilef(vec2(mx, my) + app.cam);
            let t = Tp::new(tf.x.floor() as i32, tf.y.floor() as i32);
            if w.map.in_bounds(t) && gfx::tile_visible(w, fog_pid, t) != 0 {
                if let Some(rk) = w.map.resource_kind(t) {
                    let amt = w.map.ore_at(t);
                    if amt > 0 {
                        let col = gfx::resource_color(rk);
                        let label = format!("{}  ·  {} left", gfx::resource_name(rk), amt);
                        let sub = if harvester_selected {
                            "right-click here to harvest"
                        } else {
                            "select a Harvester, then right-click here"
                        };
                        let lw = measure_text(&label, None, 16, 1.0).width.max(measure_text(sub, None, 12, 1.0).width) + 18.0;
                        let lx = (mx + 16.0).min(view.x - lw - 6.0);
                        let ly = my + 12.0;
                        draw_rectangle(lx, ly, lw, 40.0, Color::new(0.06, 0.06, 0.08, 0.92));
                        draw_rectangle(lx, ly, 4.0, 40.0, col);
                        draw_rectangle_lines(lx, ly, lw, 40.0, 1.0, Color::new(col.r, col.g, col.b, 0.7));
                        draw_text(&label, lx + 12.0, ly + 17.0, 16.0, col);
                        draw_text(sub, lx + 12.0, ly + 33.0, 12.0, Color::new(0.78, 0.78, 0.82, 1.0));
                    }
                }
            }
        }
    }

    // nuke targeting: a crosshair + the blast-radius footprint under the cursor
    if app.nuke_arm.is_some() {
        let (mx, my) = mouse_position();
        let rpx = ironvein_sim::stats::NUKE_RADIUS as f32 * gfx::TW * 0.5;
        let warn = Color::new(1.0, 0.35, 0.25, 0.9);
        draw_circle_lines(mx, my, rpx, 2.0, warn);
        draw_circle_lines(mx, my, rpx * 0.5, 1.5, Color::new(1.0, 0.5, 0.3, 0.6));
        draw_line(mx - 14.0, my, mx + 14.0, my, 1.5, warn);
        draw_line(mx, my - 14.0, mx, my + 14.0, 1.5, warn);
        draw_text("NUCLEAR STRIKE", mx + 16.0, my - 8.0, 16.0, warn);
    }

    app.mini.refresh(w, fog_pid);
    ui::draw_sidebar(w, &app.session, &app.mini, me, &app.sel, app.units_tab, app.placing, app.cam, view, app.credits_shown);
    ui::draw_chat(w, &app.chat);
    ui::draw_banner(&app.session, app.banner_age);

    // top strip: who am I, where am I
    let mode = match w.mode { Mode::Persistent => "persistent", Mode::Skirmish => "skirmish", Mode::Survival => "survival" };
    let kind = match app.session.kind { SessionKind::Solo => "offline", SessionKind::Host => "hosting", SessionKind::Client => "online" };
    draw_rectangle(0.0, 0.0, view.x, 22.0, Color::new(0.0, 0.0, 0.0, 0.45));
    let name = w.players.get(me as usize).map(|p| p.name.as_str()).unwrap_or("…");
    draw_text(
        &format!("{name}  |  {mode} · {kind} · {} peers  |  tick {}  |  fps {}", app.session.peer_count(), tick, get_fps()),
        8.0,
        16.0,
        16.0,
        Color::new(0.85, 0.85, 0.9, 1.0),
    );
    if w.players.get(me as usize).map(|p| p.defeated).unwrap_or(false) {
        draw_text("ELIMINATED — spectating", view.x * 0.5 - 120.0, 80.0, 26.0, RED);
    }

    if app.help {
        ui::draw_help();
    }

    // survival game-over: your colony fell to the night
    if w.mode == Mode::Survival && w.players.get(me as usize).map(|p| p.defeated).unwrap_or(false) {
        ui::draw_survival_over(w.tick, view);
    }

    // live alert toasts (top-centre of the play area)
    ui::draw_toasts(&app.toasts, view);

    // the persistent world map overlay
    if app.show_map {
        let m = vec2(mouse_position().0, mouse_position().1);
        ui::draw_world_map(&app.regions, &app.my_key, view, m);
    }

    // "while you were away" report, centered, on top of everything
    if let Some((log, cap_tick)) = &app.away_report {
        ui::draw_away_report(w, log, *cap_tick, view);
    }
}

// ---------------------------------------------------------------------------
// Screenshots (no deps: minimal stored-deflate PNG) — native demo mode only
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
fn screenshot(path: &str) {
    let img = get_screen_data();
    let (w, h) = (img.width as usize, img.height as usize);
    // GL framebuffers are bottom-up; flip rows
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = (h - 1 - y) * w * 4;
        rgba[y * w * 4..(y + 1) * w * 4].copy_from_slice(&img.bytes[src..src + w * 4]);
    }
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir).ok();
    }
    match write_png(path, w as u32, h as u32, &rgba) {
        Ok(_) => println!("screenshot -> {path}"),
        Err(e) => eprintln!("screenshot failed: {e}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c: u32 = 0xFFFF_FFFF;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(body);
        let mut crcbuf = Vec::with_capacity(4 + body.len());
        crcbuf.extend_from_slice(tag);
        crcbuf.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crcbuf).to_be_bytes());
    }
    // raw scanlines with filter byte 0
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    // zlib stream with stored (uncompressed) deflate blocks
    let mut z = vec![0x78u8, 0x01];
    let mut i = 0;
    while i < raw.len() {
        let n = (raw.len() - i).min(65535);
        let last = (i + n == raw.len()) as u8;
        z.push(last);
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[i..i + n]);
        i += n;
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}
