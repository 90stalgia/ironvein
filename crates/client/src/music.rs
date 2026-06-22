//! music.rs — procedural audio. The whole game is asset-free, so the sound is
//! too: nothing is fetched or shipped, everything is synthesised in-memory at
//! startup.
//!
//!  * **Music** — `generate_layers()` delegates to `crate::synth`, a Rust port of
//!    the player's `synth3.py`: Karplus-Strong power-chord guitars, hypersaw
//!    lead/arps, analog bass, formant pads, AM tension drone, and a synth drum
//!    kit, rendered as the 7 loop-aligned adaptive stems.
//!  * **SFX + ambience** — `generate_sfx()` below synthesises every one-shot
//!    (gunfire / explosions / build / harvest / radar) and the wind/water loops.
//!
//! Everything is one loop long and seam-de-clicked so the layers stay
//! phase-locked and never click at the wrap.

const SR: usize = 22050; // plenty for atmospheric game audio; halves memory/CPU

/// A tiny deterministic LCG for the SFX noise/variation (same shot every run).
struct Rng(u32);
impl Rng {
    fn f(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0 // -1..1
    }
}

/// The adaptive music layers — produced by the full physical-modelling engine
/// ported from the player's `synth3.py` (see `crate::synth`). Names match the
/// audio stem set so the adaptive intensity system drives them unchanged.
pub fn generate_layers() -> Vec<(&'static str, Vec<f32>, u16, u32)> {
    crate::synth::render_stems()
}

/// The title-screen theme — one self-contained looping stereo track (interleaved
/// f32 PCM), a softer/bedded take on the soundtrack's LIFT hook. Played on the menu
/// (see `audio::prepare_title`), composed after the adaptive stems.
pub fn generate_title() -> (Vec<f32>, u16, u32) {
    crate::synth::render_title()
}

/// The netherealm bed — one looping stereo track of dread, played in-game while
/// `realm == Nether` (see `audio::prepare_nether`). Spooky/haunting, not a tune.
pub fn generate_nether() -> (Vec<f32>, u16, u32) {
    crate::synth::render_nether()
}

// ---------------------------------------------------------------------------
// Sound effects — same philosophy as the music: nothing is fetched, every shot,
// explosion, clink and gust is SYNTHESISED here at startup. Mono, 22050 Hz.
// ---------------------------------------------------------------------------

/// Short attack ramp so a one-shot doesn't click on its first sample.
fn attack(buf: &mut [f32], n: usize) {
    let a = n.min(buf.len());
    for (i, s) in buf.iter_mut().take(a).enumerate() {
        *s *= i as f32 / a as f32;
    }
}

/// Peak-normalise to `peak` (avoids surprises when I retune a generator).
fn norm(buf: &mut [f32], peak: f32) {
    let p = buf.iter().fold(0.0f32, |a, &b| a.max(b.abs())).max(1e-4);
    let g = peak / p;
    for s in buf.iter_mut() {
        *s *= g;
    }
}

fn secs(s: f32) -> usize {
    (s * SR as f32) as usize
}

fn rifle() -> Vec<f32> {
    let mut rng = Rng(0x9e37);
    let mut o = vec![0.0f32; secs(0.08)];
    let mut lp = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let n = rng.f();
        lp += (n - lp) * 0.65; // bright noise
        *s = (lp * 0.8 + n * 0.2) * (-t * 70.0).exp();
    }
    attack(&mut o, 8);
    norm(&mut o, 0.8);
    o
}

fn tank() -> Vec<f32> {
    let mut rng = Rng(0x51ed);
    let mut o = vec![0.0f32; secs(0.30)];
    let mut ph = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let f = 65.0 + 130.0 * (-t * 32.0).exp(); // boom with a pitch drop
        ph += f / SR as f32;
        let body = (ph * std::f32::consts::TAU).sin() * (-t * 11.0).exp();
        let click = rng.f() * (-t * 130.0).exp() * 0.7; // attack transient
        *s = body * 0.85 + click;
    }
    attack(&mut o, 6);
    norm(&mut o, 0.9);
    o
}

fn explosion() -> Vec<f32> {
    let mut rng = Rng(0xb105);
    let mut o = vec![0.0f32; secs(0.8)];
    let mut lp = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let cut = 0.5 * (-t * 4.5).exp() + 0.04; // bright blast that dulls
        let n = rng.f();
        lp += (n - lp) * cut;
        let sub = (2.0 * std::f32::consts::PI * 44.0 * t).sin() * (-t * 6.0).exp(); // body
        let crackle = n * (-t * 30.0).exp() * 0.4;
        *s = (lp * 0.9 + sub * 0.7 + crackle) * (-t * 3.2).exp();
    }
    attack(&mut o, 6);
    norm(&mut o, 0.95);
    o
}

fn rocket() -> Vec<f32> {
    let mut rng = Rng(0x2c0f);
    let dur = secs(0.5);
    let mut o = vec![0.0f32; dur];
    let mut lp = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let n = rng.f();
        let cut = 0.06 + 0.5 * t;
        lp += (n - lp) * cut;
        let hp = n - lp; // hiss climbing in pitch
        let roar = (2.0 * std::f32::consts::PI * (55.0 + 70.0 * t) * t).sin();
        let env = (t * 12.0).min(1.0) * (1.0 - (i as f32 / dur as f32)).powf(0.6);
        *s = (hp * 0.6 + roar * 0.35) * env;
    }
    norm(&mut o, 0.85);
    o
}

fn build() -> Vec<f32> {
    let mut rng = Rng(0x7a11);
    let mut o = vec![0.0f32; secs(0.36)];
    // two mechanical thunks + a metallic tick
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let thunk = |t0: f32, f: f32| -> f32 {
            if t < t0 {
                return 0.0;
            }
            let d = t - t0;
            let pf = f - 25.0 * (1.0 - (-d * 30.0).exp());
            (2.0 * std::f32::consts::PI * pf * d).sin() * (-d * 16.0).exp()
        };
        let tick = rng.f() * (-t * 90.0).exp() * 0.3;
        *s = thunk(0.0, 120.0) * 0.8 + thunk(0.13, 90.0) * 0.6 + tick;
    }
    attack(&mut o, 8);
    norm(&mut o, 0.8);
    o
}

fn harvest() -> Vec<f32> {
    let mut rng = Rng(0x44ce);
    let mut o = vec![0.0f32; secs(0.2)];
    let mut lp = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let cut = 0.6 * (-t * 9.0).exp() + 0.05; // a downward swish
        lp += (rng.f() - lp) * cut;
        *s = lp * (-t * 15.0).exp();
    }
    attack(&mut o, 8);
    norm(&mut o, 0.7);
    o
}

fn mine() -> Vec<f32> {
    let mut rng = Rng(0x9b27);
    let mut o = vec![0.0f32; secs(0.26)];
    // inharmonic metallic ring + a noise tick = pickaxe clink
    let partials = [(1850.0f32, 1.0f32, 13.0f32), (2640.0, 0.6, 18.0), (3510.0, 0.4, 24.0)];
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let mut v = 0.0;
        for (f, a, d) in partials {
            v += a * (2.0 * std::f32::consts::PI * f * t).sin() * (-t * d).exp();
        }
        let tick = rng.f() * (-t * 200.0).exp() * 0.4;
        *s = v + tick;
    }
    attack(&mut o, 4);
    norm(&mut o, 0.7);
    o
}

fn radar() -> Vec<f32> {
    let mut o = vec![0.0f32; secs(0.42)];
    let ping = |t: f32, t0: f32| -> f32 {
        if t < t0 {
            return 0.0;
        }
        let d = t - t0;
        (2.0 * std::f32::consts::PI * 1180.0 * d).sin() * (-d * 13.0).exp()
    };
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        *s = ping(t, 0.0) + ping(t, 0.14) * 0.45; // blip + echo
    }
    attack(&mut o, 6);
    norm(&mut o, 0.7);
    o
}

/// A seam-de-clicked looping bed (wind/water): faded ends so the wrap is silent.
fn declick_loop(mut o: Vec<f32>, peak: f32) -> Vec<f32> {
    norm(&mut o, peak);
    let fade = secs(0.03);
    let n = o.len();
    for f in 0..fade.min(n / 2) {
        let w = f as f32 / fade as f32;
        o[f] *= w;
        o[n - 1 - f] *= w;
    }
    o
}

fn wind() -> Vec<f32> {
    let mut rng = Rng(0x1357);
    let mut o = vec![0.0f32; secs(4.0)];
    let mut lp = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let cut = 0.02 + 0.014 * (2.0 * std::f32::consts::PI * 0.13 * t).sin();
        lp += (rng.f() - lp) * cut.max(0.004);
        let amp = 0.55 + 0.45 * (2.0 * std::f32::consts::PI * 0.07 * t).sin();
        *s = lp * amp;
    }
    declick_loop(o, 0.5)
}

fn water() -> Vec<f32> {
    // A soft continuous shoreline wash — low, band-limited noise gently swelling.
    // (No discrete bubbles: those read as an annoying drip on loop.)
    let mut rng = Rng(0x2468);
    let mut o = vec![0.0f32; secs(4.0)];
    let mut lp = 0.0f32;
    let mut lp2 = 0.0f32;
    for (i, s) in o.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        lp += (rng.f() - lp) * 0.04;
        lp2 += (lp - lp2) * 0.5; // a touch brighter than pure rumble
        let swell = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 0.09 * t).sin();
        *s = lp2 * swell;
    }
    declick_loop(o, 0.4)
}

/// Every sound effect + ambient loop as (name, mono PCM, channels=1, rate). The
/// names match `audio::SOUNDS` so the existing mix volumes/throttling apply.
pub fn generate_sfx() -> Vec<(&'static str, Vec<f32>, u16, u32)> {
    let r = SR as u32;
    vec![
        ("rifle", rifle(), 1, r),
        ("tank", tank(), 1, r),
        ("explosion", explosion(), 1, r),
        ("rocket", rocket(), 1, r),
        ("build", build(), 1, r),
        ("harvest", harvest(), 1, r),
        ("mine", mine(), 1, r),
        ("radar", radar(), 1, r),
        ("wind", wind(), 1, r),
        ("water", water(), 1, r),
    ]
}

/// Write interleaved-stereo f32 PCM to a 16-bit WAV. Native only.
#[cfg(not(target_arch = "wasm32"))]
fn write_wav_stereo(path: &str, interleaved: &[f32], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let mut data: Vec<u8> = Vec::with_capacity(interleaved.len() * 2);
    for &s in interleaved {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        data.extend_from_slice(&v.to_le_bytes());
    }
    let n = data.len() as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + n).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&2u16.to_le_bytes())?; // stereo
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 4).to_le_bytes())?; // byte rate (2ch * 2bytes)
    f.write_all(&4u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits/sample
    f.write_all(b"data")?;
    f.write_all(&n.to_le_bytes())?;
    f.write_all(&data)?;
    Ok(())
}

/// Render the full music loop (all stems summed) to a 16-bit stereo WAV — an
/// audition aid for the procedural soundtrack. Native only.
#[cfg(not(target_arch = "wasm32"))]
pub fn render_wav(path: &str) -> std::io::Result<()> {
    // The full composed master (summed stems + analog console/tape glue), matching
    // synth3.py's MASTER_MIX — a truer audition than a flat stem sum.
    let (buf, _ch, rate) = crate::synth::render_master();
    write_wav_stereo(path, &buf, rate)
}

/// Audition the title-screen theme as a 16-bit stereo WAV. Native only.
#[cfg(not(target_arch = "wasm32"))]
pub fn render_title_wav(path: &str) -> std::io::Result<()> {
    let (buf, _ch, rate) = generate_title();
    write_wav_stereo(path, &buf, rate)
}

/// Audition the netherealm bed as a 16-bit stereo WAV. Native only.
#[cfg(not(target_arch = "wasm32"))]
pub fn render_nether_wav(path: &str) -> std::io::Result<()> {
    let (buf, _ch, rate) = generate_nether();
    write_wav_stereo(path, &buf, rate)
}

/// Export each of the 7 stems the realtime engine produces as its own WAV, named
/// to match the Python `synth3.py` output (`stem_kick.wav`, `stem_snare_hats.wav`,
/// …) so they can be A/B'd against the originals. These are the *final* stems the
/// game actually plays (post tanh-glue / normalise / per-stem space). Native only.
///
/// Note for comparison: the Rust loop is 22.05 kHz and 20s (one 32-beat section);
/// the Python stems are 44.1 kHz and 15 min. Same instruments/notes, different
/// rate + length.
#[cfg(not(target_arch = "wasm32"))]
pub fn render_stems_to_dir(dir: &str) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(dir)?;
    fn py_name(n: &str) -> &str {
        match n {
            "st_kick" => "stem_kick",
            "st_snare" => "stem_snare_hats",
            "st_bass" => "stem_bass",
            "st_guitars" => "stem_guitars",
            "st_pads" => "stem_pads",
            "st_arps" => "stem_arps_lead",
            "st_psycho" => "stem_psycho_tension",
            other => other,
        }
    }
    let mut written = Vec::new();
    for (name, buf, _ch, rate) in generate_layers() {
        let path = format!("{}/{}.wav", dir.trim_end_matches('/'), py_name(name));
        write_wav_stereo(&path, &buf, rate)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_loop_aligned_finite_and_audible() {
        let layers = generate_layers();
        assert_eq!(layers.len(), 7, "expect all seven stem layers");
        let len = layers[0].1.len();
        // The music stems are rendered by `crate::synth` at its own rate (44.1 kHz,
        // matched to the Python); `SR` here is the SFX rate. Only require the stems
        // to agree with each other so the adaptive layers stay phase-locked.
        let rate0 = layers[0].3;
        assert!(len > 0);
        for (name, buf, ch, rate) in &layers {
            assert_eq!(*ch, 2, "{name} stereo");
            assert_eq!(*rate, rate0, "{name} stem rate differs from the others");
            assert_eq!(buf.len(), len, "{name} must share the loop length (phase-locked)");
            let mut peak = 0.0f32;
            let mut energy = 0.0f64;
            for &s in buf {
                assert!(s.is_finite(), "{name} produced a non-finite sample");
                peak = peak.max(s.abs());
                energy += (s as f64) * (s as f64);
            }
            assert!(peak <= 1.0, "{name} peak {peak} would clip");
            assert!(energy > 1.0, "{name} is silent (energy {energy})");
        }
    }

    #[test]
    fn sfx_are_finite_audible_and_unclipped() {
        let sfx = generate_sfx();
        let names: Vec<_> = sfx.iter().map(|(n, ..)| *n).collect();
        for want in ["rifle", "tank", "explosion", "rocket", "build", "harvest", "mine", "radar", "wind", "water"] {
            assert!(names.contains(&want), "missing sfx {want}");
        }
        for (name, buf, ch, _) in &sfx {
            assert_eq!(*ch, 1, "{name} is mono");
            assert!(!buf.is_empty());
            let mut peak = 0.0f32;
            let mut energy = 0.0f64;
            for &s in buf {
                assert!(s.is_finite(), "{name} non-finite");
                peak = peak.max(s.abs());
                energy += (s as f64) * (s as f64);
            }
            assert!(peak <= 1.0, "{name} clips ({peak})");
            assert!(energy > 0.5, "{name} silent");
        }
    }

    #[test]
    fn ambient_loops_have_quiet_seams() {
        for (name, buf, _, _) in generate_sfx().into_iter().filter(|(n, ..)| *n == "wind" || *n == "water") {
            let n = buf.len();
            assert!(buf[0].abs() < 0.05 && buf[n - 1].abs() < 0.05, "{name} loop seam not silent");
        }
    }
}
