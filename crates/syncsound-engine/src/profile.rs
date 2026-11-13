//! Versioned device profiles stored in per-user app data. Loading never matches by
//! friendly name: missing endpoints are reported and require an explicit rebind.
use crate::capture::{AudioEndpoint, CaptureSource};
use crate::commands::{validate_delay, validate_volume, CommandError};
use crate::placement::{validate_distance, validate_trim, SpeakerPosition};
use crate::render::{AudioMode, ChannelRole, MAX_ENABLED_OUTPUTS};
use crate::routing::{EngineSnapshot, OutputConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// v1 (pre-release draft) had no `source`, `muted` or `delay_ms`; v2 adds them.
pub const PROFILE_SCHEMA_VERSION: u32 = 2;
const MAX_PROFILE_OUTPUTS: usize = 64;
const MAX_PROFILE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "endpoint_id", rename_all = "snake_case")]
pub enum ProfileSource {
    DefaultRender,
    Endpoint(String),
    SystemExcludingSelf,
}

impl From<&CaptureSource> for ProfileSource {
    fn from(source: &CaptureSource) -> Self {
        match source {
            CaptureSource::DefaultRender => Self::DefaultRender,
            CaptureSource::Endpoint(id) => Self::Endpoint(id.clone()),
            CaptureSource::SystemExcludingSelf => Self::SystemExcludingSelf,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileOutput {
    pub endpoint_id: String,
    /// Display hint only. Never used to select a device.
    pub name_hint: String,
    pub enabled: bool,
    pub gain: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub delay_ms: u32,
    #[serde(default)]
    pub role: ChannelRole,
    #[serde(default)]
    pub position: SpeakerPosition,
    #[serde(default)]
    pub distance_m: Option<f32>,
    #[serde(default)]
    pub trim_db: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub schema_version: u32,
    pub name: String,
    pub mode: AudioMode,
    #[serde(default = "default_source")]
    pub source: ProfileSource,
    pub outputs: Vec<ProfileOutput>,
    #[serde(default = "default_compensation")]
    pub distance_compensation: bool,
}

impl ProfileOutput {
    /// Placement defaults; tests fill the remaining fields with struct update syntax.
    #[cfg(test)]
    fn new() -> Self {
        Self {
            endpoint_id: String::new(),
            name_hint: String::new(),
            enabled: false,
            gain: 1.0,
            muted: false,
            delay_ms: 0,
            role: ChannelRole::Stereo,
            position: SpeakerPosition::Unassigned,
            distance_m: None,
            trim_db: 0.0,
        }
    }
}

fn default_compensation() -> bool {
    true
}

fn default_source() -> ProfileSource {
    ProfileSource::DefaultRender
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MissingOutput {
    pub endpoint_id: String,
    pub name_hint: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileApplyPlan {
    pub mode: AudioMode,
    /// None when the pinned source endpoint is missing; the current source is kept.
    pub source: Option<CaptureSource>,
    pub missing_source: Option<String>,
    pub outputs: Vec<OutputConfig>,
    pub missing: Vec<MissingOutput>,
    pub distance_compensation: bool,
}

pub fn validate_name(name: &str) -> Result<(), CommandError> {
    let ok = (1..=64).contains(&name.chars().count())
        && name.trim() == name
        && name
            .chars()
            // All are valid in Windows file names; no '.', '/', '\' or ':' (no traversal).
            .all(|c| {
                c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '+' | '&' | '(' | ')' | ',')
            });
    if ok {
        Ok(())
    } else {
        Err(CommandError::new(
            "invalid_profile_name",
            "Profile names use 1–64 letters, digits, spaces and - _ + & ( ) ,",
        ))
    }
}

impl Profile {
    pub fn from_snapshot(name: &str, snapshot: &EngineSnapshot) -> Result<Self, CommandError> {
        validate_name(name)?;
        let profile = Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            name: name.into(),
            mode: snapshot.mode,
            source: (&snapshot.source).into(),
            outputs: snapshot
                .outputs
                .iter()
                .map(|o| ProfileOutput {
                    endpoint_id: o.endpoint_id.clone(),
                    name_hint: o.name.clone(),
                    enabled: o.config.enabled,
                    gain: o.config.gain,
                    muted: o.config.muted,
                    delay_ms: o.config.delay_ms,
                    role: o.config.role,
                    position: o.config.position,
                    distance_m: o.config.distance_m,
                    trim_db: o.config.trim_db,
                })
                .collect(),
            distance_compensation: snapshot.distance_compensation,
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), CommandError> {
        validate_name(&self.name)?;
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(CommandError::new(
                "profile_version",
                "Unsupported profile version",
            ));
        }
        if self.outputs.len() > MAX_PROFILE_OUTPUTS {
            return Err(CommandError::new(
                "invalid_profile",
                "Too many outputs in profile",
            ));
        }
        if self.outputs.iter().filter(|o| o.enabled).count() > MAX_ENABLED_OUTPUTS {
            return Err(CommandError::new(
                "too_many_outputs",
                format!("A profile can enable at most {MAX_ENABLED_OUTPUTS} outputs"),
            ));
        }
        for (index, output) in self.outputs.iter().enumerate() {
            if output.endpoint_id.is_empty()
                || self.outputs[..index]
                    .iter()
                    .any(|o| o.endpoint_id == output.endpoint_id)
            {
                return Err(CommandError::new(
                    "invalid_profile",
                    "Duplicate or empty endpoint ID",
                ));
            }
            validate_volume(f64::from(output.gain))?;
            validate_delay(i64::from(output.delay_ms))?;
            if let Some(distance) = output.distance_m {
                validate_distance(f64::from(distance))?;
            }
            validate_trim(f64::from(output.trim_db))?;
        }
        Ok(())
    }

    /// Parses any supported schema version and migrates it to the current one.
    pub fn from_json(text: &str) -> Result<Self, CommandError> {
        let invalid = |e: serde_json::Error| CommandError::new("invalid_profile", e.to_string());
        let mut value: serde_json::Value = serde_json::from_str(text).map_err(invalid)?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| CommandError::new("invalid_profile", "Missing schema_version"))?;
        match version {
            1 => {
                // v1 → v2: absent source/muted/delay fields take their serde defaults.
                value["schema_version"] = PROFILE_SCHEMA_VERSION.into();
            }
            v if v == u64::from(PROFILE_SCHEMA_VERSION) => {}
            v if v > u64::from(PROFILE_SCHEMA_VERSION) => {
                return Err(CommandError::new(
                    "profile_version",
                    "Profile was saved by a newer SyncSound version",
                ))
            }
            _ => {
                return Err(CommandError::new(
                    "profile_version",
                    "Unsupported profile version",
                ))
            }
        }
        let profile: Profile = serde_json::from_value(value).map_err(invalid)?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("profile serializes")
    }

    /// Exact-ID matching only. Unavailable-but-known endpoints are applied (the
    /// output will report Disconnected and retry); unknown IDs are listed as missing.
    pub fn plan_apply(&self, endpoints: &[AudioEndpoint]) -> ProfileApplyPlan {
        let known = |id: &str| endpoints.iter().any(|e| e.id == id);
        let (source, missing_source) = match &self.source {
            ProfileSource::DefaultRender => (Some(CaptureSource::DefaultRender), None),
            ProfileSource::SystemExcludingSelf => (Some(CaptureSource::SystemExcludingSelf), None),
            ProfileSource::Endpoint(id) if known(id) => {
                (Some(CaptureSource::Endpoint(id.clone())), None)
            }
            ProfileSource::Endpoint(id) => (None, Some(id.clone())),
        };
        let mut outputs = Vec::new();
        let mut missing = Vec::new();
        for output in &self.outputs {
            if known(&output.endpoint_id) {
                outputs.push(OutputConfig {
                    endpoint_id: output.endpoint_id.clone(),
                    enabled: output.enabled,
                    gain: output.gain,
                    muted: output.muted,
                    delay_ms: output.delay_ms,
                    role: output.role,
                    position: output.position,
                    distance_m: output.distance_m,
                    trim_db: output.trim_db,
                });
            } else {
                missing.push(MissingOutput {
                    endpoint_id: output.endpoint_id.clone(),
                    name_hint: output.name_hint.clone(),
                    enabled: output.enabled,
                });
            }
        }
        ProfileApplyPlan {
            mode: self.mode,
            source,
            missing_source,
            outputs,
            missing,
            distance_compensation: self.distance_compensation,
        }
    }

    /// Explicit user-confirmed rebind of one saved endpoint to a different endpoint.
    pub fn rebind(&mut self, from_id: &str, to: &AudioEndpoint) -> Result<(), CommandError> {
        if self.outputs.iter().any(|o| o.endpoint_id == to.id) {
            return Err(CommandError::new(
                "invalid_rebind",
                "Target device is already in this profile",
            ));
        }
        let mut found = false;
        if let Some(output) = self.outputs.iter_mut().find(|o| o.endpoint_id == from_id) {
            output.endpoint_id = to.id.clone();
            output.name_hint = to.name.clone();
            found = true;
        }
        if self.source == ProfileSource::Endpoint(from_id.into()) {
            self.source = ProfileSource::Endpoint(to.id.clone());
            found = true;
        }
        if found {
            Ok(())
        } else {
            Err(CommandError::new(
                "invalid_rebind",
                "Saved device not found in profile",
            ))
        }
    }
}

/// Profiles live in `<app data>/profiles`, never in the install directory.
pub struct ProfileStore {
    dir: PathBuf,
}

impl ProfileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> Result<PathBuf, CommandError> {
        validate_name(name)?;
        Ok(self.dir.join(format!("{name}.json")))
    }

    pub fn save(&self, profile: &Profile) -> Result<PathBuf, CommandError> {
        profile.validate()?;
        let io = |e: std::io::Error| CommandError::new("profile_io", e.to_string());
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        let path = self.path(&profile.name)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, profile.to_json()).map_err(io)?;
        std::fs::rename(&temp, &path).map_err(io)?;
        Ok(path)
    }

    pub fn load(&self, name: &str) -> Result<Profile, CommandError> {
        let path = self.path(name)?;
        let io = |e: std::io::Error| CommandError::new("profile_io", e.to_string());
        let size = std::fs::metadata(&path)
            .map_err(|_| CommandError::new("profile_not_found", "Profile not found"))?
            .len();
        if size > MAX_PROFILE_BYTES {
            return Err(CommandError::new(
                "invalid_profile",
                "Profile file is too large",
            ));
        }
        let profile = Profile::from_json(&std::fs::read_to_string(&path).map_err(io)?)?;
        if profile.name != name {
            return Err(CommandError::new(
                "invalid_profile",
                "Profile name does not match file",
            ));
        }
        Ok(profile)
    }

    pub fn list(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let stem = name.strip_suffix(".json")?.to_string();
                validate_name(&stem).ok().map(|_| stem)
            })
            .collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EndpointState;

    fn endpoint(id: &str, name: &str) -> AudioEndpoint {
        AudioEndpoint {
            id: id.into(),
            name: name.into(),
            is_default: false,
            state: EndpointState::Active,
            mix_format: None,
            bits_per_sample: None,
            channel_mask: None,
            connection_type: None,
            bus: None,
        }
    }

    fn profile() -> Profile {
        Profile {
            schema_version: PROFILE_SCHEMA_VERSION,
            name: "Living room".into(),
            mode: AudioMode::Synchronized,
            source: ProfileSource::SystemExcludingSelf,
            distance_compensation: true,
            outputs: vec![
                ProfileOutput {
                    endpoint_id: "id-a".into(),
                    name_hint: "Speakers".into(),
                    enabled: true,
                    gain: 0.8,
                    muted: false,
                    delay_ms: 20,
                    ..ProfileOutput::new()
                },
                ProfileOutput {
                    endpoint_id: "id-b".into(),
                    name_hint: "Headphones".into(),
                    enabled: true,
                    gain: 1.0,
                    muted: true,
                    delay_ms: 0,
                    ..ProfileOutput::new()
                },
            ],
        }
    }

    #[test]
    fn round_trips_current_schema() {
        let original = profile();
        assert_eq!(Profile::from_json(&original.to_json()), Ok(original));
    }

    #[test]
    fn migrates_v1_profiles_with_defaults() {
        let v1 = r#"{"schema_version":1,"name":"Desk","mode":"balanced",
            "outputs":[{"endpoint_id":"id-a","name_hint":"Speakers","enabled":true,"gain":0.5}]}"#;
        let migrated = Profile::from_json(v1).unwrap();
        assert_eq!(migrated.schema_version, PROFILE_SCHEMA_VERSION);
        assert_eq!(migrated.source, ProfileSource::DefaultRender);
        assert!(!migrated.outputs[0].muted);
        assert_eq!(migrated.outputs[0].delay_ms, 0);
    }

    #[test]
    fn rejects_newer_invalid_and_unsafe_profiles() {
        let mut newer = profile();
        newer.schema_version = 99;
        let json = serde_json::to_string(&newer).unwrap();
        assert_eq!(
            Profile::from_json(&json).unwrap_err().code,
            "profile_version"
        );
        assert!(Profile::from_json("{}").is_err());
        assert!(Profile::from_json("not json").is_err());
        let mut loud = profile();
        loud.outputs[0].gain = 4.0;
        assert!(Profile::from_json(&loud.to_json()).is_err());
        let mut many = profile();
        many.outputs = (0..9)
            .map(|i| ProfileOutput {
                endpoint_id: format!("id-{i}"),
                name_hint: String::new(),
                enabled: true,
                gain: 1.0,
                muted: false,
                delay_ms: 0,
                ..ProfileOutput::new()
            })
            .collect();
        assert_eq!(
            Profile::from_json(&many.to_json()).unwrap_err().code,
            "too_many_outputs"
        );
        let mut dup = profile();
        dup.outputs[1].endpoint_id = "id-a".into();
        assert!(Profile::from_json(&dup.to_json()).is_err());
        assert!(validate_name("../evil").is_err());
        assert!(validate_name(r"a\b").is_err());
        assert!(validate_name("C:x").is_err());
        assert!(validate_name("Front Realtek + Rear iOX 2").is_ok());
        assert!(validate_name("Desk (music), late & quiet").is_ok());
        assert!(validate_name(" padded").is_err());
    }

    #[test]
    fn missing_devices_are_partial_applied_never_matched_by_name() {
        // "id-b" is gone, and a *different* endpoint now carries its friendly name.
        let endpoints = [endpoint("id-a", "Speakers"), endpoint("id-c", "Headphones")];
        let plan = profile().plan_apply(&endpoints);
        assert_eq!(plan.outputs.len(), 1);
        assert_eq!(plan.outputs[0].endpoint_id, "id-a");
        assert_eq!(plan.outputs[0].delay_ms, 20);
        assert_eq!(plan.missing.len(), 1);
        assert_eq!(plan.missing[0].endpoint_id, "id-b");
        assert_eq!(plan.source, Some(CaptureSource::SystemExcludingSelf));
    }

    #[test]
    fn missing_pinned_source_is_reported_not_substituted() {
        let mut p = profile();
        p.source = ProfileSource::Endpoint("gone".into());
        let plan = p.plan_apply(&[endpoint("id-a", "gone")]);
        assert_eq!(plan.source, None);
        assert_eq!(plan.missing_source.as_deref(), Some("gone"));
    }

    #[test]
    fn explicit_rebind_replaces_one_endpoint() {
        let mut p = profile();
        p.rebind("id-b", &endpoint("id-c", "New headphones"))
            .unwrap();
        assert_eq!(p.outputs[1].endpoint_id, "id-c");
        assert_eq!(p.outputs[1].name_hint, "New headphones");
        assert!(p.outputs[1].muted, "settings are preserved");
        assert!(
            p.rebind("id-a", &endpoint("id-c", "x")).is_err(),
            "no duplicates"
        );
        assert!(p.rebind("nope", &endpoint("id-z", "x")).is_err());
    }

    #[test]
    fn store_saves_atomically_and_lists_valid_profiles() {
        let dir = std::env::temp_dir().join(format!("syncsound-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = ProfileStore::new(&dir);
        assert!(store.list().is_empty());
        let saved = store.save(&profile()).unwrap();
        assert!(saved.starts_with(&dir));
        std::fs::write(dir.join("..bad.json"), "{}").unwrap();
        assert_eq!(store.list(), ["Living room"]);
        assert_eq!(store.load("Living room").unwrap(), profile());
        assert_eq!(store.load("Nope").unwrap_err().code, "profile_not_found");
        assert!(store.load("../x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
