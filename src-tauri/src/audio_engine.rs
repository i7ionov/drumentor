//! Native audio sink: oxisynth + cpal (WASAPI default, optional ASIO).

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, Stream, StreamConfig, SupportedBufferSize};
use oxisynth::{MidiEvent, SoundFont, Synth, SynthDescriptor};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::domain::{pad_to_gm_note, NoteRole, PadId, ScheduleNote};

const TARGET_BUFFER_FRAMES: u32 = 256;
const CLICK_FREQ_HZ: f32 = 1100.0;
const CLICK_DURATION_SEC: f32 = 0.05;
const CLICK_GAIN: f32 = 0.22;
const MAIN_VOLUME_CC: u8 = 7;
const DEFAULT_SPEED: f64 = 1.0;
/// Dedicated channel for live player hits so song Mute Drums (ch 9 CC7) does not silence them.
const PLAYER_DRUM_CHANNEL: u8 = 15;
const PLAYER_DRUM_BANK: u32 = 128;
const PLAYER_HIT_DURATION_SEC: f32 = 0.35;

pub const EVENT_AUDIO_CLICK: &str = "audio:click";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioClickEvent {
    pub index: u32,
    pub wall_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimedKind {
    ProgramChange { channel: u8, program: u8 },
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    Click { index: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimedEvent {
    at_sample: u64,
    seq: u64,
    kind: TimedKind,
}

impl PartialOrd for TimedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimedEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Invert so earlier samples are "greater" and pop first from BinaryHeap (max-heap).
        self.at_sample
            .cmp(&other.at_sample)
            .reverse()
            .then_with(|| self.seq.cmp(&other.seq).reverse())
    }
}

struct SharedAudio {
    synth: Synth,
    sample_rate: f32,
    sample_pos: u64,
    session_origin_ms: f64,
    audio_origin_samples: u64,
    speed: f64,
    armed: bool,
    pending: BinaryHeap<TimedEvent>,
    event_seq: u64,
    program_by_channel: HashMap<u8, u8>,
    drum_channels: HashSet<u8>,
    mute_drums: bool,
    /// SoundFont feedback for mapped MIDI pad hits (independent of song mute).
    play_player_drums: bool,
    /// Remaining samples of an in-progress click beep.
    click_left: u32,
    click_phase: f32,
    /// Master click loudness 0.0–1.0 (multiplies [`CLICK_GAIN`]).
    click_volume: f32,
    /// App handle for emitting click events (set after setup).
    app: Option<AppHandle>,
    epoch: Instant,
}

impl SharedAudio {
    fn wall_ms(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.0
    }

    fn session_to_sample(&self, when_ms: f64) -> u64 {
        let rate = if self.speed > 0.0 {
            self.speed
        } else {
            DEFAULT_SPEED
        };
        let delta_sec = (when_ms - self.session_origin_ms) / 1000.0 / rate;
        let delta_samples = (delta_sec * f64::from(self.sample_rate)).round() as i64;
        let origin = self.audio_origin_samples as i64;
        origin.saturating_add(delta_samples).max(0) as u64
    }

    fn push_event(&mut self, at_sample: u64, kind: TimedKind) {
        self.event_seq = self.event_seq.wrapping_add(1);
        self.pending.push(TimedEvent {
            at_sample,
            seq: self.event_seq,
            kind,
        });
    }

    fn apply_event(&mut self, kind: TimedKind) {
        match kind {
            TimedKind::ProgramChange { channel, program } => {
                let _ = self.synth.send_event(MidiEvent::ProgramChange {
                    channel,
                    program_id: program,
                });
                self.program_by_channel.insert(channel, program);
            }
            TimedKind::NoteOn {
                channel,
                note,
                velocity,
            } => {
                let _ = self.synth.send_event(MidiEvent::NoteOn {
                    channel,
                    key: note,
                    vel: velocity,
                });
            }
            TimedKind::NoteOff { channel, note } => {
                let _ = self.synth.send_event(MidiEvent::NoteOff {
                    channel,
                    key: note,
                });
            }
            TimedKind::Click { index } => {
                self.click_left = (self.sample_rate * CLICK_DURATION_SEC).round() as u32;
                self.click_phase = 0.0;
                if let Some(app) = &self.app {
                    let _ = app.emit(
                        EVENT_AUDIO_CLICK,
                        AudioClickEvent {
                            index,
                            wall_ms: self.wall_ms(),
                        },
                    );
                }
            }
        }
    }

    fn cancel_all(&mut self) {
        self.pending.clear();
        self.click_left = 0;
        for ch in 0..16u8 {
            let _ = self.synth.send_event(MidiEvent::AllNotesOff { channel: ch });
            let _ = self.synth.send_event(MidiEvent::AllSoundOff { channel: ch });
        }
        self.program_by_channel.clear();
        self.ensure_player_drum_channel();
    }

    fn ensure_player_drum_channel(&mut self) {
        let _ = self.synth.select_bank(PLAYER_DRUM_CHANNEL, PLAYER_DRUM_BANK);
        let _ = self.synth.send_event(MidiEvent::ProgramChange {
            channel: PLAYER_DRUM_CHANNEL,
            program_id: 0,
        });
        let _ = self.synth.send_event(MidiEvent::ControlChange {
            channel: PLAYER_DRUM_CHANNEL,
            ctrl: MAIN_VOLUME_CC,
            value: 127,
        });
        self.program_by_channel.insert(PLAYER_DRUM_CHANNEL, 0);
    }

    fn set_mute_drums(&mut self, muted: bool) {
        self.mute_drums = muted;
        let volume = if muted { 0 } else { 127 };
        let mut channels: HashSet<u8> = HashSet::from([9]);
        channels.extend(self.drum_channels.iter().copied());
        // Never mute the dedicated player-hit channel.
        channels.remove(&PLAYER_DRUM_CHANNEL);
        for ch in channels {
            let _ = self.synth.send_event(MidiEvent::ControlChange {
                channel: ch,
                ctrl: MAIN_VOLUME_CC,
                value: volume,
            });
            if muted {
                let _ = self.synth.send_event(MidiEvent::AllNotesOff { channel: ch });
            }
        }
    }

    fn arm(&mut self, position_ms: f64, speed: f64) {
        self.session_origin_ms = position_ms;
        self.audio_origin_samples = self.sample_pos;
        self.speed = if speed.is_finite() && speed > 0.0 {
            speed
        } else {
            DEFAULT_SPEED
        };
        self.armed = true;
    }

    fn render_block(&mut self, frames: usize, interleaved: &mut [f32], channels: usize) {
        let mut left = vec![0f32; frames];
        let mut right = vec![0f32; frames];

        // Apply due MIDI/click events sample-accurately in small chunks.
        let mut rendered = 0usize;
        while rendered < frames {
            let block_start = self.sample_pos + rendered as u64;
            while let Some(top) = self.pending.peek() {
                if top.at_sample > block_start {
                    break;
                }
                let ev = self.pending.pop().expect("peeked");
                self.apply_event(ev.kind);
            }

            let next_event_at = self.pending.peek().map(|e| e.at_sample);
            let max_chunk = frames - rendered;
            let chunk = match next_event_at {
                Some(at) if at > block_start => {
                    ((at - block_start) as usize).clamp(1, max_chunk)
                }
                Some(_) => 1,
                None => max_chunk,
            };

            let end = rendered + chunk;
            self.synth.write_f32(
                chunk,
                &mut left[rendered..end],
                0,
                1,
                &mut right[rendered..end],
                0,
                1,
            );

            // Mix click beep into this chunk.
            if self.click_left > 0 {
                let sr = self.sample_rate;
                for i in rendered..end {
                    if self.click_left == 0 {
                        break;
                    }
                    let env = (self.click_left as f32
                        / (sr * CLICK_DURATION_SEC).max(1.0))
                    .clamp(0.0, 1.0);
                    let gain = CLICK_GAIN * self.click_volume.clamp(0.0, 1.0);
                    let sample =
                        (self.click_phase * std::f32::consts::TAU).sin() * gain * env;
                    left[i] = (left[i] + sample).clamp(-1.0, 1.0);
                    right[i] = (right[i] + sample).clamp(-1.0, 1.0);
                    self.click_phase += CLICK_FREQ_HZ / sr;
                    if self.click_phase >= 1.0 {
                        self.click_phase -= 1.0;
                    }
                    self.click_left -= 1;
                }
            }

            rendered = end;
        }

        self.sample_pos += frames as u64;

        if channels == 1 {
            for (i, out) in interleaved.iter_mut().enumerate().take(frames) {
                *out = (left[i] + right[i]) * 0.5;
            }
        } else {
            for i in 0..frames {
                let base = i * channels;
                interleaved[base] = left[i];
                interleaved[base + 1] = right[i];
                for c in 2..channels {
                    interleaved[base + c] = 0.0;
                }
            }
        }
    }
}

pub struct AudioEngine {
    inner: Mutex<EngineInner>,
    shared: Arc<Mutex<SharedAudio>>,
    epoch: Instant,
}

/// Holds the cpal stream. Marked Send/Sync so Tauri can manage `AudioEngine`.
/// The stream is only created/dropped from command/setup paths; the audio
/// callback only touches `SharedAudio` via `Arc<Mutex<_>>`.
struct EngineInner {
    stream: Option<Stream>,
    device_id: Option<String>,
}

// SAFETY: cpal marks Stream !Send/!Sync when multiple hosts are compiled in.
// We never move the Stream to the audio callback thread; we only keep it alive
// in managed state and rebuild it on the control path.
unsafe impl Send for EngineInner {}
unsafe impl Sync for EngineInner {}

pub struct AudioEngineHandle(pub Arc<AudioEngine>);

impl std::ops::Deref for AudioEngineHandle {
    type Target = AudioEngine;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AudioEngine {
    pub fn create(soundfont_path: PathBuf) -> Result<Self, String> {
        let sample_rate = 44100.0f32;
        let mut synth = Synth::new(SynthDescriptor {
            sample_rate,
            ..SynthDescriptor::default()
        })
        .map_err(|e| format!("synth init: {e}"))?;

        {
            let file = File::open(&soundfont_path).map_err(|e| {
                format!(
                    "Failed to open SoundFont {}: {e}",
                    soundfont_path.display()
                )
            })?;
            let mut reader = BufReader::new(file);
            let font = SoundFont::load(&mut reader).map_err(|e| format!("SoundFont load: {e}"))?;
            synth.add_font(font, true);
        }

        let epoch = Instant::now();
        let shared = Arc::new(Mutex::new(SharedAudio {
            synth,
            sample_rate,
            sample_pos: 0,
            session_origin_ms: 0.0,
            audio_origin_samples: 0,
            speed: DEFAULT_SPEED,
            armed: false,
            pending: BinaryHeap::new(),
            event_seq: 0,
            program_by_channel: HashMap::new(),
            drum_channels: HashSet::new(),
            mute_drums: false,
            play_player_drums: true,
            click_left: 0,
            click_phase: 0.0,
            click_volume: 1.0,
            app: None,
            epoch,
        }));

        {
            let mut s = shared.lock().map_err(|e| e.to_string())?;
            s.ensure_player_drum_channel();
        }

        let engine = Self {
            inner: Mutex::new(EngineInner {
                stream: None,
                device_id: None,
            }),
            shared,
            epoch,
        };
        Ok(engine)
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        if let Ok(mut s) = self.shared.lock() {
            s.app = Some(app);
        }
    }

    pub fn wall_ms(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.0
    }

    pub fn open_default(&self) -> Result<AudioDeviceInfo, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default audio output device".to_string())?;
        let name = device
            .name()
            .unwrap_or_else(|_| "Default Output".into());
        let host_name = host_id_name(&host.id());
        let id = device_id(&host_name, &name);
        self.open_device_by_id(&id)?;
        Ok(AudioDeviceInfo {
            id,
            name,
            host: host_name,
        })
    }

    pub fn open_device_by_id(&self, id: &str) -> Result<(), String> {
        let (host_name, device_name) = split_device_id(id)?;
        let device = find_output_device(&host_name, &device_name)?;
        self.start_stream(device, id.to_string())
    }

    pub fn current_device_id(&self) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.device_id.clone())
    }

    pub fn list_devices() -> Result<Vec<AudioDeviceInfo>, String> {
        let mut out = Vec::new();
        for host_id in cpal::available_hosts() {
            let host_name = host_id_name(&host_id);
            let Ok(host) = cpal::host_from_id(host_id) else {
                continue;
            };
            let Ok(devices) = host.output_devices() else {
                continue;
            };
            for device in devices {
                let Ok(name) = device.name() else {
                    continue;
                };
                out.push(AudioDeviceInfo {
                    id: device_id(&host_name, &name),
                    name,
                    host: host_name.clone(),
                });
            }
        }
        // Prefer WASAPI / default host first for stable UI order.
        out.sort_by(|a, b| {
            host_sort_key(&a.host)
                .cmp(&host_sort_key(&b.host))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    pub fn arm(&self, position_ms: u64, speed: f64) {
        if let Ok(mut s) = self.shared.lock() {
            s.arm(position_ms as f64, speed);
        }
    }

    pub fn set_speed(&self, speed: f64) {
        if let Ok(mut s) = self.shared.lock() {
            if speed.is_finite() && speed > 0.0 {
                s.speed = speed;
            }
        }
    }

    pub fn cancel_all(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.cancel_all();
        }
    }

    pub fn set_mute_drums(&self, muted: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.set_mute_drums(muted);
        }
    }

    pub fn set_play_player_drums(&self, enabled: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.play_player_drums = enabled;
            if !enabled {
                let _ = s.synth.send_event(MidiEvent::AllNotesOff {
                    channel: PLAYER_DRUM_CHANNEL,
                });
            } else {
                s.ensure_player_drum_channel();
            }
        }
    }

    /// Immediate SoundFont hit for a mapped pad (independent of transport arm / song mute).
    pub fn play_player_drum_hit(&self, pad: PadId, velocity: u8) {
        let Ok(mut s) = self.shared.lock() else {
            return;
        };
        if !s.play_player_drums {
            return;
        }
        let note = pad_to_gm_note(pad);
        let vel = velocity.clamp(1, 127);
        s.ensure_player_drum_channel();
        let at = s.sample_pos;
        s.push_event(
            at,
            TimedKind::NoteOn {
                channel: PLAYER_DRUM_CHANNEL,
                note,
                velocity: vel,
            },
        );
        let end = at + (f64::from(s.sample_rate) * f64::from(PLAYER_HIT_DURATION_SEC)).round() as u64;
        s.push_event(
            end,
            TimedKind::NoteOff {
                channel: PLAYER_DRUM_CHANNEL,
                note,
            },
        );
    }

    pub fn set_click_volume(&self, volume: f32) {
        if let Ok(mut s) = self.shared.lock() {
            s.click_volume = if volume.is_finite() {
                volume.clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
    }

    /// Schedule a click at session timeline `when_ms` (same clock as notes).
    pub fn schedule_click(&self, when_ms: u64, index: u32) {
        let Ok(mut s) = self.shared.lock() else {
            return;
        };
        if !s.armed || s.click_volume <= 0.0 {
            return;
        }
        let start = s.session_to_sample(when_ms as f64);
        if start + (s.sample_rate as u64 / 20) < s.sample_pos {
            return;
        }
        let start = start.max(s.sample_pos);
        s.push_event(start, TimedKind::Click { index });
    }

    /// Drop pending click events without touching MIDI notes.
    pub fn clear_scheduled_clicks(&self) {
        if let Ok(mut s) = self.shared.lock() {
            let kept: Vec<TimedEvent> = s
                .pending
                .drain()
                .filter(|e| !matches!(e.kind, TimedKind::Click { .. }))
                .collect();
            s.pending = kept.into_iter().collect();
            s.click_left = 0;
        }
    }

    pub fn schedule_note(&self, note: &ScheduleNote) {
        let Ok(mut s) = self.shared.lock() else {
            return;
        };
        if !s.armed {
            return;
        }

        if note.role == NoteRole::Drum {
            s.drum_channels.insert(note.channel);
            if s.mute_drums {
                return;
            }
        }

        let start = s.session_to_sample(note.when_ms as f64);
        // Drop notes already far in the past.
        if start + (s.sample_rate as u64 / 20) < s.sample_pos {
            return;
        }
        let start = start.max(s.sample_pos);

        let last_program = s.program_by_channel.get(&note.channel).copied();
        if last_program != Some(note.program) {
            let prog_at = start.saturating_sub((s.sample_rate * 0.002) as u64);
            let at = prog_at.max(s.sample_pos);
            s.push_event(
                at,
                TimedKind::ProgramChange {
                    channel: note.channel,
                    program: note.program,
                },
            );
        }

        s.push_event(
            start,
            TimedKind::NoteOn {
                channel: note.channel,
                note: note.note,
                velocity: note.velocity,
            },
        );

        let rate = if s.speed > 0.0 { s.speed } else { DEFAULT_SPEED };
        let dur_sec = (note.duration_ms.max(1) as f64) / 1000.0 / rate;
        let end = start + (dur_sec * f64::from(s.sample_rate)).round() as u64;
        s.push_event(
            end,
            TimedKind::NoteOff {
                channel: note.channel,
                note: note.note,
            },
        );
    }

    /// Immediate click (returns wall_ms when queued).
    pub fn play_click(&self) -> Result<f64, String> {
        let mut s = self.shared.lock().map_err(|e| e.to_string())?;
        let wall = s.wall_ms();
        let at = s.sample_pos;
        s.push_event(at, TimedKind::Click { index: 0 });
        Ok(wall)
    }

    /// Schedule a metronome train; emits `audio:click` as each click plays.
    pub fn start_click_train(
        &self,
        count: u32,
        interval_ms: u64,
        lead_in_ms: u64,
    ) -> Result<(), String> {
        let mut s = self.shared.lock().map_err(|e| e.to_string())?;
        // Drop prior scheduled clicks only (keep music notes if any).
        let kept: Vec<TimedEvent> = s
            .pending
            .drain()
            .filter(|e| !matches!(e.kind, TimedKind::Click { .. }))
            .collect();
        s.pending = kept.into_iter().collect();

        let sr = s.sample_rate as f64;
        let lead_samples = (lead_in_ms as f64 / 1000.0 * sr).round() as u64;
        let interval_samples = (interval_ms as f64 / 1000.0 * sr).round() as u64;
        let base = s.sample_pos + lead_samples;
        for i in 0..count {
            s.push_event(
                base + u64::from(i) * interval_samples,
                TimedKind::Click { index: i },
            );
        }
        Ok(())
    }

    pub fn stop_click_train(&self) {
        self.clear_scheduled_clicks();
    }

    fn start_stream(&self, device: Device, device_id: String) -> Result<(), String> {
        let supported = device
            .default_output_config()
            .map_err(|e| format!("output config: {e}"))?;

        let sample_format = supported.sample_format();
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let buffer_size = pick_buffer_size(supported.buffer_size(), TARGET_BUFFER_FRAMES);

        let mut config = StreamConfig {
            channels,
            sample_rate,
            buffer_size,
        };

        {
            let mut s = self.shared.lock().map_err(|e| e.to_string())?;
            let rate = sample_rate.0 as f32;
            s.sample_rate = rate;
            s.synth.set_sample_rate(rate);
            s.sample_pos = 0;
            s.audio_origin_samples = 0;
            s.pending.clear();
            s.click_left = 0;
            s.ensure_player_drum_channel();
        }

        let shared = Arc::clone(&self.shared);
        let channels_usize = channels as usize;
        let err_fn = |err| eprintln!("audio stream error: {err}");

        let build = |cfg: &StreamConfig| -> Result<Stream, String> {
            match sample_format {
                SampleFormat::F32 => {
                    let shared = Arc::clone(&shared);
                    device
                        .build_output_stream(
                            cfg,
                            move |data: &mut [f32], _| {
                                let frames = data.len() / channels_usize;
                                if let Ok(mut s) = shared.lock() {
                                    s.render_block(frames, data, channels_usize);
                                } else {
                                    data.fill(0.0);
                                }
                            },
                            err_fn,
                            None,
                        )
                        .map_err(|e| format!("build stream: {e}"))
                }
                SampleFormat::I16 => {
                    let shared = Arc::clone(&shared);
                    device
                        .build_output_stream(
                            cfg,
                            move |data: &mut [i16], _| {
                                let frames = data.len() / channels_usize;
                                let mut tmp = vec![0f32; data.len()];
                                if let Ok(mut s) = shared.lock() {
                                    s.render_block(frames, &mut tmp, channels_usize);
                                }
                                for (out, sample) in data.iter_mut().zip(tmp.iter()) {
                                    *out = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                                }
                            },
                            err_fn,
                            None,
                        )
                        .map_err(|e| format!("build stream: {e}"))
                }
                SampleFormat::U16 => {
                    let shared = Arc::clone(&shared);
                    device
                        .build_output_stream(
                            cfg,
                            move |data: &mut [u16], _| {
                                let frames = data.len() / channels_usize;
                                let mut tmp = vec![0f32; data.len()];
                                if let Ok(mut s) = shared.lock() {
                                    s.render_block(frames, &mut tmp, channels_usize);
                                }
                                for (out, sample) in data.iter_mut().zip(tmp.iter()) {
                                    let s = sample.clamp(-1.0, 1.0);
                                    *out = ((s * 0.5 + 0.5) * u16::MAX as f32) as u16;
                                }
                            },
                            err_fn,
                            None,
                        )
                        .map_err(|e| format!("build stream: {e}"))
                }
                // ASIO drivers commonly expose I32 (and sometimes F64).
                SampleFormat::I32 => {
                    let shared = Arc::clone(&shared);
                    device
                        .build_output_stream(
                            cfg,
                            move |data: &mut [i32], _| {
                                let frames = data.len() / channels_usize;
                                let mut tmp = vec![0f32; data.len()];
                                if let Ok(mut s) = shared.lock() {
                                    s.render_block(frames, &mut tmp, channels_usize);
                                }
                                for (out, sample) in data.iter_mut().zip(tmp.iter()) {
                                    *out = (sample.clamp(-1.0, 1.0) as f64 * i32::MAX as f64)
                                        as i32;
                                }
                            },
                            err_fn,
                            None,
                        )
                        .map_err(|e| format!("build stream: {e}"))
                }
                SampleFormat::F64 => {
                    let shared = Arc::clone(&shared);
                    device
                        .build_output_stream(
                            cfg,
                            move |data: &mut [f64], _| {
                                let frames = data.len() / channels_usize;
                                let mut tmp = vec![0f32; data.len()];
                                if let Ok(mut s) = shared.lock() {
                                    s.render_block(frames, &mut tmp, channels_usize);
                                }
                                for (out, sample) in data.iter_mut().zip(tmp.iter()) {
                                    *out = f64::from(*sample);
                                }
                            },
                            err_fn,
                            None,
                        )
                        .map_err(|e| format!("build stream: {e}"))
                }
                other => Err(format!("unsupported sample format: {other:?}")),
            }
        };

        let stream = match build(&config) {
            Ok(s) => s,
            Err(first_err) => {
                // Some hosts reject Fixed buffer sizes — fall back to Default.
                config.buffer_size = BufferSize::Default;
                build(&config).map_err(|e| format!("{first_err}; fallback failed: {e}"))?
            }
        };

        stream.play().map_err(|e| format!("play stream: {e}"))?;

        let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
        // Drop previous stream first (stops it).
        inner.stream = Some(stream);
        inner.device_id = Some(device_id);
        Ok(())
    }
}

fn pick_buffer_size(supported: &SupportedBufferSize, target: u32) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => {
            let frames = target.clamp(*min, *max);
            BufferSize::Fixed(frames)
        }
        SupportedBufferSize::Unknown => BufferSize::Fixed(target),
    }
}

fn host_id_name(id: &cpal::HostId) -> String {
    #[cfg(target_os = "windows")]
    {
        match id {
            cpal::HostId::Wasapi => "wasapi".into(),
            #[cfg(feature = "asio")]
            cpal::HostId::Asio => "asio".into(),
        }
    }
    #[cfg(target_os = "macos")]
    {
        match id {
            cpal::HostId::CoreAudio => "coreaudio".into(),
            _ => "default".into(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        match id {
            cpal::HostId::Alsa => "alsa".into(),
            #[cfg(feature = "jack")]
            cpal::HostId::Jack => "jack".into(),
            _ => "default".into(),
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = id;
        "default".into()
    }
}

fn host_sort_key(host: &str) -> u8 {
    match host {
        "wasapi" | "coreaudio" | "alsa" => 0,
        "asio" | "jack" => 1,
        _ => 2,
    }
}

fn device_id(host: &str, name: &str) -> String {
    format!("{host}::{name}")
}

fn split_device_id(id: &str) -> Result<(String, String), String> {
    let (host, name) = id
        .split_once("::")
        .ok_or_else(|| format!("invalid audio device id: {id}"))?;
    Ok((host.to_string(), name.to_string()))
}

fn find_output_device(host_name: &str, device_name: &str) -> Result<Device, String> {
    for host_id in cpal::available_hosts() {
        if host_id_name(&host_id) != host_name {
            continue;
        }
        let host = cpal::host_from_id(host_id).map_err(|e| e.to_string())?;
        for device in host.output_devices().map_err(|e| e.to_string())? {
            if device.name().ok().as_deref() == Some(device_name) {
                return Ok(device);
            }
        }
    }
    Err(format!("audio device not found: {host_name}::{device_name}"))
}

/// Resolve SoundFont path: Tauri resource dir, then repo `public/soundfonts` for dev.
///
/// Bundled layout: `tauri.conf.json` maps the sf3 to `FluidR3.sf3` in the resource
/// dir. Older bundles that used `../public/...` may still place it under `_up_/`.
pub fn resolve_soundfont_path(resource_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    let mut candidates: VecDeque<PathBuf> = VecDeque::new();
    if let Some(dir) = resource_dir {
        candidates.push_back(dir.join("FluidR3.sf3"));
        candidates.push_back(dir.join("soundfonts").join("FluidR3.sf3"));
        // Legacy Tauri rewrite of `../public/soundfonts/FluidR3.sf3`
        candidates.push_back(dir.join("_up_/public/soundfonts/FluidR3.sf3"));
    }
    // Dev: src-tauri cwd → ../public/soundfonts
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push_back(cwd.join("../public/soundfonts/FluidR3.sf3"));
        candidates.push_back(cwd.join("public/soundfonts/FluidR3.sf3"));
    }
    // Relative to executable (release next to .exe, or installer root)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push_back(dir.join("FluidR3.sf3"));
            candidates.push_back(dir.join("soundfonts").join("FluidR3.sf3"));
            candidates.push_back(dir.join("_up_/public/soundfonts/FluidR3.sf3"));
            candidates.push_back(dir.join("../public/soundfonts/FluidR3.sf3"));
        }
    }

    for path in candidates {
        if let Ok(canon) = path.canonicalize() {
            if canon.is_file() {
                return Ok(canon);
            }
        } else if path.is_file() {
            return Ok(path);
        }
    }
    Err(
        "FluidR3.sf3 not found. Place it in public/soundfonts/ (dev) or bundle resources."
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_roundtrip() {
        let id = device_id("wasapi", "Speakers");
        let (h, n) = split_device_id(&id).unwrap();
        assert_eq!(h, "wasapi");
        assert_eq!(n, "Speakers");
    }
}
