use std::fs;
use std::path::Path;

use midir::MidiInput;
use midly::{MetaMessage, MidiMessage, Smf, TrackEventKind};
use thiserror::Error;

use crate::domain::{
    gm_note_to_pad, ExpectedHit, MidiInputPort, NoteRole, ScheduleNote, SongSummary, TrackInfo,
};

#[derive(Debug, Error)]
pub enum MidiError {
    #[error("failed to read MIDI file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse MIDI file: {0}")]
    Parse(String),
    #[error("MIDI input error: {0}")]
    Input(String),
    #[error("track {0} out of range")]
    TrackOutOfRange(u16),
}

/// Absolute-tick tempo change. Tempo from this tick onward until the next point.
#[derive(Debug, Clone)]
struct TempoPoint {
    tick: u32,
    us_per_quarter: u32,
}

#[derive(Debug, Clone, Copy)]
struct TimeSignaturePoint {
    tick: u32,
    numerator: u8,
    denominator_pow: u8,
}

pub struct ParsedMidi {
    pub summary: SongSummary,
    pub bytes: Vec<u8>,
}

pub fn parse_midi_file(path: &str) -> Result<ParsedMidi, MidiError> {
    let bytes = fs::read(path)?;
    let summary = summarize_midi(&bytes, path)?;
    Ok(ParsedMidi { summary, bytes })
}

fn decode_midi_text(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(bytes).ok()?;
    // MIDI strings are often null-padded; bare trim() leaves \0 and the UI looks blank.
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// GM / MIDI default for Main Volume (CC7).
const DEFAULT_TRACK_VOLUME: u8 = 100;
const CC_MAIN_VOLUME: u8 = 7;

/// Initial CC7 per MIDI channel: last Main Volume at/before the channel's first note
/// (controllers often live on a conductor track, while notes are on other tracks).
fn channel_setup_volumes(smf: &Smf<'_>) -> [u8; 16] {
    let mut first_note_tick: [Option<u32>; 16] = [None; 16];
    let mut cc7_events: Vec<(u32, u8, u8)> = Vec::new(); // tick, channel, value

    for track in &smf.tracks {
        let mut abs_tick: u32 = 0;
        for event in track.iter() {
            abs_tick = abs_tick.saturating_add(u32::from(event.delta.as_int()));
            let TrackEventKind::Midi { channel, message } = event.kind else {
                continue;
            };
            let ch = channel.as_int() as usize;
            if ch >= 16 {
                continue;
            }
            match message {
                MidiMessage::Controller { controller, value }
                    if controller.as_int() == CC_MAIN_VOLUME =>
                {
                    cc7_events.push((abs_tick, ch as u8, value.as_int()));
                }
                MidiMessage::NoteOn { vel, .. } if vel.as_int() > 0 => {
                    first_note_tick[ch] = Some(match first_note_tick[ch] {
                        Some(t) => t.min(abs_tick),
                        None => abs_tick,
                    });
                }
                _ => {}
            }
        }
    }

    cc7_events.sort_by_key(|(tick, ch, _)| (*tick, *ch));

    let mut volumes = [DEFAULT_TRACK_VOLUME; 16];
    for &(tick, ch, value) in &cc7_events {
        let limit = first_note_tick[ch as usize].unwrap_or(u32::MAX);
        if tick <= limit {
            volumes[ch as usize] = value;
        }
    }
    volumes
}

/// Dominant MIDI channel for a track (by note count); `None` if no notes.
fn track_primary_channel(track: &midly::Track<'_>) -> Option<u8> {
    let mut counts = [0u32; 16];
    for event in track.iter() {
        if let TrackEventKind::Midi {
            channel,
            message: MidiMessage::NoteOn { vel, .. },
        } = event.kind
        {
            if vel.as_int() > 0 {
                let ch = channel.as_int() as usize;
                if ch < 16 {
                    counts[ch] += 1;
                }
            }
        }
    }
    counts
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| *n)
        .filter(|(_, n)| **n > 0)
        .map(|(ch, _)| ch as u8)
}

pub fn summarize_midi(bytes: &[u8], path: &str) -> Result<SongSummary, MidiError> {
    let smf = Smf::parse(bytes).map_err(|e| MidiError::Parse(e.to_string()))?;
    let ticks_per_beat = ticks_per_beat(&smf);
    let tempo_map = build_tempo_map(&smf);
    let channel_volumes = channel_setup_volumes(&smf);

    let mut tracks = Vec::with_capacity(smf.tracks.len());
    let mut max_ticks: u32 = 0;
    let mut suggested: Option<(u16, f32)> = None;

    for (index, track) in smf.tracks.iter().enumerate() {
        let id = index as u16;
        let mut track_name: Option<String> = None;
        let mut instrument_name: Option<String> = None;
        let mut note_count: u32 = 0;
        let mut percussion_notes: u32 = 0;
        let mut channel10_notes: u32 = 0;
        let mut abs_tick: u32 = 0;
        // Last CC7 on this track before its first NoteOn (local override).
        let mut local_volume_before_notes: Option<u8> = None;
        let mut saw_note = false;

        for event in track.iter() {
            abs_tick = abs_tick.saturating_add(u32::from(event.delta.as_int()));
            match event.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(name_bytes)) => {
                    if let Some(s) = decode_midi_text(name_bytes) {
                        track_name = Some(s);
                    }
                }
                TrackEventKind::Meta(MetaMessage::InstrumentName(name_bytes)) => {
                    if let Some(s) = decode_midi_text(name_bytes) {
                        instrument_name = Some(s);
                    }
                }
                TrackEventKind::Midi { channel, message } => match message {
                    MidiMessage::Controller { controller, value }
                        if controller.as_int() == CC_MAIN_VOLUME && !saw_note =>
                    {
                        local_volume_before_notes = Some(value.as_int());
                    }
                    MidiMessage::NoteOn { key, vel } => {
                        if vel.as_int() > 0 {
                            saw_note = true;
                            note_count += 1;
                            let note = key.as_int();
                            if (35..=81).contains(&note) {
                                percussion_notes += 1;
                            }
                            if channel.as_int() == 9 {
                                channel10_notes += 1;
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        max_ticks = max_ticks.max(abs_tick);

        let name = track_name
            .or(instrument_name)
            .unwrap_or_else(|| format!("Track {id}"));

        let name_lower = name.to_lowercase();
        let name_bonus = if ["drum", "drums", "kit", "percussion", "удар"]
            .iter()
            .any(|k| name_lower.contains(k))
        {
            2.0
        } else {
            0.0
        };

        let perc_ratio = if note_count == 0 {
            0.0
        } else {
            percussion_notes as f32 / note_count as f32
        };
        let ch10_ratio = if note_count == 0 {
            0.0
        } else {
            channel10_notes as f32 / note_count as f32
        };

        let drum_score = ch10_ratio * 4.0 + perc_ratio * 3.0 + name_bonus;
        let is_drum_candidate = drum_score >= 2.0 && note_count > 0;

        if is_drum_candidate {
            match suggested {
                Some((_, best)) if drum_score <= best => {}
                _ => suggested = Some((id, drum_score)),
            }
        }

        // Prefer CC7 written on this track; else volume of the track's primary channel
        // (Guitar Pro / DAW exports often put CC7 on a conductor track by channel).
        let volume = local_volume_before_notes.unwrap_or_else(|| {
            track_primary_channel(track)
                .map(|ch| channel_volumes[ch as usize])
                .unwrap_or(DEFAULT_TRACK_VOLUME)
        });

        tracks.push(TrackInfo {
            id,
            name,
            note_count,
            is_drum_candidate,
            drum_score,
            volume,
        });
    }

    let duration_ms = tick_to_ms(max_ticks, ticks_per_beat, &tempo_map);
    let bar_boundaries_ms = bar_boundaries_for_smf(&smf, ticks_per_beat, &tempo_map, max_ticks);

    Ok(SongSummary {
        path: Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string(),
        track_count: tracks.len() as u16,
        duration_ms,
        bar_boundaries_ms,
        tracks,
        suggested_drum_track_id: suggested.map(|(id, _)| id),
    })
}

const DEFAULT_NOTE_DURATION_MS: u64 = 250;

/// Build GM-mapped expected hits for a drum track. Duration is song-wide.
pub fn build_expected_hits(
    bytes: &[u8],
    track_id: u16,
) -> Result<(Vec<ExpectedHit>, u64), MidiError> {
    let smf = Smf::parse(bytes).map_err(|e| MidiError::Parse(e.to_string()))?;
    let ticks_per_beat = ticks_per_beat(&smf);
    let tempo_map = build_tempo_map(&smf);

    let track = smf
        .tracks
        .get(track_id as usize)
        .ok_or(MidiError::TrackOutOfRange(track_id))?;

    let mut hits = Vec::new();
    let mut abs_tick: u32 = 0;
    let mut uid: u64 = 0;

    for event in track.iter() {
        abs_tick = abs_tick.saturating_add(u32::from(event.delta.as_int()));
        if let TrackEventKind::Midi { message, .. } = event.kind {
            if let MidiMessage::NoteOn { key, vel } = message {
                let velocity = vel.as_int();
                if velocity == 0 {
                    continue;
                }
                if let Some(pad_id) = gm_note_to_pad(key.as_int()) {
                    hits.push(ExpectedHit {
                        uid,
                        pad_id,
                        time_ms: tick_to_ms(abs_tick, ticks_per_beat, &tempo_map),
                        velocity,
                    });
                    uid += 1;
                }
            }
        }
    }

    hits.sort_by_key(|h| (h.time_ms, h.uid));
    let duration_ms = song_duration_ms(&smf, ticks_per_beat, &tempo_map);

    Ok((hits, duration_ms))
}

/// Build playback notes for all tracks (drum + backing) with durations and programs.
pub fn build_playback_notes(
    bytes: &[u8],
    drum_track_id: u16,
) -> Result<(Vec<ScheduleNote>, u64), MidiError> {
    let smf = Smf::parse(bytes).map_err(|e| MidiError::Parse(e.to_string()))?;
    if drum_track_id as usize >= smf.tracks.len() {
        return Err(MidiError::TrackOutOfRange(drum_track_id));
    }

    let ticks_per_beat = ticks_per_beat(&smf);
    let tempo_map = build_tempo_map(&smf);
    let mut notes = Vec::new();
    let mut uid: u64 = 0;

    for (track_index, track) in smf.tracks.iter().enumerate() {
        let role = if track_index as u16 == drum_track_id {
            NoteRole::Drum
        } else {
            NoteRole::Backing
        };

        // Active noteOns keyed by (channel, note) → (start_tick, velocity, program)
        let mut active: Vec<((u8, u8), (u32, u8, u8))> = Vec::new();
        let mut programs = [0u8; 16];
        let mut abs_tick: u32 = 0;

        let track_id = track_index as u16;

        for event in track.iter() {
            abs_tick = abs_tick.saturating_add(u32::from(event.delta.as_int()));
            let TrackEventKind::Midi { channel, message } = event.kind else {
                continue;
            };
            let ch = channel.as_int();

            match message {
                MidiMessage::ProgramChange { program } => {
                    programs[ch as usize] = program.as_int();
                }
                MidiMessage::NoteOn { key, vel } => {
                    let note = key.as_int();
                    let velocity = vel.as_int();
                    if velocity == 0 {
                        close_active_note(
                            &mut active,
                            &mut notes,
                            &mut uid,
                            track_id,
                            role,
                            ch,
                            note,
                            abs_tick,
                            ticks_per_beat,
                            &tempo_map,
                        );
                    } else {
                        // Retrigger: close previous same key if still open
                        close_active_note(
                            &mut active,
                            &mut notes,
                            &mut uid,
                            track_id,
                            role,
                            ch,
                            note,
                            abs_tick,
                            ticks_per_beat,
                            &tempo_map,
                        );
                        active.push(((ch, note), (abs_tick, velocity, programs[ch as usize])));
                    }
                }
                MidiMessage::NoteOff { key, .. } => {
                    close_active_note(
                        &mut active,
                        &mut notes,
                        &mut uid,
                        track_id,
                        role,
                        ch,
                        key.as_int(),
                        abs_tick,
                        ticks_per_beat,
                        &tempo_map,
                    );
                }
                _ => {}
            }
        }

        // Notes still open at end of track → fallback duration from last tick
        for ((ch, note), (start_tick, velocity, program)) in active.drain(..) {
            let when_ms = tick_to_ms(start_tick, ticks_per_beat, &tempo_map);
            let end_ms = tick_to_ms(abs_tick, ticks_per_beat, &tempo_map);
            let duration_ms = (end_ms.saturating_sub(when_ms)).max(DEFAULT_NOTE_DURATION_MS);
            notes.push(ScheduleNote {
                uid,
                track_id,
                role,
                channel: ch,
                program,
                note,
                velocity,
                when_ms,
                duration_ms,
            });
            uid += 1;
        }
    }

    notes.sort_by_key(|n| (n.when_ms, n.uid));
    let duration_ms = song_duration_ms(&smf, ticks_per_beat, &tempo_map);
    Ok((notes, duration_ms))
}

fn close_active_note(
    active: &mut Vec<((u8, u8), (u32, u8, u8))>,
    notes: &mut Vec<ScheduleNote>,
    uid: &mut u64,
    track_id: u16,
    role: NoteRole,
    channel: u8,
    note: u8,
    end_tick: u32,
    ticks_per_beat: u32,
    tempo_map: &[TempoPoint],
) {
    if let Some(idx) = active.iter().rposition(|(k, _)| *k == (channel, note)) {
        let ((ch, n), (start_tick, velocity, program)) = active.remove(idx);
        let when_ms = tick_to_ms(start_tick, ticks_per_beat, tempo_map);
        let end_ms = tick_to_ms(end_tick, ticks_per_beat, tempo_map);
        let duration_ms = (end_ms.saturating_sub(when_ms)).max(1);
        notes.push(ScheduleNote {
            uid: *uid,
            track_id,
            role,
            channel: ch,
            program,
            note: n,
            velocity,
            when_ms,
            duration_ms,
        });
        *uid += 1;
    }
}

fn song_duration_ms(smf: &Smf, ticks_per_beat: u32, tempo_map: &[TempoPoint]) -> u64 {
    let mut max_ticks: u32 = 0;
    for track in &smf.tracks {
        let mut t: u32 = 0;
        for event in track.iter() {
            t = t.saturating_add(u32::from(event.delta.as_int()));
        }
        max_ticks = max_ticks.max(t);
    }
    tick_to_ms(max_ticks, ticks_per_beat, tempo_map)
}

/// Quarter-note beat times in session ms (respects MIDI tempo map).
pub fn build_metronome_beats(bytes: &[u8]) -> Result<Vec<u64>, MidiError> {
    let smf = Smf::parse(bytes).map_err(|e| MidiError::Parse(e.to_string()))?;
    let ticks_per_beat = ticks_per_beat(&smf);
    if ticks_per_beat == 0 {
        return Ok(Vec::new());
    }
    let tempo_map = build_tempo_map(&smf);
    let duration_ms = song_duration_ms(&smf, ticks_per_beat, &tempo_map);

    let mut beats = Vec::new();
    let mut tick: u32 = 0;
    loop {
        let ms = tick_to_ms(tick, ticks_per_beat, &tempo_map);
        if ms > duration_ms {
            break;
        }
        beats.push(ms);
        let Some(next) = tick.checked_add(ticks_per_beat) else {
            break;
        };
        // Guard against pathological tempo maps that don't advance time.
        if next == tick || beats.len() > 500_000 {
            break;
        }
        tick = next;
    }
    Ok(beats)
}

fn build_time_signature_map(smf: &Smf) -> Vec<TimeSignaturePoint> {
    let mut points = Vec::new();
    for track in &smf.tracks {
        let mut abs_tick = 0u32;
        for event in track {
            abs_tick = abs_tick.saturating_add(event.delta.as_int());
            if let TrackEventKind::Meta(MetaMessage::TimeSignature(
                numerator,
                denominator_pow,
                _,
                _,
            )) = event.kind
            {
                points.push(TimeSignaturePoint {
                    tick: abs_tick,
                    numerator: numerator.max(1),
                    denominator_pow,
                });
            }
        }
    }
    points.sort_by_key(|point| point.tick);
    let mut deduped: Vec<TimeSignaturePoint> = Vec::new();
    for point in points {
        if let Some(last) = deduped.last_mut() {
            if last.tick == point.tick {
                *last = point;
                continue;
            }
        }
        deduped.push(point);
    }
    if deduped.first().map(|point| point.tick) != Some(0) {
        deduped.insert(
            0,
            TimeSignaturePoint {
                tick: 0,
                numerator: 4,
                denominator_pow: 2,
            },
        );
    }
    deduped
}

fn bar_ticks(signatures: &[TimeSignaturePoint], ticks_per_quarter: u32, max_tick: u32) -> Vec<u32> {
    let mut out = Vec::new();
    if ticks_per_quarter == 0 {
        return vec![0, max_tick];
    }
    for (index, signature) in signatures.iter().enumerate() {
        if signature.tick > max_tick {
            break;
        }
        let segment_end = signatures
            .get(index + 1)
            .map(|next| next.tick.min(max_tick))
            .unwrap_or(max_tick);
        out.push(signature.tick);
        let denominator = 1u64
            .checked_shl(u32::from(signature.denominator_pow))
            .unwrap_or(u64::MAX);
        let length = (u64::from(ticks_per_quarter)
            .saturating_mul(u64::from(signature.numerator))
            .saturating_mul(4)
            / denominator)
            .max(1) as u32;
        let mut tick = signature.tick;
        while let Some(next) = tick.checked_add(length) {
            if next >= segment_end {
                break;
            }
            out.push(next);
            tick = next;
        }
    }
    out.push(max_tick);
    out.sort_unstable();
    out.dedup();
    out
}

fn bar_boundaries_for_smf(
    smf: &Smf,
    ticks_per_quarter: u32,
    tempo_map: &[TempoPoint],
    max_tick: u32,
) -> Vec<u64> {
    let signatures = build_time_signature_map(smf);
    let mut boundaries: Vec<u64> = bar_ticks(&signatures, ticks_per_quarter, max_tick)
        .into_iter()
        .map(|tick| tick_to_ms(tick, ticks_per_quarter, tempo_map))
        .collect();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}

/// Quarter-note duration in session ms at `position_ms` (MIDI tempo map).
/// Falls back to 500 ms (120 BPM) when the file has no tempo meta.
pub fn quarter_ms_at(bytes: &[u8], position_ms: u64) -> Result<u64, MidiError> {
    let smf = Smf::parse(bytes).map_err(|e| MidiError::Parse(e.to_string()))?;
    let tpb = ticks_per_beat(&smf);
    let tempo_map = build_tempo_map(&smf);

    let mut us_per_quarter = tempo_map
        .first()
        .map(|p| p.us_per_quarter)
        .unwrap_or(500_000);

    for point in &tempo_map {
        let at_ms = tick_to_ms(point.tick, tpb, &tempo_map);
        if at_ms > position_ms {
            break;
        }
        us_per_quarter = point.us_per_quarter;
    }

    Ok(((f64::from(us_per_quarter) / 1000.0).round() as u64).max(1))
}

fn ticks_per_beat(smf: &Smf) -> u32 {
    match smf.header.timing {
        midly::Timing::Metrical(t) => t.as_int() as u32,
        midly::Timing::Timecode(fps, subframe) => (fps.as_f32() * f32::from(subframe)) as u32,
    }
}

fn build_tempo_map(smf: &Smf) -> Vec<TempoPoint> {
    let mut points = Vec::new();
    for track in &smf.tracks {
        let mut abs_tick: u32 = 0;
        for event in track.iter() {
            abs_tick = abs_tick.saturating_add(u32::from(event.delta.as_int()));
            if let TrackEventKind::Meta(MetaMessage::Tempo(tempo)) = event.kind {
                points.push(TempoPoint {
                    tick: abs_tick,
                    us_per_quarter: tempo.as_int(),
                });
            }
        }
    }

    points.sort_by_key(|p| p.tick);
    // Last tempo wins on duplicate ticks.
    let mut deduped: Vec<TempoPoint> = Vec::new();
    for p in points {
        if let Some(last) = deduped.last_mut() {
            if last.tick == p.tick {
                *last = p;
                continue;
            }
        }
        deduped.push(p);
    }

    if deduped.is_empty() || deduped[0].tick != 0 {
        deduped.insert(
            0,
            TempoPoint {
                tick: 0,
                us_per_quarter: 500_000,
            },
        );
    }

    deduped
}

fn tick_to_ms(tick: u32, ticks_per_beat: u32, tempo_map: &[TempoPoint]) -> u64 {
    if ticks_per_beat == 0 || tick == 0 {
        return 0;
    }

    let mut ms_accum = 0.0_f64;
    let mut cursor = 0_u32;
    let mut us_per_quarter = tempo_map
        .first()
        .map(|p| p.us_per_quarter)
        .unwrap_or(500_000);

    for point in tempo_map {
        let segment_end = point.tick.min(tick);
        if segment_end > cursor {
            let dt = f64::from(segment_end - cursor);
            ms_accum += dt * f64::from(us_per_quarter) / f64::from(ticks_per_beat) / 1000.0;
            cursor = segment_end;
        }
        if point.tick >= tick {
            break;
        }
        us_per_quarter = point.us_per_quarter;
        cursor = point.tick;
    }

    if tick > cursor {
        let dt = f64::from(tick - cursor);
        ms_accum += dt * f64::from(us_per_quarter) / f64::from(ticks_per_beat) / 1000.0;
    }

    ms_accum.round() as u64
}

pub fn list_midi_input_ports() -> Result<Vec<MidiInputPort>, MidiError> {
    let midi_in =
        MidiInput::new("drumentor-input-probe").map_err(|e| MidiError::Input(e.to_string()))?;

    let ports = midi_in
        .ports()
        .iter()
        .enumerate()
        .map(|(i, port)| {
            let name = midi_in
                .port_name(port)
                .unwrap_or_else(|_| format!("MIDI Input {i}"));
            MidiInputPort {
                id: i.to_string(),
                name,
            }
        })
        .collect();

    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal Type-0 SMF: 96 tpq, no tempo meta → defaults to 120 BPM.
    fn midi_no_tempo() -> Vec<u8> {
        vec![
            0x4D, 0x54, 0x68, 0x64, // MThd
            0x00, 0x00, 0x00, 0x06, // header length
            0x00, 0x00, // format 0
            0x00, 0x01, // 1 track
            0x00, 0x60, // 96 tpq
            0x4D, 0x54, 0x72, 0x6B, // MTrk
            0x00, 0x00, 0x00, 0x04, // track length
            0x00, 0xFF, 0x2F, 0x00, // end of track
        ]
    }

    /// 120 BPM at tick 0, then 60 BPM (1_000_000 µs/q) at tick 96 (= 500 ms).
    fn midi_tempo_change() -> Vec<u8> {
        vec![
            0x4D, 0x54, 0x68, 0x64, // MThd
            0x00, 0x00, 0x00, 0x06, 0x00, 0x00, // format 0
            0x00, 0x01, // 1 track
            0x00, 0x60, // 96 tpq
            0x4D, 0x54, 0x72, 0x6B, // MTrk
            0x00, 0x00, 0x00, 0x14, // track length = 20
            // delta 0, tempo 500_000 (120 BPM)
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20,
            // delta 96 (0x60), tempo 1_000_000 (60 BPM)
            0x60, 0xFF, 0x51, 0x03, 0x0F, 0x42, 0x40, // delta 0, end of track
            0x00, 0xFF, 0x2F, 0x00,
        ]
    }

    #[test]
    fn quarter_ms_defaults_to_500_at_120bpm() {
        let bytes = midi_no_tempo();
        assert_eq!(quarter_ms_at(&bytes, 0).unwrap(), 500);
        assert_eq!(quarter_ms_at(&bytes, 10_000).unwrap(), 500);
    }

    #[test]
    fn quarter_ms_follows_tempo_map() {
        let bytes = midi_tempo_change();
        // Before the change (at 0 ms): 120 BPM → 500 ms
        assert_eq!(quarter_ms_at(&bytes, 0).unwrap(), 500);
        assert_eq!(quarter_ms_at(&bytes, 499).unwrap(), 500);
        // At/after tick 96 (= 500 ms): 60 BPM → 1000 ms
        assert_eq!(quarter_ms_at(&bytes, 500).unwrap(), 1000);
        assert_eq!(quarter_ms_at(&bytes, 2_000).unwrap(), 1000);
    }

    #[test]
    fn bar_grid_defaults_to_four_four() {
        let signatures = vec![TimeSignaturePoint {
            tick: 0,
            numerator: 4,
            denominator_pow: 2,
        }];
        assert_eq!(bar_ticks(&signatures, 96, 960), vec![0, 384, 768, 960]);
    }

    #[test]
    fn bar_grid_handles_three_four_and_signature_change() {
        let signatures = vec![
            TimeSignaturePoint {
                tick: 0,
                numerator: 3,
                denominator_pow: 2,
            },
            TimeSignaturePoint {
                tick: 576,
                numerator: 4,
                denominator_pow: 2,
            },
        ];
        assert_eq!(
            bar_ticks(&signatures, 96, 1_200),
            vec![0, 288, 576, 960, 1_200]
        );
    }

    #[test]
    fn bar_boundaries_follow_tempo_changes() {
        let tempo = vec![
            TempoPoint {
                tick: 0,
                us_per_quarter: 500_000,
            },
            TempoPoint {
                tick: 384,
                us_per_quarter: 1_000_000,
            },
        ];
        let signatures = vec![TimeSignaturePoint {
            tick: 0,
            numerator: 4,
            denominator_pow: 2,
        }];
        let times: Vec<u64> = bar_ticks(&signatures, 96, 768)
            .into_iter()
            .map(|tick| tick_to_ms(tick, 96, &tempo))
            .collect();
        assert_eq!(times, vec![0, 2_000, 6_000]);
    }

    /// Format 1: CC7 on conductor track for channel 0, notes on a separate track.
    fn midi_cc7_on_conductor() -> Vec<u8> {
        // Track 0: CC7 ch0 = 64, end
        // Track 1: NoteOn ch0, NoteOff, end
        vec![
            0x4D, 0x54, 0x68, 0x64, // MThd
            0x00, 0x00, 0x00, 0x06, 0x00, 0x01, // format 1
            0x00, 0x02, // 2 tracks
            0x00, 0x60, // 96 tpq
            // --- Track 0 (conductor): CC7 channel 0 value 64 ---
            0x4D, 0x54, 0x72, 0x6B, 0x00, 0x00, 0x00, 0x08, 0x00, 0xB0, 0x07,
            0x40, // CC7 = 64 on ch 0
            0x00, 0xFF, 0x2F, 0x00, // --- Track 1 (notes on ch 0) ---
            0x4D, 0x54, 0x72, 0x6B, 0x00, 0x00, 0x00, 0x0B, 0x00, 0x90, 0x3C,
            0x40, // NoteOn C4 vel 64
            0x60, 0x80, 0x3C, 0x40, // NoteOff after 96 ticks
            0x00, 0xFF, 0x2F, 0x00,
        ]
    }

    #[test]
    fn track_volume_from_conductor_cc7() {
        let summary = summarize_midi(&midi_cc7_on_conductor(), "test.mid").unwrap();
        assert_eq!(summary.tracks.len(), 2);
        assert_eq!(summary.tracks[0].volume, 64); // conductor itself saw local CC7
        assert_eq!(summary.tracks[1].note_count, 1);
        assert_eq!(
            summary.tracks[1].volume, 64,
            "note track should inherit channel 0 CC7 from conductor"
        );
    }
}
