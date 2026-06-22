//! entity.rs — fat entity struct + generational arena.
//!
//! Deliberately NOT a full ECS yet: at this scale a single struct with stable
//! Vec-index iteration is simpler, cache-friendly, and — critically — has a
//! trivially deterministic iteration order. The arena API is shaped so the
//! interior can be swapped for a real archetype ECS later without touching systems.

use crate::ser::{DResult, R, W};
use crate::stats::{stats, Kind};
use crate::{Fp, Pid, Tp};

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Eid {
    pub idx: u32,
    pub gen: u32,
}

impl Eid {
    pub const NONE: Eid = Eid { idx: u32::MAX, gen: 0 };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum HarvestPhase {
    Idle = 0,
    ToOre = 1,
    Mining = 2,
    ToRefinery = 3,
    Unloading = 4,
}

impl HarvestPhase {
    fn from_u8(v: u8) -> HarvestPhase {
        match v {
            1 => HarvestPhase::ToOre,
            2 => HarvestPhase::Mining,
            3 => HarvestPhase::ToRefinery,
            4 => HarvestPhase::Unloading,
            _ => HarvestPhase::Idle,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ent {
    pub id: Eid,
    pub owner: Pid,
    pub kind: Kind,
    /// Units: center position. Buildings: top-left tile center is `pos.tile()`.
    pub pos: Fp,
    pub face: u8,
    pub hp: i32,

    // -- movement --
    /// Path waypoints stored reversed (pop() yields the next tile).
    pub path: Vec<Tp>,
    pub goal: Option<Tp>,
    pub follow: Option<Eid>, // moving toward an entity (attack/capture chase)
    pub aggressive: bool,    // attack-move: engage targets of opportunity

    // -- combat --
    pub target: Option<Eid>,
    pub cooldown: u16,
    pub scan_in: u16,
    /// repath attempts since last goal progress (anti-thrash)
    pub stuck: u8,
    /// cadence timer for re-pathing toward a moving follow target
    pub follow_t: u16,

    // -- economy (harvester) --
    pub cargo: u16,
    /// what's in the hopper: 0 = ore (→credits), 1 = wood, 2 = stone
    pub cargo_kind: u8,
    pub hphase: HarvestPhase,
    pub ore_tile: Option<Tp>,
    pub work_t: u16, // generic timer (mining chunks / unloading)
    /// ticks the harvester has made no progress while travelling — a
    /// watchdog so it can never wedge permanently (resets on progress)
    pub stall: u16,

    // -- production (buildings) --
    pub queue: Vec<Kind>,
    pub prod_progress: u32,
    pub rally: Option<Tp>,

    // -- construction (buildings under construction) --
    pub done: bool,
    pub con_progress: u32,

    /// Essence-smoke exposure (netherealm). Builds while in a cloud, decays out of
    /// it; past a threshold the unit is a doomed "sleeper", then it turns. Ephemeral
    /// (not serialised/hashed) — a transient like a timer, reset on load.
    pub corrupt: u16,
}

impl Ent {
    pub fn new(id: Eid, owner: Pid, kind: Kind, pos: Fp) -> Ent {
        let st = stats(kind);
        Ent {
            id,
            owner,
            kind,
            pos,
            face: 8,
            hp: st.max_hp,
            path: Vec::new(),
            goal: None,
            follow: None,
            aggressive: false,
            target: None,
            cooldown: 0,
            scan_in: 0,
            stuck: 0,
            follow_t: 0,
            cargo: 0,
            cargo_kind: 0,
            hphase: HarvestPhase::Idle,
            ore_tile: None,
            work_t: 0,
            stall: 0,
            queue: Vec::new(),
            prod_progress: 0,
            rally: None,
            done: true,
            con_progress: 0,
            corrupt: 0,
        }
    }

    pub fn tile(&self) -> Tp {
        self.pos.tile()
    }
    pub fn foot(&self) -> (i32, i32) {
        stats(self.kind).footprint
    }
    /// Center of a building's footprint in fixed-point space.
    pub fn center(&self) -> Fp {
        if self.kind.is_building() {
            let t = self.tile();
            let (fw, fh) = self.foot();
            Fp { x: t.x * 256 + fw * 128, y: t.y * 256 + fh * 128 }
        } else {
            self.pos
        }
    }

    pub fn ser(&self, w: &mut W) {
        w.u32(self.id.idx);
        w.u32(self.id.gen);
        w.u8(self.owner);
        w.u8(self.kind as u8);
        w.i32(self.pos.x);
        w.i32(self.pos.y);
        w.u8(self.face);
        w.i32(self.hp);
        w.u16(self.path.len() as u16);
        for t in &self.path {
            w.i32(t.x);
            w.i32(t.y);
        }
        ser_opt_tp(w, self.goal);
        ser_opt_eid(w, self.follow);
        w.bool(self.aggressive);
        ser_opt_eid(w, self.target);
        w.u16(self.cooldown);
        w.u16(self.scan_in);
        w.u8(self.stuck);
        w.u16(self.follow_t);
        w.u16(self.cargo);
        w.u8(self.cargo_kind);
        w.u8(self.hphase as u8);
        ser_opt_tp(w, self.ore_tile);
        w.u16(self.work_t);
        w.u16(self.stall);
        w.u8(self.queue.len() as u8);
        for k in &self.queue {
            w.u8(*k as u8);
        }
        w.u32(self.prod_progress);
        ser_opt_tp(w, self.rally);
        w.bool(self.done);
        w.u32(self.con_progress);
    }

    pub fn de(r: &mut R) -> DResult<Ent> {
        let idx = r.u32()?;
        let gen = r.u32()?;
        let owner = r.u8()?;
        let kind = Kind::from_u8(r.u8()?).ok_or(crate::ser::DecodeErr)?;
        let px = r.i32()?;
        let py = r.i32()?;
        let mut e = Ent::new(Eid { idx, gen }, owner, kind, Fp { x: px, y: py });
        e.face = r.u8()?;
        e.hp = r.i32()?;
        let np = r.u16()? as usize;
        for _ in 0..np {
            let x = r.i32()?;
            let y = r.i32()?;
            e.path.push(Tp::new(x, y));
        }
        e.goal = de_opt_tp(r)?;
        e.follow = de_opt_eid(r)?;
        e.aggressive = r.bool()?;
        e.target = de_opt_eid(r)?;
        e.cooldown = r.u16()?;
        e.scan_in = r.u16()?;
        e.stuck = r.u8()?;
        e.follow_t = r.u16()?;
        e.cargo = r.u16()?;
        e.cargo_kind = r.u8()?;
        e.hphase = HarvestPhase::from_u8(r.u8()?);
        e.ore_tile = de_opt_tp(r)?;
        e.work_t = r.u16()?;
        e.stall = r.u16()?;
        let nq = r.u8()? as usize;
        e.queue.clear();
        for _ in 0..nq {
            if let Some(k) = Kind::from_u8(r.u8()?) {
                e.queue.push(k);
            }
        }
        e.prod_progress = r.u32()?;
        e.rally = de_opt_tp(r)?;
        e.done = r.bool()?;
        e.con_progress = r.u32()?;
        Ok(e)
    }
}

fn ser_opt_tp(w: &mut W, v: Option<Tp>) {
    match v {
        Some(t) => {
            w.bool(true);
            w.i32(t.x);
            w.i32(t.y);
        }
        None => w.bool(false),
    }
}
fn de_opt_tp(r: &mut R) -> DResult<Option<Tp>> {
    if r.bool()? {
        let x = r.i32()?;
        let y = r.i32()?;
        Ok(Some(Tp::new(x, y)))
    } else {
        Ok(None)
    }
}
fn ser_opt_eid(w: &mut W, v: Option<Eid>) {
    match v {
        Some(e) => {
            w.bool(true);
            w.u32(e.idx);
            w.u32(e.gen);
        }
        None => w.bool(false),
    }
}
fn de_opt_eid(r: &mut R) -> DResult<Option<Eid>> {
    if r.bool()? {
        let idx = r.u32()?;
        let gen = r.u32()?;
        Ok(Some(Eid { idx, gen }))
    } else {
        Ok(None)
    }
}

/// Generational arena. Slot indices are stable, generations detect stale Eids.
/// Iteration is always ascending slot order => deterministic.
#[derive(Clone, Default)]
pub struct Arena {
    pub slots: Vec<Option<Ent>>,
    pub gens: Vec<u32>,
    pub free: Vec<u32>, // kept sorted descending so we always reuse the LOWEST index (determinism + compactness)
}

impl Arena {
    pub fn new() -> Arena {
        Arena::default()
    }

    pub fn spawn(&mut self, owner: Pid, kind: Kind, pos: Fp) -> Eid {
        let idx = match self.free.pop() {
            Some(i) => i,
            None => {
                self.slots.push(None);
                self.gens.push(0);
                (self.slots.len() - 1) as u32
            }
        };
        let gen = self.gens[idx as usize];
        let id = Eid { idx, gen };
        self.slots[idx as usize] = Some(Ent::new(id, owner, kind, pos));
        id
    }

    pub fn despawn(&mut self, id: Eid) {
        let i = id.idx as usize;
        if i < self.slots.len() && self.gens[i] == id.gen && self.slots[i].is_some() {
            self.slots[i] = None;
            self.gens[i] = self.gens[i].wrapping_add(1);
            // insert keeping descending order so pop() yields lowest index
            let pos = self.free.partition_point(|&x| x > id.idx);
            self.free.insert(pos, id.idx);
        }
    }

    pub fn get(&self, id: Eid) -> Option<&Ent> {
        let i = id.idx as usize;
        if i < self.slots.len() && self.gens[i] == id.gen {
            self.slots[i].as_ref()
        } else {
            None
        }
    }
    pub fn get_mut(&mut self, id: Eid) -> Option<&mut Ent> {
        let i = id.idx as usize;
        if i < self.slots.len() && self.gens[i] == id.gen {
            self.slots[i].as_mut()
        } else {
            None
        }
    }
    /// Take an entity out for processing (put it back with `put`). Lets a system
    /// mutate one entity while reading the rest of the arena.
    pub fn take(&mut self, idx: usize) -> Option<Ent> {
        self.slots.get_mut(idx).and_then(|s| s.take())
    }
    pub fn put(&mut self, idx: usize, e: Ent) {
        self.slots[idx] = Some(e);
    }
    pub fn len_slots(&self) -> usize {
        self.slots.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Ent> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }
    pub fn count(&self) -> usize {
        self.iter().count()
    }

    pub fn ser(&self, w: &mut W) {
        w.u32(self.slots.len() as u32);
        for (i, s) in self.slots.iter().enumerate() {
            match s {
                Some(e) => {
                    w.bool(true);
                    e.ser(w);
                }
                None => {
                    w.bool(false);
                    w.u32(self.gens[i]);
                }
            }
        }
    }

    pub fn de(r: &mut R) -> DResult<Arena> {
        let n = r.u32()? as usize;
        if n > 200_000 {
            return Err(crate::ser::DecodeErr);
        }
        let mut a = Arena::new();
        for i in 0..n {
            if r.bool()? {
                let e = Ent::de(r)?;
                a.gens.push(e.id.gen);
                a.slots.push(Some(e));
            } else {
                let g = r.u32()?;
                a.gens.push(g);
                a.slots.push(None);
                a.free.push(i as u32);
            }
        }
        // free list: descending so lowest pops first
        a.free.sort_unstable_by(|x, y| y.cmp(x));
        Ok(a)
    }
}
