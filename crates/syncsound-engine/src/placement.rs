//! Speaker placement and sound-system presets. Portable and allocation-free on
//! the audio path: the engine computes these on the control thread and pushes
//! the results into each output's atomics.
use crate::commands::CommandError;
use crate::render::{ms_to_frames, ChannelRole, MAX_ALIGN_MS};
use crate::routing::OutputConfig;
use serde::{Deserialize, Serialize};

/// Speed of sound at ~20 °C.
pub const SPEED_OF_SOUND_M_S: f64 = 343.0;
pub const MIN_DISTANCE_M: f32 = 0.2;
pub const MAX_DISTANCE_M: f32 = 20.0;
pub const MIN_TRIM_DB: f32 = -24.0;
/// Inverse-distance level compensation never cuts an output below this.
const MIN_DISTANCE_LEVEL: f32 = 0.25;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerPosition {
    #[default]
    Unassigned,
    FrontLeft,
    Front,
    FrontRight,
    Left,
    Right,
    RearLeft,
    Rear,
    RearRight,
}

impl SpeakerPosition {
    pub fn is_rear(self) -> bool {
        matches!(
            self,
            SpeakerPosition::RearLeft | SpeakerPosition::Rear | SpeakerPosition::RearRight
        )
    }
    pub fn is_left(self) -> bool {
        matches!(
            self,
            SpeakerPosition::FrontLeft | SpeakerPosition::Left | SpeakerPosition::RearLeft
        )
    }
    pub fn is_right(self) -> bool {
        matches!(
            self,
            SpeakerPosition::FrontRight | SpeakerPosition::Right | SpeakerPosition::RearRight
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundPreset {
    /// Every output plays full stereo at its own level.
    Stereo,
    /// Front outputs at full level; outputs placed at the rear get −3 dB.
    FrontRear,
    /// Exactly two outputs form one stereo pair: one plays left, the other right.
    LeftRightSplit,
    /// Every output plays the same mono mix.
    PartyMono,
}

pub const REAR_TRIM_DB: f32 = -3.0;

/// Effective per-output compensation derived from placement and trim.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub delay_frames: u32,
    /// Linear multiplier in (0, 1].
    pub level: f32,
}

pub fn db_to_factor(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn validate_distance(distance_m: f64) -> Result<f32, CommandError> {
    if !distance_m.is_finite()
        || !(f64::from(MIN_DISTANCE_M)..=f64::from(MAX_DISTANCE_M)).contains(&distance_m)
    {
        return Err(CommandError::new(
            "invalid_distance",
            format!("Distance must be between {MIN_DISTANCE_M} and {MAX_DISTANCE_M} m"),
        ));
    }
    Ok(distance_m as f32)
}

pub fn validate_trim(trim_db: f64) -> Result<f32, CommandError> {
    if !trim_db.is_finite() || !(f64::from(MIN_TRIM_DB)..=0.0).contains(&trim_db) {
        return Err(CommandError::new(
            "invalid_trim",
            format!("Level trim must be between {MIN_TRIM_DB} and 0 dB"),
        ));
    }
    Ok(trim_db as f32)
}

/// Per-output placement for every config, in order. With compensation on, outputs
/// nearer than the farthest *enabled* output are delayed so sound from all of
/// them arrives at the seat together, and turned down by inverse distance so
/// they are about as loud there. Outputs without a distance get no compensation.
/// Trim always applies.
pub fn compute_placement(outputs: &[OutputConfig], compensate: bool, rate: u32) -> Vec<Placement> {
    let farthest = outputs
        .iter()
        .filter(|o| o.enabled)
        .filter_map(|o| o.distance_m)
        .fold(None::<f32>, |max, d| Some(max.map_or(d, |m| m.max(d))));
    let max_frames = u64::from(ms_to_frames(MAX_ALIGN_MS, rate));
    outputs
        .iter()
        .map(|o| {
            let trim = db_to_factor(o.trim_db.clamp(MIN_TRIM_DB, 0.0));
            match (compensate, o.distance_m, farthest) {
                (true, Some(d), Some(far)) if o.enabled => {
                    let seconds = f64::from(far - d).max(0.0) / SPEED_OF_SOUND_M_S;
                    let frames = ((seconds * f64::from(rate)).round() as u64).min(max_frames);
                    let level = (d / far).clamp(MIN_DISTANCE_LEVEL, 1.0);
                    Placement {
                        delay_frames: frames as u32,
                        level: level * trim,
                    }
                }
                _ => Placement {
                    delay_frames: 0,
                    level: trim,
                },
            }
        })
        .collect()
}

/// Sets role and trim on the enabled outputs for a preset. Positions, distances,
/// gain and delay are left as the user set them.
pub fn apply_preset(preset: SoundPreset, outputs: &mut [OutputConfig]) -> Result<(), CommandError> {
    let enabled: Vec<usize> = (0..outputs.len()).filter(|i| outputs[*i].enabled).collect();
    if enabled.is_empty() {
        return Err(CommandError::new(
            "invalid_preset",
            "Turn on at least one output first",
        ));
    }
    match preset {
        SoundPreset::Stereo | SoundPreset::PartyMono => {
            let role = if preset == SoundPreset::Stereo {
                ChannelRole::Stereo
            } else {
                ChannelRole::Mono
            };
            for i in enabled {
                outputs[i].role = role;
                outputs[i].trim_db = 0.0;
            }
        }
        SoundPreset::FrontRear => {
            if !enabled.iter().any(|i| outputs[*i].position.is_rear()) {
                return Err(CommandError::new(
                    "invalid_preset",
                    "Set the position of at least one output to Rear first",
                ));
            }
            for i in enabled {
                outputs[i].role = ChannelRole::Stereo;
                outputs[i].trim_db = if outputs[i].position.is_rear() {
                    REAR_TRIM_DB
                } else {
                    0.0
                };
            }
        }
        SoundPreset::LeftRightSplit => {
            if enabled.len() != 2 {
                return Err(CommandError::new(
                    "invalid_preset",
                    "Left/right split needs exactly two outputs turned on",
                ));
            }
            let (a, b) = (enabled[0], enabled[1]);
            // Positions decide which is left; otherwise keep list order.
            let (left, right) = if outputs[b].position.is_left() || outputs[a].position.is_right() {
                (b, a)
            } else {
                (a, b)
            };
            outputs[left].role = ChannelRole::LeftOnly;
            outputs[right].role = ChannelRole::RightOnly;
            outputs[left].trim_db = 0.0;
            outputs[right].trim_db = 0.0;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(id: &str, position: SpeakerPosition, distance: Option<f32>) -> OutputConfig {
        OutputConfig {
            enabled: true,
            position,
            distance_m: distance,
            ..OutputConfig::new(id)
        }
    }

    #[test]
    fn nearer_outputs_are_delayed_and_turned_down() {
        let outputs = [
            out("front", SpeakerPosition::Front, Some(3.43)),
            out("rear", SpeakerPosition::Rear, Some(1.715)),
        ];
        let p = compute_placement(&outputs, true, 48_000);
        assert_eq!(
            p[0],
            Placement {
                delay_frames: 0,
                level: 1.0
            }
        );
        // 1.715 m closer = 5 ms = 240 frames, half the distance = half the level.
        assert_eq!(p[1].delay_frames, 240);
        assert!((p[1].level - 0.5).abs() < 1e-6);
    }

    #[test]
    fn compensation_off_or_unknown_distance_only_applies_trim() {
        let mut outputs = [
            out("a", SpeakerPosition::Front, Some(4.0)),
            out("b", SpeakerPosition::Rear, None),
        ];
        outputs[1].trim_db = -6.0;
        let on = compute_placement(&outputs, true, 48_000);
        assert_eq!(on[1].delay_frames, 0);
        assert!((on[1].level - db_to_factor(-6.0)).abs() < 1e-6);
        let off = compute_placement(&outputs, false, 48_000);
        assert_eq!(
            off[0],
            Placement {
                delay_frames: 0,
                level: 1.0
            }
        );
    }

    #[test]
    fn disabled_outputs_do_not_set_the_reference_and_level_is_floored() {
        let mut outputs = [
            out("near", SpeakerPosition::Front, Some(0.2)),
            out("far", SpeakerPosition::Rear, Some(2.0)),
            out("off", SpeakerPosition::Rear, Some(20.0)),
        ];
        outputs[2].enabled = false;
        let p = compute_placement(&outputs, true, 48_000);
        assert_eq!(p[1].delay_frames, 0, "far enabled output is the reference");
        assert_eq!(p[0].level, MIN_DISTANCE_LEVEL);
        assert_eq!(p[2].delay_frames, 0);
    }

    #[test]
    fn presets_set_roles_and_rear_level() {
        let mut outputs = vec![
            out("front", SpeakerPosition::Front, None),
            out("rear", SpeakerPosition::Rear, None),
        ];
        apply_preset(SoundPreset::FrontRear, &mut outputs).unwrap();
        assert_eq!(outputs[0].trim_db, 0.0);
        assert_eq!(outputs[1].trim_db, REAR_TRIM_DB);
        apply_preset(SoundPreset::PartyMono, &mut outputs).unwrap();
        assert!(outputs
            .iter()
            .all(|o| o.role == ChannelRole::Mono && o.trim_db == 0.0));
        apply_preset(SoundPreset::Stereo, &mut outputs).unwrap();
        assert!(outputs.iter().all(|o| o.role == ChannelRole::Stereo));
    }

    #[test]
    fn left_right_split_uses_positions_and_needs_two_outputs() {
        let mut outputs = vec![
            out("a", SpeakerPosition::Right, None),
            out("b", SpeakerPosition::Left, None),
        ];
        apply_preset(SoundPreset::LeftRightSplit, &mut outputs).unwrap();
        assert_eq!(outputs[0].role, ChannelRole::RightOnly);
        assert_eq!(outputs[1].role, ChannelRole::LeftOnly);
        outputs.push(out("c", SpeakerPosition::Front, None));
        assert!(apply_preset(SoundPreset::LeftRightSplit, &mut outputs).is_err());
    }

    #[test]
    fn front_rear_requires_a_rear_output_and_presets_need_outputs() {
        let mut outputs = vec![out("a", SpeakerPosition::Front, None)];
        assert!(apply_preset(SoundPreset::FrontRear, &mut outputs).is_err());
        outputs[0].enabled = false;
        assert!(apply_preset(SoundPreset::Stereo, &mut outputs).is_err());
    }

    #[test]
    fn validates_distance_and_trim() {
        assert!(validate_distance(0.1).is_err());
        assert!(validate_distance(f64::NAN).is_err());
        assert!(validate_distance(25.0).is_err());
        assert_eq!(validate_distance(1.5), Ok(1.5));
        assert!(validate_trim(1.0).is_err());
        assert!(validate_trim(-30.0).is_err());
        assert_eq!(validate_trim(-3.0), Ok(-3.0));
    }
}
