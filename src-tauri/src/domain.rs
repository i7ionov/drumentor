use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackInfo {
    pub id: u16,
    pub name: String,
    pub note_count: u32,
    pub is_drum_candidate: bool,
    pub drum_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongSummary {
    pub path: String,
    pub track_count: u16,
    pub duration_ms: u64,
    pub tracks: Vec<TrackInfo>,
    pub suggested_drum_track_id: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiInputPort {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportState {
    pub status: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    /// Playback rate multiplier (0.1–1.0). Song timeline stays in ms at 1×.
    pub speed: f64,
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            status: "stopped".into(),
            position_ms: 0,
            duration_ms: 0,
            speed: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedHit {
    pub uid: u64,
    pub pad_id: PadId,
    pub time_ms: u64,
    pub velocity: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionEvent {
    pub position_ms: u64,
    pub status: String,
    pub duration_ms: u64,
    pub speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightEvent {
    pub pad_id: PadId,
    pub at_ms: u64,
    pub velocity: u8,
    pub uid: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteRole {
    Drum,
    Backing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleNote {
    pub uid: u64,
    pub role: NoteRole,
    pub channel: u8,
    pub program: u8,
    pub note: u8,
    pub velocity: u8,
    pub when_ms: u64,
    pub duration_ms: u64,
}

/// Logical drum kit pads — mirrors docs/DOMAIN.md
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PadId {
    Kick,
    Snare,
    HiHatClosed,
    HiHatOpen,
    HiHatPedal,
    TomHigh,
    TomMid,
    FloorTom,
    Crash,
    Ride,
    RideBell,
    Splash,
}

/// Required for pad-map wizard + kit highlight (MVP).
#[allow(dead_code)]
pub const REQUIRED_PADS: &[PadId] = &[
    PadId::Kick,
    PadId::Snare,
    PadId::HiHatClosed,
    PadId::HiHatOpen,
    PadId::TomHigh,
    PadId::TomMid,
    PadId::FloorTom,
    PadId::Crash,
    PadId::Ride,
];

/// Skippable in wizard; still resolved from GM notes.
#[allow(dead_code)]
pub const OPTIONAL_PADS: &[PadId] = &[PadId::HiHatPedal, PadId::RideBell, PadId::Splash];

impl PadId {
    #[allow(dead_code)]
    pub fn is_required(self) -> bool {
        REQUIRED_PADS.contains(&self)
    }
}

/// GM note → PadId. Many notes may collapse to one pad (many-to-one).
pub fn gm_note_to_pad(note: u8) -> Option<PadId> {
    match note {
        35 | 36 => Some(PadId::Kick),
        38 | 40 => Some(PadId::Snare),
        // 44 = pedal HH (common in Guitar Pro exports) → show as closed hat
        42 | 44 => Some(PadId::HiHatClosed),
        46 => Some(PadId::HiHatOpen),
        48 | 50 => Some(PadId::TomHigh),
        45 | 47 => Some(PadId::TomMid),
        41 | 43 => Some(PadId::FloorTom),
        49 | 57 => Some(PadId::Crash),
        51 | 59 => Some(PadId::Ride),
        53 => Some(PadId::RideBell),
        55 => Some(PadId::Splash),
        _ => None,
    }
}

/// Canonical GM percussion key for SoundFont feedback of a mapped pad.
pub fn pad_to_gm_note(pad: PadId) -> u8 {
    match pad {
        PadId::Kick => 36,
        PadId::Snare => 38,
        PadId::HiHatClosed => 42,
        PadId::HiHatOpen => 46,
        PadId::HiHatPedal => 44,
        PadId::TomHigh => 50,
        PadId::TomMid => 47,
        PadId::FloorTom => 43,
        PadId::Crash => 49,
        PadId::Ride => 51,
        PadId::RideBell => 53,
        PadId::Splash => 55,
    }
}

/// Optional pads without a device binding fold into a playable stand-in
/// (e.g. no splash cymbal → song splash notes count as Crash).
pub fn fold_pad_for_device_map(
    pad: PadId,
    mapped_pads: &std::collections::HashSet<PadId>,
) -> PadId {
    match pad {
        PadId::Splash if !mapped_pads.contains(&PadId::Splash) => PadId::Crash,
        PadId::RideBell if !mapped_pads.contains(&PadId::RideBell) => PadId::Ride,
        other => other,
    }
}

pub fn fold_expected_hits(
    hits: &[ExpectedHit],
    profile: Option<&PadMapProfile>,
) -> Vec<ExpectedHit> {
    let mapped: std::collections::HashSet<PadId> = profile
        .map(|p| p.bindings.iter().map(|b| b.pad_id).collect())
        .unwrap_or_default();
    hits.iter()
        .map(|h| ExpectedHit {
            uid: h.uid,
            pad_id: fold_pad_for_device_map(h.pad_id, &mapped),
            time_ms: h.time_ms,
            velocity: h.velocity,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PadBinding {
    pub pad_id: PadId,
    pub midi_note: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PadMapProfile {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name_hint: Option<String>,
    pub schema_version: u32,
    pub bindings: Vec<PadBinding>,
    pub created_at: String,
    pub updated_at: String,
}

impl PadMapProfile {
    #[allow(dead_code)]
    pub fn is_valid_for_practice(&self) -> bool {
        REQUIRED_PADS
            .iter()
            .all(|pad| self.bindings.iter().any(|b| b.pad_id == *pad))
    }

    /// Resolve device note → PadId. Channel-specific bindings win over channel-agnostic.
    pub fn map_note(&self, note: u8, channel: u8) -> Option<PadId> {
        let mut channel_match: Option<PadId> = None;
        let mut any_match: Option<PadId> = None;
        for binding in &self.bindings {
            if binding.midi_note != note {
                continue;
            }
            match binding.channel {
                Some(ch) if ch == channel => channel_match = Some(binding.pad_id),
                None => any_match = Some(binding.pad_id),
                _ => {}
            }
        }
        channel_match.or(any_match)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingHit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_id: Option<PadId>,
    pub raw_note: u8,
    pub raw_channel: u8,
    pub velocity: u8,
    /// Session time in ms (after latency offset when practice is active).
    pub time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiNoteOnEvent {
    pub note: u8,
    pub channel: u8,
    pub velocity: u8,
    pub time_ms: u64,
    /// Monotonic ms from native audio engine epoch (for latency calibration).
    #[serde(default)]
    pub wall_ms: f64,
}

/// Scoring windows (ms) — docs/SCORING.md
pub const WINDOW_PERFECT_MS: i64 = 20;
pub const WINDOW_GOOD_MS: i64 = 50;
pub const WINDOW_OK_MS: i64 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Judgement {
    Perfect,
    Good,
    Ok,
    Miss,
    Wrong,
    Extra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgementEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_uid: Option<u64>,
    pub pad_id: PadId,
    pub judgement: Judgement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HitCounts {
    pub perfect: u32,
    pub good: u32,
    pub ok: u32,
    pub miss: u32,
    pub wrong: u32,
    pub extra: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drum_track_id: Option<u16>,
    pub started_at: String,
    pub ended_at: String,
    pub total_expected: u32,
    pub hit_counts: HitCounts,
    pub note_accuracy: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_mean_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_abs_mean_ms: Option<f64>,
    pub score_percent: u32,
}

impl Judgement {
    pub fn from_abs_delta(abs_delta_ms: i64) -> Option<Self> {
        if abs_delta_ms <= WINDOW_PERFECT_MS {
            Some(Judgement::Perfect)
        } else if abs_delta_ms <= WINDOW_GOOD_MS {
            Some(Judgement::Good)
        } else if abs_delta_ms <= WINDOW_OK_MS {
            Some(Judgement::Ok)
        } else {
            None
        }
    }
}
