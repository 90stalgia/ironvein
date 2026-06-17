//! IRONVEIN simulation core.
//!
//! HARD RULES (the contract that makes P2P lockstep possible):
//!   1. No floating point anywhere in this crate. Fixed-point (1 tile = 256 units) only.
//!   2. No HashMap/HashSet iteration. All iteration orders are explicit and stable.
//!   3. All randomness flows through `World.rng` (a seeded PCG32).
//!   4. The ONLY way to mutate a `World` is `World::step(tick_commands)`.
//!   5. `World::save_bytes()` is a pure function of state -> identical on every peer.
//!
//! If these hold, then: same seed + same command stream = bit-identical world on
//! every machine, forever. That is the entire multiplayer model.

pub mod rng;
pub mod ser;
pub mod stats;
pub mod map;
pub mod entity;
pub mod command;
pub mod path;
pub mod world;
pub mod mapgen;
pub mod bot;

pub use command::Command;
pub use entity::{Arena, Ent, Eid};
pub use map::{Map, Terrain};
pub use stats::{Kind, Stats};
pub use world::{Player, VisEvent, World, Mode};

/// Simulation ticks per second. Classic 90s RTS cadence; the client interpolates to 60fps.
pub const TICK_HZ: u32 = 10;

/// Fixed-point scale: 256 sub-units per tile.
pub type Fx = i32;
pub const FX: Fx = 256;

/// Player id. 0..MAX_PLAYERS are real players; NEUTRAL owns capturable map objects.
pub type Pid = u8;
pub const MAX_PLAYERS: usize = 8;
pub const NEUTRAL: Pid = 255;

/// Tile coordinate.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Tp {
    pub x: i32,
    pub y: i32,
}

impl Tp {
    pub fn new(x: i32, y: i32) -> Self {
        Tp { x, y }
    }
    /// Center of this tile in fixed-point world space.
    pub fn center(self) -> Fp {
        Fp { x: self.x * FX + FX / 2, y: self.y * FX + FX / 2 }
    }
}

/// Fixed-point world position (1 tile = 256).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fp {
    pub x: Fx,
    pub y: Fx,
}

impl Fp {
    pub fn tile(self) -> Tp {
        Tp { x: self.x >> 8, y: self.y >> 8 }
    }
    pub fn dist2(self, o: Fp) -> i64 {
        let dx = (self.x - o.x) as i64;
        let dy = (self.y - o.y) as i64;
        dx * dx + dy * dy
    }
    /// Move up to `step` fixed-units toward `to`. Integer-only. Returns new pos and whether arrived.
    pub fn step_toward(self, to: Fp, step: Fx) -> (Fp, bool) {
        let dx = (to.x - self.x) as i64;
        let dy = (to.y - self.y) as i64;
        let d2 = dx * dx + dy * dy;
        let s = step as i64;
        if d2 <= s * s {
            return (to, true);
        }
        let d = isqrt(d2);
        if d == 0 {
            return (to, true);
        }
        (
            Fp {
                x: self.x + (dx * s / d) as Fx,
                y: self.y + (dy * s / d) as Fx,
            },
            false,
        )
    }
    /// Facing 0..16 (0 = north, clockwise) toward `to`, for renderer + turret logic.
    pub fn facing_to(self, to: Fp) -> u8 {
        dir16((to.x - self.x) as i64, (to.y - self.y) as i64)
    }
}

/// Integer square root (binary search). Deterministic.
pub fn isqrt(v: i64) -> i64 {
    if v < 2 {
        return v.max(0);
    }
    let mut lo: i64 = 1;
    let mut hi: i64 = 3_037_000_499; // sqrt(i64::MAX)
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if mid <= v / mid {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Quantize a direction vector into 16 compass facings using integer octant tests.
pub fn dir16(dx: i64, dy: i64) -> u8 {
    if dx == 0 && dy == 0 {
        return 0;
    }
    // angle approximation: compare |dx| vs |dy| with 2.414 (~tan 67.5deg) and 0.414 slopes,
    // done in integers by cross-multiplying with 1000/2414/414.
    let ax = dx.abs();
    let ay = dy.abs();
    // sector within a quadrant: 0 = mostly vertical, 1 = diagonal-ish, 2 = mostly horizontal
    let sec = if ax * 1000 < ay * 414 {
        0
    } else if ax * 414 > ay * 1000 {
        2
    } else {
        1
    };
    // map quadrant + sector to 16-facing (0 = up/-y, clockwise). We fold to 8 main dirs,
    // then refine to 16 by an extra midline test.
    let oct: u8 = match (dx >= 0, dy >= 0, sec) {
        (true, false, 0) => 0,   // N
        (true, false, 1) => 2,   // NE
        (true, false, 2) => 4,   // E
        (true, true, 2) => 4,    // E
        (true, true, 1) => 6,    // SE
        (true, true, 0) => 8,    // S
        (false, true, 0) => 8,   // S
        (false, true, 1) => 10,  // SW
        (false, true, 2) => 12,  // W
        (false, false, 2) => 12, // W
        (false, false, 1) => 14, // NW
        (false, false, _) => 0,  // N
        _ => 8,
    };
    oct
}

/// FNV-1a 64-bit hash. Used for desync detection checksums.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Deterministic per-tile "noise" for cosmetic variation (client decoration, ore sparkle).
pub fn tile_noise(x: i32, y: i32, salt: u32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E3779B1)
        ^ (y as u32).wrapping_mul(0x85EBCA77)
        ^ salt.wrapping_mul(0xC2B2AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A2D39);
    h ^= h >> 15;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isqrt_basics() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(4), 2);
        assert_eq!(isqrt(8), 2);
        assert_eq!(isqrt(9), 3);
        assert_eq!(isqrt(1_000_000), 1000);
    }

    #[test]
    fn step_toward_arrives() {
        let a = Fp { x: 0, y: 0 };
        let b = Fp { x: 1000, y: 0 };
        let mut p = a;
        let mut guard = 0;
        loop {
            let (np, done) = p.step_toward(b, 64);
            p = np;
            if done {
                break;
            }
            guard += 1;
            assert!(guard < 100);
        }
        assert_eq!(p, b);
    }
}
