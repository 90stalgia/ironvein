//! path.rs — A* on the tile grid. 8-directional, no corner cutting,
//! hard expansion budget with best-partial-path fallback (units head toward
//! the closest reachable point instead of freezing).
//!
//! Determinism notes: BinaryHeap pops are tie-broken by an explicit monotonic
//! sequence number, and neighbor order is a fixed constant array.

use crate::map::Map;
use crate::Tp;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

const DIRS: [(i32, i32, i32); 8] = [
    (0, -1, 10),
    (1, 0, 10),
    (0, 1, 10),
    (-1, 0, 10),
    (1, -1, 14),
    (1, 1, 14),
    (-1, 1, 14),
    (-1, -1, 14),
];

const MAX_EXPAND: usize = 6000;

fn heur(a: Tp, b: Tp) -> i32 {
    // octile distance * 10
    let dx = (a.x - b.x).abs();
    let dy = (a.y - b.y).abs();
    let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
    14 * lo + 10 * (hi - lo)
}

/// Find a path from `from` to `to`. Returns waypoints in REVERSE order
/// (so callers `pop()` the next tile), excluding the starting tile.
/// If `to` is unreachable/blocked, paths to the closest expanded tile instead.
///
/// `gates` lists tiles to treat as walkable even though they're blocked — used
/// for friendly gates, which physically block enemies but let their owner (and
/// allies) pass straight through. Empty for ordinary pathing.
pub fn find(map: &Map, from: Tp, to: Tp, accept_adjacent: bool, gates: &[Tp]) -> Vec<Tp> {
    let walkable = |t: Tp| map.walkable(t) || gates.contains(&t);
    if from == to {
        return Vec::new();
    }
    let w = map.w;
    let h = map.h;
    let n = (w * h) as usize;
    let idx = |t: Tp| (t.y * w + t.x) as usize;

    // Per-call scratch. For PoC scale (128x128, dozens of paths/sec) allocation
    // here is fine; a real perf pass would pool these.
    let mut g = vec![i32::MAX; n];
    let mut came: Vec<u32> = vec![u32::MAX; n];
    let mut closed = vec![false; n];

    let mut open: BinaryHeap<Reverse<(i32, u32, i32, i32)>> = BinaryHeap::new();
    let mut seq: u32 = 0;

    g[idx(from)] = 0;
    open.push(Reverse((heur(from, to), seq, from.x, from.y)));

    let mut best = from;
    let mut best_h = heur(from, to);
    let mut expanded = 0usize;
    let mut found = false;

    while let Some(Reverse((_f, _s, cx, cy))) = open.pop() {
        let cur = Tp::new(cx, cy);
        let ci = idx(cur);
        if closed[ci] {
            continue;
        }
        closed[ci] = true;
        expanded += 1;

        let hh = heur(cur, to);
        if hh < best_h {
            best_h = hh;
            best = cur;
        }
        if cur == to || (accept_adjacent && hh <= 14) {
            best = cur;
            found = true;
            break;
        }
        if expanded >= MAX_EXPAND {
            break;
        }

        for &(dx, dy, cost) in DIRS.iter() {
            let nt = Tp::new(cur.x + dx, cur.y + dy);
            if !map.in_bounds(nt) {
                continue;
            }
            // target tile itself may be blocked when accept_adjacent (attacking a building)
            let target_ok = accept_adjacent && nt == to;
            if !walkable(nt) && !target_ok && nt != to {
                continue;
            }
            if !walkable(nt) && nt == to && !accept_adjacent {
                continue; // destination occupied; partial path will get us near
            }
            // no diagonal corner cutting
            if dx != 0 && dy != 0 {
                let a = Tp::new(cur.x + dx, cur.y);
                let b = Tp::new(cur.x, cur.y + dy);
                if !walkable(a) || !walkable(b) {
                    continue;
                }
            }
            let ni = idx(nt);
            if closed[ni] {
                continue;
            }
            let ng = g[ci].saturating_add(cost);
            if ng < g[ni] {
                g[ni] = ng;
                came[ni] = ci as u32;
                seq += 1;
                open.push(Reverse((ng + heur(nt, to), seq, nt.x, nt.y)));
            }
        }
    }

    let goal = if found { best } else { best };
    if goal == from {
        return Vec::new();
    }
    // reconstruct (already reversed: goal first, next-step last)
    let mut out = Vec::new();
    let mut cur = idx(goal);
    let start = idx(from);
    let mut guard = 0;
    while cur != start && cur != u32::MAX as usize {
        out.push(Tp::new(cur as i32 % w, cur as i32 / w));
        let p = came[cur];
        if p == u32::MAX {
            break;
        }
        cur = p as usize;
        guard += 1;
        if guard > n {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Terrain;

    #[test]
    fn routes_around_water() {
        let mut m = Map::new(20, 20);
        // vertical water wall with one gap
        for y in 0..20 {
            m.set_terrain(Tp::new(10, y), Terrain::Water);
        }
        m.set_terrain(Tp::new(10, 15), Terrain::Grass);
        let p = find(&m, Tp::new(2, 2), Tp::new(18, 2), false, &[]);
        assert!(!p.is_empty());
        // first popped waypoint is the last element
        // path must pass through the gap column at x=10,y=15
        assert!(p.iter().any(|t| *t == Tp::new(10, 15)));
        // and never stand on water
        for t in &p {
            assert!(m.terrain_at(*t).passable(), "stepped on {:?}", m.terrain_at(*t));
        }
        // ends at the goal
        assert_eq!(p[0], Tp::new(18, 2));
    }

    #[test]
    fn unreachable_gives_partial() {
        let mut m = Map::new(20, 20);
        for y in 0..20 {
            m.set_terrain(Tp::new(10, y), Terrain::Water);
        }
        let p = find(&m, Tp::new(2, 2), Tp::new(18, 2), false, &[]);
        // can't cross; should still produce movement toward the wall
        assert!(!p.is_empty());
        for t in &p {
            assert!(t.x < 10);
        }
    }
}
