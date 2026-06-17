//! bot.rs — a simple opponent. Architecturally important: the bot has NO special
//! powers. It reads the world and emits the exact same `Command`s a human client
//! does. In multiplayer, exactly one peer "drives" each bot and its commands are
//! replicated like anyone else's — so bots are deterministic for free.

use crate::command::Command;
use crate::stats::{stats, Kind};
use crate::world::World;
use crate::{Eid, Pid, Tp};

pub struct Bot {
    pub pid: Pid,
    pub name: String,
    next_attack: u32,
    joined_sent: bool,
}

impl Bot {
    pub fn new(pid: Pid, name: String) -> Bot {
        Bot { pid, name, next_attack: 2200, joined_sent: false }
    }

    /// Called every tick; emits commands occasionally. Pure function of (self, world).
    pub fn think(&mut self, w: &World) -> Vec<Command> {
        let mut out = Vec::new();
        let joined = w
            .players
            .get(self.pid as usize)
            .map(|p| p.joined)
            .unwrap_or(false);
        if !joined {
            if !self.joined_sent {
                self.joined_sent = true;
                out.push(Command::Join { name: self.name.clone(), color: self.pid % 8, key: [0; 32] });
            }
            return out;
        }
        // think on a stride, offset by pid so bots don't all act the same tick
        if (w.tick + self.pid as u32 * 3) % 10 != 0 {
            return out;
        }
        let p = &w.players[self.pid as usize];
        if p.defeated {
            return out;
        }

        // census
        let mut conyard: Option<Tp> = None;
        let mut barracks: Option<Eid> = None;
        let mut factory: Option<Eid> = None;
        // census counts, indexed by Kind-as-u8 — must cover the whole Kind enum
        // (currently up to Starship = 39), since count() below indexes it directly
        let mut n = [0u32; 64];
        let mut army: Vec<Eid> = Vec::new();
        let mut harvesters: Vec<Eid> = Vec::new();
        for e in w.ents.iter() {
            if e.owner != self.pid {
                continue;
            }
            let k = e.kind as usize;
            if k < n.len() {
                n[k] += 1;
            }
            match e.kind {
                Kind::ConYard | Kind::Starship => conyard = Some(e.tile()),
                Kind::Barracks if e.done => barracks = Some(e.id),
                Kind::Factory if e.done => factory = Some(e.id),
                Kind::Rifleman
                | Kind::Rocketeer
                | Kind::Buggy
                | Kind::Tank
                | Kind::Grenadier
                | Kind::Flamer
                | Kind::Sniper
                | Kind::Artillery
                | Kind::HeavyTank => army.push(e.id),
                Kind::Harvester => harvesters.push(e.id),
                _ => {}
            }
        }
        let Some(home) = conyard else { return out };
        let count = |k: Kind| n[k as usize];

        // keep timber & stone stocked: rotate a spare harvester onto the nearest
        // forest / rock when a resource runs low (the rest auto-mine ore)
        if !harvesters.is_empty() && w.tick % 80 == 0 {
            let h = harvesters[harvesters.len() - 1];
            let want = if p.wood < 350 && p.wood <= p.stone {
                Some(1u8)
            } else if p.stone < 350 {
                Some(2u8)
            } else if p.wood < 350 {
                Some(1u8)
            } else {
                None
            };
            if let Some(k) = want {
                if let Some(t) = nearest_resource(w, home, k, 44) {
                    out.push(Command::Harvest { units: vec![h], tile: t });
                }
            }
        }

        // --- build order (one placement attempt per think) ---
        let want: Option<Kind> = if count(Kind::PowerPlant) == 0 {
            Some(Kind::PowerPlant)
        } else if count(Kind::Refinery) == 0 {
            Some(Kind::Refinery)
        } else if count(Kind::Barracks) == 0 {
            Some(Kind::Barracks)
        } else if p.low_power() || (count(Kind::PowerPlant) < 2 && count(Kind::Factory) > 0) {
            Some(Kind::PowerPlant)
        } else if count(Kind::Factory) == 0 {
            Some(Kind::Factory)
        } else if count(Kind::GuardTower) < 2 {
            Some(Kind::GuardTower)
        } else if count(Kind::Pillbox) == 0 && p.credits > 600 {
            Some(Kind::Pillbox)
        } else if count(Kind::Radar) == 0 && p.credits > 1200 {
            Some(Kind::Radar)
        } else if count(Kind::RepairDepot) == 0 && count(Kind::Factory) > 0 && p.credits > 1000 {
            Some(Kind::RepairDepot)
        } else if count(Kind::CannonTower) < 2 && p.credits > 1400 {
            Some(Kind::CannonTower)
        } else if count(Kind::TechCenter) == 0 && count(Kind::Factory) > 0 && p.credits > 1800 {
            Some(Kind::TechCenter)
        } else if count(Kind::OreSilo) < 2 && p.credits > 1400 {
            Some(Kind::OreSilo)
        } else if count(Kind::MissileTurret) < 2 && count(Kind::TechCenter) > 0 && p.credits > 1600 {
            Some(Kind::MissileTurret)
        } else if count(Kind::Reactor) == 0 && count(Kind::TechCenter) > 0 && p.credits > 1000 {
            Some(Kind::Reactor)
        } else if count(Kind::MedBay) == 0 && p.credits > 1200 {
            Some(Kind::MedBay)
        } else if (p.starving || p.food < p.food_cap / 4) && count(Kind::Farm) < 8 && p.credits > 500 {
            Some(Kind::Farm) // feed the army before anything else when hungry
        } else if count(Kind::FoodSilo) == 0 && p.credits > 800 {
            Some(Kind::FoodSilo)
        } else if count(Kind::Obelisk) < 2 && count(Kind::TechCenter) > 0 && p.essence >= 200 && p.credits > 1500 {
            Some(Kind::Obelisk)
        } else if p.unit_count + 4 >= p.unit_cap {
            Some(Kind::House)
        } else if count(Kind::Refinery) < 2 && p.credits > 2600 {
            Some(Kind::Refinery)
        } else if count(Kind::Farm) < 5 && p.credits > 1200 {
            Some(Kind::Farm)
        } else {
            None
        };
        if let Some(kind) = want {
            if p.credits >= stats(kind).cost {
                if let Some(at) = find_spot(w, self.pid, kind, home) {
                    out.push(Command::Build { kind, at });
                }
            }
        }

        // --- training ---
        if let Some(b) = barracks {
            let has_tech = count(Kind::TechCenter) > 0;
            let infantry = count(Kind::Rifleman)
                + count(Kind::Rocketeer)
                + count(Kind::Grenadier)
                + count(Kind::Flamer)
                + count(Kind::Sniper);
            if infantry < 10 && p.credits > 600 {
                let r = count(Kind::Rifleman);
                let kind = if has_tech && count(Kind::Sniper) < 2 && p.credits > 1000 {
                    Kind::Sniper
                } else if count(Kind::Rocketeer) * 3 < r {
                    Kind::Rocketeer
                } else if count(Kind::Grenadier) * 4 < r {
                    Kind::Grenadier
                } else if count(Kind::Flamer) * 5 < r {
                    Kind::Flamer
                } else {
                    Kind::Rifleman
                };
                out.push(Command::Train { building: b, kind });
            }
        }
        if let Some(f) = factory {
            let has_tech = count(Kind::TechCenter) > 0;
            if count(Kind::Harvester) < 2 && p.credits > 1400 {
                out.push(Command::Train { building: f, kind: Kind::Harvester });
            } else if has_tech && count(Kind::Champion) < 2 && p.essence >= 150 && p.credits > 1800 {
                out.push(Command::Train { building: f, kind: Kind::Champion });
            } else if has_tech && count(Kind::HeavyTank) < 3 && p.credits > 1700 {
                out.push(Command::Train { building: f, kind: Kind::HeavyTank });
            } else if has_tech && count(Kind::Artillery) < 2 && p.credits > 1200 {
                out.push(Command::Train { building: f, kind: Kind::Artillery });
            } else if count(Kind::Tank) < 5 && p.credits > 1300 {
                out.push(Command::Train { building: f, kind: Kind::Tank });
            }
        }

        // --- attack waves --- (difficulty tunes muster size + cadence)
        let (muster, rearm) = match w.difficulty {
            0 => (9, 2600), // easy: rarer, bigger waves
            2 => (4, 1100), // hard: relentless pressure
            _ => (6, 1800), // normal
        };
        if w.tick >= self.next_attack && army.len() >= muster {
            self.next_attack = w.tick + rearm;
            if let Some(target) = nearest_enemy_building(w, self.pid, home) {
                out.push(Command::AttackMove { units: army, to: target });
            }
        }
        out
    }
}

fn find_spot(w: &World, pid: Pid, kind: Kind, home: Tp) -> Option<Tp> {
    // spiral outward from home; towers go a bit further out by skipping the inner ring
    let start_r = if kind == Kind::GuardTower { 5 } else { 2 };
    let start_r: i32 = start_r;
    for r in start_r..14 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let at = Tp::new(home.x + dx, home.y + dy);
                if w.can_place(pid, kind, at) {
                    return Some(at);
                }
            }
        }
    }
    None
}

/// Nearest tile of resource `kind` (0 ore, 1 wood, 2 stone) for the bot to send
/// a harvester to. Deterministic ring scan.
fn nearest_resource(w: &World, from: Tp, kind: u8, max_r: i32) -> Option<Tp> {
    for r in 1..=max_r {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let t = Tp::new(from.x + dx, from.y + dy);
                if w.map.resource_kind(t) == Some(kind) {
                    return Some(t);
                }
            }
        }
    }
    None
}

fn nearest_enemy_building(w: &World, pid: Pid, from: Tp) -> Option<Tp> {
    let mut best: Option<(i64, Tp)> = None;
    for e in w.ents.iter() {
        if e.owner == pid || e.owner == crate::NEUTRAL || !e.kind.is_building() {
            continue;
        }
        if w.players
            .get(e.owner as usize)
            .map(|p| p.defeated)
            .unwrap_or(false)
        {
            continue;
        }
        let t = e.tile();
        let d = ((t.x - from.x) as i64).pow(2) + ((t.y - from.y) as i64).pow(2);
        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, t));
        }
    }
    best.map(|(_, t)| t)
}

impl Bot {
    /// Test helper: copy attack-wave timing so two bot instances behave identically.
    pub fn clone_timing_from(&mut self, other: &Bot) {
        self.next_attack = other.next_attack;
        self.joined_sent = other.joined_sent;
    }
}
