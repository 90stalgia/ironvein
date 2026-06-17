//! map.rs — the tile world. Terrain layer + ore layer + footprint-block layer.

use crate::ser::{DResult, R, W};
use crate::{Tp, NEUTRAL, Pid};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Terrain {
    Grass = 0,
    Dirt = 1,
    Road = 2,
    Water = 3,
    Bridge = 4,
    Rock = 5,
    Tree = 6,
    Sand = 7,
    Snow = 8,
    Ice = 9,
    Marsh = 10,
    Mountain = 11,
}

impl Terrain {
    pub fn from_u8(v: u8) -> Terrain {
        match v {
            1 => Terrain::Dirt,
            2 => Terrain::Road,
            3 => Terrain::Water,
            4 => Terrain::Bridge,
            5 => Terrain::Rock,
            6 => Terrain::Tree,
            7 => Terrain::Sand,
            8 => Terrain::Snow,
            9 => Terrain::Ice,
            10 => Terrain::Marsh,
            11 => Terrain::Mountain,
            _ => Terrain::Grass,
        }
    }
    pub fn passable(self) -> bool {
        matches!(
            self,
            Terrain::Grass
                | Terrain::Dirt
                | Terrain::Road
                | Terrain::Bridge
                | Terrain::Sand
                | Terrain::Snow
                | Terrain::Ice
                | Terrain::Marsh
        )
    }
    /// Can a building footprint cover this tile?
    pub fn buildable(self) -> bool {
        matches!(self, Terrain::Grass | Terrain::Dirt | Terrain::Road | Terrain::Sand | Terrain::Snow)
    }
    /// Natural ground that can hold ore / host a regrowth node.
    pub fn ground(self) -> bool {
        matches!(self, Terrain::Grass | Terrain::Dirt | Terrain::Sand | Terrain::Snow)
    }
    /// Movement-speed multiplier in percent (100 = normal). Roads are fast,
    /// snow/marsh slow you down, ice is a touch slippery-quick.
    pub fn speed_pct(self) -> i32 {
        match self {
            Terrain::Road => 125,
            Terrain::Ice => 115,
            Terrain::Snow => 75,
            Terrain::Marsh => 50,
            _ => 100,
        }
    }
}

pub const MAX_ORE: u16 = 700;

#[derive(Clone)]
pub struct Map {
    pub w: i32,
    pub h: i32,
    pub terrain: Vec<u8>,
    /// Ore amount per tile (only meaningful on Grass/Dirt).
    pub ore: Vec<u16>,
    /// Tiles that regrow ore around them ("ore nodes" — the renewable economy).
    pub nodes: Vec<Tp>,
    /// Footprint blocking: 0 = free, else (entity arena index + 1).
    pub block: Vec<u32>,
    /// Designated settlement sites; players spawn at the first unclaimed one.
    pub spawns: Vec<Tp>,
    pub spawn_used: Vec<Pid>, // parallel to spawns; NEUTRAL = free
}

impl Map {
    pub fn new(w: i32, h: i32) -> Map {
        let n = (w * h) as usize;
        Map {
            w,
            h,
            terrain: vec![0; n],
            ore: vec![0; n],
            nodes: Vec::new(),
            block: vec![0; n],
            spawns: Vec::new(),
            spawn_used: Vec::new(),
        }
    }

    #[inline]
    pub fn in_bounds(&self, t: Tp) -> bool {
        t.x >= 0 && t.y >= 0 && t.x < self.w && t.y < self.h
    }
    #[inline]
    pub fn idx(&self, t: Tp) -> usize {
        (t.y * self.w + t.x) as usize
    }
    pub fn terrain_at(&self, t: Tp) -> Terrain {
        if !self.in_bounds(t) {
            return Terrain::Rock;
        }
        Terrain::from_u8(self.terrain[self.idx(t)])
    }
    pub fn set_terrain(&mut self, t: Tp, ter: Terrain) {
        if self.in_bounds(t) {
            let i = self.idx(t);
            self.terrain[i] = ter as u8;
        }
    }
    pub fn ore_at(&self, t: Tp) -> u16 {
        if !self.in_bounds(t) {
            return 0;
        }
        self.ore[self.idx(t)]
    }
    pub fn set_ore(&mut self, t: Tp, v: u16) {
        if self.in_bounds(t) {
            let i = self.idx(t);
            self.ore[i] = v.min(MAX_ORE);
        }
    }
    /// What resource a tile holds: 0 = ore (→credits) on open ground, 1 = wood
    /// from a tree, 2 = stone from rock/mountain. None if it's not harvestable.
    /// The AMOUNT is `ore_at`; the TYPE is implied by the terrain.
    pub fn resource_kind(&self, t: Tp) -> Option<u8> {
        if self.ore_at(t) == 0 {
            return None;
        }
        match self.terrain_at(t) {
            Terrain::Grass | Terrain::Dirt | Terrain::Sand | Terrain::Snow => Some(0),
            Terrain::Tree => Some(1),
            Terrain::Rock | Terrain::Mountain => Some(2),
            _ => None,
        }
    }
    /// A chopped-out tree or mined-out rock collapses into open ground —
    /// harvesting literally reshapes the map.
    pub fn clear_resource(&mut self, t: Tp) {
        match self.terrain_at(t) {
            Terrain::Tree => self.set_terrain(t, Terrain::Grass),
            Terrain::Rock | Terrain::Mountain => self.set_terrain(t, Terrain::Dirt),
            _ => {}
        }
    }
    pub fn blocked_by(&self, t: Tp) -> u32 {
        if !self.in_bounds(t) {
            return u32::MAX;
        }
        self.block[self.idx(t)]
    }
    /// Passable for unit movement: terrain passable AND no building footprint.
    pub fn walkable(&self, t: Tp) -> bool {
        self.in_bounds(t) && self.terrain_at(t).passable() && self.blocked_by(t) == 0
    }
    pub fn stamp_block(&mut self, at: Tp, foot: (i32, i32), ent_idx_plus1: u32) {
        for dy in 0..foot.1 {
            for dx in 0..foot.0 {
                let t = Tp::new(at.x + dx, at.y + dy);
                if self.in_bounds(t) {
                    let i = self.idx(t);
                    self.block[i] = ent_idx_plus1;
                }
            }
        }
    }
    pub fn clear_block(&mut self, at: Tp, foot: (i32, i32)) {
        self.stamp_block(at, foot, 0);
    }

    pub fn ser(&self, w: &mut W) {
        w.i32(self.w);
        w.i32(self.h);
        // terrain RLE: maps are mostly uniform runs, keep snapshots small for late-join.
        let mut i = 0usize;
        let n = self.terrain.len();
        let mut runs: Vec<(u8, u32)> = Vec::new();
        while i < n {
            let v = self.terrain[i];
            let mut len = 1u32;
            while i + (len as usize) < n && self.terrain[i + len as usize] == v {
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
        // ore: sparse (tile index, amount)
        let mut ore_tiles: Vec<(u32, u16)> = Vec::new();
        for (i, &o) in self.ore.iter().enumerate() {
            if o > 0 {
                ore_tiles.push((i as u32, o));
            }
        }
        w.u32(ore_tiles.len() as u32);
        for (i, o) in ore_tiles {
            w.u32(i);
            w.u16(o);
        }
        w.u16(self.nodes.len() as u16);
        for t in &self.nodes {
            w.i32(t.x);
            w.i32(t.y);
        }
        w.u16(self.spawns.len() as u16);
        for (s, used) in self.spawns.iter().zip(self.spawn_used.iter()) {
            w.i32(s.x);
            w.i32(s.y);
            w.u8(*used);
        }
        // block grid is NOT serialized; it is recomputed from entities on load.
    }

    pub fn de(r: &mut R) -> DResult<Map> {
        let w = r.i32()?;
        let h = r.i32()?;
        if w <= 0 || h <= 0 || w > 1024 || h > 1024 {
            return Err(crate::ser::DecodeErr);
        }
        let mut m = Map::new(w, h);
        let nruns = r.u32()? as usize;
        let mut pos = 0usize;
        for _ in 0..nruns {
            let v = r.u8()?;
            let len = r.u32()? as usize;
            if pos + len > m.terrain.len() {
                return Err(crate::ser::DecodeErr);
            }
            for j in 0..len {
                m.terrain[pos + j] = v;
            }
            pos += len;
        }
        let nore = r.u32()? as usize;
        for _ in 0..nore {
            let i = r.u32()? as usize;
            let o = r.u16()?;
            if i < m.ore.len() {
                m.ore[i] = o;
            }
        }
        let nn = r.u16()? as usize;
        for _ in 0..nn {
            let x = r.i32()?;
            let y = r.i32()?;
            m.nodes.push(Tp::new(x, y));
        }
        let ns = r.u16()? as usize;
        for _ in 0..ns {
            let x = r.i32()?;
            let y = r.i32()?;
            let u = r.u8()?;
            m.spawns.push(Tp::new(x, y));
            m.spawn_used.push(u);
        }
        Ok(m)
    }

    pub fn free_spawn(&self) -> Option<usize> {
        self.spawn_used.iter().position(|&u| u == NEUTRAL)
    }
}
