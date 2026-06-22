# IRONVEIN

![IRONVEIN](media/ironvein_1.png)

**A persistent-world real-time strategy game on a frontier that never sleeps.**

Land your colonists on a strange world and carve a base out of raw ore, timber and
stone — then hold it. Rival settlers want your ground. And when the sun goes down,
something worse climbs out of the dark. Log off and the world keeps turning; your
colony stands until someone with engineers and a grudge tears it down.

It's a love letter to the old Westwood RTS — ore trucks, war factories, "construction
complete" — with one big twist: **there's no server, no campaign, and no end screen.**
Just a living valley with its own clock, its own monsters, and a base with your name
on it.

## ▶ Play right now, in your browser

**https://90stalgia.github.io/ironvein/**

No install, no download, no account. The whole game is WebAssembly served off a
static page — single-player and skirmish start instantly. Multiplayer is pure
peer-to-peer (WebRTC): you and your friends connect *directly*, with no lobby
server and nothing to host.

## The world

- **It persists.** Every peer autosaves the identical world once a minute, so
  *anyone's* save can re-host it. Leave, and your colony keeps standing for everyone
  else; come back under the same name and reclaim it. A tiny headless **seed node**
  can keep a valley alive 24/7 on a spare box.
- **It has a day and a night.** Build, mine and expand under the sun. After dark the
  frontier turns murderous — **zombies, werewolves and vampires** pour out of the
  black and throw themselves at your walls until the dawn burns them to ash. Hold out
  long enough and worse things take notice: **the Lich**, and the puppeteer behind it
  all, **the Warlock**.
- **It's procedural to the bone.** Every map is freshly generated, every sprite is
  drawn in code, and every sound — the soundtrack, the gunfire, even the title theme —
  is *synthesised* at runtime. No art files, no audio files. Nothing is downloaded;
  nothing is shipped.

## Build a colony, then defend it

A deep frontier economy with real logistics: **harvesters** mine glittering ore into
credits, crews **chop timber** and **quarry stone**, **farms** grow food and
**hunters** bring in meat — your soldiers eat, so a town that can't feed itself
starves. Refineries, power, radar, repair bays, war factories: the full base-building
loop, all of it placed tile by tile.

Climb the tech tree and the defenses get mean. A **Tech Center** unlocks the good
toys — long-range **Missile Turrets**, screaming **Tesla Coils** (high-voltage
zappers that melt armour and structures; they're power-hungry, so feed them from a
**Reactor**), and a map-wiping nuclear **Missile Silo**. And for those willing to
farm the dark for **Essence**, the arcane top tier opens up: the death-ray **Obelisk**
and the one-soldier-army **Champion**.

## Three ways to play

- **Persistent** — the headline mode. Drop into a shared, always-on valley where your
  base outlives your session and the war never really stops.
- **Skirmish** — classic deathmatch against bots that expand, harvest, build armies
  and raid across the river.
- **Survival** — just you against the night. Pick your difficulty and count the dawns.

## Controls

Right panel has the minimap, resources, power and the build tabs. Left-drag to
select, right-click to act. `F1` shows the full manual in-game. The essentials:

| input               | action                                              |
|---------------------|-----------------------------------------------------|
| left drag           | select your units                                   |
| right click         | move · attack · harvest · rally (context-sensitive) |
| `A` + click         | attack-move                                         |
| `S` / `X`           | stop · sell selected building                       |
| `Ctrl+1..9`, `1..9` | save / recall control group                         |
| `Enter`             | chat (`/give 2 500` wires 500 credits to player 2)  |
| `F5`                | save now (it autosaves every minute anyway)         |
| `H`, arrows, edges  | camera home · scroll                                |

## Run it natively

The desktop build wants only Rust and a desktop GL. On Debian/Ubuntu:

```sh
sudo apt install build-essential libx11-dev libxi-dev libgl1-mesa-dev libasound2-dev
cargo build --release

./target/release/ironvein                                # your valley, one bot neighbor
./target/release/ironvein --bots 3 --map skirmish        # deathmatch
./target/release/ironvein --name Ada --color 2           # pick a callsign & color
```

Native multiplayer is plain TCP — one peer hosts, the rest join into a full mesh
(LAN, VPN/Tailscale, or a port-forward); join live, mid-game:

```sh
./target/release/ironvein --host 47777 --name Ada
./target/release/ironvein --join 192.168.1.20:47777 --name Bo
```

Keep a world alive forever with the headless keeper, and resurrect it from any
player's autosave if the box dies:

```sh
./target/release/ironvein-seed --port 47777 --bots 1
./target/release/ironvein --load saves/world.iv --host 47777   # any save can re-host
```

## Under the hood — for the curious

One Rust workspace, with some genuinely stubborn engineering if you care to look: a
**bit-deterministic** simulation (no floats, one RNG, integer fixed-point) so every
peer computes a byte-identical world from the same inputs; **lockstep peer-to-peer**
play with **signed** commands over WebRTC + Nostr and no trusted server; cross-checked
state hashes that halt loudly instead of desyncing silently; and **host migration**
that keeps a world alive when the host drops. `ARCHITECTURE.md` is the full story;
`CLAUDE.md` is the map of the codebase.

```
crates/sim     the deterministic world — economy, combat, pathfinding, the night
crates/net     lockstep sessions: freeze-join, departure canon, host migration
crates/client  the macroquad client: procedural art, synthesised audio, the UI
crates/seed    the headless world keeper
```

## Screenshots

![IRONVEIN](media/ironvein_2.png)
![IRONVEIN](media/ironvein_3.png)

Generated with `ironvein --demo --bots 2 --map skirmish` — an unattended observer
match that drops PNGs into `shots/` (it even runs under `xvfb-run` on a headless box).
