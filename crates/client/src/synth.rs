//! synth.rs — a faithful Rust port of the player's `synth3.py` procedural music
//! engine. Same instruments, same E-minor drone material, AND the same **section
//! arrangement** (`SOFT_VERSE → GROOVE → GROOVE_ARPS → TENSION → LIFT`) — that
//! quiet/loud/quiet dynamic IS the piece, so we render the composed master mix
//! (with the real cabinet-IR + SM57 guitar chain and the analog master glue), not
//! a single looping section.
//!
//! Departures from the Python, all for runtime cost: the dispersive Karplus-Strong
//! guitars run as real-time delay lines (integer delay → frac-poly fractional FIR →
//! allpass dispersion cascade → `sustain`, O(samples)) instead of scipy's
//! giant-denominator `lfilter`, but reproduce the same per-string stiffness, the
//! two-pass bridge coupling, and the excitation/swell; we render a representative
//! ~110-beat arc of the structure (≈70s loop), not the full 15 minutes; and the
//! guitar cabinet IR is baked from the reference (see `cabinet_ir`) because its tone
//! is too sensitive to scipy's exact `filtfilt` edge numerics to recompute cheaply.
//! Everything else — the body/pickup resonances, tube drive, power-amp sag, speaker
//! breakup, FFT cabinet convolution, SM57 transformer, per-stem delay/reverb,
//! console crosstalk, tape wow/flutter, ducking hiss and master saturation — is
//! ported. The numpy RNG is not reproduced bit-for-bit (it only drives
//! humanisation), so beat-for-beat sample identity isn't a goal; timbral identity is.

use std::f64::consts::PI;
use std::sync::OnceLock;

// Baked guitar cabinet impulse response, generated from synth3.py (seed 42). See
// `cabinet_ir()` for why this is a constant rather than computed at runtime.
include!("synth_ir.rs");

const SR: usize = 44_100; // match the Python so cutoffs / IR lengths behave identically
const BPM: f64 = 96.0;

fn beat() -> f64 {
    60.0 / BPM
}
fn samples_of(dur_beats: f64) -> usize {
    (dur_beats * beat() * SR as f64) as usize
}

// One representative pass of the SONG_STRUCTURE: quiet verse → groove → arps →
// tension build → loud lift, then it loops back to the verse (the dynamic reset).
const STRUCTURE: &[(&str, usize)] =
    &[("SOFT_VERSE", 32), ("GROOVE", 16), ("GROOVE_ARPS", 24), ("TENSION", 16), ("LIFT", 24)];
fn total_beats() -> usize {
    STRUCTURE.iter().map(|(_, n)| n).sum()
}
fn master_samples() -> usize {
    samples_of(total_beats() as f64)
}

// ---------------------------------------------------------------------------
// RNG (humanisation only — needn't match numpy)
// ---------------------------------------------------------------------------
struct Rng {
    s: u64,
}
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng { s: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0xDA3E_39CB_94B9_5BDB) | 1 }
    }
    fn next(&mut self) -> u64 {
        let mut x = self.s;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.s = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.uniform()
    }
    fn normal(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

// ---------------------------------------------------------------------------
// Stereo buffer
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct Stereo {
    l: Vec<f64>,
    r: Vec<f64>,
}
impl Stereo {
    fn zeros(n: usize) -> Stereo {
        Stereo { l: vec![0.0; n], r: vec![0.0; n] }
    }
    fn mono(m: Vec<f64>) -> Stereo {
        Stereo { l: m.clone(), r: m }
    }
    fn len(&self) -> usize {
        self.l.len()
    }
    fn scale(&mut self, g: f64) {
        for v in self.l.iter_mut() {
            *v *= g;
        }
        for v in self.r.iter_mut() {
            *v *= g;
        }
    }
}

// ---------------------------------------------------------------------------
// DSP primitives (the scipy.signal subset the synth uses)
// ---------------------------------------------------------------------------
fn sawtooth(phase: f64, width: f64) -> f64 {
    let mut t = phase / (2.0 * PI);
    t -= t.floor();
    if width >= 0.999 {
        2.0 * t - 1.0
    } else if t < width {
        2.0 * t / width - 1.0
    } else {
        2.0 * (1.0 - t) / (1.0 - width) - 1.0
    }
}

fn poly_mul(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; a.len() + b.len() - 1];
    for (i, &av) in a.iter().enumerate() {
        for (j, &bv) in b.iter().enumerate() {
            out[i + j] += av * bv;
        }
    }
    out
}

fn lfilter(b: &[f64], a: &[f64], x: &[f64]) -> Vec<f64> {
    let a0 = a[0];
    let nb: Vec<f64> = b.iter().map(|v| v / a0).collect();
    let na: Vec<f64> = a.iter().map(|v| v / a0).collect();
    let order = nb.len().max(na.len());
    let mut z = vec![0.0; order];
    let mut y = vec![0.0; x.len()];
    for i in 0..x.len() {
        let xn = x[i];
        let yn = nb[0] * xn + z[0];
        for j in 1..order {
            let bj = nb.get(j).copied().unwrap_or(0.0);
            let aj = na.get(j).copied().unwrap_or(0.0);
            z[j - 1] = bj * xn + z[j] - aj * yn;
        }
        y[i] = yn;
    }
    y
}

fn filtfilt(b: &[f64], a: &[f64], x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let pad = (3 * b.len().max(a.len())).min(n.saturating_sub(1));
    let run = |sig: &[f64]| -> Vec<f64> {
        let f = lfilter(b, a, sig);
        let mut rev: Vec<f64> = f.into_iter().rev().collect();
        rev = lfilter(b, a, &rev);
        rev.reverse();
        rev
    };
    if n < 2 || pad == 0 {
        return run(x);
    }
    let mut ext = Vec::with_capacity(n + 2 * pad);
    for i in (1..=pad).rev() {
        ext.push(2.0 * x[0] - x[i]);
    }
    ext.extend_from_slice(x);
    for i in 1..=pad {
        ext.push(2.0 * x[n - 1] - x[n - 1 - i]);
    }
    let y = run(&ext);
    y[pad..pad + n].to_vec()
}

fn butter(order: usize, wn: f64, hp: bool) -> (Vec<f64>, Vec<f64>) {
    let wn = wn.clamp(1e-4, 0.999);
    let w0 = PI * wn;
    let (cw, sw) = (w0.cos(), w0.sin());
    let mut b = vec![1.0];
    let mut a = vec![1.0];
    for k in 0..order / 2 {
        // Butterworth biquad pole angle from the negative real axis. For even order
        // the pairs sit at (2k+1)·π/(2n); for odd order one pole is real and the
        // pairs are at (k+1)·π/n — getting this wrong detunes the section's Q.
        let angle = if order % 2 == 0 {
            PI * (2.0 * k as f64 + 1.0) / (2.0 * order as f64)
        } else {
            PI * (k as f64 + 1.0) / order as f64
        };
        let q = 1.0 / (2.0 * angle.cos());
        let alpha = sw / (2.0 * q);
        let (mut sb, sa) = if hp {
            (vec![(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0], vec![1.0 + alpha, -2.0 * cw, 1.0 - alpha])
        } else {
            (vec![(1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0], vec![1.0 + alpha, -2.0 * cw, 1.0 - alpha])
        };
        let a0 = sa[0];
        for v in sb.iter_mut() {
            *v /= a0;
        }
        let sa: Vec<f64> = sa.iter().map(|v| v / a0).collect();
        b = poly_mul(&b, &sb);
        a = poly_mul(&a, &sa);
    }
    if order % 2 == 1 {
        let kk = (w0 / 2.0).tan();
        let (sb, sa) = if hp {
            (vec![1.0 / (1.0 + kk), -1.0 / (1.0 + kk)], vec![1.0, (kk - 1.0) / (1.0 + kk)])
        } else {
            (vec![kk / (1.0 + kk), kk / (1.0 + kk)], vec![1.0, (kk - 1.0) / (1.0 + kk)])
        };
        b = poly_mul(&b, &sb);
        a = poly_mul(&a, &sa);
    }
    (b, a)
}

/// `filter_audio` from the Python.
fn filt(x: &[f64], cutoff_hz: f64, hp: bool, order: usize) -> Vec<f64> {
    if x.len() < 15 {
        return x.to_vec();
    }
    let wn = cutoff_hz / (SR as f64 / 2.0);
    let (b, a) = butter(order, wn, hp);
    filtfilt(&b, &a, x)
}
fn lp(x: &[f64], c: f64) -> Vec<f64> {
    filt(x, c, false, 2)
}

/// scipy.signal.iirpeak applied causally (lfilter).
fn iirpeak(x: &[f64], freq: f64, q: f64) -> Vec<f64> {
    let w0 = (freq / (SR as f64 / 2.0)).clamp(1e-4, 0.999) * PI;
    let beta = (w0 / q / 2.0).tan();
    let gain = 1.0 / (1.0 + beta);
    let b = [1.0 - gain, 0.0, -(1.0 - gain)];
    let a = [1.0, -2.0 * gain * w0.cos(), 2.0 * gain - 1.0];
    lfilter(&b, &a, x)
}

fn envelope(buf: &mut [f64], a: f64, d: f64, s: f64, r: f64) {
    let len = buf.len();
    let (mut ai, mut di, mut ri) = ((a * SR as f64) as usize, (d * SR as f64) as usize, (r * SR as f64) as usize);
    if ai + di + ri > len {
        ai = len / 20;
        di = len / 10;
        ri = len / 10;
    }
    let si = len.saturating_sub(ai + di + ri);
    let mut idx = 0;
    for i in 0..ai {
        let v = i as f64 / ai.max(1) as f64;
        buf[idx] *= v * v;
        idx += 1;
    }
    for i in 0..di {
        buf[idx] *= 1.0 + (s - 1.0) * (i as f64 / di.max(1) as f64);
        idx += 1;
    }
    for _ in 0..si {
        buf[idx] *= s;
        idx += 1;
    }
    for i in 0..ri {
        let v = s * (1.0 - i as f64 / ri.max(1) as f64);
        buf[idx] *= v * v;
        idx += 1;
    }
}
fn envelope_st(s: &mut Stereo, a: f64, d: f64, sus: f64, r: f64) {
    envelope(&mut s.l, a, d, sus, r);
    envelope(&mut s.r, a, d, sus, r);
}

// Iterative radix-2 FFT + 'same' convolution (for the cabinet / room IRs).
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cwr, mut cwi) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let a = i + k;
                let bb = i + k + len / 2;
                let tr = re[bb] * cwr - im[bb] * cwi;
                let ti = re[bb] * cwi + im[bb] * cwr;
                re[bb] = re[a] - tr;
                im[bb] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncwr = cwr * wr - cwi * wi;
                cwi = cwr * wi + cwi * wr;
                cwr = ncwr;
            }
            i += len;
        }
        len <<= 1;
    }
    if inverse {
        for v in re.iter_mut() {
            *v /= n as f64;
        }
        for v in im.iter_mut() {
            *v /= n as f64;
        }
    }
}

fn fftconvolve_same(x: &[f64], h: &[f64]) -> Vec<f64> {
    if x.is_empty() || h.is_empty() {
        return x.to_vec();
    }
    let full = x.len() + h.len() - 1;
    let mut n = 1;
    while n < full {
        n <<= 1;
    }
    let mut xr = vec![0.0; n];
    let mut xi = vec![0.0; n];
    let mut hr = vec![0.0; n];
    let mut hi = vec![0.0; n];
    xr[..x.len()].copy_from_slice(x);
    hr[..h.len()].copy_from_slice(h);
    fft(&mut xr, &mut xi, false);
    fft(&mut hr, &mut hi, false);
    for i in 0..n {
        let r = xr[i] * hr[i] - xi[i] * hi[i];
        let im = xr[i] * hi[i] + xi[i] * hr[i];
        xr[i] = r;
        xi[i] = im;
    }
    fft(&mut xr, &mut xi, true);
    let start = (h.len() - 1) / 2;
    xr[start..start + x.len()].to_vec()
}

fn note_to_freq(n: &str) -> f64 {
    if n == "R" {
        return 0.0;
    }
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let (name, oct) = n.split_at(n.len() - 1);
    let octave: i32 = oct.parse().unwrap_or(4);
    let idx = names.iter().position(|&x| x == name).unwrap_or(0) as i32;
    440.0 * 2f64.powf((idx - 9 + (octave - 4) * 12) as f64 / 12.0)
}

// ---------------------------------------------------------------------------
// Impulse responses (computed once)
// ---------------------------------------------------------------------------
/// The guitar speaker cabinet impulse response.
///
/// This is a *baked* constant (`CABINET_IR_BAKED`, see `synth_ir.rs`) rather than
/// computed at runtime. The Python `generate_virtual_acoustic_ir()` leans heavily
/// on scipy's exact `filtfilt` (steady-state edge conditions) and `iirpeak`
/// numerics; our lightweight DSP layer's `filtfilt` uses zero initial conditions,
/// which reshapes this short, decay-windowed IR's early samples enough to make the
/// cabinet ~2× too bright — and since *only the guitars* run through this IR, that
/// divergence was the dominant reason the guitars didn't match. The IR is fully
/// deterministic (seed 42) and computed once, so baking the reference bytes makes
/// the cabinet identical to the Python by construction at zero runtime cost.
fn cabinet_ir() -> &'static Vec<f64> {
    static IR: OnceLock<Vec<f64>> = OnceLock::new();
    IR.get_or_init(|| CABINET_IR_BAKED.iter().map(|&v| v as f64).collect())
}

fn drum_room_ir() -> &'static Vec<f64> {
    static IR: OnceLock<Vec<f64>> = OnceLock::new();
    IR.get_or_init(|| {
        let n = (SR as f64 * 0.15) as usize;
        let mut rng = Rng::new(88);
        let raw: Vec<f64> = (0..n).map(|i| rng.normal() * (-(i as f64 / SR as f64) * 40.0).exp()).collect();
        lp(&raw, 3500.0)
    })
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

/// Phase delay (in samples) of one first-order allpass `(coef + z⁻¹)/(1 + coef·z⁻¹)`
/// at angular frequency `w`. Ported from the Python `_allpass_phase_delay`; used to
/// pre-compensate the delay-line length so the dispersed string stays in tune.
fn allpass_phase_delay(coef: f64, w: f64) -> f64 {
    if w <= 1e-9 {
        return (1.0 - coef) / (1.0 + coef);
    }
    let num_ph = (-w.sin()).atan2(coef + w.cos());
    let den_ph = (-coef * w.sin()).atan2(1.0 + coef * w.cos());
    -(num_ph - den_ph) / w
}

/// The fixed parameters of one dispersive Karplus-Strong loop (Python
/// `_ks_loop_filter`): integer delay + fractional remainder, the loop gain
/// `sustain`, and the allpass dispersion (`coef`, `sections`). Captured per string
/// so the *same* loop can later be re-driven by the string-coupling bus.
#[derive(Clone)]
struct KsLoop {
    nd: usize,
    frac: f64,
    sustain: f64,
    coef: f64,
    sections: usize,
}

fn ks_loop(freq: f64, muted: bool, stiffness: f64, sections: usize, rng: &mut Rng) -> KsLoop {
    // Tuned delay = period − 0.5 (frac-poly group delay) − allpass dispersion delay.
    let coef = -stiffness.abs();
    let w0 = 2.0 * PI * freq / SR as f64;
    let ap_delay = if sections > 0 { sections as f64 * allpass_phase_delay(coef, w0) } else { 0.0 };
    let total = (SR as f64 / freq - 0.5 - ap_delay).max(1.0);
    let nd = total.floor() as usize;
    let frac = total - nd as f64;
    let sustain = if muted { rng.range(0.85, 0.92) } else { rng.range(0.996, 0.999) };
    KsLoop { nd, frac, sustain, coef, sections }
}

/// The plucking excitation placed in a `samples`-long buffer: a filtered noise
/// burst (pick-position comb + attack click) starting at the strum `offset`, plus
/// an occasional feedback swell from sample 0 (Python `_ks_excitation`).
#[allow(clippy::too_many_arguments)]
fn ks_excitation(freq: f64, samples: usize, velocity: f64, muted: bool, pick_pos: f64, attack: f64, offset: usize, rng: &mut Rng) -> Vec<f64> {
    let exc_n = ((SR as f64 / freq - 0.5) as usize).max(2);
    let mut burst = vec![0.0f64; exc_n];
    for v in burst.iter_mut() {
        *v = (rng.normal() * (2.0 + velocity * 4.0) * (0.7 + 0.6 * attack)).tanh();
    }
    let cutoff = (if muted { 800.0 + velocity * 1000.0 } else { 2000.0 + velocity * 8000.0 }) * (0.6 + 0.8 * attack);
    let mut burst = lp(&burst, cutoff.min(SR as f64 * 0.45));
    let pp = ((pick_pos * exc_n as f64) as usize).max(1);
    if pp < exc_n {
        let orig = burst.clone();
        for i in pp..exc_n {
            burst[i] -= orig[i - pp] * 0.8;
        }
    }
    let click_len = ((exc_n as f64 * (0.10 + 0.10 * attack)) as usize).max(1);
    let mut click = vec![0.0f64; click_len];
    for v in click.iter_mut() {
        *v = rng.normal();
    }
    let click = filt(&click, (2500.0 + 4000.0 * attack).min(SR as f64 * 0.45), true, 2);
    for i in 0..click_len.min(exc_n) {
        let env = (1.0 - i as f64 / click_len as f64).powi(2);
        burst[i] += click[i] * env * 0.25 * attack * velocity;
    }
    for v in burst.iter_mut() {
        *v *= velocity;
    }
    let mut x = vec![0.0f64; samples];
    let o = offset.min(samples);
    for i in 0..exc_n.min(samples - o) {
        x[o + i] = burst[i];
    }
    // Occasional feedback "swell": a cubic-ramped sine 2–4× above the fundamental
    // injected over the first 0.5 s, as in the Python `_ks_excitation` tail.
    if !muted && velocity > 0.8 && rng.uniform() > 0.7 {
        let mult = [2.0, 3.0, 4.0][((rng.uniform() * 3.0) as usize).min(2)];
        let fb_freq = freq * mult;
        let swell_len = (SR / 2).min(samples);
        for (i, xi) in x.iter_mut().enumerate().take(swell_len) {
            let env = (i as f64 / swell_len as f64).powi(3);
            *xi += (2.0 * PI * fb_freq * (i as f64 / SR as f64)).sin() * env * 0.3;
        }
    }
    x
}

/// Run a dispersive KS loop driven sample-by-sample by `input` (the excitation, or
/// the coupling drive). The feedback path is: integer delay → 3-tap fractional FIR
/// (Python `frac_poly`) → cascade of `sections` first-order allpasses (string
/// stiffness/inharmonicity) → `sustain` gain. Brightness/decay come from `sustain`
/// and the frac-poly's gentle top roll-off, exactly as in the Python.
fn ks_resonate(input: &[f64], p: &KsLoop) -> Vec<f64> {
    let (wa, wb, wc) = (0.5 * (1.0 - p.frac), 0.5, 0.5 * p.frac);
    let blen = p.nd + 4;
    let mut buf = vec![0.0f64; blen];
    let mut ax = vec![0.0f64; p.sections]; // allpass previous inputs
    let mut ay = vec![0.0f64; p.sections]; // allpass previous outputs
    let mut out = vec![0.0f64; input.len()];
    let mut wp = 0usize;
    for i in 0..input.len() {
        let d0 = (wp + blen - p.nd) % blen;
        let d1 = (wp + blen - p.nd - 1) % blen;
        let d2 = (wp + blen - p.nd - 2) % blen;
        let mut v = wa * buf[d0] + wb * buf[d1] + wc * buf[d2];
        for s in 0..p.sections {
            let x = v;
            let y = p.coef * x + ax[s] - p.coef * ay[s];
            ax[s] = x;
            ay[s] = y;
            v = y;
        }
        let y = input[i] + p.sustain * v;
        buf[wp] = y;
        out[i] = y;
        wp = (wp + 1) % blen;
    }
    out
}

/// KS power-chord guitar with the full Python post-chain: body/pickup resonance,
/// tube drive, power-amp sag, speaker breakup, cabinet IR convolution, SM57.
fn synth_ks_guitar(root: &str, dur_beats: f64, vol: f64, muted: bool, velocity: f64, rr: u64) -> Stereo {
    let root_freq = note_to_freq(root);
    let samples = samples_of(dur_beats);
    if root_freq <= 0.0 || samples == 0 {
        return Stereo::zeros(samples.max(1));
    }
    let mut rng = Rng::new(0x6711 ^ rr.wrapping_mul(2654435761));
    let intervals = [1.0, 1.4983, 2.0]; // root, fifth, octave
    let stiffness = [0.83, 0.66, 0.56]; // per-string inharmonicity (dispersion)
    let sections = [2usize, 3, 2]; // allpass sections per string
    let bridge = [1.0, 0.85, 0.70];
    let pp_base = 0.30 - 0.14 * velocity;
    let strum_gap = 0.018 - 0.010 * velocity;
    let order = if rr % 2 == 0 { [0usize, 1, 2] } else { [2, 1, 0] };

    // Render the three strings per channel (root/fifth/octave), each its own
    // dispersive KS loop, with the strum offset baked into the excitation.
    let mut sl: Vec<Vec<f64>> = Vec::with_capacity(3);
    let mut sr_ch: Vec<Vec<f64>> = Vec::with_capacity(3);
    let mut loops_l: Vec<KsLoop> = Vec::with_capacity(3);
    let mut loops_r: Vec<KsLoop> = Vec::with_capacity(3);
    for (si, &iv) in intervals.iter().enumerate() {
        let f = root_freq * iv;
        let pp = (pp_base + (si as f64 - 1.0) * 0.02 + rng.range(-0.015, 0.015)).clamp(0.08, 0.45);
        let atk = (0.55 + 0.55 * velocity + (1.0 - si as f64) * 0.05 + rng.range(-0.08, 0.08)).clamp(0.2, 1.4);
        let off = (order.iter().position(|&x| x == si).unwrap() as f64 * strum_gap * SR as f64) as usize;
        let fl = f * rng.range(0.998, 1.0);
        let fr = f * rng.range(1.0, 1.002);
        let lp_l = ks_loop(fl, muted, stiffness[si], sections[si], &mut rng);
        let lp_r = ks_loop(fr, muted, stiffness[si], sections[si], &mut rng);
        let ex_l = ks_excitation(fl, samples, velocity, muted, pp, atk, off, &mut rng);
        let ex_r = ks_excitation(fr, samples, velocity, muted, pp, atk, off, &mut rng);
        sl.push(ks_resonate(&ex_l, &lp_l));
        sr_ch.push(ks_resonate(&ex_r, &lp_r));
        loops_l.push(lp_l);
        loops_r.push(lp_r);
    }

    // Two-pass bridge coupling (Python): a bandlimited sum of the strings drives
    // each string's own loop, exchanging energy → the sympathetic beating/swell.
    if !muted {
        let mut coupling = 0.06;
        for _ in 0..2 {
            for (strings, loops) in [(&mut sl, &loops_l), (&mut sr_ch, &loops_r)] {
                let mut bus = vec![0.0f64; samples];
                for (si, s) in strings.iter().enumerate() {
                    for i in 0..samples {
                        bus[i] += bridge[si] * s[i];
                    }
                }
                let bus = lp(&bus, 5000.0);
                let added: Vec<Vec<f64>> = (0..3)
                    .map(|si| {
                        let drive: Vec<f64> = (0..samples).map(|i| (bus[i] - bridge[si] * strings[si][i]) * coupling).collect();
                        ks_resonate(&drive, &loops[si])
                    })
                    .collect();
                for (si, add) in added.into_iter().enumerate() {
                    for i in 0..samples {
                        strings[si][i] += add[i];
                    }
                }
            }
            coupling *= 0.5;
        }
    }

    // Final mix is the unweighted string sum (bridge weighting is coupling-only).
    let mut l = vec![0.0f64; samples];
    let mut r = vec![0.0f64; samples];
    for si in 0..3 {
        for i in 0..samples {
            l[i] += sl[si][i];
            r[i] += sr_ch[si][i];
        }
    }

    let mut wave = Stereo { l, r };
    for ch in [&mut wave.l, &mut wave.r] {
        // pickup magnetic resonance + body resonances
        let pick = iirpeak(ch, 3500.0, 1.2);
        let b1 = iirpeak(ch, 110.0, 2.0);
        let b2 = iirpeak(ch, 350.0, 1.5);
        for i in 0..samples {
            ch[i] += pick[i] * 0.45 + (b1[i] * 1.5 + b2[i]) * 0.25;
        }
        // tube pre-EQ mids
        let mids = filt(&lp(ch, 1200.0 + velocity * 400.0), 600.0, true, 2);
        for i in 0..samples {
            ch[i] += mids[i] * (2.0 + velocity);
        }
        // tube drive (asymmetric)
        let drive = if muted { 20.0 } else { 45.0 } * (0.5 + 0.5 * velocity);
        for v in ch.iter_mut() {
            *v *= drive;
            *v = if *v > 0.0 { 1.2 * v.tanh() } else { -0.8 + 0.8 * (*v * 1.2).exp() };
        }
        // power-supply sag
        let absx: Vec<f64> = ch.iter().map(|v| v.abs()).collect();
        let rms = filt(&absx, 15.0, false, 1);
        for i in 0..samples {
            ch[i] *= 1.0 / (1.0 + rms[i] * 0.4);
        }
        // transient speaker breakup
        let hp = filt(ch, 1200.0, true, 2);
        for i in 0..samples {
            ch[i] = (ch[i] * (1.0 + hp[i].abs() * 2.5)).tanh();
        }
        if muted {
            *ch = lp(ch, 1500.0 + velocity * 500.0);
        }
    }
    envelope_st(&mut wave, 0.005, 0.1, 0.9, 0.2);
    // virtual cabinet IR + SM57 transformer
    let ir = cabinet_ir();
    for ch in [&mut wave.l, &mut wave.r] {
        let conv = fftconvolve_same(ch, ir);
        for i in 0..samples {
            let v = conv[i];
            ch[i] = if v > 0.0 { 1.1 * v.tanh() } else { -0.9 + 0.9 * (v * 1.1).exp() };
        }
    }
    wave.scale(vol * 0.22);
    wave
}

fn synth_hypersaw(note_a: &str, note_b: &str, dur_beats: f64, vol: f64, rr: u64) -> Stereo {
    let mut f1 = note_to_freq(note_a);
    let f2 = note_to_freq(note_b);
    let samples = samples_of(dur_beats);
    if f1 <= 0.0 && f2 <= 0.0 {
        return Stereo::zeros(samples.max(1));
    }
    if f1 <= 0.0 {
        f1 = f2;
    }
    let mut rng = Rng::new(0x5A17 ^ rr.wrapping_mul(40503));
    let det_l = [-0.012, -0.008, -0.004, 0.0, 0.004, 0.008, 0.012];
    let det_r = [-0.014, -0.009, -0.003, 0.001, 0.005, 0.010, 0.015];
    let drift_rate = rng.range(0.1, 0.5);
    let ph_l: Vec<f64> = (0..7).map(|_| rng.uniform() * 2.0 * PI).collect();
    let ph_r: Vec<f64> = (0..7).map(|_| rng.uniform() * 2.0 * PI).collect();
    let mut l = vec![0.0f64; samples];
    let mut r = vec![0.0f64; samples];
    let mut accs_l = [0.0f64; 7];
    let mut accs_r = [0.0f64; 7];
    for i in 0..samples {
        let t = i as f64 / SR as f64;
        let prog = i as f64 / samples as f64;
        let base = f1 + (f2 - f1) * prog;
        let f = base * (1.0 + (2.0 * PI * drift_rate * t).sin() * 0.003);
        for k in 0..7 {
            accs_l[k] += f * (1.0 + det_l[k]) / SR as f64;
            accs_r[k] += f * (1.0 + det_r[k]) / SR as f64;
            l[i] += sawtooth(2.0 * PI * accs_l[k] + ph_l[k], 1.0);
            r[i] += sawtooth(2.0 * PI * accs_r[k] + ph_r[k], 1.0);
        }
        l[i] /= 7.0;
        r[i] /= 7.0;
    }
    let mut wave = Stereo { l, r };
    for ch in [&mut wave.l, &mut wave.r] {
        let bright = lp(ch, 5000.0);
        let dark = lp(ch, 1200.0);
        for i in 0..samples {
            let blend = (-(i as f64 / SR as f64) * 5.0).exp();
            ch[i] = ((bright[i] * blend + dark[i] * (1.0 - blend)) * 1.5).tanh();
        }
    }
    envelope_st(&mut wave, 0.03, 0.15, 0.6, 0.2);
    wave.scale(vol * 0.4);
    wave
}

fn synth_bass(note: &str, dur_beats: f64, vol: f64, muted: bool, rr: u64) -> Stereo {
    let freq = note_to_freq(note);
    let samples = samples_of(dur_beats);
    if freq <= 0.0 || samples == 0 {
        return Stereo::zeros(samples.max(1));
    }
    let mut rng = Rng::new(0xBA55 ^ rr.wrapping_mul(2246822519));
    let dr1 = rng.range(0.1, 0.3);
    let dr2 = rng.range(0.2, 0.4);
    let mut l = vec![0.0f64; samples];
    let mut r = vec![0.0f64; samples];
    for i in 0..samples {
        let t = i as f64 / SR as f64;
        let d1 = (2.0 * PI * dr1 * t).sin() * 0.004;
        let d2 = (2.0 * PI * dr2 * t).sin() * 0.004;
        let s1 = sawtooth(2.0 * PI * freq * (1.0 + d1) * t, 1.0);
        let s2 = sawtooth(2.0 * PI * freq * 1.008 * (1.0 + d2) * t, 1.0);
        let s3 = sawtooth(2.0 * PI * (freq / 1.008) * (1.0 - d1) * t, 1.0);
        let sub = (2.0 * PI * (freq / 2.0) * (1.0 + d1 * 0.5) * t).sin() * 1.5;
        l[i] = s1 + s2 * 0.8 + sub;
        r[i] = s1 + s3 * 0.8 + sub;
    }
    let mut wave = Stereo { l, r };
    for ch in [&mut wave.l, &mut wave.r] {
        for v in ch.iter_mut() {
            *v = if *v > 0.0 { 1.2 * (*v * 2.0).tanh() } else { -0.8 + 0.8 * (*v * 2.4).exp() };
        }
        let bright = iirpeak(ch, (freq * 6.0).min(20000.0), 1.5);
        let mut bb = lp(ch, 2500.0);
        let dark = lp(ch, if muted { 400.0 } else { 800.0 });
        let env_speed = if muted { 25.0 } else { 8.0 };
        for i in 0..samples {
            let blend = (-(i as f64 / SR as f64) * env_speed).exp();
            bb[i] = (bright[i] * 1.2 + bb[i]) * blend + dark[i] * (1.0 - blend);
        }
        *ch = bb;
    }
    envelope_st(&mut wave, 0.01, 0.3, 0.6, 0.1);
    wave.scale(vol * 0.45);
    wave
}

fn synth_pad(note: &str, dur_beats: f64, vol: f64, rr: u64) -> Stereo {
    let freq = note_to_freq(note);
    let samples = samples_of(dur_beats);
    if freq <= 0.0 || samples == 0 {
        return Stereo::zeros(samples.max(1));
    }
    let mut rng = Rng::new(0x9AD0 ^ rr.wrapping_mul(2654435761));
    let lr1 = rng.range(0.08, 0.12);
    let lr2 = rng.range(0.12, 0.18);
    let mut l = vec![0.0f64; samples];
    let mut r = vec![0.0f64; samples];
    let mut lfo1 = vec![0.0f64; samples];
    let mut lfo2 = vec![0.0f64; samples];
    for i in 0..samples {
        let t = i as f64 / SR as f64;
        let p1 = (2.0 * PI * lr1 * t).sin() * 0.5 + 0.5;
        let p2 = (2.0 * PI * lr2 * t).sin() * 0.5 + 0.5;
        lfo1[i] = p1;
        lfo2[i] = p2;
        l[i] = sawtooth(2.0 * PI * freq * (1.0 + p1 * 0.006) * t, 1.0) + (2.0 * PI * freq * 2.01 * t).sin() * 0.3;
        r[i] = sawtooth(2.0 * PI * freq * (1.0 - p2 * 0.006) * t, 1.0) + (2.0 * PI * freq * 1.99 * t).sin() * 0.3;
    }
    let mut wave = Stereo { l, r };
    for ch in [&mut wave.l, &mut wave.r] {
        let f1h = filt(&lp(ch, 1000.0), 800.0, true, 2);
        let f1l = filt(&lp(ch, 600.0), 400.0, true, 2);
        let f2h = filt(&lp(ch, 1600.0), 1400.0, true, 2);
        let f2l = filt(&lp(ch, 1000.0), 800.0, true, 2);
        let warm = lp(ch, 400.0);
        for i in 0..samples {
            let form1 = f1h[i] * lfo1[i] + f1l[i] * (1.0 - lfo1[i]);
            let form2 = f2h[i] * lfo2[i] + f2l[i] * (1.0 - lfo2[i]);
            ch[i] = (form1 * 1.5 + form2 * 1.2 + warm[i] * 1.5).tanh();
        }
    }
    envelope_st(&mut wave, 3.0, 1.0, 0.8, 3.0);
    wave.scale(vol * 0.7);
    wave
}

fn synth_psycho(note: &str, dur_beats: f64, vol: f64, intensity: f64, rr: u64) -> Stereo {
    let freq = note_to_freq(note);
    let samples = samples_of(dur_beats);
    if freq <= 0.0 || samples == 0 {
        return Stereo::zeros(samples.max(1));
    }
    let mut rng = Rng::new(0x9501 ^ rr.wrapping_mul(40503));
    let am_jit = rng.range(0.1, 0.3);
    let am_depth = rng.range(0.2, 0.6);
    let mut l = vec![0.0f64; samples];
    let mut r = vec![0.0f64; samples];
    let mut am_acc = 0.0f64;
    for i in 0..samples {
        let t = i as f64 / SR as f64;
        let drift = (2.0 * PI * 0.15 * t).sin() * 0.015;
        let fl = freq * (1.0 + drift);
        let fr = freq * (1.0 - drift) + 18.5;
        let cl = sawtooth(2.0 * PI * fl * t, 0.5);
        let cr = sawtooth(2.0 * PI * fr * t, 0.5);
        let sub = sawtooth(2.0 * PI * (freq * 0.5) * t, 0.5) * 0.8;
        let diss = sawtooth(2.0 * PI * (freq * 1.05946) * t, 0.5) * 0.3;
        let am_rate = 6.5 + intensity * 1.5 + (2.0 * PI * am_jit * t).sin() * am_depth;
        am_acc += am_rate / SR as f64;
        let am = 0.55 + 0.45 * (2.0 * PI * am_acc).sin();
        l[i] = (cl + sub + diss) * am;
        r[i] = (cr + sub + diss) * am;
    }
    let mut wave = Stereo { l, r };
    for ch in [&mut wave.l, &mut wave.r] {
        for v in ch.iter_mut() {
            *v = (*v * (1.5 + intensity)).tanh();
        }
        *ch = lp(ch, (1500.0 + intensity * 1500.0).min(20000.0));
    }
    envelope_st(&mut wave, 2.5, 1.2, 0.9, 2.0);
    wave.scale(vol * 0.5);
    wave
}

fn drum(piece: &str, dur_beats: f64, vol: f64, rr: u64) -> Stereo {
    let samples = samples_of(dur_beats);
    if samples == 0 {
        return Stereo::zeros(1);
    }
    let mut rng = Rng::new(0xD7 ^ rr.wrapping_mul(2246822519) ^ piece.bytes().map(|b| b as u64).sum::<u64>().wrapping_mul(2654435761));
    let t: Vec<f64> = (0..samples).map(|i| i as f64 / SR as f64).collect();
    let transient: Vec<f64> = t.iter().map(|&x| (-x * 200.0).exp() * 1.3 + 1.0).collect();
    let mut m = vec![0.0f64; samples];
    let room = 0.08;
    let mut room_mix = 0.08;
    match piece {
        "kick" => {
            let pmod = rng.range(0.98, 1.02);
            let drop = rng.range(55.0, 65.0);
            let base = rng.range(180.0, 220.0);
            let mut ph = 0.0;
            let mut click = vec![0.0f64; samples];
            for i in 0..samples {
                let pitch = base * pmod * (-t[i] * drop).exp() + 45.0;
                ph += pitch / SR as f64;
                m[i] = (2.0 * PI * ph).sin();
                click[i] = rng.normal();
            }
            let click = filt(&click, rng.range(3800.0, 4200.0), true, 2);
            let cd = rng.range(350.0, 450.0);
            let bd = rng.range(8.0, 12.0);
            for i in 0..samples {
                m[i] = ((m[i] + click[i] * (-t[i] * cd).exp() * 1.5) * 3.0 * transient[i]).tanh() * (-t[i] * bd).exp();
            }
            room_mix = room;
        }
        "snare" => {
            let pmod = rng.range(0.97, 1.03);
            let decay = rng.range(22.0, 28.0);
            let mut noise = vec![0.0f64; samples];
            for i in 0..samples {
                let mode1 = (2.0 * PI * (250.0 * pmod * (-t[i] * 30.0).exp() + 150.0) * t[i]).sin() * (-t[i] * 20.0).exp();
                let mode2 = (2.0 * PI * 300.0 * pmod * t[i]).sin() * (-t[i] * 40.0).exp() * 0.4;
                m[i] = mode1 + mode2;
                noise[i] = rng.normal();
            }
            let nz = filt(&noise, 2000.0, true, 2);
            for i in 0..samples {
                let crack = rng.normal() * (-t[i] * 300.0).exp() * 1.4;
                m[i] = ((m[i] + nz[i] * (-t[i] * decay).exp() + crack) * 2.5 * transient[i]).tanh();
            }
            room_mix = 0.18;
        }
        "hihat" => {
            let decay = rng.range(45.0, 65.0);
            let mut noise = vec![0.0f64; samples];
            for i in 0..samples {
                noise[i] = rng.normal() * 0.7 + (2.0 * PI * 7500.0 * t[i]).sin() * 0.3;
            }
            let nz = filt(&filt(&noise, 8000.0, true, 2), 6000.0, true, 2);
            for i in 0..samples {
                m[i] = (nz[i] * (-t[i] * decay).exp() * 1.5).tanh();
            }
            room_mix = 0.05;
        }
        _ => {}
    }
    let vmul = match piece {
        "snare" => 0.8,
        "hihat" => 0.25,
        _ => 1.0,
    };
    // a touch of room (drum room IR)
    let dry = m.clone();
    let wet = fftconvolve_same(&dry, drum_room_ir());
    for i in 0..samples {
        m[i] += wet[i] * room_mix;
    }
    let mut s = Stereo::mono(m);
    s.scale(vol * vmul);
    s
}

// ---------------------------------------------------------------------------
// Mixer + voice cache
// ---------------------------------------------------------------------------
struct Mix {
    kick: Stereo,
    snare: Stereo,
    bass: Stereo,
    guitars: Stereo,
    pads: Stereo,
    arps: Stereo,
    psycho: Stereo,
    rng: Rng,
    cache: std::collections::HashMap<String, Stereo>,
}
impl Mix {
    fn new(n: usize) -> Mix {
        Mix {
            kick: Stereo::zeros(n),
            snare: Stereo::zeros(n),
            bass: Stereo::zeros(n),
            guitars: Stereo::zeros(n),
            pads: Stereo::zeros(n),
            arps: Stereo::zeros(n),
            psycho: Stereo::zeros(n),
            rng: Rng::new(0xC0FFEE),
            cache: std::collections::HashMap::new(),
        }
    }
    fn cached(&mut self, key: String, f: impl FnOnce() -> Stereo) -> Stereo {
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let v = f();
        self.cache.insert(key, v.clone());
        v
    }
    // Resolve (and cache) a stem clip, then paint it — all under one &mut borrow,
    // so callers don't have to evaluate `cached` while `mix` is also borrowed by `put`.
    fn lay(&mut self, stem: &str, key: String, f: impl FnOnce() -> Stereo, at: f64, pan: f64, shift_ms: f64) {
        let audio = self.cached(key, f);
        paint(stem_mut(self, stem), &audio, at, pan, shift_ms);
    }
}

fn stem_mut<'a>(mix: &'a mut Mix, name: &str) -> &'a mut Stereo {
    match name {
        "kick" => &mut mix.kick,
        "snare_hats" => &mut mix.snare,
        "bass" => &mut mix.bass,
        "guitars" => &mut mix.guitars,
        "pads" => &mut mix.pads,
        "arps" => &mut mix.arps,
        _ => &mut mix.psycho,
    }
}

fn paint(stem: &mut Stereo, audio: &Stereo, start_beat: f64, pan: f64, shift_ms: f64) {
    let total = stem.len();
    if total == 0 {
        return;
    }
    let shift = ((shift_ms / 1000.0) * SR as f64) as i64;
    let mut start = (start_beat * beat() * SR as f64) as i64 + shift;
    start = ((start % total as i64) + total as i64) % total as i64;
    let (pl, pr) = (((pan + 1.0) * PI / 4.0).cos(), ((pan + 1.0) * PI / 4.0).sin());
    for i in 0..audio.len() {
        let idx = ((start as usize) + i) % total;
        stem.l[idx] += audio.l[i] * pl;
        stem.r[idx] += audio.r[i] * pr;
    }
}

fn put(mix: &mut Mix, stem: &str, audio: Stereo, at: f64, pan: f64, shift_ms: f64) {
    paint(stem_mut(mix, stem), &audio, at, pan, shift_ms);
}

// ---------------------------------------------------------------------------
// Section builders (ported from synth3.py)
// ---------------------------------------------------------------------------
const ARP_PATTERN: [&str; 4] = ["E4", "G4", "D4", "B3"];
const LEAD: [(&str, &str, f64); 4] =
    [("E5", "E5", 2.0), ("G5", "F#5", 1.0), ("D5", "D5", 1.0), ("C5", "B4", 4.0)];

fn draw_syncopated_beat(mix: &mut Mix, start_beat: usize, length: usize, intensity: f64, hat_var: bool) {
    for b in 0..length {
        let ab = (start_beat + b) as f64;
        let rrk = (b % 8) as u64;
        let ks = mix.rng.range(-1.0, 1.5);
        let kshft2 = mix.rng.range(-2.0, 2.0);
        let kshft3 = mix.rng.range(-2.0, 2.0);
        mix.lay("kick", format!("k{rrk}f"), || drum("kick", beat() / beat(), 1.0, rrk), ab, 0.0, ks);
        mix.lay("kick", format!("k{}p", (rrk + 1) % 8), || drum("kick", 1.0, 0.7, (rrk + 1) % 8), ab + 0.75, 0.0, kshft2);
        mix.lay("kick", format!("k{}q", (rrk + 2) % 8), || drum("kick", 1.0, 1.0, (rrk + 2) % 8), ab + 1.5, 0.0, kshft3);
        if intensity > 0.5 {
            let ss = mix.rng.range(1.0, 4.0);
            let sg = mix.rng.range(-3.0, 3.0);
            mix.lay("snare_hats", format!("s{}", b % 8), || drum("snare", 1.0, 0.9 * intensity, (b % 8) as u64), ab + 1.0, 0.0, ss);
            mix.lay("snare_hats", format!("s{}", (b + 1) % 8), || drum("snare", 1.0, 0.9 * intensity, ((b + 1) % 8) as u64), ab + 3.0, 0.0, ss);
            mix.lay("snare_hats", format!("sg{}", (b + 2) % 8), || drum("snare", 0.25, 0.3 * intensity, ((b + 2) % 8) as u64), ab + 2.75, 0.0, sg);
        }
        if hat_var && b % 2 == 0 {
            for h in 0..8u64 {
                let hs = mix.rng.range(-2.5, 2.5);
                mix.lay("snare_hats", format!("h{}", h % 8), || drum("hihat", 0.125, 0.5, h % 8), ab + h as f64 * 0.125, 0.3, hs);
            }
        } else {
            for h in 0..4u64 {
                let hs = mix.rng.range(-2.5, 2.5);
                mix.lay("snare_hats", format!("H{}", h % 8), || drum("hihat", 0.25, 0.6, h % 8), ab + h as f64 * 0.25, 0.3, hs);
            }
        }
    }
}

fn build_groove(mix: &mut Mix, start_beat: usize, length: usize, add_arps: bool) {
    draw_syncopated_beat(mix, start_beat, length, 1.0, false);
    for b in 0..length {
        let ab = (start_beat + b) as f64;
        let rb = (b % 8) as u64;
        mix.lay("bass", format!("bE{rb}h"), || synth_bass("E2", 0.5, 0.8, false, rb), ab, 0.0, 0.0);
        mix.lay("bass", format!("bE{}q", (rb + 1) % 8), || synth_bass("E2", 0.25, 0.6, false, (rb + 1) % 8), ab + 0.5, 0.0, 0.0);
        mix.lay("bass", format!("bE{}r", (rb + 2) % 8), || synth_bass("E2", 0.25, 0.6, false, (rb + 2) % 8), ab + 0.75, 0.0, 0.0);
        mix.lay("bass", format!("bD{}", (rb + 3) % 8), || synth_bass("D2", 0.5, 0.8, false, (rb + 3) % 8), ab + 1.5, 0.0, 0.0);
        if add_arps {
            for i in 0..4 {
                let note = ARP_PATTERN[((start_beat + b) * 4 + i) % 4];
                let pan = if i % 2 == 0 { -0.6 } else { 0.6 };
                mix.lay("arps", format!("a{note}{}", (b + i) % 8), || synth_hypersaw(note, note, 0.25, 0.5, ((b + i) % 8) as u64), ab + i as f64 * 0.25, pan, 0.0);
            }
        }
    }
    let mut b = 0;
    while b < length {
        let note = if ((b / 16) % 2) == 0 { "E3" } else { "C3" };
        put(mix, "pads", synth_pad(note, 16.0, 0.5, (b % 8) as u64), (start_beat + b) as f64, 0.0, 0.0);
        b += 16;
    }
}

fn build_soft_verse(mix: &mut Mix, start_beat: usize, length: usize) {
    let variation = (start_beat / 32) % 4;
    draw_syncopated_beat(mix, start_beat, length, 0.2, variation == 2);
    for b in 0..length {
        let ab = (start_beat + b) as f64;
        let rb = (b % 8) as u64;
        if variation == 3 && b % 2 == 0 {
            mix.lay("bass", format!("vE1h{rb}"), || synth_bass("E1", 0.5, 0.6, true, rb), ab, 0.0, 0.0);
            mix.lay("bass", format!("vE1h{}", (rb + 1) % 8), || synth_bass("E1", 0.5, 0.6, true, (rb + 1) % 8), ab + 0.5, 0.0, 0.0);
        } else {
            mix.lay("bass", format!("vE1q{rb}"), || synth_bass("E1", 0.25, 0.7, true, rb), ab, 0.0, 0.0);
            mix.lay("bass", format!("vE2q{}", (rb + 1) % 8), || synth_bass("E2", 0.25, 0.5, true, (rb + 1) % 8), ab + 0.25, 0.0, 0.0);
            mix.lay("bass", format!("vE1q2{}", (rb + 2) % 8), || synth_bass("E1", 0.25, 0.7, true, (rb + 2) % 8), ab + 0.75, 0.0, 0.0);
        }
        if variation == 1 && b % 4 == 0 {
            put(mix, "arps", synth_hypersaw("E5", "E5", 0.5, 0.3, rb), ab, 0.7, 0.0);
        }
    }
    let mut b = 0;
    while b < length {
        put(mix, "pads", synth_pad("E3", 8.0, 0.7, (b % 8) as u64), (start_beat + b) as f64, 0.0, 0.0);
        let n2 = if ((b + 8) / 16) % 2 == 0 { "C3" } else if variation < 2 { "D3" } else { "A2" };
        put(mix, "pads", synth_pad(n2, 8.0, 0.7, ((b + 1) % 8) as u64), (start_beat + b + 8) as f64, 0.0, 0.0);
        b += 16;
    }
}

fn build_tension(mix: &mut Mix, start_beat: usize, length: usize) {
    for b in 0..length {
        let ab = (start_beat + b) as f64;
        let prog = b as f64 / length as f64;
        let rb = (b % 8) as u64;
        if b % 16 == 0 {
            put(mix, "pads", synth_pad("E2", 16.0, 0.25, rb), ab, 0.0, 0.0);
        }
        let pvol = 0.65 + prog * 0.35;
        put(mix, "psycho", synth_psycho("E2", 1.0, pvol, (0.4 + prog * 0.6).min(2.2), rb), ab, 0.0, 0.0);
        let swell = 0.35 + prog * 0.65;
        if prog < 0.5 {
            if b % 2 == 0 {
                put(mix, "kick", drum("kick", 1.0, 0.65 * swell, rb), ab, 0.0, 0.0);
                put(mix, "kick", drum("kick", 1.0, 0.95 * swell, (rb + 1) % 8), ab + 0.5, 0.0, 0.0);
            }
        } else if prog < 0.82 {
            put(mix, "kick", drum("kick", 1.0, 0.8 * swell, rb), ab, 0.0, 0.0);
            put(mix, "kick", drum("kick", 1.0, 1.0 * swell, (rb + 1) % 8), ab + 0.5, 0.0, 0.0);
            put(mix, "guitars", synth_ks_guitar("E2", 0.5, swell * 0.7, true, 0.7, rb), ab, -0.5, 0.0);
            put(mix, "guitars", synth_ks_guitar("E2", 0.5, swell * 0.7, true, 0.9, (rb + 1) % 8), ab + 0.5, 0.5, 0.0);
            put(mix, "bass", synth_bass("E2", 1.0, 0.75, false, rb), ab, 0.0, 0.0);
        } else {
            for i in 0..4 {
                put(mix, "kick", drum("kick", 0.25, swell.min(1.0), (rb + i) % 8), ab + i as f64 * 0.25, 0.0, 0.0);
            }
            put(mix, "snare_hats", drum("snare", 0.25, swell, rb), ab, 0.0, 0.0);
            put(mix, "snare_hats", drum("snare", 0.25, swell, (rb + 1) % 8), ab + 0.5, 0.0, 0.0);
            put(mix, "guitars", synth_ks_guitar("E2", 0.25, swell.min(1.0), false, 0.9, rb), ab, -0.7, 0.0);
            put(mix, "guitars", synth_ks_guitar("E2", 0.25, swell.min(1.0), false, 1.0, (rb + 1) % 8), ab + 0.5, 0.7, 0.0);
            put(mix, "bass", synth_bass("E2", 1.0, 0.95, false, rb), ab, 0.0, 0.0);
        }
    }
}

fn build_lift(mix: &mut Mix, start_beat: usize, length: usize) {
    let mut b = 0;
    while b < length {
        let root = if (b / 8) % 2 == 0 { "E2" } else { "C2" };
        let rb = (b / 8) as u64 % 8;
        put(mix, "guitars", synth_ks_guitar(root, 8.0, 0.65, false, 1.0, rb), (start_beat + b) as f64, -0.7, 0.0);
        put(mix, "guitars", synth_ks_guitar(root, 8.0, 0.65, false, 0.95, (rb + 1) % 8), (start_beat + b) as f64, 0.7, 0.0);
        let mut mt = (start_beat + b) as f64;
        for (n1, n2, dur) in LEAD {
            put(mix, "arps", synth_hypersaw(n1, n2, dur, 1.0, rb), mt, 0.0, 0.0);
            mt += dur;
        }
        b += 8;
    }
    for b in 0..length {
        let ab = (start_beat + b) as f64;
        let rb = (b % 8) as u64;
        mix.lay("kick", format!("Lk{rb}"), || drum("kick", 1.0, 1.0, rb), ab, 0.0, 0.0);
        mix.lay("kick", format!("Lk{}", (rb + 1) % 8), || drum("kick", 1.0, 1.0, (rb + 1) % 8), ab + 0.5, 0.0, 0.0);
        if b % 2 == 1 {
            mix.lay("snare_hats", format!("Ls{rb}"), || drum("snare", 1.0, 1.0, rb), ab, 0.0, 2.0);
        }
        let root = if (b / 8) % 2 == 0 { "E2" } else { "C2" };
        mix.lay("bass", format!("Lb{root}{rb}"), || synth_bass(root, 1.0, 1.0, false, rb), ab, 0.0, 0.0);
    }
}

/// The title-screen arrangement: the LIFT hook (the `E5·G5→F#5·D5·C5→B4` lead over
/// the E2/C2 changes) but reframed as something catchier and more *bedded* than the
/// driving in-game LIFT — a warm sustained pad bed, clean low-velocity guitars
/// instead of the cranked power chords, a soft pedal bass, a gentle counter-arp for
/// movement, and a laid-back half-time pulse rather than four-on-the-floor. It is
/// its own self-contained loop (not part of the gameplay `STRUCTURE`).
fn build_title(mix: &mut Mix, length: usize) {
    // Everything goes through the voice cache (`mix.lay`) and the cache keys are
    // chosen to repeat across phrases, so the whole loop renders only a handful of
    // distinct voices (≈4 guitars, ≈4 pads, a dozen arps) instead of hundreds.
    // Warm pad bed: root pedal E3/C3 (+ a quiet fifth for shimmer) every 2 bars.
    let mut b = 0;
    while b < length {
        let even = (b / 8) % 2 == 0;
        let root = if even { "E3" } else { "C3" };
        let fifth = if even { "B3" } else { "G3" };
        mix.lay("pads", format!("Tp{root}"), move || synth_pad(root, 8.0, 0.58, 0), b as f64, 0.0, 0.0);
        mix.lay("pads", format!("Tp{fifth}"), move || synth_pad(fifth, 8.0, 0.28, 3), b as f64, 0.0, 0.0);
        b += 8;
    }
    // The catchy lead hook + a soft clean power chord underneath each phrase. Low
    // guitar velocity keeps it clean/warm rather than cranked & fizzy.
    let mut b = 0;
    while b < length {
        let even = (b / 8) % 2 == 0;
        let root = if even { "E2" } else { "C2" };
        let g = if even { 0u64 } else { 4 }; // shared per-root seed → 4 cached guitars
        mix.lay("guitars", format!("Tg{root}L"), move || synth_ks_guitar(root, 8.0, 0.30, false, 0.5, g), b as f64, -0.5, 0.0);
        mix.lay("guitars", format!("Tg{root}R"), move || synth_ks_guitar(root, 8.0, 0.30, false, 0.45, g + 1), b as f64, 0.5, 0.0);
        let rb = (b / 8) as u64 % 2; // 2 lead variants, tied to the E2/C2 alternation
        let mut mt = b as f64;
        for (n1, n2, dur) in LEAD {
            mix.lay("arps", format!("Tl{n1}{n2}{rb}"), move || synth_hypersaw(n1, n2, dur, 0.72, rb), mt, 0.0, 0.0);
            mt += dur;
        }
        b += 8;
    }
    // Gentle Em7 counter-arp (two notes per beat), quiet and panned wide for motion.
    for b in 0..length {
        let ab = b as f64;
        for i in 0..2usize {
            let note = ARP_PATTERN[(b * 2 + i) % 4];
            let rr = ((b + i) % 8) as u64;
            let pan = if i % 2 == 0 { -0.6 } else { 0.6 };
            mix.lay("arps", format!("Ta{note}{rr}"), move || synth_hypersaw(note, note, 0.5, 0.2, rr), ab + i as f64 * 0.5, pan, 0.0);
        }
    }
    // Soft pedal bass on the downbeat + a laid-back half-time pulse.
    for b in 0..length {
        let ab = b as f64;
        let rb = (b % 8) as u64;
        let root = if (b / 8) % 2 == 0 { "E2" } else { "C2" };
        mix.lay("bass", format!("Tb{root}{rb}"), move || synth_bass(root, 1.0, 0.52, false, rb), ab, 0.0, 0.0);
        if b % 2 == 0 {
            mix.lay("kick", format!("Tk{rb}"), move || drum("kick", 1.0, 0.5, rb), ab, 0.0, 0.0);
        } else {
            mix.lay("snare_hats", format!("Tn{rb}"), move || drum("snare", 1.0, 0.18, rb), ab, 0.0, 0.0); // soft backbeat
        }
        for h in 0..2u64 {
            let rrh = (rb + h) % 8;
            mix.lay("snare_hats", format!("Th{rrh}"), move || drum("hihat", 0.5, 0.25, rrh), ab + h as f64 * 0.5, 0.3, 0.0);
        }
    }
}

// ---------------------------------------------------------------------------
// Arrangement → processed stems
// ---------------------------------------------------------------------------
fn stereo_space(s: &mut Stereo) {
    let n = s.len();
    let d = ((beat() * 0.75) * SR as f64) as usize;
    if d < n {
        let (ol, or) = (s.l.clone(), s.r.clone());
        for i in 0..n {
            s.l[i] += or[(i + n - d) % n] * 0.3;
            s.r[i] += ol[(i + n - d) % n] * 0.3;
        }
    }
    let (ol, or) = (s.l.clone(), s.r.clone());
    for (tap, fb) in [(0.029, 0.6), (0.041, 0.5), (0.053, 0.4), (0.079, 0.3), (0.113, 0.2)] {
        let ds = (tap * SR as f64) as usize;
        for i in 0..n {
            s.l[i] += or[(i + n - ds) % n] * fb * 0.3 * 0.3;
            s.r[i] += ol[(i + n - ds) % n] * fb * 0.3 * 0.3;
        }
    }
    s.l = lp(&s.l, 6000.0);
    s.r = lp(&s.r, 6000.0);
}

fn declick(s: &mut Stereo) {
    let fade = (0.006 * SR as f64) as usize;
    let n = s.len();
    for f in 0..fade.min(n / 2) {
        let w = f as f64 / fade as f64;
        s.l[f] *= w;
        s.r[f] *= w;
        s.l[n - 1 - f] *= w;
        s.r[n - 1 - f] *= w;
    }
}

/// Build the whole arrangement and return the 7 processed stems.
fn build_full() -> Vec<(&'static str, Stereo)> {
    let n = master_samples();
    let mut mix = Mix::new(n);
    let mut cursor = 0usize;
    for (kind, len) in STRUCTURE {
        match *kind {
            "GROOVE" => build_groove(&mut mix, cursor, *len, false),
            "GROOVE_ARPS" => build_groove(&mut mix, cursor, *len, true),
            "SOFT_VERSE" => build_soft_verse(&mut mix, cursor, *len),
            "TENSION" => build_tension(&mut mix, cursor, *len),
            "LIFT" => build_lift(&mut mix, cursor, *len),
            _ => {}
        }
        cursor += *len;
    }
    let mut stems = vec![
        ("st_kick", mix.kick),
        ("st_snare", mix.snare),
        ("st_bass", mix.bass),
        ("st_guitars", mix.guitars),
        ("st_pads", mix.pads),
        ("st_arps", mix.arps),
        ("st_psycho", mix.psycho),
    ];
    for (name, s) in stems.iter_mut() {
        if matches!(*name, "st_arps" | "st_guitars" | "st_pads") {
            stereo_space(s);
        }
    }
    stems
}

fn interleave(s: &Stereo) -> Vec<f32> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for i in 0..s.len() {
        out.push(s.l[i] as f32);
        out.push(s.r[i] as f32);
    }
    out
}

/// The 7 stems, each glued + normalised (for `--render-stems` A/B vs the Python).
pub fn render_stems() -> Vec<(&'static str, Vec<f32>, u16, u32)> {
    let mut out = Vec::new();
    for (name, mut s) in build_full() {
        for ch in [&mut s.l, &mut s.r] {
            for v in ch.iter_mut() {
                *v = (*v * 1.6).tanh();
            }
        }
        let peak = s.l.iter().chain(s.r.iter()).fold(0.0f64, |a, &b| a.max(b.abs())).max(1e-6);
        s.scale(0.9 / peak);
        declick(&mut s);
        out.push((name, interleave(&s), 2u16, SR as u32));
    }
    out
}

/// The composed master mix (summed stems + analog glue), as one looping stereo
/// track — this is what the game plays, so the section dynamics are intact.
pub fn render_master() -> (Vec<f32>, u16, u32) {
    let n = master_samples();
    let mut master = Stereo::zeros(n);
    for (_, s) in build_full() {
        for i in 0..n {
            master.l[i] += s.l[i];
            master.r[i] += s.r[i];
        }
    }
    // analog console crosstalk
    for i in 0..n {
        let (l, r) = (master.l[i], master.r[i]);
        master.l[i] = l * 0.96 + r * 0.04;
        master.r[i] = r * 0.96 + l * 0.04;
    }
    // tape wow & flutter (Catmull-Rom fractional delay)
    wow_flutter(&mut master);
    // dynamic hiss + mains hum, ducking with level
    let mut rng = Rng::new(99);
    let sumabs: Vec<f64> = (0..n).map(|i| (master.l[i] + master.r[i]).abs()).collect();
    let mrms = filt(&sumabs, 0.5, false, 1);
    let noise: Vec<f64> = (0..n).map(|_| rng.normal()).collect();
    let hiss = lp(&noise, 8000.0);
    for i in 0..n {
        let nl = 0.003 / (1.0 + mrms[i] * 8.0);
        let hum = (2.0 * PI * 60.0 * i as f64 / SR as f64).sin() * 0.001;
        master.l[i] += hiss[i] * nl + hum;
        master.r[i] += hiss[i] * nl + hum;
    }
    // master tape saturation / soft limit
    for ch in [&mut master.l, &mut master.r] {
        for v in ch.iter_mut() {
            *v = if *v > 0.0 { 1.15 * (*v * 1.5).tanh() } else { -0.85 + 0.85 * (*v * 1.8).exp() };
        }
    }
    let peak = master.l.iter().chain(master.r.iter()).fold(0.0f64, |a, &b| a.max(b.abs())).max(1e-6);
    master.scale(0.94 / peak);
    declick(&mut master);
    (interleave(&master), 2u16, SR as u32)
}

/// The title-screen theme: one self-contained ~20 s loop (32 beats @ 96 BPM) of
/// `build_title`, summed and run through the same analog glue as the master mix but
/// gentler — a warm top-end roll-off and a lower final level so it sits *bedded*
/// under the menu rather than hitting like the in-game soundtrack.
pub fn render_title() -> (Vec<f32>, u16, u32) {
    let length = 16usize; // 10 s loop: the E2→C2 hook stated once (it's fully cached,
    let n = samples_of(length as f64); // so a longer loop would just repeat verbatim)
    let mut mix = Mix::new(n);
    mix.rng = Rng::new(0x7174_1E); // distinct humanisation seed from the gameplay mix
    build_title(&mut mix, length);
    // width/reverb on the melodic stems, as build_full does for the gameplay stems
    stereo_space(&mut mix.arps);
    stereo_space(&mut mix.guitars);
    stereo_space(&mut mix.pads);
    let mut master = Stereo::zeros(n);
    for s in [&mix.kick, &mix.snare, &mix.bass, &mix.guitars, &mix.pads, &mix.arps] {
        for i in 0..n {
            master.l[i] += s.l[i];
            master.r[i] += s.r[i];
        }
    }
    // console crosstalk + tape wow/flutter
    for i in 0..n {
        let (l, r) = (master.l[i], master.r[i]);
        master.l[i] = l * 0.96 + r * 0.04;
        master.r[i] = r * 0.96 + l * 0.04;
    }
    wow_flutter(&mut master);
    // warm, "bedded" top-end roll-off + gentle glue saturation (eased enough to let
    // the lead hook sparkle through the pad bed)
    master.l = lp(&master.l, 7800.0);
    master.r = lp(&master.r, 7800.0);
    for ch in [&mut master.l, &mut master.r] {
        for v in ch.iter_mut() {
            *v = (*v * 1.1).tanh();
        }
    }
    let peak = master.l.iter().chain(master.r.iter()).fold(0.0f64, |a, &b| a.max(b.abs())).max(1e-6);
    master.scale(0.82 / peak); // softer than the 0.94 gameplay master
    declick(&mut master);
    (interleave(&master), 2u16, SR as u32)
}

/// Catmull-Rom cubic-spline tape pitch instability (wow + flutter).
fn wow_flutter(s: &mut Stereo) {
    let n = s.len();
    let read = |buf: &[f64], idx: usize, frac: f64| -> f64 {
        let p0 = buf[idx.saturating_sub(1)];
        let p1 = buf[idx];
        let p2 = buf[(idx + 1).min(n - 1)];
        let p3 = buf[(idx + 2).min(n - 1)];
        let a0 = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
        let a1 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
        let a2 = -0.5 * p0 + 0.5 * p2;
        a0 * frac.powi(3) + a1 * frac.powi(2) + a2 * frac + p1
    };
    let (ol, or) = (s.l.clone(), s.r.clone());
    for i in 0..n {
        let ti = i as f64;
        let lfo = 0.0004 * (2.0 * PI * 0.33 * ti / SR as f64).sin() + 0.00015 * (2.0 * PI * 1.7 * ti / SR as f64).sin();
        let ptr = (ti - (lfo + 0.002) * SR as f64).clamp(1.0, (n - 3) as f64);
        let idx = ptr.floor() as usize;
        let frac = ptr - idx as f64;
        s.l[i] = read(&ol, idx, frac);
        s.r[i] = read(&or, idx, frac);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_renders_finite_loopable_and_dynamic() {
        let (buf, ch, rate) = render_master();
        assert_eq!(ch, 2);
        assert_eq!(rate, SR as u32);
        assert!(!buf.is_empty());
        let mut peak = 0.0f32;
        let mut energy = 0.0f64;
        for &v in &buf {
            assert!(v.is_finite(), "non-finite master sample");
            peak = peak.max(v.abs());
            energy += (v as f64) * (v as f64);
        }
        assert!(peak <= 1.0, "master clips ({peak})");
        assert!(energy > 10.0, "master silent");
        let n = buf.len();
        assert!(buf[0].abs() < 0.06 && buf[n - 1].abs() < 0.06, "master seam not de-clicked");
    }

    #[test]
    fn stems_render_and_have_the_seven_names() {
        let stems = render_stems();
        assert_eq!(stems.len(), 7);
        for (name, buf, ch, _) in &stems {
            assert_eq!(*ch, 2);
            assert!(buf.iter().all(|v| v.is_finite()), "{name} non-finite");
        }
    }

    #[test]
    fn title_theme_renders_finite_loopable_and_audible() {
        let (buf, ch, rate) = render_title();
        assert_eq!(ch, 2);
        assert_eq!(rate, SR as u32);
        let mut peak = 0.0f32;
        let mut energy = 0.0f64;
        for &v in &buf {
            assert!(v.is_finite(), "non-finite title sample");
            peak = peak.max(v.abs());
            energy += (v as f64) * (v as f64);
        }
        assert!(peak <= 1.0, "title clips ({peak})");
        assert!(energy > 10.0, "title silent");
        let n = buf.len();
        assert!(buf[0].abs() < 0.06 && buf[n - 1].abs() < 0.06, "title seam not de-clicked");
    }

    #[test]
    fn guitar_is_finite_and_unclipped() {
        // The dispersive/coupled KS power chord must stay numerically stable.
        for (muted, vel) in [(false, 1.0), (false, 0.7), (true, 0.9)] {
            let w = synth_ks_guitar("E2", 8.0, 0.65, muted, vel, 0);
            let peak = w.l.iter().chain(w.r.iter()).fold(0.0f64, |a, &b| a.max(b.abs()));
            assert!(w.l.iter().chain(w.r.iter()).all(|v| v.is_finite()), "guitar produced a non-finite sample");
            assert!(peak <= 1.0, "guitar clips (peak {peak})");
            assert!(peak > 1e-3, "guitar silent (peak {peak})");
        }
    }
}
