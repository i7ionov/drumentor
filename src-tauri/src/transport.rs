use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Manager};

use crate::domain::{
    fold_expected_hits, ExpectedHit, HighlightEvent, HitCounts, IncomingHit, LoopRegion, PadId,
    PadMapProfile, PositionEvent, ScheduleNote, SessionSummary, TransportState, WINDOW_OK_MS,
};
use crate::hit_matcher::HitMatcher;
use crate::midi::{
    build_expected_hits, build_metronome_beats, build_playback_notes, parse_midi_file,
    quarter_ms_at, ParsedMidi,
};

const LOOKAHEAD_MS: u64 = 80;
const TICK_MS: u64 = 16;
const SPEED_MIN: f64 = 0.1;
const SPEED_MAX: f64 = 1.0;
const COUNT_IN_CLICKS: u32 = 4;
const COUNT_IN_LEAD_IN_MS: u64 = 30;

fn clamp_speed(speed: f64) -> f64 {
    if !speed.is_finite() {
        return SPEED_MAX;
    }
    // Quantize to 0.01 to avoid float drift from UI stepping.
    let quantized = (speed * 100.0).round() / 100.0;
    quantized.clamp(SPEED_MIN, SPEED_MAX)
}

pub const EVENT_JUDGEMENT: &str = "score:judgement";
pub const EVENT_SESSION_SUMMARY: &str = "score:sessionSummary";
pub const EVENT_LIVE_COUNTS: &str = "score:liveCounts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Stopped,
    Playing,
    Paused,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Stopped => "stopped",
            Status::Playing => "playing",
            Status::Paused => "paused",
        }
    }
}

pub struct SessionState {
    midi_bytes: Option<Vec<u8>>,
    song_path: Option<String>,
    status: Status,
    position_ms: u64,
    /// Monotonic song-time coordinate; unlike position_ms it never wraps during Repeat.
    virtual_position_ms: u64,
    duration_ms: u64,
    origin_instant: Option<Instant>,
    origin_position_ms: u64,
    expected_hits: Vec<ExpectedHit>,
    /// GM pad ids before optional→stand-in fold (Splash→Crash, etc.).
    raw_expected_hits: Vec<ExpectedHit>,
    next_highlight_idx: usize,
    highlight_cycle: u64,
    playback_notes: Vec<ScheduleNote>,
    next_schedule_idx: usize,
    schedule_cycle: u64,
    metronome_beats: Vec<u64>,
    next_metronome_idx: usize,
    metronome_cycle: u64,
    metronome_enabled: bool,
    drum_track_id: Option<u16>,
    /// Bumped on pause/stop/seek-while-playing so old ticker threads exit.
    play_generation: u64,
    matcher: Option<HitMatcher>,
    latency_offset_ms: i64,
    /// Playback rate (0.1–1.0). Session timeline advances as wall_elapsed * speed.
    speed: f64,
    repeat_enabled: bool,
    loop_region: Option<LoopRegion>,
    bar_boundaries_ms: Vec<u64>,
    matcher_cycle: u64,
    /// When set, Play is in metronome count-in: position frozen until this wall Instant.
    count_in_until: Option<Instant>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            midi_bytes: None,
            song_path: None,
            status: Status::Stopped,
            position_ms: 0,
            virtual_position_ms: 0,
            duration_ms: 0,
            origin_instant: None,
            origin_position_ms: 0,
            expected_hits: Vec::new(),
            raw_expected_hits: Vec::new(),
            next_highlight_idx: 0,
            highlight_cycle: 0,
            playback_notes: Vec::new(),
            next_schedule_idx: 0,
            schedule_cycle: 0,
            metronome_beats: Vec::new(),
            next_metronome_idx: 0,
            metronome_cycle: 0,
            metronome_enabled: false,
            drum_track_id: None,
            play_generation: 0,
            matcher: None,
            latency_offset_ms: 0,
            speed: 1.0,
            repeat_enabled: false,
            loop_region: None,
            bar_boundaries_ms: Vec::new(),
            matcher_cycle: 0,
            count_in_until: None,
        }
    }
}

impl SessionState {
    fn snapshot(&self) -> TransportState {
        TransportState {
            status: self.status.as_str().into(),
            position_ms: self.live_position_ms(),
            duration_ms: self.duration_ms,
            speed: self.speed,
            repeat_enabled: self.repeat_enabled,
            loop_region: self.loop_region,
        }
    }

    fn position_event(&self) -> PositionEvent {
        let snapshot = self.snapshot();
        PositionEvent {
            position_ms: snapshot.position_ms,
            status: snapshot.status,
            duration_ms: snapshot.duration_ms,
            speed: snapshot.speed,
            repeat_enabled: snapshot.repeat_enabled,
            loop_region: snapshot.loop_region,
        }
    }

    fn active_repeat_range(&self) -> Option<LoopRegion> {
        if !self.repeat_enabled || self.duration_ms == 0 {
            return None;
        }
        self.loop_region
            .filter(|region| region.start_ms < region.end_ms && region.end_ms <= self.duration_ms)
            .or(Some(LoopRegion {
                start_ms: 0,
                end_ms: self.duration_ms,
            }))
    }

    fn repeat_cycle_at(&self, virtual_ms: u64) -> u64 {
        let Some(region) = self.active_repeat_range() else {
            return 0;
        };
        if virtual_ms < region.start_ms {
            return 0;
        }
        (virtual_ms - region.start_ms) / (region.end_ms - region.start_ms)
    }

    fn song_position_for_virtual(&self, virtual_ms: u64) -> u64 {
        let Some(region) = self.active_repeat_range() else {
            return virtual_ms.min(self.duration_ms);
        };
        let length = region.end_ms - region.start_ms;
        if virtual_ms < region.start_ms {
            return region.start_ms;
        }
        region.start_ms + (virtual_ms - region.start_ms) % length
    }

    fn live_virtual_ms(&self) -> u64 {
        if self.count_in_until.is_some() {
            return self.virtual_position_ms;
        }
        if self.status == Status::Playing {
            if let Some(origin) = self.origin_instant {
                let elapsed_wall = origin.elapsed().as_millis() as f64;
                let advanced = (elapsed_wall * self.speed).round() as u64;
                let virtual_ms = self.origin_position_ms.saturating_add(advanced);
                return if self.active_repeat_range().is_some() {
                    virtual_ms
                } else {
                    virtual_ms.min(self.duration_ms)
                };
            }
        }
        self.virtual_position_ms
    }

    fn live_position_ms(&self) -> u64 {
        self.song_position_for_virtual(self.live_virtual_ms())
    }

    /// Convert wall Instant to session ms + latency offset. Only valid while Playing.
    pub fn session_time_at(&self, now: Instant) -> Option<u64> {
        if self.status != Status::Playing || self.count_in_until.is_some() {
            return None;
        }
        let origin = self.origin_instant?;
        let elapsed_wall = now
            .checked_duration_since(origin)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0);
        let advanced = (elapsed_wall * self.speed).round() as i64;
        let raw = self.origin_position_ms as i64 + advanced + self.latency_offset_ms;
        Some(self.song_position_for_virtual(raw.max(0) as u64))
    }

    /// Apply a new speed; if playing, re-anchor the clock at the current live position.
    fn set_speed_internal(&mut self, speed: f64) {
        let next = clamp_speed(speed);
        if self.status == Status::Playing && (next - self.speed).abs() >= 1e-9 {
            self.virtual_position_ms = self.live_virtual_ms();
            self.position_ms = self.live_position_ms();
            self.origin_position_ms = self.virtual_position_ms;
            self.origin_instant = Some(Instant::now());
            self.count_in_until = None;
            self.reset_cursors();
        }
        self.speed = next;
    }

    fn reset_cursors(&mut self) {
        let pos = self.position_ms;
        let cycle = self.repeat_cycle_at(self.virtual_position_ms);
        self.next_highlight_idx = self.expected_hits.partition_point(|h| h.time_ms < pos);
        self.next_schedule_idx = self.playback_notes.partition_point(|n| n.when_ms < pos);
        self.next_metronome_idx = self.metronome_beats.partition_point(|&t| t < pos);
        self.highlight_cycle = cycle;
        self.schedule_cycle = cycle;
        self.metronome_cycle = cycle;
        self.matcher_cycle = cycle;
    }

    fn stop_internal(&mut self) {
        self.play_generation = self.play_generation.wrapping_add(1);
        self.status = Status::Stopped;
        self.position_ms = 0;
        self.virtual_position_ms = 0;
        self.origin_instant = None;
        self.origin_position_ms = 0;
        self.count_in_until = None;
        self.reset_cursors();
    }

    fn pause_internal(&mut self) {
        if self.status != Status::Playing {
            return;
        }
        self.position_ms = self.live_position_ms();
        self.virtual_position_ms = self.live_virtual_ms();
        self.play_generation = self.play_generation.wrapping_add(1);
        self.status = Status::Paused;
        self.origin_instant = None;
        self.origin_position_ms = self.virtual_position_ms;
        self.count_in_until = None;
    }

    fn seek_internal(&mut self, ms: u64) {
        let was_playing = self.status == Status::Playing;
        if was_playing {
            self.play_generation = self.play_generation.wrapping_add(1);
        }
        let mut next = ms.min(self.duration_ms);
        if let Some(region) = self.active_repeat_range() {
            if next < region.start_ms || next >= region.end_ms {
                next = region.start_ms;
            }
        }
        self.position_ms = next;
        self.virtual_position_ms = next;
        self.origin_position_ms = self.virtual_position_ms;
        self.count_in_until = None;
        self.reset_cursors();
        if let Some(matcher) = self.matcher.as_mut() {
            if matcher.is_active() {
                matcher.reset_open_from(self.position_ms);
            }
        }
        if was_playing {
            self.origin_instant = Some(Instant::now());
            self.status = Status::Playing;
        } else if self.status == Status::Stopped && self.position_ms > 0 {
            self.status = Status::Paused;
        }
    }

    fn ensure_practice_started(&mut self) {
        let needs_new = match &self.matcher {
            None => true,
            Some(m) => !m.is_active(),
        };
        if !needs_new {
            return;
        }
        if self.expected_hits.is_empty() {
            return;
        }
        self.restart_matcher_for_current_mode();
    }

    fn restart_matcher_for_current_mode(&mut self) {
        let expected = if let Some(region) = self.active_repeat_range() {
            self.expected_hits
                .iter()
                .filter(|hit| hit.time_ms >= region.start_ms && hit.time_ms < region.end_ms)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.expected_hits.clone()
        };
        if expected.is_empty() {
            self.matcher = None;
            return;
        }
        self.matcher = Some(HitMatcher::start(
            &expected,
            self.position_ms,
            now_stamp(),
            self.song_path.clone(),
            self.drum_track_id,
        ));
        self.matcher_cycle = self.repeat_cycle_at(self.virtual_position_ms);
    }
}

pub struct AppSession(pub Mutex<SessionState>);

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

pub fn set_latency_offset(session: &AppSession, ms: i64) -> Result<(), String> {
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    guard.latency_offset_ms = ms;
    Ok(())
}

pub fn set_metronome_enabled(
    app: &AppHandle,
    session: &AppSession,
    enabled: bool,
) -> Result<(), String> {
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    guard.metronome_enabled = enabled;
    let pos = guard.live_position_ms();
    guard.next_metronome_idx = guard.metronome_beats.partition_point(|&t| t < pos);
    guard.metronome_cycle = guard.repeat_cycle_at(guard.live_virtual_ms());
    drop(guard);
    if !enabled {
        if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
            audio.clear_scheduled_clicks();
        }
    }
    Ok(())
}

pub fn set_metronome_volume(app: &AppHandle, volume: f64) -> Result<(), String> {
    let vol = if volume.is_finite() {
        volume.clamp(0.0, 127.0) as f32
    } else {
        100.0
    };
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.set_click_volume(vol);
        if vol <= 0.0 {
            audio.clear_scheduled_clicks();
        }
    }
    Ok(())
}

pub fn set_speed(
    app: AppHandle,
    session: &AppSession,
    speed: f64,
) -> Result<TransportState, String> {
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    guard.set_speed_internal(speed);
    let snapshot = guard.snapshot();
    let playing = guard.status == Status::Playing;
    let virtual_ms = guard.virtual_position_ms;
    let speed = guard.speed;
    let _ = app.emit("transport:position", guard.position_event());
    drop(guard);
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        if playing {
            audio.cancel_all();
            audio.arm(virtual_ms, speed);
        } else {
            audio.set_speed(speed);
        }
    }
    Ok(snapshot)
}

pub fn load_midi(session: &AppSession, path: &str) -> Result<crate::domain::SongSummary, String> {
    let parsed: ParsedMidi = parse_midi_file(path).map_err(|e| e.to_string())?;
    let metronome_beats = build_metronome_beats(&parsed.bytes).map_err(|e| e.to_string())?;
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    // Discard in-progress practice without summary (loading a new song).
    guard.matcher = None;
    guard.stop_internal();
    guard.repeat_enabled = false;
    guard.loop_region = None;
    guard.bar_boundaries_ms = parsed.summary.bar_boundaries_ms.clone();
    guard.midi_bytes = Some(parsed.bytes);
    guard.song_path = Some(parsed.summary.path.clone());
    guard.duration_ms = parsed.summary.duration_ms;
    guard.expected_hits.clear();
    guard.raw_expected_hits.clear();
    guard.playback_notes.clear();
    guard.metronome_beats = metronome_beats;
    guard.drum_track_id = None;
    guard.next_highlight_idx = 0;
    guard.next_schedule_idx = 0;
    guard.next_metronome_idx = 0;
    Ok(parsed.summary)
}

pub fn set_repeat_enabled(
    app: AppHandle,
    session: &AppSession,
    enabled: bool,
) -> Result<TransportState, String> {
    let (snapshot, playing, generation, virtual_ms, speed) = {
        let mut guard = session.0.lock().map_err(|e| e.to_string())?;
        let playing = guard.status == Status::Playing;
        let current_song_ms = guard.live_position_ms();
        guard.repeat_enabled = enabled;
        let next_song_ms = if let Some(active) = guard.active_repeat_range() {
            if current_song_ms < active.start_ms || current_song_ms >= active.end_ms {
                active.start_ms
            } else {
                current_song_ms
            }
        } else {
            current_song_ms
        };
        guard.virtual_position_ms = next_song_ms;
        guard.position_ms = next_song_ms;
        guard.origin_position_ms = guard.virtual_position_ms;
        guard.origin_instant = playing.then(Instant::now);
        guard.count_in_until = None;
        guard.matcher = None;
        if enabled && playing {
            guard.restart_matcher_for_current_mode();
        }
        if playing {
            guard.play_generation = guard.play_generation.wrapping_add(1);
        }
        guard.reset_cursors();
        let generation = guard.play_generation;
        let snapshot = guard.snapshot();
        let _ = app.emit("transport:position", guard.position_event());
        (
            snapshot,
            playing,
            generation,
            guard.virtual_position_ms,
            guard.speed,
        )
    };
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.cancel_all();
        if playing {
            audio.arm(virtual_ms, speed);
        }
    }
    let _ = app.emit(EVENT_LIVE_COUNTS, HitCounts::default());
    if playing {
        spawn_ticker(app, generation);
    }
    Ok(snapshot)
}

pub fn set_loop_region(
    app: AppHandle,
    session: &AppSession,
    region: Option<LoopRegion>,
) -> Result<TransportState, String> {
    let (snapshot, playing, generation, virtual_ms, speed) = {
        let mut guard = session.0.lock().map_err(|e| e.to_string())?;
        if let Some(candidate) = region {
            let valid = candidate.start_ms < candidate.end_ms
                && candidate.end_ms <= guard.duration_ms
                && guard.bar_boundaries_ms.contains(&candidate.start_ms)
                && guard.bar_boundaries_ms.contains(&candidate.end_ms);
            if !valid {
                return Err("loop region must use song bar boundaries".into());
            }
        }
        let playing = guard.status == Status::Playing;
        let current_song_ms = guard.live_position_ms();
        guard.loop_region = region;
        let next_song_ms = if let Some(active) = guard.active_repeat_range() {
            if current_song_ms < active.start_ms || current_song_ms >= active.end_ms {
                active.start_ms
            } else {
                current_song_ms
            }
        } else {
            current_song_ms
        };
        guard.position_ms = next_song_ms;
        guard.virtual_position_ms = next_song_ms;
        guard.origin_position_ms = next_song_ms;
        guard.origin_instant = playing.then(Instant::now);
        guard.count_in_until = None;
        if guard.repeat_enabled && playing {
            guard.restart_matcher_for_current_mode();
        }
        if playing {
            guard.play_generation = guard.play_generation.wrapping_add(1);
        }
        guard.reset_cursors();
        let generation = guard.play_generation;
        let snapshot = guard.snapshot();
        let _ = app.emit("transport:position", guard.position_event());
        (snapshot, playing, generation, next_song_ms, guard.speed)
    };
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.cancel_all();
        if playing {
            audio.arm(virtual_ms, speed);
        }
    }
    if playing {
        spawn_ticker(app, generation);
    }
    Ok(snapshot)
}

pub fn set_drum_track(
    session: &AppSession,
    track_id: u16,
    profile: Option<&PadMapProfile>,
) -> Result<(), String> {
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    let bytes = guard
        .midi_bytes
        .clone()
        .ok_or_else(|| "no MIDI file loaded".to_string())?;
    let (hits, duration_ms) = build_expected_hits(&bytes, track_id).map_err(|e| e.to_string())?;
    let (playback_notes, _) = build_playback_notes(&bytes, track_id).map_err(|e| e.to_string())?;
    guard.matcher = None;
    guard.stop_internal();
    guard.raw_expected_hits = hits;
    guard.expected_hits = fold_expected_hits(&guard.raw_expected_hits, profile);
    guard.playback_notes = playback_notes;
    guard.duration_ms = duration_ms;
    guard.drum_track_id = Some(track_id);
    guard.reset_cursors();
    Ok(())
}

/// Re-apply optional-pad stand-ins from the active device map (e.g. no Splash → Crash).
pub fn apply_pad_map_fold(
    session: &AppSession,
    profile: Option<&PadMapProfile>,
) -> Result<(), String> {
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    if guard.raw_expected_hits.is_empty() {
        return Ok(());
    }
    let pos = guard.live_position_ms();
    guard.expected_hits = fold_expected_hits(&guard.raw_expected_hits, profile);
    guard.next_highlight_idx = 0;
    guard.highlight_cycle = guard.repeat_cycle_at(guard.live_virtual_ms());
    while guard.next_highlight_idx < guard.expected_hits.len()
        && guard.expected_hits[guard.next_highlight_idx].time_ms < pos.saturating_add(LOOKAHEAD_MS)
    {
        guard.next_highlight_idx += 1;
    }
    Ok(())
}

pub fn get_state(session: &AppSession) -> Result<TransportState, String> {
    let guard = session.0.lock().map_err(|e| e.to_string())?;
    Ok(guard.snapshot())
}

pub fn get_expected_hits(session: &AppSession) -> Result<Vec<ExpectedHit>, String> {
    let guard = session.0.lock().map_err(|e| e.to_string())?;
    Ok(guard.expected_hits.clone())
}

pub fn play(app: AppHandle, session: &AppSession) -> Result<TransportState, String> {
    let generation = {
        let mut guard = session.0.lock().map_err(|e| e.to_string())?;
        if guard.midi_bytes.is_none() {
            return Err("no MIDI file loaded".into());
        }
        if guard.drum_track_id.is_none() {
            return Err("no drum track selected".into());
        }
        if guard.status == Status::Playing {
            return Ok(guard.snapshot());
        }
        if guard.position_ms >= guard.duration_ms && guard.duration_ms > 0 {
            guard.position_ms = 0;
            guard.virtual_position_ms = 0;
            guard.reset_cursors();
            // Restarting from end → new practice run.
            guard.matcher = None;
        }
        if let Some(region) = guard.active_repeat_range() {
            if guard.position_ms < region.start_ms || guard.position_ms >= region.end_ms {
                guard.position_ms = region.start_ms;
                guard.virtual_position_ms = region.start_ms;
                guard.reset_cursors();
            }
        }
        guard.ensure_practice_started();
        if !guard.repeat_enabled {
            if let Some(matcher) = guard.matcher.as_ref() {
                let _ = app.emit(EVENT_LIVE_COUNTS, matcher.live_counts());
            }
        }
        guard.status = Status::Playing;
        guard.play_generation = guard.play_generation.wrapping_add(1);
        let generation = guard.play_generation;
        let position_ms = guard.position_ms;
        let virtual_ms = guard.virtual_position_ms;
        let speed = guard.speed;

        let count_in_train = if guard.metronome_enabled {
            let bytes = guard.midi_bytes.as_ref().expect("checked above");
            let quarter_ms = quarter_ms_at(bytes, position_ms).unwrap_or(500).max(1);
            let interval_wall_ms = ((quarter_ms as f64) / speed).round().max(1.0) as u64;
            let until = Instant::now()
                + Duration::from_millis(
                    COUNT_IN_LEAD_IN_MS + u64::from(COUNT_IN_CLICKS) * interval_wall_ms,
                );
            guard.count_in_until = Some(until);
            guard.origin_instant = None;
            guard.origin_position_ms = virtual_ms;
            Some(interval_wall_ms)
        } else {
            guard.count_in_until = None;
            guard.origin_instant = Some(Instant::now());
            guard.origin_position_ms = virtual_ms;
            None
        };

        let _ = app.emit("transport:position", guard.position_event());
        if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
            audio.cancel_all();
            if let Some(interval_wall_ms) = count_in_train {
                let _ =
                    audio.start_click_train(COUNT_IN_CLICKS, interval_wall_ms, COUNT_IN_LEAD_IN_MS);
            } else {
                audio.arm(virtual_ms, speed);
            }
        }
        generation
    };

    spawn_ticker(app, generation);
    let guard = session.0.lock().map_err(|e| e.to_string())?;
    Ok(guard.snapshot())
}

pub fn pause(app: AppHandle, session: &AppSession) -> Result<TransportState, String> {
    let mut guard = session.0.lock().map_err(|e| e.to_string())?;
    guard.pause_internal();
    let snapshot = guard.snapshot();
    let _ = app.emit("transport:position", guard.position_event());
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.cancel_all();
    }
    Ok(snapshot)
}

pub fn stop(app: AppHandle, session: &AppSession) -> Result<TransportState, String> {
    let summary = {
        let mut guard = session.0.lock().map_err(|e| e.to_string())?;
        let summary = if guard.repeat_enabled {
            guard.matcher = None;
            None
        } else {
            take_summary_if_active(&mut guard)
        };
        guard.stop_internal();
        summary
    };
    if let Some(summary) = summary {
        persist_and_emit_summary(&app, summary);
    }
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.cancel_all();
    }
    let guard = session.0.lock().map_err(|e| e.to_string())?;
    let snapshot = guard.snapshot();
    let _ = app.emit("transport:position", guard.position_event());
    Ok(snapshot)
}

pub fn seek(
    app: AppHandle,
    session: &AppSession,
    position_ms: u64,
) -> Result<TransportState, String> {
    let (snapshot, should_respawn, generation, live_counts, speed, virtual_ms, playing) = {
        let mut guard = session.0.lock().map_err(|e| e.to_string())?;
        if guard.midi_bytes.is_none() {
            return Err("no MIDI file loaded".into());
        }
        let was_playing = guard.status == Status::Playing;
        guard.seek_internal(position_ms);
        let generation = guard.play_generation;
        let live_counts = (!guard.repeat_enabled)
            .then(|| {
                guard
                    .matcher
                    .as_ref()
                    .filter(|m| m.is_active())
                    .map(|m| m.live_counts())
            })
            .flatten();
        let snapshot = guard.snapshot();
        let speed = guard.speed;
        let virtual_ms = guard.virtual_position_ms;
        let playing = was_playing && guard.status == Status::Playing;
        let _ = app.emit("transport:position", guard.position_event());
        (
            snapshot,
            playing,
            generation,
            live_counts,
            speed,
            virtual_ms,
            playing,
        )
    };

    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.cancel_all();
        if playing {
            audio.arm(virtual_ms, speed);
        }
    }

    if let Some(counts) = live_counts {
        let _ = app.emit(EVENT_LIVE_COUNTS, counts);
    }

    if should_respawn {
        spawn_ticker(app, generation);
    }
    Ok(snapshot)
}

/// Handle a mapped MIDI hit: convert to session time and run HitMatcher.
pub fn handle_incoming_hit(
    app: &AppHandle,
    session: &AppSession,
    pad_id: PadId,
    velocity: u8,
    raw_note: u8,
    raw_channel: u8,
) {
    if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
        audio.play_player_drum_hit(pad_id, velocity);
    }

    let now = Instant::now();
    let Ok(mut guard) = session.0.lock() else {
        return;
    };

    let session_ms = match guard.session_time_at(now) {
        Some(ms) => ms,
        None => {
            // Not playing — still flash UI with frozen position.
            let hit = IncomingHit {
                pad_id: Some(pad_id),
                raw_note,
                raw_channel,
                velocity,
                time_ms: guard.position_ms,
            };
            let _ = app.emit("midi:incomingHit", &hit);
            return;
        }
    };

    let hit = IncomingHit {
        pad_id: Some(pad_id),
        raw_note,
        raw_channel,
        velocity,
        time_ms: session_ms,
    };
    let _ = app.emit("midi:incomingHit", &hit);

    if guard.repeat_enabled {
        let cycle = guard.repeat_cycle_at(guard.live_virtual_ms());
        if cycle != guard.matcher_cycle {
            guard.restart_matcher_for_current_mode();
            guard.matcher_cycle = cycle;
        }
    }

    let Some(matcher) = guard.matcher.as_mut().filter(|m| m.is_active()) else {
        return;
    };

    let event = matcher.on_incoming(pad_id, session_ms);
    let counts = matcher.live_counts();
    let _ = app.emit(EVENT_JUDGEMENT, &event);
    if !guard.repeat_enabled {
        let _ = app.emit(EVENT_LIVE_COUNTS, counts);
    }
}

fn take_summary_if_active(guard: &mut SessionState) -> Option<SessionSummary> {
    let matcher = guard.matcher.as_mut()?;
    if !matcher.is_active() {
        return None;
    }
    let counts = matcher.live_counts();
    let had_activity =
        counts.perfect + counts.good + counts.ok + counts.miss + counts.wrong + counts.extra > 0;
    if !had_activity {
        // Play→immediate Stop with no hits/misses yet — discard quietly.
        let _ = matcher.finalize(now_stamp());
        return None;
    }
    Some(matcher.finalize(now_stamp()))
}

fn persist_and_emit_summary(app: &AppHandle, summary: SessionSummary) {
    if let Some(db) = app.try_state::<crate::DbState>() {
        let _ = db.0.insert_session(&summary);
    }
    let _ = app.emit(EVENT_SESSION_SUMMARY, &summary);
    let _ = app.emit(EVENT_LIVE_COUNTS, HitCounts::default());
}

fn spawn_ticker(app: AppHandle, generation: u64) {
    std::thread::spawn(move || {
        let session = app.state::<AppSession>();
        loop {
            std::thread::sleep(Duration::from_millis(TICK_MS));

            let Ok(mut guard) = session.0.lock() else {
                break;
            };

            if guard.play_generation != generation || guard.status != Status::Playing {
                break;
            }

            // Count-in: freeze song clock; clicks already scheduled on the audio engine.
            if let Some(until) = guard.count_in_until {
                if Instant::now() < until {
                    let event = guard.position_event();
                    drop(guard);
                    let _ = app.emit("transport:position", event);
                    continue;
                }
                // Count-in finished → arm song playback from the frozen position.
                guard.count_in_until = None;
                let virtual_ms = guard.virtual_position_ms;
                let speed = guard.speed;
                guard.origin_instant = Some(Instant::now());
                guard.origin_position_ms = virtual_ms;
                guard.reset_cursors();
                if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
                    audio.cancel_all();
                    audio.arm(virtual_ms, speed);
                }
            }

            let virtual_position = guard.live_virtual_ms();
            let position = guard.song_position_for_virtual(virtual_position);
            guard.virtual_position_ms = virtual_position;
            guard.position_ms = position;

            let current_cycle = guard.repeat_cycle_at(virtual_position);
            if guard.repeat_enabled && current_cycle != guard.matcher_cycle {
                if let Some(region) = guard.active_repeat_range() {
                    if let Some(matcher) = guard.matcher.as_mut().filter(|m| m.is_active()) {
                        let finish_at = region.end_ms.saturating_add(WINDOW_OK_MS.max(0) as u64);
                        for event in matcher.expire_misses(finish_at) {
                            let _ = app.emit(EVENT_JUDGEMENT, event);
                        }
                    }
                    guard.restart_matcher_for_current_mode();
                    guard.matcher_cycle = current_cycle;
                }
            }

            // Expire misses relative to session clock.
            if let Some(matcher) = guard.matcher.as_mut().filter(|m| m.is_active()) {
                let miss_events = matcher.expire_misses(position);
                let counts = matcher.live_counts();
                for event in &miss_events {
                    let _ = app.emit(EVENT_JUDGEMENT, event);
                }
                if !guard.repeat_enabled && !miss_events.is_empty() {
                    let _ = app.emit(EVENT_LIVE_COUNTS, counts);
                }
            }

            let horizon = virtual_position.saturating_add(LOOKAHEAD_MS);
            let repeat_range = guard.active_repeat_range();

            if let Some(region) = repeat_range {
                let length = region.end_ms - region.start_ms;
                let first = guard
                    .expected_hits
                    .partition_point(|hit| hit.time_ms < region.start_ms);
                let last = guard
                    .expected_hits
                    .partition_point(|hit| hit.time_ms < region.end_ms);
                if first < last {
                    loop {
                        if guard.next_highlight_idx < first || guard.next_highlight_idx >= last {
                            guard.next_highlight_idx = first;
                            guard.highlight_cycle = guard.highlight_cycle.saturating_add(1);
                        }
                        let hit = &guard.expected_hits[guard.next_highlight_idx];
                        let occurrence = hit
                            .time_ms
                            .saturating_add(guard.highlight_cycle.saturating_mul(length));
                        if occurrence > horizon {
                            break;
                        }
                        let _ = app.emit(
                            "transport:highlight",
                            HighlightEvent {
                                pad_id: hit.pad_id,
                                at_ms: hit.time_ms,
                                velocity: hit.velocity,
                                uid: hit.uid,
                            },
                        );
                        guard.next_highlight_idx += 1;
                    }
                }
            } else {
                while guard.next_highlight_idx < guard.expected_hits.len() {
                    let hit = &guard.expected_hits[guard.next_highlight_idx];
                    if hit.time_ms > horizon {
                        break;
                    }
                    let _ = app.emit(
                        "transport:highlight",
                        HighlightEvent {
                            pad_id: hit.pad_id,
                            at_ms: hit.time_ms,
                            velocity: hit.velocity,
                            uid: hit.uid,
                        },
                    );
                    guard.next_highlight_idx += 1;
                }
            }

            let mut due_notes: Vec<(ScheduleNote, u64)> = Vec::new();
            if let Some(region) = repeat_range {
                let length = region.end_ms - region.start_ms;
                let first = guard
                    .playback_notes
                    .partition_point(|note| note.when_ms < region.start_ms);
                let last = guard
                    .playback_notes
                    .partition_point(|note| note.when_ms < region.end_ms);
                if first < last {
                    loop {
                        if guard.next_schedule_idx < first || guard.next_schedule_idx >= last {
                            guard.next_schedule_idx = first;
                            guard.schedule_cycle = guard.schedule_cycle.saturating_add(1);
                        }
                        let note = &guard.playback_notes[guard.next_schedule_idx];
                        let occurrence = note
                            .when_ms
                            .saturating_add(guard.schedule_cycle.saturating_mul(length));
                        if occurrence > horizon {
                            break;
                        }
                        due_notes.push((note.clone(), occurrence));
                        guard.next_schedule_idx += 1;
                    }
                }
            } else {
                while guard.next_schedule_idx < guard.playback_notes.len() {
                    let note = &guard.playback_notes[guard.next_schedule_idx];
                    if note.when_ms > horizon {
                        break;
                    }
                    due_notes.push((note.clone(), note.when_ms));
                    guard.next_schedule_idx += 1;
                }
            }

            let mut due_clicks: Vec<(u64, u32)> = Vec::new();
            if guard.metronome_enabled {
                if let Some(region) = repeat_range {
                    let length = region.end_ms - region.start_ms;
                    let first = guard
                        .metronome_beats
                        .partition_point(|&beat| beat < region.start_ms);
                    let last = guard
                        .metronome_beats
                        .partition_point(|&beat| beat < region.end_ms);
                    if first < last {
                        loop {
                            if guard.next_metronome_idx < first || guard.next_metronome_idx >= last
                            {
                                guard.next_metronome_idx = first;
                                guard.metronome_cycle = guard.metronome_cycle.saturating_add(1);
                            }
                            let beat = guard.metronome_beats[guard.next_metronome_idx];
                            let occurrence =
                                beat.saturating_add(guard.metronome_cycle.saturating_mul(length));
                            if occurrence > horizon {
                                break;
                            }
                            due_clicks.push((occurrence, guard.next_metronome_idx as u32));
                            guard.next_metronome_idx += 1;
                        }
                    }
                } else {
                    while guard.next_metronome_idx < guard.metronome_beats.len() {
                        let when_ms = guard.metronome_beats[guard.next_metronome_idx];
                        if when_ms > horizon {
                            break;
                        }
                        due_clicks.push((when_ms, guard.next_metronome_idx as u32));
                        guard.next_metronome_idx += 1;
                    }
                }
            }

            let finished = repeat_range.is_none()
                && virtual_position >= guard.duration_ms
                && guard.duration_ms > 0;
            if finished {
                guard.position_ms = guard.duration_ms;
                guard.play_generation = guard.play_generation.wrapping_add(1);
                guard.status = Status::Stopped;
                guard.origin_instant = None;
                guard.origin_position_ms = 0;
                guard.count_in_until = None;
                let summary = take_summary_if_active(&mut guard);
                let event = guard.position_event();
                drop(guard);
                if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
                    for (note, occurrence) in &due_notes {
                        audio.schedule_note_at(note, *occurrence);
                    }
                    for &(occurrence, index) in &due_clicks {
                        audio.schedule_click_at(occurrence, index);
                    }
                    audio.cancel_all();
                }
                if let Some(summary) = summary {
                    persist_and_emit_summary(&app, summary);
                }
                let _ = app.emit("transport:position", event);
                break;
            }

            drop(guard);
            if let Some(audio) = app.try_state::<crate::audio_engine::AudioEngineHandle>() {
                for (note, occurrence) in &due_notes {
                    audio.schedule_note_at(note, *occurrence);
                }
                for &(occurrence, index) in &due_clicks {
                    audio.schedule_click_at(occurrence, index);
                }
            }
            let Ok(guard) = session.0.lock() else {
                break;
            };
            if guard.play_generation != generation || guard.status != Status::Playing {
                break;
            }
            let event = guard.position_event();
            drop(guard);
            let _ = app.emit("transport:position", event);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repeating_state(region: Option<LoopRegion>) -> SessionState {
        SessionState {
            duration_ms: 8_000,
            repeat_enabled: true,
            loop_region: region,
            ..SessionState::default()
        }
    }

    #[test]
    fn region_repeat_wraps_exactly_at_end() {
        let state = repeating_state(Some(LoopRegion {
            start_ms: 2_000,
            end_ms: 4_000,
        }));
        assert_eq!(state.song_position_for_virtual(3_999), 3_999);
        assert_eq!(state.song_position_for_virtual(4_000), 2_000);
        assert_eq!(state.song_position_for_virtual(8_250), 2_250);
        assert_eq!(state.repeat_cycle_at(8_250), 3);
    }

    #[test]
    fn repeat_without_region_wraps_whole_song() {
        let state = repeating_state(None);
        assert_eq!(state.song_position_for_virtual(8_000), 0);
        assert_eq!(state.song_position_for_virtual(16_125), 125);
    }

    #[test]
    fn disabled_repeat_clamps_at_song_end() {
        let state = SessionState {
            duration_ms: 8_000,
            repeat_enabled: false,
            loop_region: Some(LoopRegion {
                start_ms: 2_000,
                end_ms: 4_000,
            }),
            ..SessionState::default()
        };
        assert_eq!(state.song_position_for_virtual(9_000), 8_000);
        assert_eq!(state.repeat_cycle_at(9_000), 0);
    }
}
