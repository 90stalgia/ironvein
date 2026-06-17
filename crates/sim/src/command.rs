//! command.rs — the complete vocabulary of player intent.
//!
//! EVERYTHING a player (or bot) can do is one of these. Commands are what travels
//! over the network, what gets recorded for replays, and the only mutation input
//! the simulation accepts. If you can't express it as a Command, players can't do it.

use crate::ser::{DResult, R, W};
use crate::stats::Kind;
use crate::{Eid, Tp};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// A new settler arrives in the world: claims a spawn site, gets a starter kit.
    /// `key` is the settler's identity public key — opaque bytes to the sim
    /// (verification is the net layer's job); all-zero for bots and legacy
    /// settlers. It persists in the save so a base can only be reclaimed by
    /// the keyholder, on any future host.
    Join { name: String, color: u8, key: [u8; 32] },
    /// Move selected units to a tile.
    Move { units: Vec<Eid>, to: Tp },
    /// Attack-move: advance, engaging anything hostile on the way.
    AttackMove { units: Vec<Eid>, to: Tp },
    /// Force-attack a specific entity.
    Attack { units: Vec<Eid>, target: Eid },
    /// Stop everything.
    Stop { units: Vec<Eid> },
    /// Send harvesters to a specific ore tile.
    Harvest { units: Vec<Eid>, tile: Tp },
    /// Engineer: capture a neutral or enemy building.
    Capture { unit: Eid, target: Eid },
    /// Place a building (cost is deducted, a construction site appears and self-builds).
    Build { kind: Kind, at: Tp },
    /// Queue a unit at a production building.
    Train { building: Eid, kind: Kind },
    /// Cancel the last queued item (refund).
    CancelTrain { building: Eid },
    /// Set a production building's rally point.
    SetRally { building: Eid, at: Tp },
    /// Sell a building for half its cost.
    Sell { building: Eid },
    /// Say something to everyone nearby (well, everyone — it's a small world).
    Chat { text: String },
    /// Wire credits to another player. Trade, rent, charity — the social economy.
    GiveCredits { to: u8, amount: u32 },
    /// Toggle an alliance offer toward another player. An alliance is in force
    /// when both have offered each other (mutual): no friendly fire, shared sight.
    Ally { with: u8 },
    /// Launch a charged nuke from a Missile Silo onto a target tile.
    FireNuke { silo: Eid, at: Tp },
}

impl Command {
    pub fn ser(&self, w: &mut W) {
        match self {
            Command::Join { name, color, key } => {
                w.u8(0);
                w.str(name);
                w.u8(*color);
                w.arr32(key);
            }
            Command::Move { units, to } => {
                w.u8(1);
                ser_units(w, units);
                w.i32(to.x);
                w.i32(to.y);
            }
            Command::AttackMove { units, to } => {
                w.u8(2);
                ser_units(w, units);
                w.i32(to.x);
                w.i32(to.y);
            }
            Command::Attack { units, target } => {
                w.u8(3);
                ser_units(w, units);
                w.u32(target.idx);
                w.u32(target.gen);
            }
            Command::Stop { units } => {
                w.u8(4);
                ser_units(w, units);
            }
            Command::Harvest { units, tile } => {
                w.u8(5);
                ser_units(w, units);
                w.i32(tile.x);
                w.i32(tile.y);
            }
            Command::Capture { unit, target } => {
                w.u8(6);
                w.u32(unit.idx);
                w.u32(unit.gen);
                w.u32(target.idx);
                w.u32(target.gen);
            }
            Command::Build { kind, at } => {
                w.u8(7);
                w.u8(*kind as u8);
                w.i32(at.x);
                w.i32(at.y);
            }
            Command::Train { building, kind } => {
                w.u8(8);
                w.u32(building.idx);
                w.u32(building.gen);
                w.u8(*kind as u8);
            }
            Command::CancelTrain { building } => {
                w.u8(9);
                w.u32(building.idx);
                w.u32(building.gen);
            }
            Command::SetRally { building, at } => {
                w.u8(10);
                w.u32(building.idx);
                w.u32(building.gen);
                w.i32(at.x);
                w.i32(at.y);
            }
            Command::Sell { building } => {
                w.u8(11);
                w.u32(building.idx);
                w.u32(building.gen);
            }
            Command::Chat { text } => {
                w.u8(12);
                w.str(text);
            }
            Command::GiveCredits { to, amount } => {
                w.u8(13);
                w.u8(*to);
                w.u32(*amount);
            }
            Command::Ally { with } => {
                w.u8(14);
                w.u8(*with);
            }
            Command::FireNuke { silo, at } => {
                w.u8(15);
                w.u32(silo.idx);
                w.u32(silo.gen);
                w.i32(at.x);
                w.i32(at.y);
            }
        }
    }

    pub fn de(r: &mut R) -> DResult<Command> {
        let tag = r.u8()?;
        Ok(match tag {
            0 => Command::Join { name: r.str()?, color: r.u8()?, key: r.arr32()? },
            1 => Command::Move { units: de_units(r)?, to: de_tp(r)? },
            2 => Command::AttackMove { units: de_units(r)?, to: de_tp(r)? },
            3 => Command::Attack { units: de_units(r)?, target: de_eid(r)? },
            4 => Command::Stop { units: de_units(r)? },
            5 => Command::Harvest { units: de_units(r)?, tile: de_tp(r)? },
            6 => Command::Capture { unit: de_eid(r)?, target: de_eid(r)? },
            7 => {
                let k = Kind::from_u8(r.u8()?).ok_or(crate::ser::DecodeErr)?;
                Command::Build { kind: k, at: de_tp(r)? }
            }
            8 => {
                let b = de_eid(r)?;
                let k = Kind::from_u8(r.u8()?).ok_or(crate::ser::DecodeErr)?;
                Command::Train { building: b, kind: k }
            }
            9 => Command::CancelTrain { building: de_eid(r)? },
            10 => Command::SetRally { building: de_eid(r)?, at: de_tp(r)? },
            11 => Command::Sell { building: de_eid(r)? },
            12 => Command::Chat { text: r.str()? },
            13 => Command::GiveCredits { to: r.u8()?, amount: r.u32()? },
            14 => Command::Ally { with: r.u8()? },
            15 => Command::FireNuke { silo: de_eid(r)?, at: de_tp(r)? },
            _ => return Err(crate::ser::DecodeErr),
        })
    }
}

fn ser_units(w: &mut W, units: &[Eid]) {
    let n = units.len().min(255);
    w.u8(n as u8);
    for u in &units[..n] {
        w.u32(u.idx);
        w.u32(u.gen);
    }
}
fn de_units(r: &mut R) -> DResult<Vec<Eid>> {
    let n = r.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(de_eid(r)?);
    }
    Ok(v)
}
fn de_eid(r: &mut R) -> DResult<Eid> {
    let idx = r.u32()?;
    let gen = r.u32()?;
    Ok(Eid { idx, gen })
}
fn de_tp(r: &mut R) -> DResult<Tp> {
    let x = r.i32()?;
    let y = r.i32()?;
    Ok(Tp::new(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_roundtrip() {
        let cmds = vec![
            Command::Join { name: "Ada".into(), color: 2, key: [7u8; 32] },
            Command::Move { units: vec![Eid { idx: 5, gen: 1 }], to: Tp::new(10, 12) },
            Command::Build { kind: Kind::PowerPlant, at: Tp::new(3, 4) },
            Command::Chat { text: "hello world".into() },
            Command::GiveCredits { to: 1, amount: 500 },
            Command::FireNuke { silo: Eid { idx: 3, gen: 2 }, at: Tp::new(40, 7) },
        ];
        for c in cmds {
            let mut w = W::new();
            c.ser(&mut w);
            let mut r = R::new(&w.buf);
            let back = Command::de(&mut r).unwrap();
            assert_eq!(c, back);
        }
    }
}
