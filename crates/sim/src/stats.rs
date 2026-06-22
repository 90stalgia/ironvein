//! stats.rs — the full roster. Every gameplay number lives in this one table
//! so balance is auditable and (later) hot-loadable.
//!
//! Speeds are fixed-point units per tick (256 = one tile per tick = absurdly fast;
//! infantry ~26 = 1 tile/sec at 10Hz). Ranges are in tiles. Times are in ticks.

use crate::Fx;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    // ---- buildings ----
    ConYard = 0,
    PowerPlant = 1,
    Refinery = 2,
    Barracks = 3,
    Factory = 4,
    GuardTower = 5,
    Wall = 6,
    House = 7,
    Farm = 8,
    Road = 9,
    CannonTower = 10,
    Pillbox = 11,
    Radar = 12,
    RepairDepot = 13,
    TechCenter = 14,
    Reactor = 15,
    OreSilo = 16,
    MedBay = 17,
    MissileTurret = 18,
    Gate = 19,
    // ---- units ----
    Rifleman = 20,
    Rocketeer = 21,
    Engineer = 22,
    Harvester = 23,
    Buggy = 24,
    Tank = 25,
    Grenadier = 26,
    Flamer = 27,
    Sniper = 28,
    Artillery = 29,
    HeavyTank = 30,
    // ---- the night: supernatural marauders (owned by NEUTRAL) ----
    Zombie = 31,
    Werewolf = 32,
    Vampire = 33,
    // ---- bosses: the Lich (mini-boss) and the Warlock (final boss / puppeteer) ----
    Lich = 34,
    Warlock = 35,
    // ---- tier-3: unlocked with Essence (farmed from the dark & deep mining) ----
    Obelisk = 36,  // building (note is_building's explicit inclusion)
    Champion = 37, // hero unit
    // ---- the capstone: once the nations wound the Warlock, it seizes the
    //      machinery of war and animates these corrupted hulks (NEUTRAL) ----
    HellTank = 38,
    // ---- the landing craft: your colonists' dropship, the first thing on the
    //      ground at B Proxima. Acts as a construction yard; not re-buildable ----
    Starship = 39,
    // ---- superweapon: charges, then launches a devastating nuclear strike ----
    MissileSilo = 40,
    // ---- food economy: a granary that raises your food cap, and peaceful
    //      wildlife (NEUTRAL) you hunt for meat ----
    FoodSilo = 41,
    Deer = 42,
    // ---- tier-2 defence: a high-voltage zapper. Tech-Center gated, NO Essence;
    //      power-hungry (pair it with a Reactor). Melts armour and structures —
    //      the accessible cousin of the Essence-locked Obelisk. ----
    TeslaCoil = 43,
}

pub const ALL_BUILDINGS: [Kind; 22] = [
    Kind::ConYard,
    Kind::PowerPlant,
    Kind::Refinery,
    Kind::Barracks,
    Kind::Factory,
    Kind::GuardTower,
    Kind::Wall,
    Kind::House,
    Kind::Farm,
    Kind::Road,
    Kind::CannonTower,
    Kind::Pillbox,
    Kind::Radar,
    Kind::RepairDepot,
    Kind::TechCenter,
    Kind::Reactor,
    Kind::OreSilo,
    Kind::MedBay,
    Kind::MissileTurret,
    Kind::Gate,
    Kind::Obelisk,
    Kind::TeslaCoil,
];

pub const ALL_UNITS: [Kind; 12] = [
    Kind::Rifleman,
    Kind::Rocketeer,
    Kind::Engineer,
    Kind::Harvester,
    Kind::Buggy,
    Kind::Tank,
    Kind::Grenadier,
    Kind::Flamer,
    Kind::Sniper,
    Kind::Artillery,
    Kind::HeavyTank,
    Kind::Champion,
];

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        Some(match v {
            0 => Kind::ConYard,
            1 => Kind::PowerPlant,
            2 => Kind::Refinery,
            3 => Kind::Barracks,
            4 => Kind::Factory,
            5 => Kind::GuardTower,
            6 => Kind::Wall,
            7 => Kind::House,
            8 => Kind::Farm,
            9 => Kind::Road,
            10 => Kind::CannonTower,
            11 => Kind::Pillbox,
            12 => Kind::Radar,
            13 => Kind::RepairDepot,
            14 => Kind::TechCenter,
            15 => Kind::Reactor,
            16 => Kind::OreSilo,
            17 => Kind::MedBay,
            18 => Kind::MissileTurret,
            19 => Kind::Gate,
            20 => Kind::Rifleman,
            21 => Kind::Rocketeer,
            22 => Kind::Engineer,
            23 => Kind::Harvester,
            24 => Kind::Buggy,
            25 => Kind::Tank,
            26 => Kind::Grenadier,
            27 => Kind::Flamer,
            28 => Kind::Sniper,
            29 => Kind::Artillery,
            30 => Kind::HeavyTank,
            31 => Kind::Zombie,
            32 => Kind::Werewolf,
            33 => Kind::Vampire,
            34 => Kind::Lich,
            35 => Kind::Warlock,
            36 => Kind::Obelisk,
            37 => Kind::Champion,
            38 => Kind::HellTank,
            39 => Kind::Starship,
            40 => Kind::MissileSilo,
            41 => Kind::FoodSilo,
            42 => Kind::Deer,
            43 => Kind::TeslaCoil,
            _ => return None,
        })
    }
    pub fn is_building(self) -> bool {
        // buildings are 0..20, plus a few that sit above the unit/monster range
        // (the tier-3 Obelisk, the Starship landing craft, the Missile Silo)
        (self as u8) < 20 || matches!(self, Kind::Obelisk | Kind::Starship | Kind::MissileSilo | Kind::FoodSilo | Kind::TeslaCoil)
    }
    pub fn is_unit(self) -> bool {
        !self.is_building()
    }
    pub fn is_infantry(self) -> bool {
        matches!(
            self,
            Kind::Rifleman | Kind::Rocketeer | Kind::Engineer | Kind::Grenadier | Kind::Flamer | Kind::Sniper
        )
    }
    /// Auto-firing defensive structures (acquire & shoot when idle; go offline
    /// on low power).
    pub fn is_defense(self) -> bool {
        matches!(self, Kind::GuardTower | Kind::CannonTower | Kind::Pillbox | Kind::MissileTurret | Kind::Obelisk | Kind::Starship | Kind::TeslaCoil)
    }
    /// A supernatural night-creature: NEUTRAL-owned, hostile to every nation,
    /// and burns in daylight unless it finds shade.
    pub fn is_monster(self) -> bool {
        matches!(self, Kind::Zombie | Kind::Werewolf | Kind::Vampire | Kind::Lich | Kind::Warlock | Kind::HellTank)
    }
    /// A named boss — far tougher, burns only slowly, worth a bounty.
    pub fn is_boss(self) -> bool {
        matches!(self, Kind::Lich | Kind::Warlock)
    }
    /// Too vast or too mechanical to hide from the dawn: only smoulders in
    /// daylight (bosses and the animated war-hulks) instead of flashing to ash.
    pub fn smoulders(self) -> bool {
        self.is_boss() || matches!(self, Kind::HellTank)
    }
    /// Peaceful wildlife (NEUTRAL): wanders, never attacks, flees danger, and
    /// drops meat when hunted. NOT a monster (doesn't burn, isn't auto-targeted).
    pub fn is_critter(self) -> bool {
        matches!(self, Kind::Deer)
    }
}

/// Credits trickled per `FARM_PERIOD` ticks by each finished income building.
/// (Farms feed your town; ore silos automate refining.) 0 = not an earner.
pub fn income_of(k: Kind) -> u32 {
    match k {
        // Farms no longer pay credits — they grow FOOD now (see FARM_FOOD).
        Kind::OreSilo => 3,
        _ => 0,
    }
}

/// A second prerequisite building (beyond `built_by`) the player must own,
/// finished, before this can be built or trained — the tech tier.
pub fn requires(k: Kind) -> Option<Kind> {
    match k {
        Kind::MissileTurret | Kind::Sniper | Kind::Artillery | Kind::HeavyTank | Kind::Obelisk | Kind::Champion | Kind::MissileSilo | Kind::TeslaCoil => Some(Kind::TechCenter),
        _ => None,
    }
}

/// Essence cost — the rare currency farmed from slain monsters and the heart of
/// mined-out mountains. Only the tier-3 power draws on it. 0 = needs none.
pub fn essence_cost(k: Kind) -> u32 {
    match k {
        Kind::Obelisk => 200,
        Kind::Champion => 150,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub name: &'static str,
    pub max_hp: i32,
    pub cost: u32,
    pub build_time: u32, // ticks at full power
    pub speed: Fx,       // 0 for buildings
    pub damage: i32,     // 0 = unarmed
    pub range: i32,      // tiles
    pub rof: u16,        // ticks between shots
    pub sight: i32,      // tiles
    pub power: i32,      // + produces, - consumes
    pub footprint: (i32, i32), // tiles (w, h); units are (1,1)
    /// Which building trains this unit (None for buildings / starting kit only).
    pub built_by: Option<Kind>,
}

pub fn stats(k: Kind) -> Stats {
    use Kind::*;
    match k {
        // name              hp    cost  time speed dmg range rof sight power foot  built_by
        ConYard => s("Construction Yard", 2000, 5000, 600, 0, 0, 0, 0, 7, 0, (3, 3), None),
        PowerPlant => s("Power Plant", 600, 300, 80, 0, 0, 0, 0, 4, 100, (2, 2), None),
        Refinery => s("Ore Refinery", 1200, 1800, 200, 0, 0, 0, 0, 5, -30, (3, 3), None),
        Barracks => s("Barracks", 800, 500, 100, 0, 0, 0, 0, 5, -20, (2, 2), None),
        Factory => s("War Factory", 1000, 2000, 220, 0, 0, 0, 0, 5, -30, (3, 3), None),
        GuardTower => s("Guard Tower", 500, 600, 120, 0, 22, 5, 12, 7, -20, (1, 1), None),
        Wall => s("Wall", 250, 50, 10, 0, 0, 0, 0, 1, 0, (1, 1), None),
        House => s("House", 400, 300, 80, 0, 0, 0, 0, 3, 0, (2, 2), None),
        Farm => s("Farm", 350, 500, 100, 0, 0, 0, 0, 3, 0, (2, 2), None),
        // Road is a terrain stamp, not an entity: cheap, instant, 1x1.
        Road => s("Road", 1, 20, 1, 0, 0, 0, 0, 0, 0, (1, 1), None),
        // name              hp    cost  time spd dmg rng rof sight power foot   built_by
        CannonTower => s("Cannon Tower", 700, 1000, 150, 0, 55, 6, 65, 8, -30, (1, 1), None),
        Pillbox => s("Pillbox", 400, 400, 60, 0, 14, 4, 11, 7, -10, (1, 1), None),
        Radar => s("Radar Dome", 600, 1000, 150, 0, 0, 0, 0, 18, -40, (2, 2), None),
        RepairDepot => s("Repair Depot", 900, 800, 160, 0, 0, 0, 0, 5, -30, (2, 2), None),
        // ---- tier-2 / utility buildings ----
        TechCenter => s("Tech Center", 800, 1500, 220, 0, 0, 0, 0, 6, -50, (3, 3), None),
        Reactor => s("Reactor", 800, 700, 150, 0, 0, 0, 0, 4, 250, (2, 2), None),
        OreSilo => s("Ore Silo", 500, 600, 100, 0, 0, 0, 0, 4, -10, (2, 2), None),
        MedBay => s("Med Bay", 600, 700, 130, 0, 0, 0, 0, 5, -20, (2, 2), None),
        MissileTurret => s("Missile Turret", 600, 1200, 170, 0, 60, 8, 70, 9, -40, (1, 1), None),
        Gate => s("Gate", 500, 150, 35, 0, 0, 0, 0, 0, 0, (1, 1), None),
        // tier-3 super-defence: a long-range arcane death-ray (Essence-gated)
        Obelisk => s("Obelisk", 900, 1500, 240, 0, 95, 9, 30, 11, -55, (1, 1), None),
        // tier-2 zapper: shorter range than the Obelisk and no Essence, but a
        // heavy hitter — and a power hog, so feed it from a Reactor.
        TeslaCoil => s("Tesla Coil", 650, 1300, 190, 0, 95, 7, 18, 9, -80, (1, 1), None),

        // name                hp   cost time spd dmg rng rof sight power foot   built_by
        Rifleman => s("Rifleman", 100, 100, 50, 24, 8, 3, 10, 4, 0, (1, 1), Some(Barracks)),
        Rocketeer => s("Rocketeer", 90, 300, 80, 20, 26, 5, 22, 4, 0, (1, 1), Some(Barracks)),
        Engineer => s("Engineer", 80, 500, 80, 22, 0, 0, 0, 3, 0, (1, 1), Some(Barracks)),
        Harvester => s("Ore Harvester", 700, 1100, 150, 22, 0, 0, 0, 4, 0, (1, 1), Some(Factory)),
        Buggy => s("Scout Buggy", 220, 500, 90, 48, 10, 4, 9, 6, 0, (1, 1), Some(Factory)),
        Tank => s("Battle Tank", 420, 900, 160, 30, 34, 4, 18, 5, 0, (1, 1), Some(Factory)),
        Grenadier => s("Grenadier", 110, 180, 60, 22, 18, 3, 16, 4, 0, (1, 1), Some(Barracks)),
        Flamer => s("Flame Trooper", 120, 220, 70, 20, 16, 2, 9, 4, 0, (1, 1), Some(Barracks)),
        Sniper => s("Sniper", 80, 350, 90, 22, 45, 7, 45, 7, 0, (1, 1), Some(Barracks)),
        Artillery => s("Artillery", 200, 1000, 200, 18, 65, 9, 95, 6, 0, (1, 1), Some(Factory)),
        HeavyTank => s("Heavy Tank", 820, 1600, 260, 22, 55, 5, 30, 6, 0, (1, 1), Some(Factory)),
        // night marauders: melee (range 1), no cost/builder — the dark spawns them
        Zombie => s("Zombie", 130, 0, 0, 13, 16, 1, 12, 8, 0, (1, 1), None),
        Werewolf => s("Werewolf", 220, 0, 0, 42, 26, 1, 8, 11, 0, (1, 1), None),
        Vampire => s("Vampire", 170, 0, 0, 34, 20, 1, 10, 12, 0, (1, 1), None),
        // bosses: a ranged caster (raises zombies) and the relentless puppeteer
        Lich => s("The Lich", 2800, 0, 0, 16, 44, 5, 14, 16, 0, (1, 1), None),
        Warlock => s("The Warlock", 7500, 0, 0, 16, 70, 6, 12, 20, 0, (1, 1), None),
        // tier-3 hero: a one-soldier army (Essence-gated, built at the Factory)
        Champion => s("Champion", 1700, 1800, 280, 30, 52, 4, 6, 12, 0, (1, 1), Some(Factory)),
        // capstone: a corrupted war machine the wounded Warlock animates — heavy,
        // ranged, and it only smoulders by day (no cost/builder; the dark fields it)
        HellTank => s("Hell Tank", 950, 0, 0, 20, 62, 6, 7, 13, 0, (1, 1), None),
        // food economy: a granary (raises food cap) and peaceful huntable deer
        FoodSilo => s("Food Silo", 500, 600, 100, 0, 0, 0, 0, 4, -10, (2, 2), None),
        Deer => s("Deer", 60, 0, 0, 16, 0, 0, 0, 7, 0, (1, 1), None),
        // the landing craft: a towering colony ship (5x5). It powers itself,
        // anchors building like a ConYard, and its point-defense cannons savage
        // anything that comes close. Not in ALL_BUILDINGS — can't be re-built.
        Starship => s("Starship", 5000, 5000, 0, 0, 70, 7, 5, 13, 80, (5, 5), None),
        // superweapon: charges silently, then drops a nuke anywhere on the map
        MissileSilo => s("Missile Silo", 900, 3000, 320, 0, 0, 0, 0, 6, -75, (2, 2), None),
    }
}

#[allow(clippy::too_many_arguments)]
const fn s(
    name: &'static str,
    max_hp: i32,
    cost: u32,
    build_time: u32,
    speed: Fx,
    damage: i32,
    range: i32,
    rof: u16,
    sight: i32,
    power: i32,
    footprint: (i32, i32),
    built_by: Option<Kind>,
) -> Stats {
    Stats { name, max_hp, cost, build_time, speed, damage, range, rof, sight, power, footprint, built_by }
}

/// Lumber cost (chop trees → wood). Alongside `cost` (credits, mined from ore)
/// and `stone_cost`, this is the "deep" three-resource economy: nearly
/// everything draws on timber for framing/stocks. 0 = needs none.
pub fn wood_cost(k: Kind) -> u32 {
    use Kind::*;
    match k {
        // buildings: timber framing
        ConYard => 150,
        Barracks => 80,
        Factory => 70,
        TechCenter => 80,
        House => 70,
        Refinery => 60,
        Farm => 60,
        Reactor => 50,
        MedBay => 50,
        Radar => 30,
        RepairDepot => 40,
        OreSilo => 40,
        PowerPlant => 40,
        GuardTower => 30,
        TeslaCoil => 30,
        // units: handles, stocks, crates
        Harvester => 30,
        HeavyTank => 30,
        Artillery => 30,
        Buggy | Tank => 20,
        Rifleman | Rocketeer | Engineer | Grenadier | Flamer | Sniper => 10,
        _ => 0,
    }
}

/// Stone cost (mine rock/mountain → stone). Fortifications and heavy plating
/// lean on it.
pub fn stone_cost(k: Kind) -> u32 {
    use Kind::*;
    match k {
        ConYard => 200,
        MissileSilo => 160,
        TechCenter => 120,
        Factory => 100,
        MissileTurret => 90,
        TeslaCoil => 90,
        CannonTower => 80,
        Refinery => 80,
        Gate => 70,
        Pillbox => 60,
        Reactor => 60,
        OreSilo => 60,
        FoodSilo => 50,
        Wall => 50,
        GuardTower => 40,
        Radar => 40,
        MedBay => 40,
        Barracks => 40,
        RepairDepot => 60,
        PowerPlant => 30,
        House => 30,
        Farm => 20,
        // vehicles: armour plating
        HeavyTank => 80,
        Artillery => 50,
        Tank => 40,
        Buggy => 20,
        _ => 0,
    }
}

/// Damage multiplier table (percent): attacker class vs target class keeps
/// combat from being pure rock-paper-nothing. Rockets shred vehicles/buildings,
/// rifles shred infantry.
pub fn dmg_pct(attacker: Kind, target: Kind) -> i32 {
    let rocket = matches!(
        attacker,
        Kind::Rocketeer
            | Kind::Tank
            | Kind::GuardTower
            | Kind::CannonTower
            | Kind::Grenadier
            | Kind::Artillery
            | Kind::HeavyTank
            | Kind::MissileTurret
            | Kind::Lich
            | Kind::Warlock
            | Kind::Obelisk
            | Kind::HellTank
            | Kind::Starship
            | Kind::TeslaCoil
    );
    match (rocket, target.is_infantry(), target.is_building()) {
        (true, true, _) => 60,    // explosives vs infantry: meh
        (true, false, true) => 130, // explosives vs buildings: strong
        (true, false, false) => 110, // vs vehicles
        (false, true, _) => 110,  // bullets vs infantry
        (false, false, true) => 45, // bullets vs buildings: weak
        (false, false, false) => 70, // bullets vs vehicles
    }
}

/// Houses raise the unit cap; this is the "settle down and your town grows" hook.
pub const BASE_UNIT_CAP: u32 = 24;
pub const CAP_PER_HOUSE: u32 = 8;
/// Farms trickle credits: 1 credit per FARM_PERIOD ticks.
pub const FARM_PERIOD: u32 = 4;
/// Harvester capacity (in resource units — ore, wood, or stone per load).
pub const HARVESTER_CAP: u16 = 500;
/// New settlers start with this much of each resource (enough for a first base,
/// then you must chop & mine to keep building).
pub const STARTING_CREDITS: u32 = 4000;
pub const STARTING_WOOD: u32 = 800;
pub const STARTING_STONE: u32 = 600;
/// How much a single tree / rock-or-mountain tile yields before it's cleared.
pub const TREE_WOOD: u16 = 220;
pub const ROCK_STONE: u16 = 360;
/// Buildings must be placed within this many tiles of one you own.
pub const BUILD_RADIUS: i32 = 7;

/// Food economy. Soldiers (and crews) eat; farms grow food, hunted deer and
/// foraged berries supplement it, Food Silos store it. Cooking — turning raw
/// MEAT into food — needs a House. Numbers are deliberately gentle so the big
/// landing army doesn't starve instantly; build farms / hunt to keep up.
pub const FOOD_PERIOD: u32 = 30; // ticks between food production + upkeep ticks (~3s)
pub const FARM_FOOD: u32 = 12; // food each finished Farm grows per FOOD_PERIOD
pub const STARTING_FOOD: u32 = 2400;
pub const BASE_FOOD_CAP: u32 = 2500;
pub const FOOD_PER_HOUSE: u32 = 500;
pub const FOOD_PER_SILO: u32 = 2000;
pub const DEER_MEAT: u32 = 120; // food from cooking one deer's meat
pub const BERRY_FOOD: u32 = 30; // food from a foraged wild-berry mote

/// Per-unit food upkeep, charged each FOOD_PERIOD against the stockpile. 0 for
/// machines that don't eat (harvesters fend for themselves).
pub fn food_upkeep(k: Kind) -> u32 {
    use Kind::*;
    match k {
        Champion => 2,
        Rifleman | Rocketeer | Grenadier | Flamer | Sniper | Engineer => 1, // mouths
        Tank | HeavyTank | Artillery | Buggy => 1,                          // crews
        _ => 0,
    }
}

/// Missile Silo superweapon: ticks to charge a nuke (~3 min at 10Hz), blast
/// radius in tiles, and peak damage at ground zero (linear falloff to the edge).
pub const NUKE_CHARGE: u16 = 1800;
pub const NUKE_RADIUS: i32 = 5;
pub const NUKE_DMG: i32 = 1000;
/// Each Starship raises the unit cap by this much — it ferried a whole colony.
pub const STARSHIP_CAP: u32 = 50;
