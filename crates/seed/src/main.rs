//! ironvein-seed — a headless world keeper.
//!
//! Run this on any always-on box (a Pi, a VPS, the machine under your desk)
//! and the valley stays alive while everyone sleeps: ore regrows, farms pay
//! out, offline bases stand. Every peer's save is byte-identical, so if the
//! seed dies, anyone can relaunch the world from their own autosave.
//!
//! Usage:
//!   ironvein-seed --port 47777                    fresh persistent world
//!   ironvein-seed --port 47777 --load world.iv    resume a saved world
//!   ironvein-seed --port 47777 --bots 2           keep a couple of bots around

use ironvein_net::Session;
use ironvein_sim::bot::Bot;
use ironvein_sim::world::Mode;
use ironvein_sim::{mapgen, World};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const AUTOSAVE_EVERY: u32 = 600; // ticks = 60 s

fn main() {
    let mut port = 47777u16;
    let mut load: Option<String> = None;
    let mut save_dir = String::from("saves");
    let mut seed = ironvein_sim::mapgen::POC_SEED;
    let mut nbots = 0usize;

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => { port = args[i + 1].parse().expect("--port N"); i += 1; }
            "--load" => { load = Some(args[i + 1].clone()); i += 1; }
            "--save-dir" => { save_dir = args[i + 1].clone(); i += 1; }
            "--seed" => { seed = args[i + 1].parse().expect("--seed N"); i += 1; }
            "--bots" => { nbots = args[i + 1].parse().expect("--bots N"); i += 1; }
            "--help" | "-h" => {
                println!("ironvein-seed --port N [--load FILE] [--save-dir DIR] [--seed N] [--bots N]");
                return;
            }
            other => eprintln!("(ignoring unknown arg {other})"),
        }
        i += 1;
    }

    let world = match &load {
        Some(path) => {
            let bytes = fs::read(path).expect("read save file");
            let w = World::load_bytes(&bytes).expect("parse save file");
            println!("loaded world at tick {} ({} settlers on record)", w.tick,
                w.players.iter().filter(|p| p.joined).count());
            w
        }
        None => {
            println!("seeding a fresh Verdant Divide (seed {seed:#x})");
            mapgen::verdant_divide(seed, Mode::Persistent)
        }
    };

    let bot_names: Vec<String> = (0..nbots).map(|i| format!("Bot {}", ["Gravel", "Sable", "Rust", "Moss"][i % 4])).collect();
    let identity = ironvein_net::Identity::load_or_create(&PathBuf::from(&save_dir).join("id-keeper.key"));
    let mut session = Session::host(world, identity, port, "keeper", 7, &bot_names).expect("bind port");
    session.skip_settle(); // the keeper tends the world; it doesn't homestead
    let mut bots: Vec<Bot> = session
        .bot_pids()
        .into_iter()
        .zip(bot_names.iter())
        .map(|(pid, n)| Bot::new(pid, n.clone()))
        .collect();

    fs::create_dir_all(&save_dir).ok();
    let save_path = PathBuf::from(&save_dir).join("world.iv");
    println!("IRONVEIN seed up on 0.0.0.0:{port} — players join with: ironvein --join <this-ip>:{port}");
    println!("autosaving to {} every {}s", save_path.display(), AUTOSAVE_EVERY / 10);

    let mut last = Instant::now();
    let mut last_saved_tick = session.world.tick;
    let mut last_thought = u32::MAX;
    let mut last_report = Instant::now();
    loop {
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;

        if session.world.tick != last_thought {
            last_thought = session.world.tick;
            for b in bots.iter_mut() {
                for c in b.think(&session.world) {
                    session.queue_as(b.pid, c);
                }
            }
        }
        session.update(dt.min(0.25));

        let t = session.world.tick;
        if t / AUTOSAVE_EVERY != last_saved_tick / AUTOSAVE_EVERY {
            last_saved_tick = t;
            let bytes = session.world.save_bytes();
            let tmp = save_path.with_extension("iv.tmp");
            if fs::write(&tmp, &bytes).and_then(|_| fs::rename(&tmp, &save_path)).is_err() {
                eprintln!("autosave failed (tick {t})");
            }
        }
        if last_report.elapsed() > Duration::from_secs(30) {
            last_report = Instant::now();
            let settlers = session.world.players.iter().filter(|p| p.joined).count();
            println!(
                "tick {t} | {} peers online | {settlers} settlements | hash {:016x} | {}",
                session.peer_count(),
                session.world.hash(),
                session.status
            );
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}
