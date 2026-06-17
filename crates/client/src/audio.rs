//! audio.rs — the client's sound layer. Presentation only: it reacts to the
//! sim's `VisEvent` stream and never feeds back into it, so it can be fully
//! non-deterministic (volumes, throttles, wall-clock) without touching the
//! lockstep contract.
//!
//! wasm: a thin FFI to `js/ironvein_audio.js` (Web Audio). native: a `rodio`
//! backend that streams the same procedural assets straight off disk (it even
//! prefers the full-quality `.wav` soundtrack over the browser's compressed
//! `.ogg`). Asset paths are resolved relative to the working directory, same
//! as `saves/`.

#[cfg(target_arch = "wasm32")]
mod ffi {
    extern "C" {
        fn ivn_audio_load(np: *const u8, nl: usize, up: *const u8, ul: usize);
        fn ivn_audio_play(np: *const u8, nl: usize, vol: f32, looping: u32);
        fn ivn_audio_gain(np: *const u8, nl: usize, vol: f32);
    }
    pub fn load(name: &str, url: &str) {
        unsafe { ivn_audio_load(name.as_ptr(), name.len(), url.as_ptr(), url.len()) };
    }
    pub fn play(name: &str, vol: f32, looping: bool) {
        unsafe { ivn_audio_play(name.as_ptr(), name.len(), vol, looping as u32) };
    }
    pub fn gain(name: &str, vol: f32) {
        unsafe { ivn_audio_gain(name.as_ptr(), name.len(), vol) };
    }
    pub fn update() {} // JS loops the ambience itself; nothing to pump here
}

#[cfg(not(target_arch = "wasm32"))]
mod ffi {
    use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::BufReader;

    /// A looping bed: we keep two decoders queued in the sink at all times so
    /// the loop is gapless, re-streaming a fresh one off disk as each finishes
    /// (no need to buffer a 15-minute track in RAM).
    struct Bed {
        sink: Sink,
        path: String,
        name: String,
    }

    struct Native {
        _stream: OutputStream, // must stay alive for audio to play
        handle: OutputStreamHandle,
        paths: HashMap<String, String>,
        beds: Vec<Bed>,
    }

    thread_local! {
        static A: RefCell<Option<Native>> = RefCell::new(None);
    }

    /// Run `f` against the (lazily-initialised) audio state. Returns None if the
    /// output device couldn't be opened — audio is best-effort, never fatal.
    fn with<R>(f: impl FnOnce(&mut Native) -> R) -> Option<R> {
        A.with(|cell| {
            let mut b = cell.borrow_mut();
            if b.is_none() {
                match OutputStream::try_default() {
                    Ok((s, h)) => {
                        *b = Some(Native { _stream: s, handle: h, paths: HashMap::new(), beds: Vec::new() });
                    }
                    Err(_) => return None,
                }
            }
            b.as_mut().map(f)
        })
    }

    fn decode(path: &str) -> Option<Decoder<BufReader<File>>> {
        Decoder::new(BufReader::new(File::open(path).ok()?)).ok()
    }

    pub fn load(name: &str, url: &str) {
        // Native prefers the uncompressed .wav sibling if it's present.
        let mut path = url.to_string();
        if let Some(stem) = url.strip_suffix(".ogg") {
            let wav = format!("{stem}.wav");
            if std::path::Path::new(&wav).exists() {
                path = wav;
            }
        }
        with(|a| {
            a.paths.insert(name.to_string(), path);
        });
    }

    pub fn play(name: &str, vol: f32, looping: bool) {
        with(|a| {
            let Some(path) = a.paths.get(name).cloned() else { return };
            if looping {
                if let Ok(sink) = Sink::try_new(&a.handle) {
                    sink.set_volume(vol);
                    for _ in 0..2 {
                        if let Some(d) = decode(&path) {
                            sink.append(d);
                        }
                    }
                    a.beds.push(Bed { sink, path, name: name.to_string() });
                }
            } else if let (Some(d), Ok(sink)) = (decode(&path), Sink::try_new(&a.handle)) {
                sink.set_volume(vol);
                sink.append(d);
                sink.detach(); // play to completion, then free itself
            }
        });
    }

    pub fn gain(name: &str, vol: f32) {
        with(|a| {
            for bed in &a.beds {
                if bed.name == name {
                    bed.sink.set_volume(vol);
                }
            }
        });
    }

    /// Top up each looping bed so two clips are always queued (gapless). Call
    /// once a frame.
    pub fn update() {
        with(|a| {
            for bed in &a.beds {
                while bed.sink.len() < 2 {
                    match decode(&bed.path) {
                        Some(d) => bed.sink.append(d),
                        None => break,
                    }
                }
            }
        });
    }
}

/// The sound bank: (logical name, asset URL, mix volume). Names are what the
/// rest of the client refers to; URLs are served next to index.html.
const SOUNDS: &[(&str, &str, f32)] = &[
    ("music", "background_track.ogg", 0.34),
    ("wind", "sfx_ambient_wind.wav", 0.20),
    ("water", "sfx_ambient_water.wav", 0.16),
    ("explosion", "sfx_explosion.wav", 0.75),
    ("tank", "sfx_tank_shot.wav", 0.45),
    ("rifle", "sfx_rifle_burst.wav", 0.40),
    ("rocket", "sfx_rocket_launch.wav", 0.60),
    ("build", "sfx_building_construct.wav", 0.55),
    ("harvest", "sfx_farming_harvest.wav", 0.45),
    ("mine", "sfx_mining_pickaxe.wav", 0.50),
    ("radar", "sfx_radar_ping.wav", 0.35),
];

fn volume_of(name: &str) -> f32 {
    SOUNDS.iter().find(|(n, _, _)| *n == name).map(|(_, _, v)| *v).unwrap_or(0.5)
}

/// Sounds that belong to the "music" category (the rest are SFX). The music
/// slider controls these; the SFX slider controls everything else.
fn is_music(name: &str) -> bool {
    matches!(name, "music" | "wind" | "water")
}

// User-set mix levels (0..1), applied live from the settings panel. Stored as
// f32 bits in atomics so the free functions stay lock-free; defaults until the
// first `set_volumes` (which `main` calls before any sound starts).
use std::sync::atomic::{AtomicU32, Ordering};
static MASTER: AtomicU32 = AtomicU32::new(0x3F666666); // 0.9
static MUSIC: AtomicU32 = AtomicU32::new(0x3F333333); // 0.7
static SFX: AtomicU32 = AtomicU32::new(0x3F666666); // 0.9

fn level(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// The final mix for a sound = bank volume × its category slider × master.
fn mix(name: &str) -> f32 {
    let cat = if is_music(name) { level(&MUSIC) } else { level(&SFX) };
    volume_of(name) * cat * level(&MASTER)
}

/// Apply new mix levels and push them live to any running music/ambience.
pub fn set_volumes(master: f32, music: f32, sfx: f32) {
    MASTER.store(master.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    MUSIC.store(music.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    SFX.store(sfx.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    for name in ["music", "wind", "water"] {
        ffi::gain(name, mix(name));
    }
}

/// Kick off loading every asset (fire-and-forget; the JS decodes async and
/// `play` no-ops until a buffer is ready). Call once at startup.
pub fn preload() {
    for (name, url, _) in SOUNDS {
        ffi::load(name, url);
    }
}

/// Start the looping ambience + music bed. Safe to call once; the JS guards
/// against a buffer not being decoded yet (it auto-starts the loop on decode)
/// and against the autoplay lock (it resumes on the first user gesture).
pub fn start_ambience() {
    ffi::play("music", mix("music"), true);
    ffi::play("wind", mix("wind"), true);
    ffi::play("water", mix("water"), true);
}

/// Fire a one-shot at its mixed volume, scaled by `gain` (e.g. a big explosion
/// is louder). The JS dedupes a flood of identical hits in the same instant.
pub fn sfx(name: &str, gain: f32) {
    ffi::play(name, (mix(name) * gain).min(1.0), false);
}

/// Pump the audio backend once per frame (keeps native loops gapless; no-op on
/// wasm, where the browser loops them).
pub fn update() {
    ffi::update();
}

/// Toggling pick between two variant sounds, so repeated shots/harvests don't
/// sound like a copy machine. Client-side only — never feeds the sim.
pub fn sfx_either(a: &str, b: &str, gain: f32) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let pick = if N.fetch_add(1, Ordering::Relaxed) & 1 == 0 { a } else { b };
    sfx(pick, gain);
}
