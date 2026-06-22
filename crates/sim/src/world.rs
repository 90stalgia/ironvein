//! world.rs — the whole game, as one deterministic state machine.
//!
//! `World::step(commands)` is the ONLY mutator. Systems run in a fixed order;
//! every iteration is in stable slot order; every random draw goes through
//! `self.rng`. Hold that line and multiplayer is just "ship the commands".

use crate::command::Command;
use crate::entity::{Arena, Eid, Ent, HarvestPhase};
use crate::map::{Map, Terrain, MAX_ORE};
use crate::path;
use crate::rng::Pcg32;
use crate::ser::{DResult, DecodeErr, R, W, SAVE_MAGIC, SAVE_VERSION};
use crate::stats::*;
use crate::{fnv64, Fp, Pid, Tp, FX, MAX_PLAYERS, NEUTRAL};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The world just runs. No winners, no losers, autosaved forever. Move in.
    Persistent = 0,
    /// Classic: lose everything and you're out.
    Skirmish = 1,
    /// Solo vs. the night: no rivals, a denser/faster horde, and you're out if
    /// the dark overruns you. The escalating-nights content is the whole game.
    Survival = 2,
}

impl Mode {
    pub fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::Skirmish,
            2 => Mode::Survival,
            _ => Mode::Persistent,
        }
    }
    /// Modes where losing everything eliminates you (vs. the persistent world).
    pub fn has_defeat(self) -> bool {
        matches!(self, Mode::Skirmish | Mode::Survival)
    }
}

/// Which world the match is in. Slaying the Warlock opens a rift; marching a
/// force through it is a one-way **descent** into the netherealm (the map is
/// regenerated, the surviving army carried down). Part of world state (save v12).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Realm {
    Overworld = 0,
    Nether = 1,
}
impl Realm {
    pub fn from_u8(v: u8) -> Realm {
        match v {
            1 => Realm::Nether,
            _ => Realm::Overworld,
        }
    }
}

/// What happened to a settlement while its owner was away — the "since you
/// left" report. Accumulates whenever the owner isn't actively issuing
/// commands; reset to a fresh window on any deliberate action (so a present
/// player never gets a report, an absent one returns to a full accounting).
#[derive(Clone)]
pub struct AwayLog {
    /// tick the current away window began (the owner's last action)
    pub from_tick: u32,
    pub credits_gained: u32, // ore + farm income earned offline
    pub credits_lost: u32,   // looted by raiders
    pub buildings_lost: u16,
    pub units_lost: u16,
    pub attacks: u16,    // hostile kills landed against you
    pub last_foe: u8,    // pid of the last attacker (NEUTRAL = none)
}

impl AwayLog {
    pub fn new(tick: u32) -> AwayLog {
        AwayLog { from_tick: tick, credits_gained: 0, credits_lost: 0, buildings_lost: 0, units_lost: 0, attacks: 0, last_foe: NEUTRAL }
    }
    /// Did anything worth a report happen?
    pub fn eventful(&self) -> bool {
        self.credits_gained > 0 || self.credits_lost > 0 || self.buildings_lost > 0 || self.units_lost > 0
    }
}

#[derive(Clone)]
pub struct Player {
    pub joined: bool,
    pub name: String,
    pub color: u8,
    /// Identity public key, opaque to the sim (all-zero for bots/legacy).
    /// Persisted so only the keyholder can reclaim this settlement.
    pub key: [u8; 32],
    pub credits: u32,
    /// lumber (chop trees) and stone (mine rock/mountain) — the other two of the
    /// three resources every build draws on
    pub wood: u32,
    pub stone: u32,
    /// the rare currency: farmed from slain monsters and the heart of mined-out
    /// mountains, spent only on tier-3 power (Obelisk, Champion)
    pub essence: u32,
    /// food stockpile (grown on Farms, hunted from deer, foraged from berries);
    /// soldiers eat it, Food Silos & Houses store it. v11.
    pub food: u32,
    pub defeated: bool,
    // recomputed every tick (still serialized: keeps the hash honest & load trivial)
    pub power_made: i32,
    pub power_used: i32,
    pub unit_cap: u32,
    pub unit_count: u32,
    /// derived: food storage cap, and whether the town ran out of food this tick
    pub food_cap: u32,
    pub starving: bool,
    /// 0 = unseen, 1 = explored, 2 = visible
    pub fog: Vec<u8>,
    /// the "while you were away" accounting
    pub away: AwayLog,
    /// bit i set = "I offer alliance to player i". An alliance is in force only
    /// when both players have each other's bit set (mutual). Allies don't fire
    /// on each other and share vision.
    pub ally_mask: u8,
}

impl Player {
    fn empty(map_tiles: usize) -> Player {
        Player {
            joined: false,
            name: String::new(),
            color: 0,
            key: [0; 32],
            credits: 0,
            wood: 0,
            stone: 0,
            essence: 0,
            food: 0,
            defeated: false,
            power_made: 0,
            power_used: 0,
            unit_cap: 0,
            unit_count: 0,
            food_cap: 0,
            starving: false,
            fog: vec![0; map_tiles],
            away: AwayLog::new(0),
            ally_mask: 0,
        }
    }
    pub fn low_power(&self) -> bool {
        self.power_used > self.power_made
    }
}

/// Transient render cues for the client. Never serialized, never hashed —
/// the world's truth does not depend on who is watching.
#[derive(Clone, Debug)]
pub enum VisEvent {
    Shot { from: Fp, to: Fp, rocket: bool },
    Die { at: Fp, big: bool },
    Built { at: Fp },
    Captured { at: Fp },
    Unload { at: Fp, amount: u32 },
    Pickup { at: Fp, amount: u32, kind: u8 },
    Nuke { at: Fp },
}

/// A mote of Essence a slain monster drops onto its corpse-tile. Any owned unit
/// that comes within `LOOT_PICKUP_R2` vacuums it up; it fades after `LOOT_TTL`.
/// Part of world state (and the hash) — so the chase is bit-identical everywhere.
#[derive(Clone, Copy, Debug)]
pub struct Loot {
    pub tile: Tp,
    pub amount: u32,
    /// 0 = essence, 1 = berries (food, no cooking), 2 = raw meat (needs a House
    /// to cook into food). Routes the payout in `sys_loot`.
    pub kind: u8,
    pub born: u32, // tick it dropped (for the fade-out)
}

/// Loot kinds.
pub const LOOT_ESSENCE: u8 = 0;
pub const LOOT_BERRY: u8 = 1;
pub const LOOT_MEAT: u8 = 2;

/// How long a dropped mote lingers before it winks out (~90s at 10Hz).
pub const LOOT_TTL: u32 = 900;
/// Auto-vacuum radius, squared, in fixed-point sub-units (1.5 tiles).
const LOOT_PICKUP_R2: i64 = (FX as i64 * 3 / 2) * (FX as i64 * 3 / 2);

/// Ticks per full day/night cycle (matches the renderer's daylight phase).
pub const DAY_LEN: u32 = 6000;

/// Is it currently night — when the supernatural stirs? Integer-only (the sim
/// is float-free), sitting inside the visually-dark half of the cycle so the
/// monsters appear when the screen darkens and burn off as dawn breaks.
pub fn is_night(tick: u32) -> bool {
    let p = tick % DAY_LEN;
    (600..2700).contains(&p)
}

/// Which night this is (1-indexed). Difficulty scales with it; the dark grows
/// bolder the longer your settlement endures.
pub fn night_count(tick: u32) -> u32 {
    tick / DAY_LEN + 1
}

/// Every 7th night the moon runs red: a horde pours from every edge, and on the
/// deep blood moons the bosses walk. The whole escalation hangs off this.
pub fn is_blood_moon(tick: u32) -> bool {
    night_count(tick) % 7 == 0
}

pub struct World {
    pub tick: u32,
    pub mode: Mode,
    pub map: Map,
    pub ents: Arena,
    pub players: Vec<Player>,
    pub rng: Pcg32,
    /// (tick, pid, text) — last 50 lines. Part of world state (and the hash);
    /// chat history persists with the world, like graffiti.
    pub chat: Vec<(u32, Pid, String)>,
    /// The reckoning's reward: after the nations slay the Warlock, the dark
    /// recedes — no new horde spawns until this tick. Part of world state (v8).
    pub peace_until: u32,
    /// Essence motes dropped by slain monsters, waiting to be collected (v9).
    pub loot: Vec<Loot>,
    /// 0 = easy, 1 = normal, 2 = hard. Scales the night horde (v10). Part of
    /// world state so every peer breeds the identical difficulty-tuned swarm.
    pub difficulty: u8,
    /// Overworld until the alliance descends through the Warlock's rift (v12).
    pub realm: Realm,
    /// Tick the descent fires once a unit reaches the portal (0 = not pending) —
    /// a short, dread-building countdown. Ephemeral: not serialised or hashed
    /// (it's re-armed from the portal each tick, like `events`).
    pub descent_at: u32,
    pub events: Vec<VisEvent>,
}

struct DmgEvent {
    target: Eid,
    amount: i32,
    from: Fp,
    rocket: bool,
    by_owner: Pid,
}

impl World {
    pub fn new(map: Map, seed: u64, mode: Mode) -> World {
        World {
            tick: 0,
            mode,
            map,
            ents: Arena::new(),
            players: Vec::new(),
            rng: Pcg32::new(seed),
            chat: Vec::new(),
            peace_until: 0,
            loot: Vec::new(),
            difficulty: 1,
            realm: Realm::Overworld,
            descent_at: 0,
            events: Vec::new(),
        }
    }

    /// Total built-up value a player holds in this region (sum of standing
    /// building cost). Persists offline, since bases persist.
    pub fn territory(&self, pid: Pid) -> u64 {
        let mut v = 0u64;
        for e in self.ents.iter() {
            if e.owner == pid && e.kind.is_building() && e.done {
                v += stats(e.kind).cost as u64;
            }
        }
        v
    }

    /// The settler who controls this region: the one with the most territory.
    /// Ties go to the lowest pid. `None` if nobody has built anything. This is
    /// the region's standing "owner" on the world map — it doesn't change when
    /// you log off, because your base doesn't.
    pub fn dominant(&self) -> Option<Pid> {
        let mut best: Option<(u64, Pid)> = None;
        for (i, p) in self.players.iter().enumerate() {
            if !p.joined {
                continue;
            }
            let v = self.territory(i as Pid);
            if v > 0 && best.map(|(bv, _)| v > bv).unwrap_or(true) {
                best = Some((v, i as Pid));
            }
        }
        best.map(|(_, pid)| pid)
    }

    /// Are two players mutual allies (each offering the other)? Never true for
    /// the same player or NEUTRAL.
    pub fn allied(&self, a: Pid, b: Pid) -> bool {
        if a == b || a == NEUTRAL || b == NEUTRAL {
            return false;
        }
        let ma = self.players.get(a as usize).map(|p| p.ally_mask).unwrap_or(0);
        let mb = self.players.get(b as usize).map(|p| p.ally_mask).unwrap_or(0);
        (ma >> b) & 1 == 1 && (mb >> a) & 1 == 1
    }

    /// Gate tiles a unit of `owner` may walk through — its own and allies'.
    /// Gates stamp `block` (so they bar enemies, who must batter them down),
    /// but the pathfinder is handed these tiles as passable for friendlies.
    /// Returns empty (no allocation churn) when there are no gates at all.
    fn open_gates_for(&self, owner: Pid) -> Vec<Tp> {
        let mut out = Vec::new();
        for e in self.ents.iter() {
            if e.kind == Kind::Gate && e.hp > 0 && (e.owner == owner || self.allied(owner, e.owner)) {
                out.push(e.tile());
            }
        }
        out
    }

    // =====================================================================
    // THE tick function
    // =====================================================================
    pub fn step(&mut self, commands: &[(Pid, Command)]) {
        self.events.clear();
        for (pid, cmd) in commands {
            self.apply_command(*pid, cmd);
        }
        self.sys_construction();
        self.sys_production();
        self.sys_harvest();
        self.sys_movement();
        self.sys_combat_and_capture();
        self.sys_monsters();
        self.sys_wildlife();
        self.sys_loot();
        self.sys_ore_regen();
        self.sys_support();
        self.sys_portal();
        self.sys_recompute();
        if self.tick % 2 == 0 {
            self.sys_fog();
        }
        self.tick = self.tick.wrapping_add(1);
    }

    // =====================================================================
    // Commands
    // =====================================================================
    fn apply_command(&mut self, pid: Pid, cmd: &Command) {
        // any deliberate in-game action means the owner is present: start a
        // fresh away window so they never get a report for time they were
        // actually here. Join is excluded — it's idempotent for an existing
        // base, and the report is read from the snapshot before the rejoiner's
        // first real action resets it.
        if !matches!(cmd, Command::Join { .. }) {
            if let Some(p) = self.players.get_mut(pid as usize) {
                p.away = AwayLog::new(self.tick);
            }
        }
        match cmd {
            Command::Join { name, color, key } => self.cmd_join(pid, name, *color, key),
            Command::Move { units, to } => self.cmd_move(pid, units, *to, false),
            Command::AttackMove { units, to } => self.cmd_move(pid, units, *to, true),
            Command::Attack { units, target } => self.cmd_attack(pid, units, *target),
            Command::Stop { units } => self.cmd_stop(pid, units),
            Command::Harvest { units, tile } => self.cmd_harvest(pid, units, *tile),
            Command::Capture { unit, target } => self.cmd_capture(pid, *unit, *target),
            Command::Build { kind, at } => self.cmd_build(pid, *kind, *at),
            Command::Train { building, kind } => self.cmd_train(pid, *building, *kind),
            Command::CancelTrain { building } => self.cmd_cancel(pid, *building),
            Command::SetRally { building, at } => self.cmd_rally(pid, *building, *at),
            Command::Sell { building } => self.cmd_sell(pid, *building),
            Command::Chat { text } => self.cmd_chat(pid, text),
            Command::GiveCredits { to, amount } => self.cmd_give(pid, *to, *amount),
            Command::Ally { with } => self.cmd_ally(pid, *with),
            Command::FireNuke { silo, at } => self.cmd_nuke(pid, *silo, *at),
        }
    }

    fn player(&self, pid: Pid) -> Option<&Player> {
        self.players.get(pid as usize).filter(|p| p.joined)
    }

    fn cmd_join(&mut self, pid: Pid, name: &str, color: u8, key: &[u8; 32]) {
        if pid as usize >= MAX_PLAYERS {
            return;
        }
        let tiles = (self.map.w * self.map.h) as usize;
        while self.players.len() <= pid as usize {
            self.players.push(Player::empty(tiles));
        }
        if self.players[pid as usize].joined {
            // rejoining an existing settlement: your base waited for you.
            // A legacy (zero-key) settlement adopts its keyholder's key on
            // first keyed rejoin; a keyed one never changes hands.
            let p = &mut self.players[pid as usize];
            if p.key == [0; 32] {
                p.key = *key;
            }
            return;
        }
        let site_i = match self.map.free_spawn() {
            Some(i) => i,
            None => return, // world is full; the net layer should have refused earlier
        };
        let site = self.map.spawns[site_i];
        self.map.spawn_used[site_i] = pid;

        // Pick a colour that doesn't clash with anyone already in the world: if the
        // requested one is taken (e.g. a second player defaulting to red), bump to
        // the lowest free colour. Deterministic, so every peer agrees. Falls back
        // to the request only if all 8 are in use.
        let want = color % 8;
        let taken: [bool; 8] = {
            let mut t = [false; 8];
            for (i, p) in self.players.iter().enumerate() {
                if i != pid as usize && p.joined {
                    t[(p.color % 8) as usize] = true;
                }
            }
            t
        };
        let chosen = if taken[want as usize] {
            (0..8).find(|c| !taken[*c as usize]).unwrap_or(want)
        } else {
            want
        };

        {
            let p = &mut self.players[pid as usize];
            p.joined = true;
            p.name = name.chars().take(16).collect();
            p.color = chosen;
            p.key = *key;
            p.credits = STARTING_CREDITS;
            p.wood = crate::stats::STARTING_WOOD;
            p.stone = crate::stats::STARTING_STONE;
            p.food = crate::stats::STARTING_FOOD;
        }

        // You don't break ground on B Proxima empty-handed: your towering colony
        // ship sets down (it doubles as your construction yard) and the whole
        // contingent it carried — harvesters, infantry, armour — pours down the ramp.
        let foot = stats(Kind::Starship).footprint;
        // the ship flattens and scorches its landing pad so the 5x5 always fits
        for dy in 0..foot.1 {
            for dx in 0..foot.0 {
                let t = Tp::new(site.x + dx, site.y + dy);
                if self.map.in_bounds(t) {
                    self.map.set_ore(t, 0);
                    self.map.set_terrain(t, Terrain::Dirt);
                }
            }
        }
        let ship = self.ents.spawn(pid, Kind::Starship, Fp { x: site.x * FX, y: site.y * FX });
        self.map.stamp_block(site, foot, ship.idx + 1);

        // the landing party: a full colonial expeditionary force, fanned out
        // below the ship in widening ranks
        let contingent: [(Kind, usize); 5] = [
            (Kind::Harvester, 10),
            (Kind::Rifleman, 20),
            (Kind::Rocketeer, 10),
            (Kind::Tank, 10),
            (Kind::Buggy, 5),
        ];
        let muster = Tp::new(site.x + foot.0 / 2, site.y + foot.1 + 1);
        let mut rank = 0usize;
        let mut harvesters: Vec<Eid> = Vec::new();
        for (kind, count) in contingent {
            for _ in 0..count {
                let col = (rank % 9) as i32 - 4;
                let row = (rank / 9) as i32;
                let spot = Tp::new(muster.x + col, muster.y + row);
                if let Some(t) = self.find_free_tile_near(spot, 10) {
                    let id = self.ents.spawn(pid, kind, t.center());
                    if kind == Kind::Harvester {
                        harvesters.push(id);
                    }
                }
                rank += 1;
            }
        }
        // fan a third of the harvesters onto wood and a third onto stone (the
        // rest gather ore), so you touch down already working all three resources
        // instead of every truck piling onto the gold.
        for (i, &hid) in harvesters.iter().enumerate() {
            let want: u8 = match i % 3 {
                1 => 1, // wood
                2 => 2, // stone
                _ => 0, // ore (the default — leave it be)
            };
            if want == 0 {
                continue;
            }
            let Some(hp) = self.ents.get(hid).map(|e| e.tile()) else { continue };
            let Some(rt) = self.find_resource_near(hp, want, 60) else { continue };
            let adj = !self.map.terrain_at(rt).passable();
            let path = path::find(&self.map, hp, rt, adj, &[]);
            if let Some(e) = self.ents.get_mut(hid) {
                e.ore_tile = Some(rt);
                e.hphase = HarvestPhase::ToOre;
                e.goal = Some(rt);
                e.path = path;
            }
        }
        let nm = self.players[pid as usize].name.clone();
        self.push_chat(NEUTRAL, format!("{} made planetfall on B Proxima", nm));
    }

    fn cmd_move(&mut self, pid: Pid, units: &[Eid], to: Tp, aggressive: bool) {
        if !self.map.in_bounds(to) {
            return;
        }
        let spread = self.spread_tiles(to, units.len().min(80));
        let gates = self.open_gates_for(pid);
        let mut si = 0usize;
        for &uid in units.iter().take(80) {
            let dest = *spread.get(si).unwrap_or(&to);
            let p = if let Some(e) = self.ents.get(uid) {
                if e.owner != pid || !e.kind.is_unit() {
                    continue;
                }
                path::find(&self.map, e.tile(), dest, false, &gates)
            } else {
                continue;
            };
            if let Some(e) = self.ents.get_mut(uid) {
                e.target = None;
                e.follow = None;
                e.aggressive = aggressive;
                e.stuck = 0;
                e.goal = Some(dest);
                e.path = p;
                if e.kind == Kind::Harvester {
                    e.hphase = HarvestPhase::Idle;
                    e.work_t = 200; // the player said go HERE; don't immediately re-task
                }
                si += 1;
            }
        }
    }

    fn cmd_attack(&mut self, pid: Pid, units: &[Eid], target: Eid) {
        let (t_tile, t_is_b) = match self.ents.get(target) {
            Some(t) => (t.tile(), t.kind.is_building()),
            None => return,
        };
        let gates = self.open_gates_for(pid);
        for &uid in units.iter().take(80) {
            let p = if let Some(e) = self.ents.get(uid) {
                if e.owner != pid || !e.kind.is_unit() || uid == target || stats(e.kind).damage <= 0 {
                    continue;
                }
                path::find(&self.map, e.tile(), t_tile, t_is_b, &gates)
            } else {
                continue;
            };
            if let Some(e) = self.ents.get_mut(uid) {
                e.target = Some(target);
                e.follow = Some(target);
                e.aggressive = false;
                e.goal = Some(t_tile);
                e.stuck = 0;
                e.path = p;
            }
        }
    }

    fn cmd_stop(&mut self, pid: Pid, units: &[Eid]) {
        for &uid in units.iter().take(80) {
            if let Some(e) = self.ents.get_mut(uid) {
                if e.owner != pid {
                    continue;
                }
                e.path.clear();
                e.goal = None;
                e.follow = None;
                e.target = None;
                e.aggressive = false;
                if e.kind == Kind::Harvester {
                    e.hphase = HarvestPhase::Idle;
                    e.work_t = 50;
                }
            }
        }
    }

    fn cmd_harvest(&mut self, pid: Pid, units: &[Eid], tile: Tp) {
        // ore on open ground, wood in a tree, or stone in rock/mountain
        let Some(new_rk) = self.map.resource_kind(tile) else {
            return;
        };
        // impassable resources (trees, rock) are worked from an adjacent tile
        let adj = !self.map.terrain_at(tile).passable();
        let gates = self.open_gates_for(pid);
        for &uid in units.iter().take(80) {
            // validate ownership/kind/generation, and note a mismatched load
            let Some(carrying_other) = self
                .ents
                .get(uid)
                .filter(|e| e.owner == pid && e.kind == Kind::Harvester)
                .map(|e| e.cargo > 0 && e.cargo_kind != new_rk)
            else {
                continue;
            };
            // own the entity so we can route it to a refinery if needed
            let Some(mut e) = self.ents.take(uid.idx as usize) else {
                continue;
            };
            e.ore_tile = Some(tile); // the assignment — sticky across a forced dump
            e.target = None;
            e.follow = None;
            e.stuck = 0;
            e.stall = 0;
            e.work_t = 0;
            if carrying_other {
                // carrying a different resource: dump it first (can't mix loads),
                // then seek_more_or_unload returns to this assigned tile.
                self.route_to_refinery(&mut e);
            } else {
                e.hphase = HarvestPhase::ToOre;
                e.goal = Some(tile);
                e.path = path::find(&self.map, e.tile(), tile, adj, &gates);
            }
            self.ents.put(uid.idx as usize, e);
        }
    }

    fn cmd_capture(&mut self, pid: Pid, unit: Eid, target: Eid) {
        let t_tile = match self.ents.get(target) {
            Some(t) if t.kind.is_building() && t.owner != pid => t.tile(),
            _ => return,
        };
        let gates = self.open_gates_for(pid);
        let p = if let Some(e) = self.ents.get(unit) {
            if e.owner != pid || e.kind != Kind::Engineer {
                return;
            }
            path::find(&self.map, e.tile(), t_tile, true, &gates)
        } else {
            return;
        };
        if let Some(e) = self.ents.get_mut(unit) {
            e.target = Some(target);
            e.follow = Some(target);
            e.goal = Some(t_tile);
            e.stuck = 0;
            e.path = p;
        }
    }

    /// Shared by sim validation and the client's placement preview — one source of truth.
    pub fn can_place(&self, pid: Pid, kind: Kind, at: Tp) -> bool {
        if !kind.is_building() {
            return false;
        }
        let (fw, fh) = stats(kind).footprint;
        for dy in 0..fh {
            for dx in 0..fw {
                let t = Tp::new(at.x + dx, at.y + dy);
                if !self.map.in_bounds(t)
                    || !self.map.terrain_at(t).buildable()
                    || self.map.blocked_by(t) != 0
                    || self.map.ore_at(t) > 0
                {
                    return false;
                }
            }
        }
        // no unit standing in the footprint
        for e in self.ents.iter() {
            if e.kind.is_unit() {
                let t = e.tile();
                if t.x >= at.x && t.x < at.x + fw && t.y >= at.y && t.y < at.y + fh {
                    return false;
                }
            }
        }
        // roads may also extend from any existing road tile, so networks can
        // grow outward without hugging a building
        if kind == Kind::Road {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                if self.map.terrain_at(Tp::new(at.x + dx, at.y + dy)) == Terrain::Road {
                    return true;
                }
            }
        }
        // must be near one of your finished buildings
        for e in self.ents.iter() {
            if e.owner == pid && e.kind.is_building() && e.done {
                let (bw, bh) = e.foot();
                let bt = e.tile();
                let gx = (bt.x - (at.x + fw - 1)).max(at.x - (bt.x + bw - 1)).max(0);
                let gy = (bt.y - (at.y + fh - 1)).max(at.y - (bt.y + bh - 1)).max(0);
                if gx <= BUILD_RADIUS && gy <= BUILD_RADIUS {
                    return true;
                }
            }
        }
        false
    }

    /// Does `pid` own a finished building of `kind`? (Tech-tree prerequisite.)
    fn has_done_building(&self, pid: Pid, kind: Kind) -> bool {
        self.ents.iter().any(|e| e.owner == pid && e.kind == kind && e.done)
    }

    fn cmd_build(&mut self, pid: Pid, kind: Kind, at: Tp) {
        let cost = stats(kind).cost;
        let wcost = crate::stats::wood_cost(kind);
        let scost = crate::stats::stone_cost(kind);
        let ecost = crate::stats::essence_cost(kind);
        let ok = match self.player(pid) {
            Some(p) => p.credits >= cost && p.wood >= wcost && p.stone >= scost && p.essence >= ecost && !p.defeated,
            None => false,
        };
        if !ok || !self.can_place(pid, kind, at) {
            return;
        }
        // tech prerequisite (e.g. a Missile Turret needs a Tech Center)
        if let Some(req) = crate::stats::requires(kind) {
            if !self.has_done_building(pid, req) {
                return;
            }
        }
        let p = &mut self.players[pid as usize];
        p.credits -= cost;
        p.wood -= wcost;
        p.stone -= scost;
        p.essence -= ecost;
        // Roads aren't entities — they're a terrain change, so units can drive
        // over them (and move 25% faster). Stamp and we're done.
        if kind == Kind::Road {
            self.map.set_terrain(at, Terrain::Road);
            self.events.push(VisEvent::Built { at: at.center() });
            return;
        }
        let id = self.ents.spawn(pid, kind, Fp { x: at.x * FX, y: at.y * FX });
        let foot = stats(kind).footprint;
        if let Some(e) = self.ents.get_mut(id) {
            e.done = false;
            e.con_progress = 0;
            e.hp = (stats(kind).max_hp / 10).max(1);
        }
        self.map.stamp_block(at, foot, id.idx + 1);
    }

    /// Launch a charged nuke from `silo` onto `at`: a circular blast that flattens
    /// EVERYTHING in radius (friend, foe, monster — a nuke doesn't take sides),
    /// damage falling off linearly to the edge. Resets the silo's charge.
    fn cmd_nuke(&mut self, pid: Pid, silo: Eid, at: Tp) {
        let ready = self
            .ents
            .get(silo)
            .map(|e| e.owner == pid && e.done && e.kind == Kind::MissileSilo && e.work_t >= crate::stats::NUKE_CHARGE)
            .unwrap_or(false);
        if !ready || !self.map.in_bounds(at) {
            return;
        }
        if let Some(e) = self.ents.get_mut(silo) {
            e.work_t = 0;
        }
        let center = at.center();
        let reach = crate::stats::NUKE_RADIUS * FX;
        let mut casualties: Vec<Eid> = Vec::new();
        for i in 0..self.ents.len_slots() {
            let Some(e) = self.ents.slots[i].as_mut() else { continue };
            if e.hp <= 0 {
                continue;
            }
            let dist = crate::isqrt(e.center().dist2(center)) as i32;
            if dist > reach {
                continue;
            }
            // linear falloff: full damage at ground zero, ~0 at the rim
            let dmg = crate::stats::NUKE_DMG * (reach - dist) / reach;
            e.hp -= dmg.max(1);
            if e.hp <= 0 {
                casualties.push(e.id);
            }
        }
        self.events.push(VisEvent::Nuke { at: center });
        let who = self.players.get(pid as usize).map(|p| p.name.clone()).unwrap_or_default();
        self.push_chat(NEUTRAL, format!("NUCLEAR LAUNCH DETECTED — {who} struck the field!"));
        for id in casualties {
            self.kill(id);
        }
    }

    fn cmd_train(&mut self, pid: Pid, building: Eid, kind: Kind) {
        let cost = stats(kind).cost;
        let wcost = crate::stats::wood_cost(kind);
        let scost = crate::stats::stone_cost(kind);
        let ecost = crate::stats::essence_cost(kind);
        // a starving town can't feed new mouths — no training until food recovers
        let starving = self.player(pid).map(|p| p.starving).unwrap_or(false);
        if starving && crate::stats::food_upkeep(kind) > 0 {
            return;
        }
        let afford = self
            .player(pid)
            .map(|p| p.credits >= cost && p.wood >= wcost && p.stone >= scost && p.essence >= ecost)
            .unwrap_or(false);
        if !afford {
            return;
        }
        // tech prerequisite (e.g. a Heavy Tank needs a Tech Center)
        if let Some(req) = crate::stats::requires(kind) {
            if !self.has_done_building(pid, req) {
                return;
            }
        }
        let mut paid = false;
        if let Some(b) = self.ents.get_mut(building) {
            if b.owner == pid && b.done && stats(kind).built_by == Some(b.kind) && b.queue.len() < 8 {
                b.queue.push(kind);
                paid = true;
            }
        }
        if paid {
            let p = &mut self.players[pid as usize];
            p.credits -= cost;
            p.wood -= wcost;
            p.stone -= scost;
            p.essence -= ecost;
        }
    }

    fn cmd_cancel(&mut self, pid: Pid, building: Eid) {
        let mut refund = 0u32;
        if let Some(b) = self.ents.get_mut(building) {
            if b.owner == pid {
                if let Some(k) = b.queue.pop() {
                    refund = stats(k).cost;
                    if b.queue.is_empty() {
                        b.prod_progress = 0;
                    }
                }
            }
        }
        if refund > 0 {
            if let Some(p) = self.players.get_mut(pid as usize) {
                p.credits += refund;
            }
        }
    }

    fn cmd_rally(&mut self, pid: Pid, building: Eid, at: Tp) {
        if !self.map.in_bounds(at) {
            return;
        }
        if let Some(b) = self.ents.get_mut(building) {
            if b.owner == pid && b.kind.is_building() {
                b.rally = Some(at);
            }
        }
    }

    fn cmd_sell(&mut self, pid: Pid, building: Eid) {
        let mut info = None;
        if let Some(b) = self.ents.get(building) {
            if b.owner == pid && b.kind.is_building() && b.done {
                info = Some((stats(b.kind).cost / 2, b.tile(), b.foot(), b.center()));
            }
        }
        if let Some((amount, tile, foot, c)) = info {
            self.map.clear_block(tile, foot);
            self.ents.despawn(building);
            self.players[pid as usize].credits += amount;
            self.events.push(VisEvent::Die { at: c, big: false });
        }
    }

    fn cmd_chat(&mut self, pid: Pid, text: &str) {
        let t: String = text.chars().take(120).collect();
        if t.trim().is_empty() {
            return;
        }
        self.push_chat(pid, t);
    }

    fn push_chat(&mut self, pid: Pid, text: String) {
        self.chat.push((self.tick, pid, text));
        let n = self.chat.len();
        if n > 50 {
            self.chat.drain(0..n - 50);
        }
    }

    fn cmd_give(&mut self, pid: Pid, to: Pid, amount: u32) {
        if pid == to || amount == 0 {
            return;
        }
        let to_ok = self.player(to).is_some();
        let from_ok = self.player(pid).map(|p| p.credits >= amount).unwrap_or(false);
        if to_ok && from_ok {
            self.players[pid as usize].credits -= amount;
            self.players[to as usize].credits += amount;
            let (a, b) = (
                self.players[pid as usize].name.clone(),
                self.players[to as usize].name.clone(),
            );
            self.push_chat(NEUTRAL, format!("{} wired {} credits to {}", a, amount, b));
        }
    }

    /// Toggle our alliance offer toward `with`. The pact is in force only once
    /// both have offered; toggling off (or one side withdrawing) breaks it.
    fn cmd_ally(&mut self, pid: Pid, with: Pid) {
        if pid == with || with as usize >= MAX_PLAYERS || self.player(with).is_none() {
            return;
        }
        let now_offering;
        {
            let Some(p) = self.players.get_mut(pid as usize) else { return };
            p.ally_mask ^= 1 << with;
            now_offering = (p.ally_mask >> with) & 1 == 1;
        }
        let (a, b) = (
            self.players[pid as usize].name.clone(),
            self.players[with as usize].name.clone(),
        );
        if now_offering {
            if self.allied(pid, with) {
                self.push_chat(NEUTRAL, format!("{} and {} are now allied", a, b));
            } else {
                self.push_chat(NEUTRAL, format!("{} offers an alliance to {}", a, b));
            }
        } else {
            self.push_chat(NEUTRAL, format!("{} withdrew from alliance with {}", a, b));
        }
    }

    // =====================================================================
    // Systems
    // =====================================================================

    fn sys_construction(&mut self) {
        let low: Vec<bool> = self.players.iter().map(|p| p.low_power()).collect();
        let mut completed: Vec<(Eid, Kind, Tp)> = Vec::new();
        for i in 0..self.ents.len_slots() {
            let Some(e) = self.ents.slots[i].as_mut() else { continue };
            if !e.kind.is_building() || e.done {
                continue;
            }
            let rate = if low.get(e.owner as usize).copied().unwrap_or(false) { 1 } else { 2 };
            e.con_progress += rate;
            let total = stats(e.kind).build_time * 2;
            let maxhp = stats(e.kind).max_hp;
            e.hp = ((maxhp as i64 * e.con_progress.min(total) as i64) / total as i64).max(1) as i32;
            if e.con_progress >= total {
                e.done = true;
                e.hp = maxhp;
                completed.push((e.id, e.kind, e.tile()));
            }
        }
        for (id, kind, tile) in completed {
            let c = self.ents.get(id).map(|e| e.center()).unwrap_or(tile.center());
            self.events.push(VisEvent::Built { at: c });
            if kind == Kind::Refinery {
                // a refinery comes with a free harvester — same deal as the classics
                let owner = self.ents.get(id).map(|e| e.owner).unwrap_or(NEUTRAL);
                if owner != NEUTRAL {
                    if let Some(t) = self.find_free_tile_near(Tp::new(tile.x + 1, tile.y + 3), 5) {
                        self.ents.spawn(owner, Kind::Harvester, t.center());
                    }
                }
            }
        }
    }

    fn sys_production(&mut self) {
        let low: Vec<bool> = self.players.iter().map(|p| p.low_power()).collect();
        let counts: Vec<u32> = self.players.iter().map(|p| p.unit_count).collect();
        let caps: Vec<u32> = self.players.iter().map(|p| p.unit_cap).collect();
        let mut spawned: Vec<u32> = vec![0; self.players.len()];
        let mut to_spawn: Vec<(Pid, Kind, Tp, Option<Tp>)> = Vec::new();

        // Missile Silos quietly charge their nuke (slower while browned-out); the
        // charge lives in `work_t`, which buildings otherwise ignore — and it's
        // already serialized, so a half-charged silo survives a save.
        for i in 0..self.ents.len_slots() {
            let Some(e) = self.ents.slots[i].as_mut() else { continue };
            if e.kind == Kind::MissileSilo && e.done && e.work_t < crate::stats::NUKE_CHARGE {
                let rate = if low.get(e.owner as usize).copied().unwrap_or(false) { 1 } else { 2 };
                e.work_t = (e.work_t + rate).min(crate::stats::NUKE_CHARGE);
            }
        }

        for i in 0..self.ents.len_slots() {
            let Some(e) = self.ents.slots[i].as_mut() else { continue };
            if !e.kind.is_building() || !e.done || e.queue.is_empty() {
                continue;
            }
            let pid = e.owner as usize;
            let rate = if low.get(pid).copied().unwrap_or(false) { 1 } else { 2 };
            let kind = e.queue[0];
            let total = stats(kind).build_time * 2;
            if e.prod_progress < total {
                e.prod_progress += rate;
            }
            if e.prod_progress >= total {
                // at the unit cap: hold the finished unit until room frees up
                let count = counts.get(pid).copied().unwrap_or(0) + spawned.get(pid).copied().unwrap_or(0);
                if count >= caps.get(pid).copied().unwrap_or(0) {
                    continue;
                }
                let tile = e.tile();
                let foot = e.foot();
                let exit = Tp::new(tile.x + foot.0 / 2, tile.y + foot.1);
                e.queue.remove(0);
                e.prod_progress = 0;
                if pid < spawned.len() {
                    spawned[pid] += 1;
                }
                to_spawn.push((e.owner, kind, exit, e.rally));
            }
        }
        for (owner, kind, exit, rally) in to_spawn {
            if let Some(t) = self.find_free_tile_near(exit, 6) {
                let id = self.ents.spawn(owner, kind, t.center());
                if let Some(r) = rally {
                    let p = path::find(&self.map, t, r, false, &self.open_gates_for(owner));
                    if let Some(u) = self.ents.get_mut(id) {
                        u.goal = Some(r);
                        u.path = p;
                    }
                }
            }
        }
    }

    fn sys_harvest(&mut self) {
        for i in 0..self.ents.len_slots() {
            let Some(mut e) = self.ents.take(i) else { continue };
            if e.kind != Kind::Harvester || e.hp <= 0 {
                self.ents.put(i, e);
                continue;
            }
            match e.hphase {
                HarvestPhase::Idle => {
                    if e.work_t > 0 {
                        e.work_t -= 1;
                    } else if e.cargo >= HARVESTER_CAP / 2
                        && self.nearest_refinery(e.owner, e.pos).is_some()
                    {
                        self.route_to_refinery(&mut e);
                    } else if e.goal.is_none() {
                        if let Some(t) = self.find_ore_near(e.tile(), 48) {
                            e.ore_tile = Some(t);
                            e.hphase = HarvestPhase::ToOre;
                            e.goal = Some(t);
                            e.stuck = 0;
                            e.path = path::find(&self.map, e.tile(), t, false, &self.open_gates_for(e.owner));
                        } else {
                            e.work_t = 30; // nothing to mine right now; nap
                        }
                    }
                }
                HarvestPhase::ToOre => {
                    if e.path.is_empty() {
                        // arrived if we're on the resource (ground ore) or beside
                        // it (a tree/rock worked from the next tile over)
                        let here = e.tile();
                        let arrived = e
                            .ore_tile
                            .map(|tt| self.map.resource_kind(tt).is_some() && (here.x - tt.x).abs() <= 1 && (here.y - tt.y).abs() <= 1)
                            .unwrap_or(false);
                        if arrived {
                            e.hphase = HarvestPhase::Mining;
                            e.goal = None;
                            e.work_t = 0;
                        } else if e.goal.is_none() {
                            e.hphase = HarvestPhase::Idle;
                            e.work_t = 5;
                        }
                    }
                }
                HarvestPhase::Mining => {
                    e.work_t += 1;
                    if e.work_t >= 6 {
                        e.work_t = 0;
                        let t = e.ore_tile.unwrap_or_else(|| e.tile());
                        match self.map.resource_kind(t) {
                            None => self.seek_more_or_unload(&mut e),
                            // don't mix loads: carrying a different resource? unload first
                            Some(rk) if e.cargo > 0 && e.cargo_kind != rk => self.route_to_refinery(&mut e),
                            Some(rk) => {
                                e.cargo_kind = rk;
                                let avail = self.map.ore_at(t);
                                let take = 40.min(avail).min(HARVESTER_CAP - e.cargo);
                                self.map.set_ore(t, avail - take);
                                e.cargo += take;
                                e.face = (e.face + 1) % 16; // grinding/chopping wiggle
                                if avail - take == 0 {
                                    // the heart of a mined-out mountain holds Essence —
                                    // and in the netherealm, the obsidian is rich with it
                                    let ess = match self.map.terrain_at(t) {
                                        Terrain::Mountain => 25,
                                        Terrain::Obsidian => 50,
                                        _ => 0,
                                    };
                                    if ess > 0 {
                                        if let Some(p) = self.players.get_mut(e.owner as usize) {
                                            p.essence = p.essence.saturating_add(ess);
                                        }
                                    }
                                    self.map.clear_resource(t); // chopped/mined out → open ground
                                }
                                if e.cargo >= HARVESTER_CAP {
                                    self.route_to_refinery(&mut e);
                                } else if self.map.ore_at(t) == 0 {
                                    self.seek_more_or_unload(&mut e);
                                }
                            }
                        }
                    }
                }
                HarvestPhase::ToRefinery => {
                    if e.path.is_empty() && e.goal.is_none() {
                        if let Some(rid) = self.nearest_refinery(e.owner, e.pos) {
                            let near = self
                                .ents
                                .get(rid)
                                .map(|r| e.pos.dist2(r.center()) <= (3 * FX as i64) * (3 * FX as i64))
                                .unwrap_or(false);
                            if near {
                                e.hphase = HarvestPhase::Unloading;
                                e.work_t = 0;
                            } else {
                                self.route_to_refinery(&mut e);
                            }
                        } else {
                            e.hphase = HarvestPhase::Idle;
                            e.work_t = 40;
                        }
                    }
                }
                HarvestPhase::Unloading => {
                    let chunk = 25.min(e.cargo);
                    e.cargo -= chunk;
                    if let Some(p) = self.players.get_mut(e.owner as usize) {
                        match e.cargo_kind {
                            1 => p.wood = p.wood.saturating_add(chunk as u32),
                            2 => p.stone = p.stone.saturating_add(chunk as u32),
                            _ => {
                                p.credits += chunk as u32;
                                p.away.credits_gained = p.away.credits_gained.saturating_add(chunk as u32);
                            }
                        }
                    }
                    if e.cargo == 0 {
                        self.events.push(VisEvent::Unload { at: e.pos, amount: HARVESTER_CAP as u32 });
                        self.seek_more_or_unload(&mut e); // back to the same resource if any
                    }
                }
            }
            // progress watchdog: a harvester that spends too long in a
            // travelling phase without reaching ore or a refinery is wedged
            // (blocked dock, unreachable field). Reset it so it re-plans from
            // scratch — it can never hang permanently.
            match e.hphase {
                HarvestPhase::ToOre | HarvestPhase::ToRefinery => {
                    // only count time we're genuinely wedged (no path and unable
                    // to re-plan) — NOT honest long-distance travel, or a harvester
                    // sent to a far forest would give up and default back to ore
                    if e.path.is_empty() {
                        e.stall = e.stall.saturating_add(1);
                        if e.stall > 120 {
                            e.stall = 0;
                            e.hphase = HarvestPhase::Idle;
                            e.work_t = 0;
                            e.goal = None;
                        }
                    } else {
                        e.stall = 0;
                    }
                }
                _ => e.stall = 0, // mining/unloading/idle = making progress
            }
            self.ents.put(i, e);
        }
    }

    /// Route a harvester to the nearest reachable tile bordering its closest
    /// refinery (scanning the whole perimeter, not one fixed dock that might
    /// be blocked). Arrival is then detected by proximity in `ToRefinery`.
    fn route_to_refinery(&mut self, e: &mut Ent) {
        let Some(rid) = self.nearest_refinery(e.owner, e.pos) else {
            e.hphase = HarvestPhase::Idle;
            e.work_t = 40;
            return;
        };
        let Some(r) = self.ents.get(rid) else {
            e.hphase = HarvestPhase::Idle;
            e.work_t = 40;
            return;
        };
        let rt = r.tile();
        let (fw, fh) = r.foot();
        // perimeter ring around the footprint, nearest to the harvester first
        let mut docks: Vec<Tp> = Vec::new();
        for dx in -1..=fw {
            for dy in -1..=fh {
                let on_ring = dx == -1 || dx == fw || dy == -1 || dy == fh;
                let t = Tp::new(rt.x + dx, rt.y + dy);
                if on_ring && self.map.walkable(t) {
                    docks.push(t);
                }
            }
        }
        docks.sort_by_key(|t| t.center().dist2(e.pos));
        e.hphase = HarvestPhase::ToRefinery;
        e.goal = None;
        e.stuck = 0;
        e.path.clear();
        let gates = self.open_gates_for(e.owner);
        for d in docks {
            let p = path::find(&self.map, e.tile(), d, true, &gates);
            if !p.is_empty() {
                e.path = p;
                e.goal = Some(d);
                break;
            }
        }
        // if nothing was reachable the path stays empty; the proximity check
        // and the watchdog above keep it from hanging.
    }

    fn nearest_refinery(&self, owner: Pid, from: Fp) -> Option<Eid> {
        let mut best: Option<(i64, Eid)> = None;
        for e in self.ents.iter() {
            if e.owner == owner && e.kind == Kind::Refinery && e.done && e.hp > 0 {
                let d = from.dist2(e.center());
                if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best = Some((d, e.id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Expanding-ring scan for the nearest ore tile. Deterministic scan order.
    /// After a harvester depletes a tile (or finishes unloading), send it to the
    /// next reachable patch of the SAME resource; failing that, fall back to ore;
    /// failing that, unload what it has or nap.
    fn seek_more_or_unload(&mut self, e: &mut Ent) {
        let here = e.tile();
        // 1) Return to the tile we were ASSIGNED if it still holds resource. This
        //    is what keeps an explicit "go mine that stone" order from reverting
        //    to ore after the harvester is forced to dump a mismatched ore load
        //    first ("don't mix loads"). The assignment, not the hopper, wins.
        let assigned = e.ore_tile.filter(|t| self.map.ore_at(*t) > 0 && self.map.resource_kind(*t).is_some());
        // 2) else the nearest patch of what we were gathering, 3) else fall to ore
        let want = assigned.and_then(|t| self.map.resource_kind(t)).unwrap_or(e.cargo_kind);
        let to = assigned
            .or_else(|| self.find_resource_near(here, want, 10))
            .or_else(|| if want != 0 { self.find_resource_near(here, 0, 40) } else { None });
        if let Some(nt) = to {
            let adj = !self.map.terrain_at(nt).passable();
            e.ore_tile = Some(nt);
            e.hphase = HarvestPhase::ToOre;
            e.goal = Some(nt);
            e.stuck = 0;
            e.path = path::find(&self.map, here, nt, adj, &self.open_gates_for(e.owner));
        } else if e.cargo > 0 {
            self.route_to_refinery(e);
        } else {
            e.hphase = HarvestPhase::Idle;
            e.work_t = 10;
        }
    }

    /// Nearest tile holding resource `want` (0 ore, 1 wood, 2 stone) that a
    /// harvester can actually work — the tile itself if walkable (ground ore),
    /// or one with a walkable neighbour (a tree/rock worked from beside it).
    fn find_resource_near(&self, from: Tp, want: u8, max_r: i32) -> Option<Tp> {
        let reachable = |t: Tp| {
            self.map.resource_kind(t) == Some(want)
                && (self.map.walkable(t)
                    || [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| self.map.walkable(Tp::new(t.x + dx, t.y + dy))))
        };
        if reachable(from) {
            return Some(from);
        }
        for r in 1..=max_r {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let t = Tp::new(from.x + dx, from.y + dy);
                    if reachable(t) {
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    fn find_ore_near(&self, from: Tp, max_r: i32) -> Option<Tp> {
        if self.map.ore_at(from) > 0 {
            return Some(from);
        }
        for r in 1..=max_r {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue; // ring perimeter only
                    }
                    let t = Tp::new(from.x + dx, from.y + dy);
                    if self.map.ore_at(t) > 0 && self.map.walkable(t) {
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    /// Nearest walkable tile not occupied by another unit. Used for spawning.
    pub fn find_free_tile_near(&self, from: Tp, max_r: i32) -> Option<Tp> {
        let occupied = |t: Tp| {
            self.ents
                .iter()
                .any(|e| e.kind.is_unit() && e.tile() == t)
        };
        for r in 0i32..=max_r {
            for dy in -r..=r {
                for dx in -r..=r {
                    if r > 0 && dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let t = Tp::new(from.x + dx, from.y + dy);
                    if self.map.walkable(t) && !occupied(t) {
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    /// N distinct walkable tiles near a destination, for group-move spreading.
    fn spread_tiles(&self, around: Tp, n: usize) -> Vec<Tp> {
        let mut out = Vec::with_capacity(n);
        'outer: for r in 0i32..10 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if r > 0 && dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let t = Tp::new(around.x + dx, around.y + dy);
                    if self.map.walkable(t) {
                        out.push(t);
                        if out.len() >= n {
                            break 'outer;
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            out.push(around);
        }
        out
    }

    fn sys_movement(&mut self) {
        for i in 0..self.ents.len_slots() {
            let Some(mut e) = self.ents.take(i) else { continue };
            if !e.kind.is_unit() || e.hp <= 0 {
                self.ents.put(i, e);
                continue;
            }
            // friendly gates this unit may walk through (own + allies')
            let gates = self.open_gates_for(e.owner);
            // following a (possibly moving) entity: re-path on a timer
            if let Some(fid) = e.follow {
                if e.follow_t > 0 {
                    e.follow_t -= 1;
                }
                match self.ents.get(fid) {
                    Some(t) => {
                        if e.follow_t == 0 {
                            let tt = t.tile();
                            let t_is_b = t.kind.is_building();
                            let endpoint = e.path.first().copied().or(e.goal);
                            let moved = endpoint
                                .map(|p| (p.x - tt.x).abs().max((p.y - tt.y).abs()) > 1)
                                .unwrap_or(true);
                            if moved {
                                e.goal = Some(tt);
                                e.stuck = 0;
                                e.path = path::find(&self.map, e.tile(), tt, t_is_b, &gates);
                            }
                            e.follow_t = 8;
                        }
                    }
                    None => {
                        e.follow = None;
                    }
                }
            }
            // walk the path
            if let Some(&next) = e.path.last() {
                if !self.map.walkable(next) && !gates.contains(&next) {
                    if Some(next) == e.goal {
                        // destination itself is occupied (e.g. attacking a building): close enough
                        e.path.clear();
                        e.goal = None;
                    } else if let Some(g) = e.goal {
                        if e.stuck < 3 {
                            e.stuck += 1;
                            e.path = path::find(&self.map, e.tile(), g, false, &gates);
                        } else {
                            e.path.clear();
                            e.goal = None;
                        }
                    } else {
                        e.path.clear();
                    }
                } else {
                    let dest = next.center();
                    // terrain modulates speed: roads/ice quick, snow/marsh slow
                    let sp = stats(e.kind).speed * self.map.terrain_at(e.tile()).speed_pct() / 100;
                    let (np, arrived) = e.pos.step_toward(dest, sp);
                    if np != e.pos {
                        e.face = e.pos.facing_to(dest);
                    }
                    e.pos = np;
                    if arrived {
                        e.path.pop();
                        if e.path.is_empty() {
                            match e.goal {
                                Some(g) if g == e.tile() => {
                                    e.goal = None;
                                    e.stuck = 0;
                                }
                                Some(g) => {
                                    // partial path ended short; one more try
                                    if e.stuck < 3 {
                                        e.stuck += 1;
                                        e.path = path::find(&self.map, e.tile(), g, false, &gates);
                                        if e.path.is_empty() {
                                            e.goal = None;
                                        }
                                    } else {
                                        e.goal = None;
                                    }
                                }
                                None => {}
                            }
                        }
                    }
                }
            } else if let Some(g) = e.goal {
                if g == e.tile() {
                    e.goal = None;
                    e.stuck = 0;
                } else if e.stuck < 3 {
                    e.stuck += 1;
                    e.path = path::find(&self.map, e.tile(), g, false, &gates);
                    if e.path.is_empty() {
                        e.goal = None;
                    }
                } else {
                    e.goal = None;
                }
            }
            self.ents.put(i, e);
        }

        // gentle separation: units sharing a tile nudge apart (deterministic pair order)
        let mut occ: Vec<(i32, usize)> = Vec::new();
        for i in 0..self.ents.len_slots() {
            if let Some(e) = self.ents.slots[i].as_ref() {
                if e.kind.is_unit()
                    && e.hp > 0
                    && !matches!(e.hphase, HarvestPhase::Mining | HarvestPhase::Unloading)
                {
                    let t = e.tile();
                    occ.push((t.y * self.map.w + t.x, i));
                }
            }
        }
        occ.sort_unstable();
        for w in occ.windows(2) {
            let (ta, ia) = w[0];
            let (tb, ib) = w[1];
            if ta != tb {
                continue;
            }
            let pa = self.ents.slots[ia].as_ref().map(|e| e.pos);
            let (Some(pa), Some(eb)) = (pa, self.ents.slots[ib].as_mut()) else { continue };
            let mut dx = (eb.pos.x - pa.x).signum() * 5;
            let mut dy = (eb.pos.y - pa.y).signum() * 5;
            if dx == 0 && dy == 0 {
                dx = ((ib as i32 & 1) * 2 - 1) * 5;
                dy = (((ib as i32 >> 1) & 1) * 2 - 1) * 5;
            }
            let np = Fp { x: eb.pos.x + dx, y: eb.pos.y + dy };
            if self.map.walkable(np.tile()) {
                eb.pos = np;
            }
        }
    }

    fn sys_combat_and_capture(&mut self) {
        struct TInfo {
            id: Eid,
            owner: Pid,
            kind: Kind,
            center: Fp,
            alive: bool,
            aggressive: bool,
            foot_half: i32,
        }
        let infos: Vec<TInfo> = self
            .ents
            .iter()
            .map(|e| TInfo {
                id: e.id,
                owner: e.owner,
                kind: e.kind,
                center: e.center(),
                alive: e.hp > 0,
                aggressive: e.aggressive,
                foot_half: e.foot().0.max(e.foot().1) * FX / 2,
            })
            .collect();
        let lows: Vec<bool> = self.players.iter().map(|p| p.low_power()).collect();
        // The reckoning's forced truce: while the Warlock walks, the nations
        // unite — no settler auto-fires on another (the lore's "one day they
        // unite to face it"). Derived from world state, so every peer agrees.
        let truce = self.ents.iter().any(|e| e.kind == Kind::Warlock && e.hp > 0);
        // "Armed": by default the night-horde has only claws — it can savage your
        // units but cannot touch buildings, and strikes at melee range. The
        // instant a boss walks (the source of the dark magic), the supernatural
        // gains "the machinery of war": grunts get guns (ranged) and turn on
        // your structures too. Derived from world state, so all peers agree.
        let armed = self.ents.iter().any(|e| e.kind.is_boss() && e.hp > 0);
        // mutual-alliance lookup (allies don't shoot or capture each other)
        let amask: Vec<u8> = self.players.iter().map(|p| p.ally_mask).collect();
        let mutual = |a: Pid, b: Pid| -> bool {
            if a == b || a == NEUTRAL || b == NEUTRAL {
                return false;
            }
            if truce {
                return true; // united against the puppeteer — hold your fire
            }
            let ma = amask.get(a as usize).copied().unwrap_or(0);
            let mb = amask.get(b as usize).copied().unwrap_or(0);
            (ma >> b) & 1 == 1 && (mb >> a) & 1 == 1
        };
        let mut dmg: Vec<DmgEvent> = Vec::new();
        let mut captures: Vec<(Eid, Eid)> = Vec::new();

        for i in 0..self.ents.len_slots() {
            let Some(mut e) = self.ents.take(i) else { continue };
            if e.hp <= 0 {
                self.ents.put(i, e);
                continue;
            }
            let st = stats(e.kind);

            // engineers: capture instead of fight
            if e.kind == Kind::Engineer {
                if let Some(tid) = e.target {
                    let info = infos.iter().find(|t| t.id == tid);
                    match info {
                        Some(t) if t.alive && t.kind.is_building() && t.owner != e.owner && !mutual(e.owner, t.owner) => {
                            let reach = (FX + t.foot_half + FX / 2) as i64;
                            if e.pos.dist2(t.center) <= reach * reach {
                                captures.push((e.id, tid));
                            }
                        }
                        _ => {
                            e.target = None;
                            e.follow = None;
                        }
                    }
                }
                self.ents.put(i, e);
                continue;
            }
            if st.damage <= 0 || (e.kind.is_building() && !e.done) {
                self.ents.put(i, e);
                continue;
            }
            let is_tower = e.kind.is_defense();
            // Night-grunt reach: claws (melee, range 1) until the dark is armed,
            // then zombie-guns (ranged). Bosses & the HellTank keep their own
            // statted range; everyone non-monster is unaffected.
            let mons_grunt = e.kind.is_monster() && !e.kind.is_boss() && e.kind != Kind::HellTank;
            let eff_range = if mons_grunt {
                if armed { 5 } else { 1 }
            } else {
                st.range
            };
            if is_tower && lows.get(e.owner as usize).copied().unwrap_or(false) {
                if e.cooldown > 0 {
                    e.cooldown -= 1;
                }
                self.ents.put(i, e); // tower offline on low power
                continue;
            }

            // validate current target (drop it if it died or just became an ally)
            if let Some(tid) = e.target {
                let ok = infos.iter().any(|t| t.id == tid && t.alive && !mutual(e.owner, t.owner));
                if !ok {
                    e.target = None;
                    if e.follow == Some(tid) {
                        e.follow = None;
                    }
                }
            }
            // acquire a target (player units/towers do; so do NEUTRAL monsters,
            // which hunt everyone — but ordinary neutrals, like the village, never)
            if e.target.is_none() && (e.owner != NEUTRAL || e.kind.is_monster() || e.aggressive) {
                if e.scan_in > 0 {
                    e.scan_in -= 1;
                }
                let idle = e.goal.is_none() && e.path.is_empty();
                if (is_tower || e.aggressive || idle) && e.scan_in == 0 {
                    e.scan_in = 5;
                    let max_r = if e.aggressive && !is_tower { st.sight } else { eff_range };
                    let max_d = (max_r * FX) as i64;
                    let mut best: Option<(i64, Eid)> = None;
                    for t in &infos {
                        // unarmed night-grunts savage units but can't touch your
                        // buildings (the lore: only "local damage" till the dark
                        // takes up the machinery of war)
                        if e.kind.is_monster() && !armed && t.kind.is_building() {
                            continue;
                        }
                        // walls & gates used to be unkillable (excluded here),
                        // which made them magic. Now they're fair game — units
                        // and towers will batter through an enemy barrier.
                        // Ordinary neutrals (the village, drifting smoke) are still
                        // off-limits, but NEUTRAL *monsters* — and units the essence
                        // smoke has CORRUPTED (NEUTRAL + aggressive) — are everyone's enemy.
                        let neutral_safe = t.owner == NEUTRAL && !t.kind.is_monster() && !t.aggressive;
                        if !t.alive || t.owner == e.owner || neutral_safe || mutual(e.owner, t.owner) {
                            continue;
                        }
                        let d2 = e.pos.dist2(t.center);
                        let r = max_d + t.foot_half as i64;
                        if d2 <= r * r && best.map(|(bd, _)| d2 < bd).unwrap_or(true) {
                            best = Some((d2, t.id));
                        }
                    }
                    if let Some((_, tid)) = best {
                        e.target = Some(tid);
                        if e.aggressive && e.kind.is_unit() {
                            e.follow = Some(tid);
                            e.follow_t = 0;
                        }
                    }
                }
            }
            // engage
            if let Some(tid) = e.target {
                if let Some(t) = infos.iter().find(|t| t.id == tid) {
                    let reach = (eff_range * FX) as i64 + t.foot_half as i64;
                    let in_range = e.pos.dist2(t.center) <= reach * reach;
                    if in_range {
                        e.face = e.pos.facing_to(t.center);
                        if e.kind.is_unit() && e.follow == Some(tid) {
                            e.path.clear(); // halt to shoot
                            e.goal = None;
                        }
                        if e.cooldown == 0 {
                            let amount = st.damage * dmg_pct(e.kind, t.kind) / 100;
                            let rocket = matches!(e.kind, Kind::Rocketeer | Kind::Tank | Kind::GuardTower | Kind::CannonTower);
                            dmg.push(DmgEvent { target: tid, amount, from: e.center(), rocket, by_owner: e.owner });
                            e.cooldown = st.rof;
                        } else {
                            e.cooldown -= 1;
                        }
                    } else {
                        if e.cooldown > 0 {
                            e.cooldown -= 1;
                        }
                        if e.kind.is_building() {
                            e.target = None; // towers drop out-of-range targets
                        }
                    }
                }
            } else if e.cooldown > 0 {
                e.cooldown -= 1;
            }
            self.ents.put(i, e);
        }

        // apply damage after all decisions (no order-dependent half-states)
        let mut deaths: Vec<(Eid, Pid, Pid, Kind)> = Vec::new(); // (id, killer, victim, kind)
        for d in dmg {
            if let Some(t) = self.ents.get_mut(d.target) {
                if t.hp > 0 {
                    t.hp -= d.amount;
                    let to = t.center();
                    let (vo, vk) = (t.owner, t.kind);
                    self.events.push(VisEvent::Shot { from: d.from, to, rocket: d.rocket });
                    if t.hp <= 0 {
                        deaths.push((d.target, d.by_owner, vo, vk));
                    }
                }
            }
        }
        for (id, killer, victim, kind) in deaths {
            // raid accounting: a real settler lost something to an enemy
            if victim != NEUTRAL && killer != NEUTRAL && killer != victim {
                if kind.is_building() {
                    // razing a building loots a cut of the owner's treasury
                    let avail = self.players.get(victim as usize).map(|p| p.credits).unwrap_or(0);
                    let take = (stats(kind).cost / 4).min(avail);
                    if let Some(vp) = self.players.get_mut(victim as usize) {
                        vp.credits -= take;
                        vp.away.credits_lost = vp.away.credits_lost.saturating_add(take);
                        vp.away.buildings_lost = vp.away.buildings_lost.saturating_add(1);
                        vp.away.attacks = vp.away.attacks.saturating_add(1);
                        vp.away.last_foe = killer;
                    }
                    if let Some(ap) = self.players.get_mut(killer as usize) {
                        ap.credits = ap.credits.saturating_add(take);
                    }
                } else if let Some(vp) = self.players.get_mut(victim as usize) {
                    vp.away.units_lost = vp.away.units_lost.saturating_add(1);
                    vp.away.attacks = vp.away.attacks.saturating_add(1);
                    vp.away.last_foe = killer;
                }
            }
            // slaying ANY monster drops a mote of Essence — the rare tier-3
            // currency — onto its corpse, for any unit to collect (sys_loot). So
            // the horde is worth hunting, not just surviving (and a corpse cut
            // down by daylight, in sys_monsters, drops nothing — kill it yourself).
            if kind.is_monster() {
                let ess = match kind {
                    Kind::Moloch => 500,
                    Kind::Balrog => 300,
                    Kind::Warlock => 250,
                    Kind::Lich => 60,
                    Kind::HellTank => 15,
                    Kind::Demon => 8,
                    Kind::Vampire => 6,
                    Kind::Werewolf => 4,
                    _ => 2,
                };
                if let Some(dt) = self.ents.get(id).map(|e| e.tile()) {
                    self.loot.push(Loot { tile: dt, amount: ess, kind: LOOT_ESSENCE, born: self.tick });
                }
            }
            // a hunted deer drops raw MEAT — cook it (needs a House) into food
            if kind == Kind::Deer && killer != NEUTRAL {
                if let Some(dt) = self.ents.get(id).map(|e| e.tile()) {
                    self.loot.push(Loot { tile: dt, amount: crate::stats::DEER_MEAT, kind: LOOT_MEAT, born: self.tick });
                }
            }
            // slaying a boss pays a hero's bounty and is shouted to the world
            if kind.is_boss() && killer != NEUTRAL {
                let bounty = if kind == Kind::Warlock { 8000 } else { 4000 };
                let mat = if kind == Kind::Warlock { 1500 } else { 800 };
                if let Some(ap) = self.players.get_mut(killer as usize) {
                    ap.credits = ap.credits.saturating_add(bounty);
                    ap.wood = ap.wood.saturating_add(mat);
                    ap.stone = ap.stone.saturating_add(mat);
                }
                let who = self.players.get(killer as usize).map(|p| p.name.clone()).unwrap_or_default();
                self.push_chat(NEUTRAL, format!("{who} has slain {}! (+{bounty}$)", stats(kind).name));
            }
            // THE RECKONING WON: felling the Warlock ends the capstone. Every
            // surviving nation shares the spoils of the united victory, and the
            // dark recedes — three days of hard-won peace before it stirs again.
            if kind == Kind::Warlock {
                self.peace_until = self.tick + DAY_LEN * 3;
                for p in self.players.iter_mut() {
                    if p.joined && !p.defeated {
                        p.credits = p.credits.saturating_add(3000);
                        p.essence = p.essence.saturating_add(120);
                    }
                }
                self.push_chat(NEUTRAL, "THE RECKONING IS WON. The puppeteer falls and the nations stand united — the dark recedes. (+3000$, +120 essence to all who endured)".into());
                // ...and a rift tears open where it fell. March a force through it
                // to descend into the netherealm it crawled from (one-way).
                if self.realm == Realm::Overworld && !self.ents.iter().any(|e| e.kind == Kind::NetherPortal) {
                    if let Some(dt) = self.ents.get(id).map(|e| e.tile()) {
                        self.open_nether_portal(dt);
                    }
                }
            }
            // THE TRUE VICTORY: felling Moloch breaks the netherealm at its source.
            if kind == Kind::Moloch {
                for p in self.players.iter_mut() {
                    if p.joined && !p.defeated {
                        p.credits = p.credits.saturating_add(8000);
                        p.essence = p.essence.saturating_add(400);
                    }
                }
                self.push_chat(NEUTRAL, "MOLOCH FALLS. The horned god of the deep is slain — B-Proxima is yours, surface and abyss. (+8000$, +400 essence)".into());
            }
            self.kill(id);
        }
        for (eng, bid) in captures {
            let new_owner = match self.ents.get(eng) {
                Some(e) => e.owner,
                None => continue,
            };
            let mut flipped = false;
            if let Some(b) = self.ents.get_mut(bid) {
                if b.hp > 0 && b.kind.is_building() && b.owner != new_owner {
                    b.owner = new_owner;
                    b.queue.clear();
                    b.prod_progress = 0;
                    b.rally = None;
                    flipped = true;
                }
            }
            if flipped {
                let at = self.ents.get(bid).map(|b| b.center()).unwrap_or(Fp { x: 0, y: 0 });
                self.events.push(VisEvent::Captured { at });
                self.ents.despawn(eng); // the engineer stays with the building
                let name = self
                    .players
                    .get(new_owner as usize)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.push_chat(NEUTRAL, format!("{} captured a building", name));
            }
        }
    }

    fn kill(&mut self, id: Eid) {
        if let Some(e) = self.ents.get(id) {
            let big = e.kind.is_building() || !e.kind.is_infantry();
            let at = e.center();
            if e.kind.is_building() {
                let (t, f) = (e.tile(), e.foot());
                self.map.clear_block(t, f);
            }
            self.events.push(VisEvent::Die { at, big });
        }
        self.ents.despawn(id);
    }

    /// The night system: the dark breeds supernatural marauders that march on
    /// the nearest settlement, fight everyone, and burn in the open at dawn
    /// (unless they reach shade). Fully deterministic — it rolls the world RNG,
    /// so every peer breeds the identical horde. The lore: a final boss pits the
    /// nations against each other and looses these things in the dark; one day
    /// they'll unite to face it — for now it can only spawn, not arm them.
    /// Spawn a NEUTRAL marauder, already on the hunt.
    fn spawn_monster(&mut self, kind: Kind, t: Tp) {
        let id = self.ents.spawn(NEUTRAL, kind, t.center());
        if let Some(m) = self.ents.get_mut(id) {
            m.aggressive = true;
        }
    }

    /// A walkable tile out on a random map edge — where the horde crawls in from.
    fn random_edge_tile(&mut self) -> Option<Tp> {
        for _ in 0..10 {
            let span = self.map.w.max(self.map.h);
            let along = self.rng.range_i32(2, span - 3);
            let t = match self.rng.range_i32(0, 3) {
                0 => Tp::new(along.min(self.map.w - 3), 2),
                1 => Tp::new(self.map.w - 3, along.min(self.map.h - 3)),
                2 => Tp::new(along.min(self.map.w - 3), self.map.h - 3),
                _ => Tp::new(2, along.min(self.map.h - 3)),
            };
            if self.map.walkable(t) {
                return Some(t);
            }
        }
        None
    }

    /// The relentless nether brood: no day/night, just escalating Demon packs and
    /// the periodic Balrog. Deterministic (rolls the world RNG), like the surface.
    fn sys_nether_horde(&mut self) {
        let alive: Vec<Pid> = self
            .players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.joined && !p.defeated)
            .map(|(i, _)| i as Pid)
            .collect();
        if alive.is_empty() {
            return;
        }
        let depth = self.tick / DAY_LEN; // escalation step
        let mut interval: u32 = 24;
        if self.mode == Mode::Survival {
            interval = interval * 2 / 3;
        }
        match self.difficulty {
            0 => interval = interval * 3 / 2,
            2 => interval = interval * 2 / 3,
            _ => {}
        }
        interval = interval.max(6);
        if self.tick % interval == 0 {
            let horde = self.ents.iter().filter(|e| e.kind.is_monster()).count();
            let mut cap = 14 + 6 * alive.len() + 4 * depth as usize;
            if self.mode == Mode::Survival {
                cap *= 2;
            }
            match self.difficulty {
                0 => cap = cap * 3 / 5,
                2 => cap = cap * 3 / 2 + 6,
                _ => {}
            }
            if horde < cap {
                let pack = self.rng.range_i32(2, 5);
                if let Some(a) = self.random_edge_tile() {
                    for _ in 0..pack {
                        let ox = self.rng.range_i32(6, 18) * if self.rng.chance(1, 2) { 1 } else { -1 };
                        let oy = self.rng.range_i32(6, 18) * if self.rng.chance(1, 2) { 1 } else { -1 };
                        let st = Tp::new((a.x + ox).clamp(1, self.map.w - 2), (a.y + oy).clamp(1, self.map.h - 2));
                        if let Some(t) = self.find_free_tile_near(st, 8) {
                            self.spawn_monster(Kind::Demon, t);
                        }
                    }
                }
            }
        }
        // the Balrog stalks the deep — one at a time, on the half-cycle beat
        if self.tick % (DAY_LEN / 2) == 0 && depth >= 1 && !self.ents.iter().any(|e| e.kind == Kind::Balrog) {
            if let Some(t) = self.random_edge_tile().and_then(|e| self.find_free_tile_near(e, 10)) {
                self.spawn_monster(Kind::Balrog, t);
                self.push_chat(NEUTRAL, "The ash splits — A BALROG strides up from the deep. Bring it down.".into());
            }
        }
        // MOLOCH wakes in the deep — the netherealm's master, one at a time.
        if self.tick % DAY_LEN == 0 && depth >= 5 && !self.ents.iter().any(|e| e.kind == Kind::Moloch) {
            if let Some(t) = self.random_edge_tile().and_then(|e| self.find_free_tile_near(e, 12)) {
                self.spawn_monster(Kind::Moloch, t);
                self.push_chat(NEUTRAL, "THE GROUND HEAVES. MOLOCH WAKES — the horned god of the deep has marked you. Run, or end it.".into());
            }
        }
        self.sys_nether_smoke();
    }

    /// Drifting essence smoke: purple clouds wander the netherealm; a unit that
    /// touches one is CORRUPTED — it turns NEUTRAL and aggressive, hunting its old
    /// allies. Smoke is unarmed and ignored by combat; its `hp` is a lifetime
    /// ticked down here so clouds dissipate. Deterministic (rolls the world RNG).
    fn sys_nether_smoke(&mut self) {
        // spawn + replenish drifting clouds (capped)
        let count = self.ents.iter().filter(|e| e.kind == Kind::EssenceSmoke).count();
        if self.tick % 20 == 0 && count < 10 {
            // a random interior walkable (ash) tile — clouds drift up everywhere
            let mut spot: Option<Tp> = None;
            for _ in 0..12 {
                let t = Tp::new(self.rng.range_i32(3, self.map.w - 4), self.rng.range_i32(3, self.map.h - 4));
                if self.map.walkable(t) && self.map.blocked_by(t) == 0 {
                    spot = Some(t);
                    break;
                }
            }
            if let Some(t) = spot {
                let id = self.ents.spawn(NEUTRAL, Kind::EssenceSmoke, t.center());
                if let Some(e) = self.ents.get_mut(id) {
                    e.hp = self.rng.range_i32(260, 600); // lifetime in ticks
                }
            }
        }
        // drift each cloud a slow random step + decay; collect victims to corrupt
        let mut corrupt: Vec<Eid> = Vec::new();
        let smokes: Vec<(Eid, Fp)> = self.ents.iter().filter(|e| e.kind == Kind::EssenceSmoke && e.hp > 0).map(|e| (e.id, e.pos)).collect();
        for (sid, spos) in &smokes {
            let (dx, dy) = (self.rng.range_i32(-1, 1) * (FX / 6), self.rng.range_i32(-1, 1) * (FX / 6));
            if let Some(s) = self.ents.get_mut(*sid) {
                s.hp -= 4;
                s.pos.x = (s.pos.x + dx).clamp(FX, (self.map.w - 2) * FX);
                s.pos.y = (s.pos.y + dy).clamp(FX, (self.map.h - 2) * FX);
                if s.hp <= 0 {
                    continue;
                }
            }
            // any owned, mortal unit within ~1 tile of the cloud succumbs
            let reach = (FX as i64 * 3 / 2) * (FX as i64 * 3 / 2);
            for e in self.ents.iter() {
                if e.kind.is_unit() && e.owner != NEUTRAL && e.hp > 0 && !e.kind.is_monster() && e.pos.dist2(*spos) <= reach {
                    corrupt.push(e.id);
                }
            }
        }
        for id in corrupt {
            if let Some(e) = self.ents.get_mut(id) {
                let at = e.center();
                e.owner = NEUTRAL;
                e.aggressive = true;
                e.target = None;
                e.goal = None;
                e.path.clear();
                self.events.push(VisEvent::Captured { at }); // a purple claim
            }
        }
        // reap dissipated clouds
        let dead: Vec<Eid> = self.ents.iter().filter(|e| e.kind == Kind::EssenceSmoke && e.hp <= 0).map(|e| e.id).collect();
        for id in dead {
            self.ents.despawn(id);
        }
    }

    /// Is this footprint in-bounds and free of blocks (terrain we can flatten)?
    fn footprint_open(&self, at: Tp, foot: (i32, i32)) -> bool {
        for dy in 0..foot.1 {
            for dx in 0..foot.0 {
                let t = Tp::new(at.x + dx, at.y + dy);
                if !self.map.in_bounds(t) || self.map.blocked_by(t) != 0 || self.map.terrain_at(t) == Terrain::Water {
                    return false;
                }
            }
        }
        true
    }

    /// Tear open the Warlock's rift near `near` — a free 2x2 NEUTRAL structure.
    fn open_nether_portal(&mut self, near: Tp) {
        let (fw, fh) = stats(Kind::NetherPortal).footprint;
        let mut spot: Option<Tp> = None;
        'search: for rad in 0..14 {
            for dy in -rad..=rad {
                for dx in -rad..=rad {
                    let at = Tp::new(near.x + dx, near.y + dy);
                    if self.footprint_open(at, (fw, fh)) {
                        spot = Some(at);
                        break 'search;
                    }
                }
            }
        }
        let Some(at) = spot else { return };
        for yy in 0..fh {
            for xx in 0..fw {
                let t = Tp::new(at.x + xx, at.y + yy);
                if !self.map.terrain_at(t).buildable() {
                    self.map.set_terrain(t, Terrain::Dirt);
                }
                self.map.set_ore(t, 0);
            }
        }
        let id = self.ents.spawn(NEUTRAL, Kind::NetherPortal, Fp { x: at.x * FX, y: at.y * FX });
        if let Some(e) = self.ents.get_mut(id) {
            e.done = true;
            e.hp = stats(Kind::NetherPortal).max_hp;
        }
        self.map.stamp_block(at, (fw, fh), id.idx + 1);
    }

    /// While a rift stands in the overworld, a player unit reaching it fires the
    /// one-way descent (deterministic: a fixed proximity check each tick).
    fn sys_portal(&mut self) {
        if self.realm != Realm::Overworld {
            return;
        }
        // the countdown is running → when it expires, the world falls
        if self.descent_at != 0 {
            if self.tick >= self.descent_at {
                self.descent_at = 0;
                self.descend_to_nether();
            }
            return;
        }
        let portal = self.ents.iter().find(|e| e.kind == Kind::NetherPortal).map(|e| (e.tile(), e.foot()));
        let Some((pt, (pw, ph))) = portal else { return };
        let reached = self.ents.iter().any(|e| {
            if !e.kind.is_unit() || e.owner == NEUTRAL || e.hp <= 0 {
                return false;
            }
            let t = e.tile();
            t.x >= pt.x - 1 && t.x <= pt.x + pw && t.y >= pt.y - 1 && t.y <= pt.y + ph
        });
        if reached {
            // arm a ~3.6 s build of dread before the world tears away
            self.descent_at = self.tick + 36;
            self.push_chat(NEUTRAL, "The rift seizes you — the world begins to tear away...".into());
        }
    }

    /// THE DESCENT — a one-way plunge into the netherealm. Regenerates the map as
    /// hell (from the world RNG, so peers match), carries each surviving player's
    /// army and stockpiles down to a fresh foothold, and drops the overworld.
    fn descend_to_nether(&mut self) {
        if self.realm == Realm::Nether {
            return;
        }
        let survivors: Vec<Pid> = self
            .players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.joined && !p.defeated)
            .map(|(i, _)| i as Pid)
            .collect();
        // carry surviving player units (owner, kind, hp); buildings & NEUTRAL drop away
        let mut carried: Vec<(Pid, Kind, i32)> = Vec::new();
        for e in self.ents.iter() {
            if e.kind.is_unit() && e.hp > 0 && survivors.contains(&e.owner) {
                carried.push((e.owner, e.kind, e.hp));
            }
        }
        // new ground + a clean slate of entities
        self.map = crate::mapgen::nether_realm(&mut self.rng);
        self.ents = Arena::new();
        self.realm = Realm::Nether;
        self.peace_until = 0;
        let sites = self.map.spawns.clone();
        let nsites = sites.len().max(1);
        // a fresh ConYard per survivor at a landing site
        for (i, &pid) in survivors.iter().enumerate() {
            let site = sites[i % nsites];
            let (fw, fh) = stats(Kind::ConYard).footprint;
            let cy = self.ents.spawn(pid, Kind::ConYard, Fp { x: site.x * FX, y: site.y * FX });
            if let Some(e) = self.ents.get_mut(cy) {
                e.done = true;
                e.hp = stats(Kind::ConYard).max_hp;
            }
            self.map.stamp_block(site, (fw, fh), cy.idx + 1);
            if let Some(u) = self.map.spawn_used.get_mut(i % nsites) {
                *u = pid;
            }
        }
        // re-land each survivor's army around their own foothold
        for (pid, kind, hp) in carried {
            let idx = survivors.iter().position(|&p| p == pid).unwrap_or(0);
            let site = sites[idx % nsites];
            if let Some(t) = self.find_free_tile_near(site, 16) {
                let id = self.ents.spawn(pid, kind, t.center());
                if let Some(e) = self.ents.get_mut(id) {
                    e.hp = hp;
                }
            }
        }
        self.push_chat(NEUTRAL, "THE DESCENT. The rift swallows the host — you stand in the netherealm now, on ash and fire, with no road home. Hunt the Balrog.".into());
    }

    /// A random open grass tile in the interior — where deer graze & berries grow.
    fn random_grass_tile(&mut self) -> Option<Tp> {
        for _ in 0..12 {
            let x = self.rng.range_i32(2, self.map.w - 3);
            let y = self.rng.range_i32(2, self.map.h - 3);
            let t = Tp::new(x, y);
            if self.map.terrain_at(t) == Terrain::Grass && self.map.walkable(t) && self.map.blocked_by(t) == 0 {
                return Some(t);
            }
        }
        None
    }

    /// Peaceful wildlife: deer graze and wander, flee when a unit gets close, and
    /// drop meat when hunted; wild berries sprout on the grass to forage. Fully
    /// deterministic (rolls the world RNG), so every peer sees the same meadow.
    fn sys_wildlife(&mut self) {
        let alive = self.players.iter().filter(|p| p.joined && !p.defeated).count().max(1);

        // spawn a deer now and then, up to a small cap
        if self.tick % 240 == 0 {
            let deer = self.ents.iter().filter(|e| e.kind == Kind::Deer).count();
            if deer < 3 + alive * 2 {
                if let Some(t) = self.random_grass_tile() {
                    self.ents.spawn(NEUTRAL, Kind::Deer, t.center());
                }
            }
        }
        // wild berries sprout on the grass to forage (food, no cooking needed)
        if self.tick % 360 == 0 {
            let berries = self.loot.iter().filter(|m| m.kind == LOOT_BERRY).count();
            if berries < 10 {
                if let Some(t) = self.random_grass_tile() {
                    self.loot.push(Loot { tile: t, amount: crate::stats::BERRY_FOOD, kind: LOOT_BERRY, born: self.tick });
                }
            }
        }
        // deer graze + bolt from danger
        if self.tick % 20 == 0 {
            let threats: Vec<Tp> = self
                .ents
                .iter()
                .filter(|e| e.owner != NEUTRAL && e.kind.is_unit() && e.hp > 0)
                .map(|e| e.tile())
                .collect();
            let (mw, mh) = (self.map.w, self.map.h);
            for i in 0..self.ents.len_slots() {
                let Some(mut e) = self.ents.take(i) else { continue };
                if e.kind != Kind::Deer || e.hp <= 0 {
                    self.ents.put(i, e);
                    continue;
                }
                let dt = e.tile();
                let threat = threats
                    .iter()
                    .filter(|t| (t.x - dt.x).abs() <= 5 && (t.y - dt.y).abs() <= 5)
                    .min_by_key(|t| (t.x - dt.x).pow(2) + (t.y - dt.y).pow(2));
                let goal = if let Some(th) = threat {
                    // bolt directly away from the nearest unit
                    Some(Tp::new((dt.x + (dt.x - th.x).signum() * 6).clamp(2, mw - 3), (dt.y + (dt.y - th.y).signum() * 6).clamp(2, mh - 3)))
                } else if e.path.is_empty() && self.rng.chance(1, 3) {
                    Some(Tp::new((dt.x + self.rng.range_i32(-5, 5)).clamp(2, mw - 3), (dt.y + self.rng.range_i32(-5, 5)).clamp(2, mh - 3)))
                } else {
                    None
                };
                if let Some(g) = goal {
                    if threat.is_some() || e.goal != Some(g) {
                        e.goal = Some(g);
                        e.path = path::find(&self.map, dt, g, false, &[]);
                        e.stuck = 0;
                    }
                }
                self.ents.put(i, e);
            }
        }
    }

    fn sys_monsters(&mut self) {
        // The netherealm has no kind sun: no dawn-burn, and a far deadlier brood.
        if self.realm == Realm::Nether {
            self.sys_nether_horde();
            return;
        }
        let night = is_night(self.tick);

        // --- DAWN BURN: anything caught in open daylight cooks (Minecraft rule) ---
        if !night {
            let mut burned: Vec<Eid> = Vec::new();
            for i in 0..self.ents.len_slots() {
                let Some(e) = self.ents.slots[i].as_mut() else { continue };
                if !e.kind.is_monster() || e.hp <= 0 {
                    continue;
                }
                let slow = e.kind.smoulders();
                let t = e.tile();
                // grunts hide under tall cover; bosses & war-hulks are too vast to
                // hide and merely smoulder — they linger into day unless you finish them
                let sheltered = !slow
                    && [(0i32, 0i32), (1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| {
                        matches!(self.map.terrain_at(Tp::new(t.x + dx, t.y + dy)), Terrain::Tree | Terrain::Rock | Terrain::Mountain)
                    });
                if !sheltered {
                    e.hp -= if slow { 3 } else { 8 };
                    if e.hp <= 0 {
                        burned.push(e.id);
                    }
                }
            }
            for id in burned {
                self.kill(id);
            }
        }

        if !night {
            return; // by day, the dark only burns — nothing new comes
        }
        if self.tick < self.peace_until {
            return; // the reckoning was won — the dark recedes for a while
        }
        let alive: Vec<Pid> = self
            .players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.joined && !p.defeated)
            .map(|(i, _)| i as Pid)
            .collect();
        if alive.is_empty() {
            return;
        }
        let night_no = self.tick / DAY_LEN; // 0-indexed escalation step
        let blood = is_blood_moon(self.tick);
        let night_tick = self.tick % DAY_LEN;
        // Survival is solo-vs-the-night: with no rivals to soak the horde, the
        // dark presses you harder — denser packs, arriving faster.
        let survival = self.mode == Mode::Survival;

        // --- NIGHT SPAWN: scales with the night; a blood moon swarms from every
        //     edge, and each night leans harder on werewolves & vampires ---
        let mut interval = if blood { 18 } else { 40 };
        if survival {
            interval = interval * 2 / 3;
        }
        // difficulty: easy slows the cadence, hard quickens it
        match self.difficulty {
            0 => interval = interval * 3 / 2,
            2 => interval = interval * 2 / 3,
            _ => {}
        }
        interval = interval.max(6);
        if self.tick % interval == 0 {
            let horde = self.ents.iter().filter(|e| e.kind.is_monster()).count();
            let mut cap = 6 + 5 * alive.len() + 3 * night_no as usize;
            if survival {
                cap = cap * 2 + 12; // the whole horde has only you to hunt
            }
            if blood {
                cap *= 2;
            }
            // difficulty: easy thins the swarm, hard thickens it
            match self.difficulty {
                0 => cap = cap * 3 / 5,
                2 => cap = cap * 3 / 2 + 4,
                _ => {}
            }
            if horde < cap {
                let pack = if blood { self.rng.range_i32(3, 6) } else { self.rng.range_i32(2, 4) };
                // origin: out past a random settlement most nights; a map edge on blood moons
                let origin = if blood {
                    self.random_edge_tile()
                } else {
                    let pick = alive[self.rng.range_i32(0, alive.len() as i32 - 1) as usize];
                    self.ents.iter().find(|e| e.owner == pick && e.kind.is_building()).map(|e| e.tile())
                };
                if let Some(a) = origin {
                    for _ in 0..pack {
                        let ox = self.rng.range_i32(8, 20) * if self.rng.chance(1, 2) { 1 } else { -1 };
                        let oy = self.rng.range_i32(8, 20) * if self.rng.chance(1, 2) { 1 } else { -1 };
                        let st = Tp::new((a.x + ox).clamp(1, self.map.w - 2), (a.y + oy).clamp(1, self.map.h - 2));
                        if let Some(t) = self.find_free_tile_near(st, 8) {
                            // weighting inflates with the night → fewer shamblers, more beasts
                            let roll = self.rng.range_i32(0, 99) + night_no as i32 * 3 + if blood { 15 } else { 0 };
                            let kind = if roll < 55 {
                                Kind::Zombie
                            } else if roll < 85 {
                                Kind::Werewolf
                            } else {
                                Kind::Vampire
                            };
                            self.spawn_monster(kind, t);
                        }
                    }
                }
            }
        }

        // --- BOSSES walk mid-night: the Lich as a foretaste on the 2nd night, then
        //     THE WARLOCK on the **third night** (rising on the third day) ---
        if night_tick == 1200 && night_no + 1 >= 2 && !self.ents.iter().any(|e| e.kind == Kind::Lich) {
            if let Some(t) = self.random_edge_tile().and_then(|e| self.find_free_tile_near(e, 10)) {
                self.spawn_monster(Kind::Lich, t);
                self.push_chat(NEUTRAL, "A chill spreads — THE LICH rises. Hold your settlement.".into());
            }
        }
        if night_tick == 1500 && night_no + 1 >= 3 && !self.ents.iter().any(|e| e.kind == Kind::Warlock) {
            if let Some(t) = self.random_edge_tile().and_then(|e| self.find_free_tile_near(e, 10)) {
                self.spawn_monster(Kind::Warlock, t);
                self.push_chat(NEUTRAL, "THE WARLOCK WALKS. The puppeteer has come for you all — unite, or fall.".into());
            }
        }

        // --- the bosses raise the dead around themselves ---
        if self.tick % 35 == 0 && self.ents.iter().filter(|e| e.kind.is_monster()).count() < 70 {
            let casters: Vec<Tp> = self.ents.iter().filter(|e| e.kind.is_boss() && e.hp > 0).map(|e| e.tile()).collect();
            for c in casters {
                let st = Tp::new(c.x + self.rng.range_i32(-3, 3), c.y + self.rng.range_i32(-3, 3));
                if let Some(t) = self.find_free_tile_near(st, 4) {
                    self.spawn_monster(Kind::Zombie, t);
                }
            }
        }

        // --- THE CAPSTONE: once the nations wound the Warlock, the puppeteer
        //     seizes the machinery of war — animating corrupted hulks from the
        //     wreckage of the very battlefields it forced upon them ---
        let warlock = self
            .ents
            .iter()
            .find(|e| e.kind == Kind::Warlock && e.hp > 0)
            .map(|e| (e.tile(), e.hp, stats(Kind::Warlock).max_hp));
        if let Some((wt, whp, wmax)) = warlock {
            // "engaged" = the united nations have torn through 30% of its health
            let engaged = whp * 10 < wmax * 7;
            let hulks = self.ents.iter().filter(|e| e.kind == Kind::HellTank && e.hp > 0).count();
            if engaged && self.tick % 90 == 0 && hulks < 4 {
                let st = Tp::new(wt.x + self.rng.range_i32(-4, 4), wt.y + self.rng.range_i32(-4, 4));
                if let Some(t) = self.find_free_tile_near(st, 6) {
                    if hulks == 0 {
                        self.push_chat(NEUTRAL, "The Warlock seizes the machinery of war — IRON HULKS rise from the wreckage!".into());
                    }
                    self.spawn_monster(Kind::HellTank, t);
                }
            }
        }

        // --- SEEK: a monster with no prey in sight marches on the nearest base ---
        if self.tick % 50 == 0 {
            let targets: Vec<Tp> = self
                .ents
                .iter()
                .filter(|e| e.owner != NEUTRAL && e.kind.is_building() && e.hp > 0)
                .map(|e| e.tile())
                .collect();
            if !targets.is_empty() {
                for i in 0..self.ents.len_slots() {
                    let Some(mut e) = self.ents.take(i) else { continue };
                    if !e.kind.is_monster() || e.hp <= 0 || e.target.is_some() {
                        self.ents.put(i, e);
                        continue;
                    }
                    let mt = e.tile();
                    let nearest = targets.iter().copied().min_by_key(|t| {
                        let dx = (t.x - mt.x) as i64;
                        let dy = (t.y - mt.y) as i64;
                        dx * dx + dy * dy
                    });
                    if let Some(g) = nearest {
                        if e.goal != Some(g) {
                            e.goal = Some(g);
                            e.stuck = 0;
                            e.path = path::find(&self.map, mt, g, true, &[]);
                        }
                    }
                    self.ents.put(i, e);
                }
            }
        }
    }

    /// Loot collection: each dropped Essence mote is vacuumed up by the first
    /// owned unit (by entity index — deterministic) within `LOOT_PICKUP_R2`, or
    /// fades after `LOOT_TTL`. Uncollected motes linger so you can fight your way
    /// to them. Awards to the *collector's* owner, not the killer — loot is
    /// contestable, which is the point.
    fn sys_loot(&mut self) {
        if self.loot.is_empty() {
            return;
        }
        let motes = std::mem::take(&mut self.loot);
        let mut keep: Vec<Loot> = Vec::with_capacity(motes.len());
        for mote in motes {
            if self.tick.saturating_sub(mote.born) >= LOOT_TTL {
                continue; // winked out
            }
            let center = mote.tile.center();
            let mut taker: Option<Pid> = None;
            for e in self.ents.iter() {
                // owned, mobile, alive (NEUTRAL excludes monsters/the village)
                if e.owner == NEUTRAL || !e.kind.is_unit() || e.hp <= 0 {
                    continue;
                }
                if e.pos.dist2(center) <= LOOT_PICKUP_R2 {
                    taker = Some(e.owner);
                    break;
                }
            }
            match taker {
                Some(pid) => {
                    // raw meat needs a House (a kitchen) to cook — else leave it to rot
                    if mote.kind == LOOT_MEAT && !self.has_done_building(pid, Kind::House) {
                        if self.tick % 30 == 0 {
                            self.push_chat(NEUTRAL, "Build a House to cook the meat you've hunted.".into());
                        }
                        keep.push(mote);
                        continue;
                    }
                    if let Some(p) = self.players.get_mut(pid as usize) {
                        match mote.kind {
                            LOOT_BERRY | LOOT_MEAT => p.food = (p.food + mote.amount).min(p.food_cap.max(p.food)),
                            _ => p.essence = p.essence.saturating_add(mote.amount),
                        }
                    }
                    self.events.push(VisEvent::Pickup { at: center, amount: mote.amount, kind: mote.kind });
                }
                None => keep.push(mote),
            }
        }
        self.loot = keep;
    }

    /// Ore slowly regrows around "nodes" — the renewable economy that keeps a
    /// persistent world worth living in (and worth fighting over).
    fn sys_ore_regen(&mut self) {
        if self.tick % 30 != 0 {
            return;
        }
        for ni in 0..self.map.nodes.len() {
            let node = self.map.nodes[ni];
            let dx = self.rng.range_i32(-3, 3);
            let dy = self.rng.range_i32(-3, 3);
            let t = Tp::new(node.x + dx, node.y + dy);
            if self.map.in_bounds(t)
                && self.map.terrain_at(t).ground()
                && self.map.blocked_by(t) == 0
            {
                let cur = self.map.ore_at(t);
                self.map.set_ore(t, (cur + 14).min(MAX_ORE));
            }
        }
    }

    /// Farms trickle credits; houses slowly heal your people nearby.
    fn sys_support(&mut self) {
        if self.tick % FARM_PERIOD == 0 {
            let mut farm_income: Vec<u32> = vec![0; self.players.len()];
            for e in self.ents.iter() {
                if e.done && (e.owner as usize) < farm_income.len() {
                    let inc = crate::stats::income_of(e.kind);
                    if inc > 0 {
                        farm_income[e.owner as usize] += inc;
                    }
                }
            }
            for (pid, inc) in farm_income.iter().enumerate() {
                self.players[pid].credits += inc;
                self.players[pid].away.credits_gained = self.players[pid].away.credits_gained.saturating_add(*inc);
            }
        }
        // Healing/repair runs EVERY tick so a unit parked at a dedicated building
        // restores in ~4 seconds — fast and obvious, not a 40-second trickle.
        // Repair Depots fix vehicles, Med Bays patch infantry (both ~maxhp/40 per
        // tick); Houses give nearby infantry only a slow ambient mend.
        let depots: Vec<(Pid, Fp)> = self.support_centers(Kind::RepairDepot);
        let bays: Vec<(Pid, Fp)> = self.support_centers(Kind::MedBay);
        let houses: Vec<(Pid, Fp)> = self.support_centers(Kind::House);
        if depots.is_empty() && bays.is_empty() && houses.is_empty() {
            return;
        }
        let dr2 = (4 * FX as i64) * (4 * FX as i64); // depot / house reach
        let br2 = (5 * FX as i64) * (5 * FX as i64); // med bay reach (a touch wider)
        let near = |list: &[(Pid, Fp)], owner: Pid, pos: Fp, r2: i64| list.iter().any(|(o, c)| *o == owner && pos.dist2(*c) <= r2);
        for i in 0..self.ents.len_slots() {
            let Some(e) = self.ents.slots[i].as_mut() else { continue };
            if !e.kind.is_unit() || e.hp <= 0 {
                continue;
            }
            let maxhp = stats(e.kind).max_hp;
            if e.hp >= maxhp {
                continue;
            }
            let fast = (maxhp / 40).max(3); // ~4s from a wreck to full
            let heal = if e.kind.is_infantry() {
                if near(&bays, e.owner, e.pos, br2) {
                    fast
                } else if near(&houses, e.owner, e.pos, dr2) {
                    (maxhp / 120).max(1) // gentle ambient town-heal
                } else {
                    0
                }
            } else if near(&depots, e.owner, e.pos, dr2) {
                fast // vehicles (incl. harvesters) at a Repair Depot
            } else {
                0
            };
            if heal > 0 {
                e.hp = (e.hp + heal).min(maxhp);
            }
        }
    }

    /// Centres of a player's finished support buildings of `kind`.
    fn support_centers(&self, kind: Kind) -> Vec<(Pid, Fp)> {
        self.ents
            .iter()
            .filter(|e| e.kind == kind && e.done)
            .map(|e| (e.owner, e.center()))
            .collect()
    }

    fn sys_recompute(&mut self) {
        let n = self.players.len();
        let mut made = vec![0i32; n];
        let mut used = vec![0i32; n];
        let mut houses = vec![0u32; n];
        let mut ships = vec![0u32; n];
        let mut farms = vec![0u32; n];
        let mut silos = vec![0u32; n]; // food silos
        let mut units = vec![0u32; n];
        let mut food_need = vec![0u32; n];
        let mut has_any = vec![false; n];
        for e in self.ents.iter() {
            let pid = e.owner as usize;
            if pid >= n {
                continue;
            }
            has_any[pid] = true;
            if e.kind.is_building() && e.done {
                let p = stats(e.kind).power;
                if p > 0 {
                    made[pid] += p;
                } else {
                    used[pid] += -p;
                }
                match e.kind {
                    Kind::House => houses[pid] += 1,
                    Kind::Starship => ships[pid] += 1,
                    Kind::Farm => farms[pid] += 1,
                    Kind::FoodSilo => silos[pid] += 1,
                    _ => {}
                }
            }
            if e.kind.is_unit() {
                units[pid] += 1;
                food_need[pid] += crate::stats::food_upkeep(e.kind);
            }
        }
        // food economy: cap from Houses + Food Silos; every FOOD_PERIOD the farms
        // grow food and the population eats. Run out and the town is "starving".
        let food_tick = self.tick % crate::stats::FOOD_PERIOD == 0;
        let mut starving_pids: Vec<Pid> = Vec::new();
        let mode = self.mode;
        let mut newly_defeated: Vec<String> = Vec::new();
        for (pid, p) in self.players.iter_mut().enumerate() {
            p.power_made = made[pid];
            p.power_used = used[pid];
            p.unit_cap = BASE_UNIT_CAP + houses[pid] * CAP_PER_HOUSE + ships[pid] * crate::stats::STARSHIP_CAP;
            p.unit_count = units[pid];
            p.food_cap = crate::stats::BASE_FOOD_CAP
                + houses[pid] * crate::stats::FOOD_PER_HOUSE
                + silos[pid] * crate::stats::FOOD_PER_SILO;
            if food_tick && p.joined && !p.defeated {
                // farms grow food (up to the cap), then the army eats
                let grown = farms[pid] * crate::stats::FARM_FOOD;
                p.food = (p.food + grown).min(p.food_cap.max(p.food));
                let demand = food_need[pid];
                if p.food >= demand {
                    p.food -= demand;
                    p.starving = false;
                } else {
                    p.food = 0;
                    if demand > 0 {
                        p.starving = true;
                        starving_pids.push(pid as Pid);
                    } else {
                        p.starving = false;
                    }
                }
            }
            if mode.has_defeat() && p.joined && !p.defeated && !has_any[pid] {
                p.defeated = true;
                newly_defeated.push(p.name.clone());
            }
        }
        // starvation bites: hungry troops slowly weaken (a gentle drain, not a
        // death-spiral) so neglecting food has teeth without instantly wiping you.
        if food_tick && !starving_pids.is_empty() {
            for i in 0..self.ents.len_slots() {
                let Some(e) = self.ents.slots[i].as_mut() else { continue };
                if !e.kind.is_unit() || e.hp <= 1 || !starving_pids.contains(&e.owner) {
                    continue;
                }
                if crate::stats::food_upkeep(e.kind) > 0 {
                    let floor = (stats(e.kind).max_hp / 3).max(1); // won't starve below 1/3 HP
                    if e.hp > floor {
                        e.hp = (e.hp - (stats(e.kind).max_hp / 60).max(1)).max(floor);
                    }
                }
            }
        }
        for name in newly_defeated {
            self.push_chat(NEUTRAL, format!("{} has been eliminated", name));
        }
    }

    fn sys_fog(&mut self) {
        let w = self.map.w;
        let h = self.map.h;
        // visible decays to explored
        for p in self.players.iter_mut() {
            if !p.joined {
                continue;
            }
            for f in p.fog.iter_mut() {
                if *f == 2 {
                    *f = 1;
                }
            }
        }
        // stamp sight circles
        let stamps: Vec<(Pid, Tp, i32)> = self
            .ents
            .iter()
            .filter(|e| (e.owner as usize) < self.players.len() && e.hp > 0)
            .map(|e| {
                let (fw, fh) = e.foot();
                let t = e.tile();
                (e.owner, Tp::new(t.x + fw / 2, t.y + fh / 2), stats(e.kind).sight)
            })
            .collect();
        for (pid, c, s) in stamps {
            let p = &mut self.players[pid as usize];
            let s2 = s * s;
            for dy in -s..=s {
                let y = c.y + dy;
                if y < 0 || y >= h {
                    continue;
                }
                for dx in -s..=s {
                    let x = c.x + dx;
                    if x < 0 || x >= w {
                        continue;
                    }
                    if dx * dx + dy * dy <= s2 {
                        p.fog[(y * w + x) as usize] = 2;
                    }
                }
            }
        }
        // shared vision: mutual allies merge each other's sight. Gated on an
        // alliance actually existing, so it's free in the common case.
        let amask: Vec<u8> = self.players.iter().map(|p| p.ally_mask).collect();
        let n = amask.len();
        let any = (0..n).any(|a| {
            (0..n).any(|b| a != b && (amask[a] >> b) & 1 == 1 && (amask[b] >> a) & 1 == 1)
        });
        if any {
            let snap: Vec<Vec<u8>> = self.players.iter().map(|p| p.fog.clone()).collect();
            for a in 0..n {
                if !self.players[a].joined {
                    continue;
                }
                for b in 0..n {
                    if a == b || (amask[a] >> b) & 1 != 1 || (amask[b] >> a) & 1 != 1 {
                        continue;
                    }
                    let bfog = &snap[b];
                    let afog = &mut self.players[a].fog;
                    let len = afog.len().min(bfog.len());
                    for i in 0..len {
                        if bfog[i] == 2 {
                            afog[i] = 2;
                        } else if bfog[i] == 1 && afog[i] == 0 {
                            afog[i] = 1;
                        }
                    }
                }
            }
        }
    }

    // =====================================================================
    // Persistence + integrity
    // =====================================================================

    pub fn save_bytes(&self) -> Vec<u8> {
        let mut w = W::new();
        w.u32(SAVE_MAGIC);
        w.u16(SAVE_VERSION);
        w.u8(self.mode as u8);
        w.u8(self.difficulty);
        w.u8(self.realm as u8);
        w.u32(self.tick);
        w.u32(self.peace_until);
        w.u64(self.rng.state);
        w.u64(self.rng.inc);
        self.map.ser(&mut w);
        w.u8(self.players.len() as u8);
        for p in &self.players {
            w.bool(p.joined);
            w.str(&p.name);
            w.u8(p.color);
            w.arr32(&p.key);
            w.u32(p.credits);
            w.u32(p.wood);
            w.u32(p.stone);
            w.u32(p.essence);
            w.u32(p.food);
            w.bool(p.defeated);
            w.i32(p.power_made);
            w.i32(p.power_used);
            w.u32(p.unit_cap);
            w.u32(p.unit_count);
            w.u32(p.food_cap);
            w.bool(p.starving);
            w.u32(p.away.from_tick);
            w.u32(p.away.credits_gained);
            w.u32(p.away.credits_lost);
            w.u16(p.away.buildings_lost);
            w.u16(p.away.units_lost);
            w.u16(p.away.attacks);
            w.u8(p.away.last_foe);
            w.u8(p.ally_mask);
            // fog RLE
            let mut runs: Vec<(u8, u32)> = Vec::new();
            let mut i = 0usize;
            while i < p.fog.len() {
                let v = p.fog[i];
                let mut len = 1u32;
                while i + (len as usize) < p.fog.len() && p.fog[i + len as usize] == v {
                    len += 1;
                }
                runs.push((v, len));
                i += len as usize;
            }
            w.u32(runs.len() as u32);
            for (v, len) in runs {
                w.u8(v);
                w.u32(len);
            }
        }
        self.ents.ser(&mut w);
        w.u16(self.chat.len() as u16);
        for (t, pid, text) in &self.chat {
            w.u32(*t);
            w.u8(*pid);
            w.str(text);
        }
        w.u16(self.loot.len() as u16);
        for m in &self.loot {
            w.i32(m.tile.x);
            w.i32(m.tile.y);
            w.u32(m.amount);
            w.u8(m.kind);
            w.u32(m.born);
        }
        w.buf
    }

    pub fn load_bytes(bytes: &[u8]) -> DResult<World> {
        let mut r = R::new(bytes);
        if r.u32()? != SAVE_MAGIC {
            return Err(DecodeErr);
        }
        // accept the current format, and v11 too: a v11 save predates the
        // netherealm, so it's simply an Overworld game (it migrates to v12 on the
        // next save). Anything older is rejected.
        let ver = r.u16()?;
        if ver != SAVE_VERSION && ver != 11 {
            return Err(DecodeErr);
        }
        let mode = Mode::from_u8(r.u8()?);
        let difficulty = r.u8()?;
        let realm = if ver >= 12 { Realm::from_u8(r.u8()?) } else { Realm::Overworld };
        let tick = r.u32()?;
        let peace_until = r.u32()?;
        let state = r.u64()?;
        let inc = r.u64()?;
        let map = Map::de(&mut r)?;
        let tiles = (map.w * map.h) as usize;
        let np = r.u8()? as usize;
        let mut players = Vec::with_capacity(np);
        for _ in 0..np {
            let mut p = Player::empty(tiles);
            p.joined = r.bool()?;
            p.name = r.str()?;
            p.color = r.u8()?;
            p.key = r.arr32()?;
            p.credits = r.u32()?;
            p.wood = r.u32()?;
            p.stone = r.u32()?;
            p.essence = r.u32()?;
            p.food = r.u32()?;
            p.defeated = r.bool()?;
            p.power_made = r.i32()?;
            p.power_used = r.i32()?;
            p.unit_cap = r.u32()?;
            p.unit_count = r.u32()?;
            p.food_cap = r.u32()?;
            p.starving = r.bool()?;
            p.away = AwayLog {
                from_tick: r.u32()?,
                credits_gained: r.u32()?,
                credits_lost: r.u32()?,
                buildings_lost: r.u16()?,
                units_lost: r.u16()?,
                attacks: r.u16()?,
                last_foe: r.u8()?,
            };
            p.ally_mask = r.u8()?;
            let nruns = r.u32()? as usize;
            let mut pos = 0usize;
            for _ in 0..nruns {
                let v = r.u8()?;
                let len = r.u32()? as usize;
                if pos + len > p.fog.len() {
                    return Err(DecodeErr);
                }
                for j in 0..len {
                    p.fog[pos + j] = v;
                }
                pos += len;
            }
            players.push(p);
        }
        let ents = Arena::de(&mut r)?;
        let nchat = r.u16()? as usize;
        let mut chat = Vec::with_capacity(nchat);
        for _ in 0..nchat {
            let t = r.u32()?;
            let pid = r.u8()?;
            let text = r.str()?;
            chat.push((t, pid, text));
        }
        let nloot = r.u16()? as usize;
        let mut loot = Vec::with_capacity(nloot);
        for _ in 0..nloot {
            let x = r.i32()?;
            let y = r.i32()?;
            let amount = r.u32()?;
            let kind = r.u8()?;
            let born = r.u32()?;
            loot.push(Loot { tile: Tp::new(x, y), amount, kind, born });
        }
        let mut world = World {
            tick,
            mode,
            map,
            ents,
            players,
            rng: Pcg32 { state, inc },
            chat,
            peace_until,
            loot,
            difficulty,
            realm,
            descent_at: 0,
            events: Vec::new(),
        };
        // the block grid is derived state: rebuild it from building entities
        for i in 0..world.ents.len_slots() {
            let (tile, foot, idx1) = match world.ents.slots[i].as_ref() {
                Some(e) if e.kind.is_building() => (e.tile(), e.foot(), e.id.idx + 1),
                _ => continue,
            };
            world.map.stamp_block(tile, foot, idx1);
        }
        Ok(world)
    }

    /// World checksum for desync detection. Identical on every honest peer.
    pub fn hash(&self) -> u64 {
        fnv64(&self.save_bytes())
    }
}

#[cfg(test)]
mod nether_tests {
    use super::*;
    use crate::mapgen;

    fn seeded() -> World {
        let mut w = mapgen::verdant_divide(42, Mode::Skirmish);
        w.step(&[(0, Command::Join { name: "Ada".into(), color: 0, key: [1; 32] })]);
        for _ in 0..20 {
            w.step(&[]);
        }
        w
    }

    // The one-way descent must be bit-deterministic (same map, same carryover on
    // every peer) and survive a save/reload — the realm is part of world state.
    #[test]
    fn descent_is_deterministic_and_loadable() {
        let mut a = seeded();
        let mut b = seeded();
        assert_eq!(a.hash(), b.hash(), "identical setup should match");
        assert!(a.ents.iter().any(|e| e.kind.is_unit() && e.owner == 0), "player should have units to carry down");

        a.descend_to_nether();
        b.descend_to_nether();
        assert_eq!(a.realm, Realm::Nether);
        assert_eq!(a.hash(), b.hash(), "the descent itself must be deterministic");
        assert!(a.ents.iter().any(|e| e.kind.is_unit() && e.owner == 0), "the army should have descended");

        // the nether horde (and everything) runs identically tick for tick
        for t in 0..400u32 {
            a.step(&[]);
            b.step(&[]);
            assert_eq!(a.hash(), b.hash(), "nether desynced at +{t}");
        }
        assert!(a.ents.iter().any(|e| e.kind == Kind::Demon), "the netherealm should be spawning demons");

        // save mid-nether, reload, and continue on the exact same hash
        let snap = a.save_bytes();
        let mut c = World::load_bytes(&snap).expect("nether save round-trips");
        assert_eq!(c.realm, Realm::Nether, "realm must persist across save/load");
        assert_eq!(a.hash(), c.hash(), "reloaded nether must match");
        for _ in 0..120 {
            a.step(&[]);
            c.step(&[]);
            assert_eq!(a.hash(), c.hash());
        }
    }
}
