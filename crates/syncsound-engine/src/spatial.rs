//! Spatial rendering: place every output on a circle around the listener, build
//! virtual channels from the stereo capture (plain stereo or a 7.1 upmix), and
//! pan each virtual channel onto the real speakers with pairwise constant-power
//! panning (2-D VBAP). "8D" rotates the whole virtual field around the listener.
//!
//! The capture is stereo, so "7.1" is an upmix (matrix-derived centre, sides and
//! rears), not discrete 7.1 content. LFE is a low-passed mono feed for outputs
//! marked as subwoofer.
//!
//! Everything the audio path reads lives in atomics; `GainMatrix::compute` never
//! allocates. Angles: degrees, 0 = front, positive = clockwise (to the right).
use serde::{Deserialize, Serialize};
use std::f32::consts::{FRAC_PI_2, PI};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

/// Two speaker points per output (its left and right channel), up to 8 outputs.
pub const MAX_POINTS: usize = 16;
pub const MAX_VIRTUAL: usize = 7;
pub const MIN_ROTATION_S: f32 = 2.0;
pub const MAX_ROTATION_S: f32 = 60.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLayout {
    /// Virtual left/right at ±30°.
    #[default]
    Stereo,
    /// C, FL, FR, SL, SR, BL, BR derived from stereo (+ LFE to subwoofers).
    Upmix71,
}

/// How far apart an output's own left and right channels sit on the circle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StereoSpread {
    /// One box, both channels at the same spot.
    #[default]
    Point,
    /// A small stereo speaker: ±15°.
    Narrow,
    /// A wide stereo pair: ±45°.
    Wide,
    /// Headphones: ±90° (left ear, right ear).
    Headphones,
}

impl StereoSpread {
    pub fn half_angle(self) -> f32 {
        match self {
            StereoSpread::Point => 0.0,
            StereoSpread::Narrow => 15.0,
            StereoSpread::Wide => 45.0,
            StereoSpread::Headphones => 90.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpatialConfig {
    pub enabled: bool,
    pub layout: SourceLayout,
    /// 8D: seconds per full turn of the sound field; None = static.
    pub rotation_s: Option<f32>,
    pub clockwise: bool,
}

impl Default for SpatialConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            layout: SourceLayout::Stereo,
            rotation_s: None,
            clockwise: true,
        }
    }
}

pub fn wrap_deg(angle: f32) -> f32 {
    let a = (angle + 180.0).rem_euclid(360.0) - 180.0;
    if a == -180.0 {
        180.0
    } else {
        a
    }
}

/// Virtual channel angles for a layout.
pub fn virtual_angles(layout: SourceLayout) -> &'static [f32] {
    match layout {
        SourceLayout::Stereo => &[-30.0, 30.0],
        //                       C    FL     FR    SL     SR    BL      BR
        SourceLayout::Upmix71 => &[0.0, -30.0, 30.0, -90.0, 90.0, -150.0, 150.0],
    }
}

/// Derives the virtual channel samples from one stereo frame.
#[inline]
pub fn virtual_channels(
    layout: SourceLayout,
    l: f32,
    r: f32,
    out: &mut [f32; MAX_VIRTUAL],
) -> usize {
    match layout {
        SourceLayout::Stereo => {
            out[0] = l;
            out[1] = r;
            2
        }
        SourceLayout::Upmix71 => {
            // Passive matrix upmix: centre from the sum, surrounds from the
            // difference (ambience), fronts keep most of the original image.
            let mid = 0.5 * (l + r);
            let side = 0.5 * (l - r);
            out[0] = 0.7 * mid;
            out[1] = 0.8 * l;
            out[2] = 0.8 * r;
            out[3] = 0.6 * side + 0.25 * l;
            out[4] = -0.6 * side + 0.25 * r;
            out[5] = 0.5 * side;
            out[6] = -0.5 * side;
            7
        }
    }
}

/// Pairwise constant-power panning of a source at `theta` onto `points`
/// (angles in degrees). Points at the same angle share the gain equally.
/// Writes one gain per point into `gains`.
pub fn pan(points: &[f32], theta: f32, gains: &mut [f32]) {
    let n = points.len();
    gains[..n].fill(0.0);
    if n == 0 {
        return;
    }
    let theta = wrap_deg(theta);
    // Distinct angles, ascending (n ≤ 16: insertion into a fixed array).
    let mut distinct = [0.0f32; MAX_POINTS];
    let mut count = 0;
    for &p in points {
        let p = wrap_deg(p);
        if distinct[..count].iter().any(|d| (d - p).abs() < 0.5) {
            continue;
        }
        let mut i = count;
        while i > 0 && distinct[i - 1] > p {
            distinct[i] = distinct[i - 1];
            i -= 1;
        }
        distinct[i] = p;
        count += 1;
    }
    let share = |gains: &mut [f32], angle: f32, g: f32| {
        let members = points
            .iter()
            .filter(|p| (wrap_deg(**p) - angle).abs() < 0.5)
            .count();
        let per = g / (members as f32).sqrt();
        for (i, p) in points.iter().enumerate() {
            if (wrap_deg(*p) - angle).abs() < 0.5 {
                gains[i] += per;
            }
        }
    };
    if count == 1 {
        share(gains, distinct[0], 1.0);
        return;
    }
    // Find the arc [a, b] (clockwise from a) that contains theta.
    for k in 0..count {
        let a = distinct[k];
        let b = distinct[(k + 1) % count];
        let span = (b - a).rem_euclid(360.0);
        let span = if span == 0.0 { 360.0 } else { span };
        let offset = (theta - a).rem_euclid(360.0);
        if offset <= span {
            let t = offset / span;
            share(gains, a, (t * FRAC_PI_2).cos());
            share(gains, b, (t * FRAC_PI_2).sin());
            return;
        }
    }
}

/// Shared spatial state: written by the control thread, read by every output.
#[derive(Debug, Default)]
pub struct SpatialShared {
    enabled: AtomicBool,
    layout: AtomicU8,
    /// Rotation period in ms, 0 = static.
    rotation_ms: AtomicU32,
    clockwise: AtomicBool,
    point_count: AtomicU8,
    points: [AtomicU32; MAX_POINTS],
}

impl SpatialShared {
    pub fn set(&self, config: SpatialConfig, points: &[f32]) {
        let n = points.len().min(MAX_POINTS);
        for (slot, p) in self.points.iter().zip(points) {
            slot.store(p.to_bits(), Ordering::Relaxed);
        }
        self.point_count.store(n as u8, Ordering::Relaxed);
        self.layout.store(
            match config.layout {
                SourceLayout::Stereo => 0,
                SourceLayout::Upmix71 => 1,
            },
            Ordering::Relaxed,
        );
        let ms = config
            .rotation_s
            .map(|s| (s.clamp(MIN_ROTATION_S, MAX_ROTATION_S) * 1000.0) as u32)
            .unwrap_or(0);
        self.rotation_ms.store(ms, Ordering::Relaxed);
        self.clockwise.store(config.clockwise, Ordering::Relaxed);
        self.enabled.store(config.enabled, Ordering::Release);
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn layout(&self) -> SourceLayout {
        if self.layout.load(Ordering::Relaxed) == 1 {
            SourceLayout::Upmix71
        } else {
            SourceLayout::Stereo
        }
    }
    /// Field rotation in degrees at a source frame position.
    pub fn rotation_at(&self, frame: u64, rate: u32) -> f32 {
        let ms = self.rotation_ms.load(Ordering::Relaxed);
        if ms == 0 || rate == 0 {
            return 0.0;
        }
        let period = u64::from(ms) * u64::from(rate) / 1000;
        let phase = (frame % period.max(1)) as f32 / period.max(1) as f32;
        let deg = phase * 360.0;
        if self.clockwise.load(Ordering::Relaxed) {
            deg
        } else {
            -deg
        }
    }
    fn load_points(&self, out: &mut [f32; MAX_POINTS]) -> usize {
        let n = usize::from(self.point_count.load(Ordering::Relaxed)).min(MAX_POINTS);
        for (o, p) in out.iter_mut().zip(&self.points[..n]) {
            *o = f32::from_bits(p.load(Ordering::Relaxed));
        }
        n
    }
}

/// Per-output 2 × virtual gain matrix. Computed once per callback (block
/// start and end) and interpolated per frame by the render pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GainMatrix {
    pub left: [f32; MAX_VIRTUAL],
    pub right: [f32; MAX_VIRTUAL],
    pub channels: usize,
}

impl GainMatrix {
    /// `slot` is this output's index among the spatial outputs: its points
    /// are `2 * slot` (left) and `2 * slot + 1` (right).
    pub fn compute(shared: &SpatialShared, slot: usize, rotation: f32) -> Self {
        let mut points = [0.0f32; MAX_POINTS];
        let n = shared.load_points(&mut points);
        let layout = shared.layout();
        let angles = virtual_angles(layout);
        let mut matrix = GainMatrix {
            channels: angles.len(),
            ..Default::default()
        };
        if 2 * slot + 1 >= n {
            return matrix;
        }
        let mut gains = [0.0f32; MAX_POINTS];
        for (v, base) in angles.iter().enumerate() {
            pan(&points[..n], base + rotation, &mut gains);
            matrix.left[v] = gains[2 * slot];
            matrix.right[v] = gains[2 * slot + 1];
        }
        matrix
    }
}

pub fn validate_azimuth(azimuth: f64) -> Result<f32, crate::commands::CommandError> {
    if !azimuth.is_finite() || !(-180.0..=180.0).contains(&azimuth) {
        return Err(crate::commands::CommandError::new(
            "invalid_azimuth",
            "Angle must be between -180 and 180 degrees",
        ));
    }
    Ok(azimuth as f32)
}

pub fn validate_spatial(config: &SpatialConfig) -> Result<(), crate::commands::CommandError> {
    if let Some(s) = config.rotation_s {
        if !s.is_finite() || !(MIN_ROTATION_S..=MAX_ROTATION_S).contains(&s) {
            return Err(crate::commands::CommandError::new(
                "invalid_rotation",
                format!("8D rotation must take {MIN_ROTATION_S}-{MAX_ROTATION_S} s per turn"),
            ));
        }
    }
    Ok(())
}

/// Map position → azimuth for outputs that were placed by name, not on the map.
pub fn position_azimuth(position: crate::placement::SpeakerPosition) -> Option<f32> {
    use crate::placement::SpeakerPosition::*;
    Some(match position {
        Unassigned => return None,
        Front => 0.0,
        FrontLeft => -30.0,
        FrontRight => 30.0,
        Left => -90.0,
        Right => 90.0,
        RearLeft => -150.0,
        Rear => 180.0,
        RearRight => 150.0,
    })
}

/// Nearest named position for an azimuth (keeps Front + Rear presets working).
pub fn nearest_position(azimuth: f32) -> crate::placement::SpeakerPosition {
    use crate::placement::SpeakerPosition::*;
    let options = [
        (Front, 0.0),
        (FrontLeft, -30.0),
        (FrontRight, 30.0),
        (Left, -90.0),
        (Right, 90.0),
        (RearLeft, -150.0),
        (Rear, 180.0),
        (RearRight, 150.0),
    ];
    options
        .iter()
        .min_by(|a, b| {
            let da = wrap_deg(azimuth - a.1).abs();
            let db = wrap_deg(azimuth - b.1).abs();
            da.partial_cmp(&db).unwrap()
        })
        .map(|o| o.0)
        .unwrap()
}

/// One-pole low-pass for the LFE feed (~120 Hz).
#[derive(Clone, Copy, Debug, Default)]
pub struct LowPass {
    state: f32,
}

impl LowPass {
    pub fn coefficient(rate: u32) -> f32 {
        1.0 - (-2.0 * PI * 120.0 / rate.max(1) as f32).exp()
    }
    #[inline]
    pub fn process(&mut self, x: f32, a: f32) -> f32 {
        self.state += a * (x - self.state);
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn power(g: &[f32]) -> f32 {
        g.iter().map(|x| x * x).sum()
    }

    #[test]
    fn pans_between_adjacent_speakers_with_constant_power() {
        let points = [-30.0, 30.0, -110.0, 110.0];
        let mut g = [0.0; 4];
        pan(&points, 0.0, &mut g);
        assert!((g[0] - g[1]).abs() < 1e-6 && g[2] == 0.0 && g[3] == 0.0);
        assert!((power(&g) - 1.0).abs() < 1e-5);
        pan(&points, 30.0, &mut g);
        assert!((g[1] - 1.0).abs() < 1e-6);
        pan(&points, 180.0, &mut g); // between rears
        assert!((g[2] - g[3]).abs() < 1e-6 && g[0] == 0.0);
        for theta in (-180..180).step_by(7) {
            pan(&points, theta as f32, &mut g);
            assert!((power(&g) - 1.0).abs() < 1e-4, "theta {theta}");
        }
    }

    #[test]
    fn coincident_points_share_gain_and_single_point_gets_everything() {
        let mut g = [0.0; 3];
        pan(&[0.0, 0.0, 180.0], 0.0, &mut g);
        assert!((g[0] - g[1]).abs() < 1e-6);
        assert!((power(&g) - 1.0).abs() < 1e-5);
        let mut one = [0.0; 1];
        pan(&[90.0], -45.0, &mut one);
        assert_eq!(one[0], 1.0);
    }

    #[test]
    fn eight_d_moves_sound_around_headphones() {
        // Headphones at the front: ears at −90° / +90°. Rotating a stereo field
        // swaps which ear hears the virtual left channel after half a turn.
        let shared = SpatialShared::default();
        let config = SpatialConfig {
            enabled: true,
            layout: SourceLayout::Stereo,
            rotation_s: Some(8.0),
            clockwise: true,
        };
        shared.set(config, &[-90.0, 90.0]);
        let rate = 48_000;
        let start = GainMatrix::compute(&shared, 0, shared.rotation_at(0, rate));
        assert!(
            start.left[0] > start.right[0],
            "virtual L starts in the left ear"
        );
        let half = GainMatrix::compute(&shared, 0, shared.rotation_at(4 * 48_000, rate));
        assert!((shared.rotation_at(4 * 48_000, rate) - 180.0).abs() < 0.01);
        assert!(
            half.right[0] > half.left[0],
            "after half a turn it is in the right ear"
        );
        let full = shared.rotation_at(8 * 48_000, rate);
        assert!(full.abs() < 0.01);
    }

    #[test]
    fn upmix_produces_seven_channels_and_centre_from_mono() {
        let mut v = [0.0; MAX_VIRTUAL];
        assert_eq!(virtual_channels(SourceLayout::Upmix71, 1.0, 1.0, &mut v), 7);
        assert!(v[0] > 0.0, "centre");
        assert_eq!(v[5], 0.0, "mono content leaves the rears silent");
        assert_eq!(virtual_channels(SourceLayout::Stereo, 0.2, 0.4, &mut v), 2);
        assert_eq!(&v[..2], &[0.2, 0.4]);
    }

    #[test]
    fn front_and_rear_speakers_split_a_side_source() {
        // Your layout: Realtek front (point at 0°), iOX 2 rear (point at 180°).
        let shared = SpatialShared::default();
        shared.set(
            SpatialConfig {
                enabled: true,
                layout: SourceLayout::Upmix71,
                rotation_s: None,
                clockwise: true,
            },
            &[0.0, 0.0, 180.0, 180.0],
        );
        let front = GainMatrix::compute(&shared, 0, 0.0);
        let rear = GainMatrix::compute(&shared, 1, 0.0);
        assert!(
            front.left[0] > 0.7 && rear.left[0] == 0.0,
            "centre is front"
        );
        assert!(rear.left[5] > front.left[5], "rear-left is mostly rear");
        let side_front = front.left[3] * front.left[3] * 2.0;
        let side_rear = rear.left[3] * rear.left[3] * 2.0;
        assert!((side_front - side_rear).abs() < 1e-4, "side splits evenly");
        assert_eq!(
            GainMatrix::compute(&shared, 5, 0.0).left,
            [0.0; MAX_VIRTUAL]
        );
    }

    #[test]
    fn positions_map_to_azimuths_and_back() {
        use crate::placement::SpeakerPosition;
        assert_eq!(position_azimuth(SpeakerPosition::RearLeft), Some(-150.0));
        assert_eq!(nearest_position(-170.0), SpeakerPosition::Rear);
        assert_eq!(nearest_position(-140.0), SpeakerPosition::RearLeft);
        assert_eq!(nearest_position(12.0), SpeakerPosition::Front);
        assert_eq!(wrap_deg(190.0), -170.0);
        assert_eq!(wrap_deg(-180.0), 180.0);
    }

    #[test]
    fn lowpass_passes_dc_and_attenuates_high_frequencies() {
        let a = LowPass::coefficient(48_000);
        let mut lp = LowPass::default();
        let mut y = 0.0;
        for _ in 0..48_000 {
            y = lp.process(1.0, a);
        }
        assert!((y - 1.0).abs() < 1e-3);
        let mut lp = LowPass::default();
        let mut peak = 0.0f32;
        for n in 0..4_800 {
            let x = if n % 2 == 0 { 1.0 } else { -1.0 }; // 24 kHz
            peak = peak.max(lp.process(x, a).abs());
        }
        assert!(peak < 0.05);
    }
}
