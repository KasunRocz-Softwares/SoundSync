//! Sound enhancement for speakers people actually own:
//!
//! - **Bass routing**: a Linkwitz-Riley crossover sends deep bass only to the
//!   output that can play it (subwoofer) and removes it from small satellites,
//!   so they play cleaner and louder.
//! - **Virtual bass**: harmonics of the bass a speaker cannot reproduce, so the
//!   ear still hears the low note ("missing fundamental").
//! - **Tilt**: one warm ↔ bright control.
//! - **Loudness**: restores the bass and treble the ear loses at low volume.
//! - **Normalize**: slow automatic level so quiet and loud sources match.
//! - **Night mode**: compression that lifts quiet sounds and tames loud ones.
//!
//! Dynamics run on the source signal (before each output's volume), so every
//! output computes the same gain. Parameters reach the render thread through a
//! seqlock of atomics: no locks or allocation in the audio path.
use crate::commands::CommandError;
use crate::eq::Biquad;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const CROSSOVER_OPTIONS: [u32; 5] = [60, 80, 100, 120, 150];
pub const MAX_TILT_DB: f32 = 6.0;
/// Loudness at 100%: bass shelf boost and treble shelf boost.
const LOUDNESS_BASS_DB: f64 = 10.0;
const LOUDNESS_TREBLE_DB: f64 = 4.0;
/// Normalize aims the long-term level here and moves at most ±12 dB.
const NORMALIZE_TARGET_RMS: f32 = 0.1; // −20 dBFS
const NORMALIZE_MAX_GAIN: f32 = 4.0;
const NORMALIZE_MIN_GAIN: f32 = 0.25;
const NORMALIZE_GATE_RMS: f32 = 0.001; // −60 dBFS: hold the gain through silence
/// Night mode compressor.
const NIGHT_THRESHOLD_DB: f32 = -30.0;
const NIGHT_RATIO: f32 = 4.0;
const NIGHT_MAKEUP_DB: f32 = 8.0;
/// Soft limiter knee: samples above this are rounded off instead of clipping.
const LIMIT_KNEE: f32 = 0.85;
pub const MAX_WIDTH: f32 = 2.0;
/// De-harsh: the 2–6 kHz band is cut while it is above this level, by up to
/// `DEHARSH_MAX_CUT_DB` × amount.
const DEHARSH_HZ: f64 = 3_500.0;
const DEHARSH_THRESHOLD_DB: f32 = -30.0;
const DEHARSH_MAX_CUT_DB: f32 = 9.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BassRole {
    /// Plays everything.
    #[default]
    Full,
    /// Small speaker: bass below the crossover is removed (and can be replaced
    /// by virtual bass).
    Satellite,
    /// Plays only the bass below the crossover, in mono.
    Subwoofer,
}

/// Settings shared by every output.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundSettings {
    /// Bass routing crossover frequency.
    pub crossover_hz: u32,
    /// −6 (warm) … +6 (bright) dB.
    pub tilt_db: f32,
    /// Loudness compensation 0…1: use more when listening quietly.
    pub loudness: f32,
    pub normalize: bool,
    pub night_mode: bool,
    /// Delay calibration also evens out speaker levels at the seat.
    pub match_levels: bool,
    /// Stereo width: 0 = mono, 1 = unchanged, up to 2 = extra wide.
    pub width: f32,
    /// De-harsh amount 0…1: cuts 2–6 kHz only while it is loud.
    pub deharsh: f32,
    /// What Measure room aims for.
    pub room_target: crate::room::RoomTarget,
}

impl Default for SoundSettings {
    fn default() -> Self {
        Self {
            crossover_hz: 100,
            tilt_db: 0.0,
            loudness: 0.0,
            normalize: false,
            night_mode: false,
            match_levels: true,
            width: 1.0,
            deharsh: 0.0,
            room_target: crate::room::RoomTarget::default(),
        }
    }
}

/// One-click tone settings for what is playing. Custom = the user's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListeningMode {
    #[default]
    Custom,
    Music,
    Movie,
    Game,
    Voice,
}

impl ListeningMode {
    pub const ALL: [ListeningMode; 4] = [
        ListeningMode::Music,
        ListeningMode::Movie,
        ListeningMode::Game,
        ListeningMode::Voice,
    ];

    /// The mode's tone on top of `base` (crossover, room target and level
    /// matching are speaker setup, not tone, so they are kept).
    pub fn apply(self, base: SoundSettings) -> SoundSettings {
        let (tilt_db, loudness, width, deharsh, normalize, night_mode) = match self {
            ListeningMode::Custom => return base,
            // Fuller, wider, slightly warm; dynamics left alone.
            ListeningMode::Music => (0.0, 0.3, 1.2, 0.3, false, false),
            // Wide and clear, gentle loudness, dialogue kept forward.
            ListeningMode::Movie => (0.5, 0.2, 1.3, 0.4, false, false),
            // Wide for positional cues, a little brighter for footsteps.
            ListeningMode::Game => (1.0, 0.2, 1.3, 0.2, false, false),
            // Calls, podcasts, videos: bright, narrow, levelled.
            ListeningMode::Voice => (1.5, 0.0, 0.8, 0.5, true, false),
        };
        SoundSettings {
            tilt_db,
            loudness,
            width,
            deharsh,
            normalize,
            night_mode,
            ..base
        }
    }

    /// The mode whose tone `settings` currently matches, else Custom.
    pub fn matching(settings: &SoundSettings) -> ListeningMode {
        Self::ALL
            .into_iter()
            .find(|m| m.apply(*settings) == *settings)
            .unwrap_or(ListeningMode::Custom)
    }
}

/// Switch to `mode` while `exe` is playing sound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppMode {
    pub exe: String,
    pub mode: ListeningMode,
}

pub const MAX_APP_MODES: usize = 16;

pub fn validate_app_modes(modes: &[AppMode]) -> Result<(), CommandError> {
    if modes.len() > MAX_APP_MODES {
        return Err(CommandError::new(
            "invalid_app_modes",
            format!("At most {MAX_APP_MODES} apps can switch the listening mode"),
        ));
    }
    for (i, m) in modes.iter().enumerate() {
        let ok = !m.exe.is_empty()
            && m.exe.len() <= 260
            && m.exe.to_ascii_lowercase().ends_with(".exe")
            && !m.exe.contains(['\\', '/', ':'])
            && m.mode != ListeningMode::Custom
            && !modes[..i].iter().any(|o| o.exe.eq_ignore_ascii_case(&m.exe));
        if !ok {
            return Err(CommandError::new(
                "invalid_app_modes",
                format!("Invalid app listening mode for {}", m.exe),
            ));
        }
    }
    Ok(())
}

impl SoundSettings {
    pub fn validate(&self) -> Result<(), CommandError> {
        let bad = |m: &str| Err(CommandError::new("invalid_sound", m));
        if !CROSSOVER_OPTIONS.contains(&self.crossover_hz) {
            return bad("Crossover must be 60, 80, 100, 120 or 150 Hz");
        }
        if !self.tilt_db.is_finite() || self.tilt_db.abs() > MAX_TILT_DB {
            return bad("Tilt must be between -6 and +6 dB");
        }
        if !self.loudness.is_finite() || !(0.0..=1.0).contains(&self.loudness) {
            return bad("Loudness must be between 0 and 100%");
        }
        if !self.width.is_finite() || !(0.0..=MAX_WIDTH).contains(&self.width) {
            return bad("Stereo width must be between 0 and 200%");
        }
        if !self.deharsh.is_finite() || !(0.0..=1.0).contains(&self.deharsh) {
            return bad("De-harsh must be between 0 and 100%");
        }
        Ok(())
    }
}

/// Everything the render thread needs for one output, precomputed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnhanceDesign {
    pub role: BassRole,
    /// Linkwitz-Riley 4th order: two identical Butterworth stages.
    pub crossover: [Biquad; 2],
    pub virtual_bass: f32,
    /// Isolates the bass the harmonics are made from.
    pub vb_extract: [Biquad; 2],
    /// Keeps only the generated harmonics.
    pub vb_shape: [Biquad; 2],
    /// Tilt low, tilt high, loudness low, loudness high.
    pub tone: [Biquad; 4],
    pub normalize: bool,
    pub night_mode: bool,
    pub width: f32,
    pub deharsh: f32,
    /// Detects (and is subtracted to cut) the harsh band.
    pub deharsh_band: Biquad,
}

impl Default for EnhanceDesign {
    fn default() -> Self {
        Self {
            role: BassRole::Full,
            crossover: [Biquad::IDENTITY; 2],
            virtual_bass: 0.0,
            vb_extract: [Biquad::IDENTITY; 2],
            vb_shape: [Biquad::IDENTITY; 2],
            tone: [Biquad::IDENTITY; 4],
            normalize: false,
            night_mode: false,
            width: 1.0,
            deharsh: 0.0,
            deharsh_band: Biquad::IDENTITY,
        }
    }
}

pub fn design(
    role: BassRole,
    virtual_bass: f32,
    sound: &SoundSettings,
    rate: u32,
) -> EnhanceDesign {
    let fs = f64::from(rate);
    let xo = f64::from(sound.crossover_hz);
    let mut d = EnhanceDesign {
        role,
        normalize: sound.normalize,
        night_mode: sound.night_mode,
        ..Default::default()
    };
    match role {
        BassRole::Full => {}
        BassRole::Satellite => d.crossover = [Biquad::high_pass(xo, fs); 2],
        BassRole::Subwoofer => d.crossover = [Biquad::low_pass(xo, fs); 2],
    }
    if role != BassRole::Subwoofer && virtual_bass > 0.0 {
        d.virtual_bass = virtual_bass.clamp(0.0, 1.0);
        d.vb_extract = [Biquad::low_pass(xo, fs), Biquad::high_pass(20.0, fs)];
        d.vb_shape = [
            Biquad::high_pass(xo, fs),
            Biquad::low_pass((xo * 4.0).min(fs * 0.45), fs),
        ];
    }
    if role != BassRole::Subwoofer {
        d.width = sound.width.clamp(0.0, MAX_WIDTH);
        if sound.deharsh > 0.0 && DEHARSH_HZ < fs * 0.4 {
            d.deharsh = sound.deharsh.clamp(0.0, 1.0);
            d.deharsh_band = Biquad::band_pass(DEHARSH_HZ, fs, 0.9);
        }
        let tilt = f64::from(sound.tilt_db) / 2.0;
        let loud = f64::from(sound.loudness);
        d.tone = [
            Biquad::low_shelf(250.0, fs, -tilt),
            Biquad::high_shelf(2_500.0, fs, tilt),
            Biquad::low_shelf(80.0, fs, LOUDNESS_BASS_DB * loud),
            Biquad::high_shelf(10_000.0_f64.min(fs * 0.4), fs, LOUDNESS_TREBLE_DB * loud),
        ];
    }
    d
}

const PARAMS: usize = 64;

impl EnhanceDesign {
    fn to_params(self) -> [f32; PARAMS] {
        let mut p = [0.0f32; PARAMS];
        let mut i = 0;
        let mut put = |v: f32| {
            p[i] = v;
            i += 1;
        };
        put(match self.role {
            BassRole::Full => 0.0,
            BassRole::Satellite => 1.0,
            BassRole::Subwoofer => 2.0,
        });
        put(self.virtual_bass);
        put(f32::from(u8::from(self.normalize)));
        put(f32::from(u8::from(self.night_mode)));
        put(self.width);
        put(self.deharsh);
        for b in self
            .crossover
            .iter()
            .chain(&self.vb_extract)
            .chain(&self.vb_shape)
            .chain(&self.tone)
            .chain(std::iter::once(&self.deharsh_band))
        {
            for v in [b.b0, b.b1, b.b2, b.a1, b.a2] {
                put(v);
            }
        }
        p
    }

    fn from_params(p: &[f32; PARAMS]) -> Self {
        let bq = |at: usize| Biquad {
            b0: p[at],
            b1: p[at + 1],
            b2: p[at + 2],
            a1: p[at + 3],
            a2: p[at + 4],
        };
        let base = 6;
        Self {
            width: p[4],
            deharsh: p[5],
            deharsh_band: bq(base + 50),
            role: match p[0] as u8 {
                1 => BassRole::Satellite,
                2 => BassRole::Subwoofer,
                _ => BassRole::Full,
            },
            virtual_bass: p[1],
            normalize: p[2] != 0.0,
            night_mode: p[3] != 0.0,
            crossover: [bq(base), bq(base + 5)],
            vb_extract: [bq(base + 10), bq(base + 15)],
            vb_shape: [bq(base + 20), bq(base + 25)],
            tone: [bq(base + 30), bq(base + 35), bq(base + 40), bq(base + 45)],
        }
    }
}

/// Seqlock-published design: control thread writes, render thread reads.
#[derive(Debug)]
pub struct EnhanceShared {
    version: AtomicU64,
    params: [AtomicU32; PARAMS],
}

impl Default for EnhanceShared {
    fn default() -> Self {
        let shared = Self {
            version: AtomicU64::new(0),
            params: std::array::from_fn(|_| AtomicU32::new(0)),
        };
        shared.publish(&EnhanceDesign::default());
        shared
    }
}

impl EnhanceShared {
    pub fn publish(&self, design: &EnhanceDesign) {
        self.version.fetch_add(1, Ordering::AcqRel);
        for (slot, v) in self.params.iter().zip(design.to_params()) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::AcqRel);
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn read(&self) -> Option<(EnhanceDesign, u64)> {
        let before = self.version.load(Ordering::Acquire);
        if before % 2 == 1 {
            return None;
        }
        let mut p = [0.0f32; PARAMS];
        for (v, slot) in p.iter_mut().zip(&self.params) {
            *v = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        let after = self.version.load(Ordering::Acquire);
        (before == after).then(|| (EnhanceDesign::from_params(&p), after))
    }
}

/// Render-thread state for one stereo output.
#[derive(Clone, Debug)]
pub struct EnhanceProcessor {
    d: EnhanceDesign,
    version: u64,
    filters_active: bool,
    crossover_state: [[(f32, f32); 2]; 2],
    vb_state: [(f32, f32); 4],
    tone_state: [[(f32, f32); 4]; 2],
    deharsh_state: [(f32, f32); 2],
    deharsh_env: f32,
    deharsh_attack: f32,
    deharsh_release: f32,
    // Dynamics
    agc_power: f32,
    agc_gain: f32,
    comp_env: f32,
    agc_detect: f32,
    agc_down: f32,
    agc_up: f32,
    comp_attack: f32,
    comp_release: f32,
}

fn one_pole(seconds: f32, rate: u32) -> f32 {
    1.0 - (-1.0 / (seconds * rate as f32)).exp()
}

fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= LIMIT_KNEE {
        x
    } else {
        let room = 1.0 - LIMIT_KNEE;
        x.signum() * (LIMIT_KNEE + room * ((a - LIMIT_KNEE) / room).tanh())
    }
}

impl EnhanceProcessor {
    pub fn new(rate: u32) -> Self {
        Self {
            d: EnhanceDesign::default(),
            version: u64::MAX,
            filters_active: false,
            crossover_state: [[(0.0, 0.0); 2]; 2],
            vb_state: [(0.0, 0.0); 4],
            tone_state: [[(0.0, 0.0); 4]; 2],
            deharsh_state: [(0.0, 0.0); 2],
            deharsh_env: 0.0,
            deharsh_attack: one_pole(0.002, rate),
            deharsh_release: one_pole(0.08, rate),
            agc_power: NORMALIZE_TARGET_RMS * NORMALIZE_TARGET_RMS,
            agc_gain: 1.0,
            comp_env: 0.0,
            agc_detect: one_pole(3.0, rate),
            agc_down: one_pole(0.5, rate),
            agc_up: one_pole(4.0, rate),
            comp_attack: one_pole(0.005, rate),
            comp_release: one_pole(0.15, rate),
        }
    }

    /// Picks up a new design if one was published. Call once per callback.
    pub fn refresh(&mut self, shared: &EnhanceShared) {
        if shared.version() == self.version {
            return;
        }
        if let Some((d, version)) = shared.read() {
            if d.role != self.d.role {
                self.crossover_state = [[(0.0, 0.0); 2]; 2];
            }
            self.filters_active = d.role != BassRole::Full
                || d.virtual_bass > 0.0
                || d.width != 1.0
                || d.deharsh > 0.0
                || d.tone.iter().any(|b| *b != Biquad::IDENTITY);
            if !d.normalize {
                self.agc_gain = 1.0;
            }
            self.d = d;
            self.version = version;
        }
    }

    /// Source-level dynamics (normalize, night mode). Run before output volume.
    #[inline]
    pub fn dynamics(&mut self, frame: &mut [f32]) {
        if !self.d.normalize && !self.d.night_mode {
            return;
        }
        let l = frame.first().copied().unwrap_or(0.0);
        let r = frame.get(1).copied().unwrap_or(l);
        let mut gain = 1.0;
        if self.d.normalize {
            let power = 0.5 * (l * l + r * r);
            self.agc_power += (power - self.agc_power) * self.agc_detect;
            let rms = self.agc_power.sqrt();
            if rms > NORMALIZE_GATE_RMS {
                let target = (NORMALIZE_TARGET_RMS / rms).clamp(NORMALIZE_MIN_GAIN, NORMALIZE_MAX_GAIN);
                let speed = if target < self.agc_gain {
                    self.agc_down
                } else {
                    self.agc_up
                };
                self.agc_gain += (target - self.agc_gain) * speed;
            }
            gain *= self.agc_gain;
        }
        if self.d.night_mode {
            let peak = l.abs().max(r.abs()) * gain;
            let coeff = if peak > self.comp_env {
                self.comp_attack
            } else {
                self.comp_release
            };
            self.comp_env += (peak - self.comp_env) * coeff;
            let level_db = 20.0 * self.comp_env.max(1e-6).log10();
            let over = (level_db - NIGHT_THRESHOLD_DB).max(0.0);
            let reduction = over * (1.0 - 1.0 / NIGHT_RATIO);
            gain *= 10f32.powf((NIGHT_MAKEUP_DB - reduction) / 20.0);
        }
        for s in frame.iter_mut().take(2) {
            *s = soft_limit(*s * gain);
        }
    }

    /// Bass routing, virtual bass and tone. Run after volume, before the EQ.
    #[inline]
    pub fn process(&mut self, frame: &mut [f32]) {
        if !self.filters_active || frame.len() < 2 {
            return;
        }
        let d = &self.d;
        let (mut l, mut r) = (frame[0], frame[1]);
        if d.width != 1.0 && d.role != BassRole::Subwoofer {
            let mid = 0.5 * (l + r);
            let side = 0.5 * (l - r) * d.width;
            l = mid + side;
            r = mid - side;
        }
        // Harmonics from the full-range signal, before the crossover removes it.
        let mut harmonics = 0.0;
        if d.virtual_bass > 0.0 {
            let mut bass = 0.5 * (l + r);
            bass = d.vb_extract[0].tick(&mut self.vb_state[0], bass);
            bass = d.vb_extract[1].tick(&mut self.vb_state[1], bass);
            // Full-wave rectification: even harmonics with the input's level.
            let mut h = bass.abs();
            h = d.vb_shape[0].tick(&mut self.vb_state[2], h);
            h = d.vb_shape[1].tick(&mut self.vb_state[3], h);
            harmonics = h * d.virtual_bass * 3.0;
        }
        match d.role {
            BassRole::Full => {}
            BassRole::Satellite => {
                for (stage, b) in d.crossover.iter().enumerate() {
                    l = b.tick(&mut self.crossover_state[stage][0], l);
                    r = b.tick(&mut self.crossover_state[stage][1], r);
                }
            }
            BassRole::Subwoofer => {
                let mut m = 0.5 * (l + r);
                for (stage, b) in d.crossover.iter().enumerate() {
                    m = b.tick(&mut self.crossover_state[stage][0], m);
                }
                l = m;
                r = m;
            }
        }
        l += harmonics;
        r += harmonics;
        for (i, b) in d.tone.iter().enumerate() {
            if *b != Biquad::IDENTITY {
                l = b.tick(&mut self.tone_state[0][i], l);
                r = b.tick(&mut self.tone_state[1][i], r);
            }
        }
        if d.deharsh > 0.0 {
            let bl = d.deharsh_band.tick(&mut self.deharsh_state[0], l);
            let br = d.deharsh_band.tick(&mut self.deharsh_state[1], r);
            let peak = bl.abs().max(br.abs());
            let coeff = if peak > self.deharsh_env {
                self.deharsh_attack
            } else {
                self.deharsh_release
            };
            self.deharsh_env += (peak - self.deharsh_env) * coeff;
            let over = 20.0 * self.deharsh_env.max(1e-6).log10() - DEHARSH_THRESHOLD_DB;
            let cut_db = (over * 0.5).clamp(0.0, DEHARSH_MAX_CUT_DB * d.deharsh);
            if cut_db > 0.0 {
                // Subtracting part of the band lowers just that band.
                let g = 1.0 - 10f32.powf(-cut_db / 20.0);
                l -= g * bl;
                r -= g * br;
            }
        }
        frame[0] = if l.is_finite() { soft_limit(l) } else { 0.0 };
        frame[1] = if r.is_finite() { soft_limit(r) } else { 0.0 };
    }
}

/// Octave-band level meter for what an output actually plays (after every
/// effect). Runs on the render thread only while the UI is watching; levels are
/// published as dBFS through atomics.
#[derive(Debug)]
pub struct SpectrumShared {
    pub watching: std::sync::atomic::AtomicBool,
    levels: [AtomicU32; 10],
}

impl Default for SpectrumShared {
    fn default() -> Self {
        Self {
            watching: std::sync::atomic::AtomicBool::new(false),
            levels: std::array::from_fn(|_| AtomicU32::new(SPECTRUM_FLOOR_DB.to_bits())),
        }
    }
}

pub const SPECTRUM_FLOOR_DB: f32 = -90.0;

impl SpectrumShared {
    pub fn levels_db(&self) -> [f32; 10] {
        std::array::from_fn(|i| f32::from_bits(self.levels[i].load(Ordering::Relaxed)))
    }
    pub fn watching(&self) -> bool {
        self.watching.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct SpectrumMeter {
    bands: [Biquad; 10],
    state: [(f32, f32); 10],
    power: [f32; 10],
    coeff: f32,
}

impl SpectrumMeter {
    pub fn new(rate: u32) -> Self {
        let fs = f64::from(rate);
        Self {
            bands: std::array::from_fn(|i| {
                let f = crate::room::BANDS[i];
                if f < fs * 0.45 {
                    Biquad::band_pass(f, fs, 1.41)
                } else {
                    Biquad {
                        b0: 0.0,
                        ..Biquad::IDENTITY
                    }
                }
            }),
            state: [(0.0, 0.0); 10],
            power: [0.0; 10],
            coeff: one_pole(0.12, rate),
        }
    }

    #[inline]
    pub fn push(&mut self, frame: &[f32]) {
        let x = match frame {
            [l, r, ..] => 0.5 * (l + r),
            [m] => *m,
            [] => 0.0,
        };
        for ((b, s), p) in self.bands.iter().zip(&mut self.state).zip(&mut self.power) {
            let y = b.tick(s, x);
            *p += (y * y - *p) * self.coeff;
        }
    }

    pub fn publish(&self, shared: &SpectrumShared) {
        for (slot, p) in shared.levels.iter().zip(self.power) {
            // ×2: a full-scale sine reads 0 dBFS.
            let db = (10.0 * (2.0 * p).max(1e-12).log10()).max(SPECTRUM_FLOOR_DB);
            slot.store(db.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn spectrum_meter_lights_the_band_of_a_tone() {
        let mut m = SpectrumMeter::new(RATE);
        for i in 0..RATE {
            let x = 0.5 * (2.0 * PI * 1_000.0 * i as f32 / RATE as f32).sin();
            m.push(&[x, x]);
        }
        let shared = SpectrumShared::default();
        m.publish(&shared);
        let levels = shared.levels_db();
        assert!((levels[5] - db(0.5)).abs() < 1.5, "1 kHz band {levels:?}");
        assert!(levels[1] < levels[5] - 20.0 && levels[9] < levels[5] - 20.0, "{levels:?}");
    }

    const RATE: u32 = 48_000;

    fn processor(role: BassRole, vb: f32, sound: SoundSettings) -> EnhanceProcessor {
        let shared = EnhanceShared::default();
        shared.publish(&design(role, vb, &sound, RATE));
        let mut p = EnhanceProcessor::new(RATE);
        p.refresh(&shared);
        p
    }

    /// RMS of the output for a sine at `freq`, after a 0.5 s settle.
    fn rms_out(p: &mut EnhanceProcessor, freq: f32, amp: f32, seconds: f32) -> f32 {
        let n = (seconds * RATE as f32) as usize;
        let settle = n / 2;
        let mut sum = 0.0;
        for i in 0..n {
            let x = amp * (2.0 * PI * freq * i as f32 / RATE as f32).sin();
            let mut f = [x, x];
            p.dynamics(&mut f);
            p.process(&mut f);
            if i >= settle {
                sum += f[0] * f[0];
            }
        }
        (sum / (n - settle) as f32).sqrt()
    }

    fn db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    #[test]
    fn defaults_are_bit_exact_passthrough() {
        let mut p = processor(BassRole::Full, 0.0, SoundSettings::default());
        let mut f = [0.3, -0.7];
        p.dynamics(&mut f);
        p.process(&mut f);
        assert_eq!(f, [0.3, -0.7]);
    }

    #[test]
    fn satellite_drops_deep_bass_and_subwoofer_keeps_only_bass() {
        let sound = SoundSettings::default(); // 100 Hz crossover
        let mut sat = processor(BassRole::Satellite, 0.0, sound);
        assert!(db(rms_out(&mut sat, 30.0, 0.5, 1.0) / 0.3536) < -20.0);
        assert!(db(rms_out(&mut sat, 1_000.0, 0.5, 0.2) / 0.3536).abs() < 0.5);
        let mut sub = processor(BassRole::Subwoofer, 0.0, sound);
        assert!(db(rms_out(&mut sub, 30.0, 0.5, 1.0) / 0.3536).abs() < 1.0);
        assert!(db(rms_out(&mut sub, 1_000.0, 0.5, 0.2) / 0.3536) < -40.0);
    }

    #[test]
    fn crossover_halves_sum_flat_in_level() {
        // Linkwitz-Riley: each side is −6 dB at the crossover.
        let sound = SoundSettings::default();
        let mut sat = processor(BassRole::Satellite, 0.0, sound);
        let mut sub = processor(BassRole::Subwoofer, 0.0, sound);
        let s = db(rms_out(&mut sat, 100.0, 0.5, 1.0) / 0.3536);
        let w = db(rms_out(&mut sub, 100.0, 0.5, 1.0) / 0.3536);
        assert!((s + 6.0).abs() < 0.7 && (w + 6.0).abs() < 0.7, "{s} {w}");
    }

    #[test]
    fn virtual_bass_adds_harmonics_a_satellite_can_play() {
        let sound = SoundSettings::default();
        let mut plain = processor(BassRole::Satellite, 0.0, sound);
        let mut vb = processor(BassRole::Satellite, 1.0, sound);
        let without = rms_out(&mut plain, 50.0, 0.5, 1.0);
        let with = rms_out(&mut vb, 50.0, 0.5, 1.0);
        assert!(db(with / without) > 10.0, "{without} → {with}");
        // Midrange is untouched by the bass path.
        let mut vb_mid = processor(BassRole::Satellite, 1.0, sound);
        assert!(db(rms_out(&mut vb_mid, 2_000.0, 0.5, 0.2) / 0.3536).abs() < 0.5);
    }

    #[test]
    fn tilt_and_loudness_shape_the_tone() {
        let bright = SoundSettings {
            tilt_db: 6.0,
            ..Default::default()
        };
        let d = design(BassRole::Full, 0.0, &bright, RATE);
        let resp = |f: f64| -> f64 {
            d.tone
                .iter()
                .map(|b| 20.0 * b.magnitude(f, f64::from(RATE)).log10())
                .sum()
        };
        assert!(resp(10_000.0) - resp(50.0) > 5.0);
        let loud = SoundSettings {
            loudness: 1.0,
            ..Default::default()
        };
        let d = design(BassRole::Full, 0.0, &loud, RATE);
        let resp = |f: f64| -> f64 {
            d.tone
                .iter()
                .map(|b| 20.0 * b.magnitude(f, f64::from(RATE)).log10())
                .sum()
        };
        assert!(resp(25.0) > 8.0 && resp(1_000.0).abs() < 1.5, "{}", resp(25.0));
    }

    #[test]
    fn normalize_brings_quiet_and_loud_sources_toward_the_target() {
        let sound = SoundSettings {
            normalize: true,
            ..Default::default()
        };
        let mut quiet = processor(BassRole::Full, 0.0, sound);
        let q = rms_out(&mut quiet, 440.0, 0.02, 30.0); // −37 dBFS RMS in
        assert!(db(q) > -30.0, "quiet raised: {}", db(q));
        let mut loud = processor(BassRole::Full, 0.0, sound);
        let l = rms_out(&mut loud, 440.0, 0.9, 30.0); // −4 dBFS RMS in
        assert!(db(l) < -10.0, "loud lowered: {}", db(l));
    }

    #[test]
    fn night_mode_narrows_the_gap_between_quiet_and_loud() {
        let sound = SoundSettings {
            night_mode: true,
            ..Default::default()
        };
        let mut a = processor(BassRole::Full, 0.0, sound);
        let mut b = processor(BassRole::Full, 0.0, sound);
        let quiet = rms_out(&mut a, 440.0, 0.01, 1.0);
        let loud = rms_out(&mut b, 440.0, 0.8, 1.0);
        let gap_in = db(0.8 / 0.01);
        let gap_out = db(loud / quiet);
        assert!(gap_out < gap_in - 15.0, "{gap_in} → {gap_out}");
        assert!(loud < 1.0);
    }

    #[test]
    fn soft_limit_never_exceeds_full_scale() {
        for x in [0.5f32, 0.9, 1.5, 10.0, -3.0] {
            assert!(soft_limit(x).abs() <= 1.0);
        }
        assert_eq!(soft_limit(0.5), 0.5);
    }

    #[test]
    fn params_round_trip() {
        let sound = SoundSettings {
            tilt_db: -3.0,
            loudness: 0.5,
            normalize: true,
            night_mode: true,
            crossover_hz: 80,
            match_levels: false,
            width: 1.4,
            deharsh: 0.6,
            ..Default::default()
        };
        let d = design(BassRole::Satellite, 0.7, &sound, RATE);
        assert_eq!(EnhanceDesign::from_params(&d.to_params()), d);
    }

    #[test]
    fn width_scales_the_side_signal_only() {
        let wide = SoundSettings {
            width: 2.0,
            ..Default::default()
        };
        let mut p = processor(BassRole::Full, 0.0, wide);
        // Mid (same in both) is unchanged; side (opposite) is doubled.
        let mut mid = [0.2, 0.2];
        p.process(&mut mid);
        assert!((mid[0] - 0.2).abs() < 1e-6 && (mid[1] - 0.2).abs() < 1e-6);
        let mut side = [0.2, -0.2];
        p.process(&mut side);
        assert!((side[0] - 0.4).abs() < 1e-6 && (side[1] + 0.4).abs() < 1e-6, "{side:?}");
        let mono = SoundSettings {
            width: 0.0,
            ..Default::default()
        };
        let mut p = processor(BassRole::Full, 0.0, mono);
        let mut f = [0.4, 0.0];
        p.process(&mut f);
        assert!((f[0] - 0.2).abs() < 1e-6 && (f[1] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn deharsh_cuts_loud_presence_band_but_not_quiet_or_bass() {
        let sound = SoundSettings {
            deharsh: 1.0,
            ..Default::default()
        };
        let mut loud = processor(BassRole::Full, 0.0, sound);
        let cut = db(rms_out(&mut loud, 3_500.0, 0.7, 0.5) / (0.7 * 0.7071));
        assert!(cut < -4.0, "loud 3.5 kHz cut by {cut} dB");
        let mut quiet = processor(BassRole::Full, 0.0, sound);
        let q = db(rms_out(&mut quiet, 3_500.0, 0.01, 0.5) / (0.01 * 0.7071));
        assert!(q.abs() < 0.5, "quiet 3.5 kHz untouched: {q}");
        let mut bass = processor(BassRole::Full, 0.0, sound);
        let b = db(rms_out(&mut bass, 100.0, 0.7, 0.5) / (0.7 * 0.7071));
        assert!(b.abs() < 1.0, "loud bass untouched: {b}");
    }

    #[test]
    fn listening_modes_set_tone_but_keep_speaker_setup() {
        let base = SoundSettings {
            crossover_hz: 80,
            match_levels: false,
            room_target: crate::room::RoomTarget::Flat,
            ..Default::default()
        };
        for mode in ListeningMode::ALL {
            let s = mode.apply(base);
            assert!(s.validate().is_ok(), "{mode:?}");
            assert_eq!(s.crossover_hz, 80);
            assert!(!s.match_levels);
            assert_eq!(s.room_target, crate::room::RoomTarget::Flat);
            assert_eq!(ListeningMode::matching(&s), mode);
        }
        assert_eq!(ListeningMode::Custom.apply(base), base);
        assert_eq!(ListeningMode::matching(&base), ListeningMode::Custom);
    }

    #[test]
    fn validates_app_modes() {
        let m = |exe: &str, mode| AppMode {
            exe: exe.into(),
            mode,
        };
        assert!(validate_app_modes(&[m("Spotify.exe", ListeningMode::Music)]).is_ok());
        assert!(validate_app_modes(&[m("..\\x.exe", ListeningMode::Music)]).is_err());
        assert!(validate_app_modes(&[m("game", ListeningMode::Game)]).is_err());
        assert!(validate_app_modes(&[m("a.exe", ListeningMode::Custom)]).is_err());
        assert!(validate_app_modes(&[
            m("a.exe", ListeningMode::Music),
            m("A.EXE", ListeningMode::Game)
        ])
        .is_err());
    }

    #[test]
    fn validates_settings() {
        assert!(SoundSettings::default().validate().is_ok());
        let mut s = SoundSettings {
            crossover_hz: 90,
            ..Default::default()
        };
        assert!(s.validate().is_err());
        s.crossover_hz = 120;
        s.tilt_db = 7.0;
        assert!(s.validate().is_err());
        s.tilt_db = 0.0;
        s.loudness = f32::NAN;
        assert!(s.validate().is_err());
    }
}
