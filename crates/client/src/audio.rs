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
        fn ivn_audio_load_pcm(np: *const u8, nl: usize, ptr: *const f32, total: usize, channels: u32, rate: u32);
        fn ivn_audio_play(np: *const u8, nl: usize, vol: f32, looping: u32);
        fn ivn_audio_gain(np: *const u8, nl: usize, vol: f32);
        fn ivn_audio_stop(np: *const u8, nl: usize);
        fn ivn_audio_have(np: *const u8, nl: usize) -> i32;
    }
    pub fn load(name: &str, url: &str) {
        unsafe { ivn_audio_load(name.as_ptr(), name.len(), url.as_ptr(), url.len()) };
    }
    pub fn load_pcm(name: &str, samples: &[f32], channels: u16, rate: u32) {
        unsafe {
            ivn_audio_load_pcm(name.as_ptr(), name.len(), samples.as_ptr(), samples.len(), channels as u32, rate)
        };
    }
    pub fn play(name: &str, vol: f32, looping: bool) {
        unsafe { ivn_audio_play(name.as_ptr(), name.len(), vol, looping as u32) };
    }
    pub fn gain(name: &str, vol: f32) {
        unsafe { ivn_audio_gain(name.as_ptr(), name.len(), vol) };
    }
    pub fn stop(name: &str) {
        unsafe { ivn_audio_stop(name.as_ptr(), name.len()) };
    }
    pub fn have(name: &str) -> bool {
        unsafe { ivn_audio_have(name.as_ptr(), name.len()) != 0 }
    }
    pub fn update() {} // JS loops the ambience itself; nothing to pump here
}

#[cfg(not(target_arch = "wasm32"))]
mod ffi {
    use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
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
        /// In-memory PCM loops (the procedurally-synthesised music layers):
        /// (interleaved samples, channels, sample rate). Played via
        /// `repeat_infinite`, so they need no disk re-stream and no top-up.
        pcm: HashMap<String, (std::sync::Arc<[f32]>, u16, u32)>,
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
                        *b = Some(Native {
                            _stream: s,
                            handle: h,
                            paths: HashMap::new(),
                            pcm: HashMap::new(),
                            beds: Vec::new(),
                        });
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

    /// Register an in-memory PCM loop (a synthesised music layer). Played by
    /// `play(name, .., true)` like any other looping bed.
    pub fn load_pcm(name: &str, samples: &[f32], channels: u16, rate: u32) {
        with(|a| {
            a.pcm.insert(name.to_string(), (samples.into(), channels, rate));
        });
    }

    pub fn play(name: &str, vol: f32, looping: bool) {
        with(|a| {
            // Synthesised PCM (music layers + every SFX) takes priority over any
            // disk asset — loops repeat forever, one-shots play once and free.
            if let Some((samples, ch, rate)) = a.pcm.get(name).cloned() {
                if let Ok(sink) = Sink::try_new(&a.handle) {
                    sink.set_volume(vol);
                    let buf = rodio::buffer::SamplesBuffer::new(ch, rate, samples.to_vec());
                    if looping {
                        sink.append(buf.repeat_infinite());
                        a.beds.push(Bed { sink, path: String::new(), name: name.to_string() });
                    } else {
                        sink.append(buf);
                        sink.detach();
                    }
                }
                return;
            }
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

    pub fn stop(name: &str) {
        with(|a| a.beds.retain(|b| b.name != name)); // dropping the Bed stops its sink
    }

    /// Native decides adaptive up front from the filesystem, so this just reports
    /// whether the asset path is known (used only to keep the wasm poll logic
    /// uniform — on native ADAPTIVE is already settled before the poll runs).
    pub fn have(name: &str) -> bool {
        with(|a| a.paths.contains_key(name)).unwrap_or(false)
    }

    /// Top up each looping bed so two clips are always queued (gapless). Call
    /// once a frame.
    pub fn update() {
        with(|a| {
            for bed in &a.beds {
                if bed.path.is_empty() {
                    continue; // PCM loop (repeat_infinite) — never needs topping up
                }
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
    // No master music loop: the soundtrack is the procedural/stem layers (see
    // `preload`/`crate::music`). Only the small ambience + SFX live here.
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

/// Adaptive-music stems (from the procedural composer's stem export). All are the
/// same length and perfectly loop-aligned, so playing them in sync reconstructs
/// the master mix — and riding each one's gain by the game's "intensity" turns
/// the soundtrack interactive. (name, file, base mix vol, fade-in intensity,
/// full-volume intensity). pads are the always-on bed; psycho_tension only roars
/// in when a boss walks or you're being overrun.
const STEMS: &[(&str, &str, f32, f32, f32)] = &[
    ("st_pads", "stem_pads.ogg", 0.42, 0.0, 0.0),
    ("st_bass", "stem_bass.ogg", 0.40, 0.12, 0.40),
    ("st_kick", "stem_kick.ogg", 0.42, 0.20, 0.46),
    ("st_snare", "stem_snare_hats.ogg", 0.34, 0.28, 0.52),
    ("st_arps", "stem_arps_lead.ogg", 0.34, 0.42, 0.70),
    ("st_guitars", "stem_guitars.ogg", 0.40, 0.50, 0.76),
    ("st_psycho", "stem_psycho_tension.ogg", 0.46, 0.66, 0.92),
];

use std::sync::atomic::AtomicBool;
static ADAPTIVE: AtomicBool = AtomicBool::new(false);
/// True while the title-screen theme is looping (so `set_volumes` can ride it).
static TITLE_ON: AtomicBool = AtomicBool::new(false);
/// True while the player is in the netherealm — the nether bed plays and the
/// overworld stems duck to silence (crossfaded by `pump_nether`/`set_music_intensity`).
static NETHER_ON: AtomicBool = AtomicBool::new(false);
static NETHER_GAIN: AtomicU32 = AtomicU32::new(0); // current ramped gain (f32 bits)
// per-stem current gain (f32 bits), ramped toward target each frame for smooth fades
static STEM_GAIN: [AtomicU32; 7] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

/// Are all the music stems present on disk? Adaptive music needs the full set.
/// Native only — the browser can't stat files, so it fetches them and decides at
/// runtime once they decode (see `poll_adaptive`).
#[cfg(not(target_arch = "wasm32"))]
fn stems_available() -> bool {
    STEMS.iter().all(|(_, f, _, _, _)| {
        std::path::Path::new(f).exists()
            || f.strip_suffix(".ogg").map(|s| std::path::Path::new(&format!("{s}.wav")).exists()).unwrap_or(false)
    })
}

fn volume_of(name: &str) -> f32 {
    SOUNDS.iter().find(|(n, _, _)| *n == name).map(|(_, _, v)| *v).unwrap_or(0.5)
}

/// Sounds that belong to the "music" category (the rest are SFX). The music
/// slider controls these; the SFX slider controls everything else.
fn is_music(name: &str) -> bool {
    matches!(name, "wind" | "water")
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
    // The music slider also rides the synth/stem layers, but those are ramped
    // every frame by `set_music_intensity`, so we only push the ambience here.
    for name in ["wind", "water"] {
        ffi::gain(name, mix(name));
    }
    // The menu's title theme is music too — keep it live with the sliders.
    if TITLE_ON.load(Ordering::Relaxed) {
        ffi::gain("title", title_gain());
    }
}

/// Load the cheap assets: the SFX one-shots + the wind/water ambience, all
/// synthesised in milliseconds. The (heavier) music engine is deferred to
/// `prepare_music` so startup isn't blocked. Call once at startup.
pub fn preload() {
    for (name, samples, ch, rate) in crate::music::generate_sfx() {
        ffi::load_pcm(name, &samples, ch, rate);
    }
}

/// Synthesise (or fetch) the music layers and start them looping. This is the
/// expensive step — the full physical-modelling engine takes ~2s — so the caller
/// shows a splash and calls it AFTER the first frame is on screen, not inside
/// `preload`. Idempotent-ish: safe to call once.
pub fn prepare_music() {
    // "both: procedural default, oggs if present" — prefer shipped ogg stems when
    // we can see them (native filesystem); the browser goes fully procedural so a
    // colony loads with zero audio download.
    #[cfg(not(target_arch = "wasm32"))]
    let use_oggs = stems_available();
    #[cfg(target_arch = "wasm32")]
    let use_oggs = false;

    if use_oggs {
        for (name, file, _, _, _) in STEMS {
            ffi::load(name, file);
        }
    } else {
        // the ported synth3.py engine — KS guitars, hypersaw, analog bass, pads…
        for (name, samples, ch, rate) in crate::music::generate_layers() {
            ffi::load_pcm(name, &samples, ch, rate);
        }
    }
    // start each layer looping in lockstep at silence; set_music_intensity rides
    // them up. (They share one loop length, so they stay phase-locked = full mix.)
    for (name, _f, _b, _fi, _fu) in STEMS {
        ffi::play(name, 0.0, true);
    }
    ADAPTIVE.store(true, Ordering::Relaxed);
}

/// The title-screen theme's playback gain — it's music, so it rides the music and
/// master sliders (the render is already mixed soft/bedded).
fn title_gain() -> f32 {
    level(&MUSIC) * level(&MASTER)
}

/// Synthesise the title-screen theme and start it looping. Composed after the
/// gameplay soundtrack (`prepare_music`); call once, before the menu. `stop_title`
/// tears it down when a game begins.
pub fn prepare_title() {
    let (samples, ch, rate) = crate::music::generate_title();
    ffi::load_pcm("title", &samples, ch, rate);
    ffi::play("title", title_gain(), true);
    TITLE_ON.store(true, Ordering::Relaxed);
}

/// Stop the title-screen theme (the game soundtrack takes over from here).
pub fn stop_title() {
    if TITLE_ON.swap(false, Ordering::Relaxed) {
        ffi::stop("title");
    }
}

/// Synthesise the netherealm bed and start it looping at silence. It rides up
/// (and the overworld stems duck) when the player descends. Composed at startup
/// alongside the rest so the descent never hitches. Call once.
pub fn prepare_nether() {
    let (samples, ch, rate) = crate::music::generate_nether();
    ffi::load_pcm("nether", &samples, ch, rate);
    ffi::play("nether", 0.0, true);
}

/// Tell the audio layer whether the player is in the netherealm. The crossfade
/// (nether bed up, stems down) is then ramped each frame by `pump_nether` +
/// `set_music_intensity`. Cheap; call every frame.
pub fn set_nether(active: bool) {
    NETHER_ON.store(active, Ordering::Relaxed);
}

/// Ramp the netherealm bed toward its target (full music level in the nether,
/// silent otherwise) — a smooth crossfade. Call every frame.
pub fn pump_nether() {
    let target = if NETHER_ON.load(Ordering::Relaxed) { level(&MUSIC) * level(&MASTER) } else { 0.0 };
    let cur = f32::from_bits(NETHER_GAIN.load(Ordering::Relaxed));
    let nv = cur + (target - cur) * 0.03; // ~0.5s glide — an ominous fade
    NETHER_GAIN.store(nv.to_bits(), Ordering::Relaxed);
    ffi::gain("nether", nv);
}

/// Browser-side upgrade: once every stem has fetched + decoded, crossfade from
/// the master loop to the layered adaptive soundtrack. No-op once adaptive (and a
/// no-op on native, where the choice was settled at preload). Call once a frame.
pub fn poll_adaptive() {
    if ADAPTIVE.load(Ordering::Relaxed) {
        return;
    }
    if !STEMS.iter().all(|(name, _, _, _, _)| ffi::have(name)) {
        return;
    }
    ffi::stop("music"); // drop the master loop
    for (name, _f, _b, _fi, _fu) in STEMS {
        ffi::play(name, 0.0, true); // bring the stems up in sync (gain rides intensity)
    }
    ADAPTIVE.store(true, Ordering::Relaxed);
}

/// True when the layered-stem soundtrack is in use (vs. the single master loop).
pub fn adaptive_music() -> bool {
    ADAPTIVE.load(Ordering::Relaxed)
}

/// Drive the interactive soundtrack: `x` in 0..1 is the current battle intensity.
/// Each stem fades in across its threshold band and is ramped smoothly so the mix
/// breathes instead of snapping. No-op unless the stems are loaded. Call per frame.
pub fn set_music_intensity(x: f32) {
    if !ADAPTIVE.load(Ordering::Relaxed) {
        return;
    }
    let x = x.clamp(0.0, 1.0);
    // in the netherealm the overworld stems duck to silence (the nether bed takes over)
    let m = if NETHER_ON.load(Ordering::Relaxed) { 0.0 } else { level(&MUSIC) * level(&MASTER) };
    for (i, (name, _f, base, fade_in, full)) in STEMS.iter().enumerate() {
        let tier = if full <= fade_in { 1.0 } else { ((x - fade_in) / (full - fade_in)).clamp(0.0, 1.0) };
        let target = base * tier * m;
        let cur = f32::from_bits(STEM_GAIN[i].load(Ordering::Relaxed));
        let nv = cur + (target - cur) * 0.06; // ~0.25s glide at 60fps
        STEM_GAIN[i].store(nv.to_bits(), Ordering::Relaxed);
        ffi::gain(name, nv);
    }
}

/// Start the looping ambience + music bed. Safe to call once; the JS guards
/// against a buffer not being decoded yet (it auto-starts the loop on decode)
/// and against the autoplay lock (it resumes on the first user gesture).
pub fn start_ambience() {
    // The music layers are started by `prepare_music`; here we only bring up the
    // (cheap) wind/water ambience so the menu isn't silent before music is ready.
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
