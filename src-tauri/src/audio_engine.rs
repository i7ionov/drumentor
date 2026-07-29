//! Native audio sink: oxisynth + cpal (WASAPI default, optional ASIO).

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, Stream, StreamConfig, SupportedBufferSize};
use oxisynth::{MidiEvent, SoundFont, Synth, SynthDescriptor};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::domain::{pad_to_gm_note, PadId, ScheduleNote};

const TARGET_BUFFER_FRAMES: u32 = 256;
const CLICK_FREQ_HZ: f32 = 1100.0;
const CLICK_DURATION_SEC: f32 = 0.05;
const CLICK_GAIN: f32 = 0.22;
const MAIN_VOLUME_CC: u8 = 7;
const DEFAULT_SPEED: f64 = 1.0;
/// Dedicated channel for live player hits (independent of song track mute/solo).
const PLAYER_DRUM_CHANNEL: u8 = 15;
const PLAYER_DRUM_BANK: u32 = 128;
const PLAYER_HIT_DURATION_SEC: f32 = 0.35;
/// MIDI-style volume: 0–127, unity at 100 (gain = volume / 100).
const VOLUME_UNITY: f32 = 100.0;
const VOLUME_MAX: f32 = 127.0;

pub const EVENT_AUDIO_CLICK: &str = "audio:click";

fn clamp_midi_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, VOLUME_MAX)
    } else {
        VOLUME_UNITY
    }
}

/// 100 → 1.0, 127 → 1.27, 0 → 0.
fn volume_gain(volume: f32) -> f32 {
    clamp_midi_volume(volume) / VOLUME_UNITY
}

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
    /// SoundFont feedback for mapped MIDI pad hits (independent of song mute).
    play_player_drums: bool,
    /// Remaining samples of an in-progress click beep.
    click_left: u32,
    click_phase: f32,
    /// Master output volume 0–127 (unity 100).
    master_volume: f32,
    /// Per MIDI track volume 0–127 (missing → 100).
    track_volume: HashMap<u16, f32>,
    track_muted: HashSet<u16>,
    track_solo: HashSet<u16>,
    /// Selected drum track (for Mute Drums shortcut).
    drum_track_id: Option<u16>,
    /// Metronome click volume 0–127 (unity 100).
    click_volume: f32,
    click_muted: bool,
    /// Live pad hit volume 0–127 (unity 100).
    player_volume: f32,
    player_muted: bool,
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
        let cc = if self.player_muted {
            0
        } else {
            (volume_gain(self.player_volume) * 127.0)
                .round()
                .clamp(0.0, 127.0) as u8
        };
        let _ = self.synth.send_event(MidiEvent::ControlChange {
            channel: PLAYER_DRUM_CHANNEL,
            ctrl: MAIN_VOLUME_CC,
            value: cc,
        });
        self.program_by_channel.insert(PLAYER_DRUM_CHANNEL, 0);
    }

    fn track_audible(&self, track_id: u16) -> bool {
        if !self.track_solo.is_empty() {
            return self.track_solo.contains(&track_id);
        }
        !self.track_muted.contains(&track_id)
    }

    fn track_gain(&self, track_id: u16) -> f32 {
        let vol = self
            .track_volume
            .get(&track_id)
            .copied()
            .unwrap_or(VOLUME_UNITY);
        volume_gain(vol)
    }

    fn reset_track_mixer(&mut self) {
        self.track_volume.clear();
        self.track_muted.clear();
        self.track_solo.clear();
        self.drum_track_id = None;
    }

    fn set_track_mute(&mut self, track_id: u16, muted: bool) {
        if muted {
            self.track_muted.insert(track_id);
        } else {
            self.track_muted.remove(&track_id);
        }
    }

    fn set_track_solo(&mut self, track_id: u16, solo: bool) {
        if solo {
            self.track_solo.insert(track_id);
        } else {
            self.track_solo.remove(&track_id);
        }
    }

    fn set_mute_drums(&mut self, muted: bool) {
        if let Some(id) = self.drum_track_id {
            self.set_track_mute(id, muted);
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
                    let gain = CLICK_GAIN * volume_gain(self.click_volume);
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

        let master = volume_gain(self.master_volume);
        if channels == 1 {
            for (i, out) in interleaved.iter_mut().enumerate().take(frames) {
                *out = ((left[i] + right[i]) * 0.5 * master).clamp(-1.0, 1.0);
            }
        } else {
            for i in 0..frames {
                let base = i * channels;
                interleaved[base] = (left[i] * master).clamp(-1.0, 1.0);
                interleaved[base + 1] = (right[i] * master).clamp(-1.0, 1.0);
                for c in 2..channels {
                    interleaved[base + c] = 0.0;
                }
            }
        }
    }
}

pub struct AudioEngine {
    /// Serialises open requests (request + reply) so two callers cannot race.
    device_tx: Mutex<Sender<DeviceCommand>>,
    device_id: Mutex<Option<String>>,
    shared: Arc<Mutex<SharedAudio>>,
    epoch: Instant,
}

enum DeviceCommand {
    Open {
        id: String,
        reply: Sender<Result<(), String>>,
    },
}

/// Owns the cpal stream on one long-lived thread (ASIO expects that).
fn device_thread(shared: Arc<Mutex<SharedAudio>>, rx: Receiver<DeviceCommand>) {
    let mut current: Option<Stream> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DeviceCommand::Open { id, reply } => {
                // ASIO allows only one loaded driver — drop before opening another.
                drop(current.take());
                let result = match open_stream(&shared, &id) {
                    Ok(stream) => {
                        current = Some(stream);
                        Ok(())
                    }
                    Err(err) => Err(err),
                };
                let _ = reply.send(result);
            }
        }
    }
}

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
            play_player_drums: true,
            click_left: 0,
            click_phase: 0.0,
            master_volume: VOLUME_UNITY,
            track_volume: HashMap::new(),
            track_muted: HashSet::new(),
            track_solo: HashSet::new(),
            drum_track_id: None,
            click_volume: VOLUME_UNITY,
            click_muted: false,
            player_volume: VOLUME_UNITY,
            player_muted: false,
            app: None,
            epoch,
        }));

        {
            let mut s = shared.lock().map_err(|e| e.to_string())?;
            s.ensure_player_drum_channel();
        }

        let (device_tx, device_rx) = mpsc::channel();
        let device_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("audio-device".into())
            .spawn(move || device_thread(device_shared, device_rx))
            .map_err(|e| format!("spawn audio-device thread: {e}"))?;

        let engine = Self {
            device_tx: Mutex::new(device_tx),
            device_id: Mutex::new(None),
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
        // Resolve id under the device lock, then open on the device thread
        // (which takes the same lock — so release first).
        let (id, name, host_name) = {
            let _guard = lock_devices();
            let host = host_by_id(cpal::default_host().id())
                .ok_or_else(|| "default audio host unavailable".to_string())?;
            let device = host
                .default_output_device()
                .ok_or_else(|| "no default audio output device".to_string())?;
            let name = device.name().unwrap_or_else(|_| "Default Output".into());
            let host_name = host_id_name(&host.id());
            (device_id(&host_name, &name), name, host_name)
        };
        self.open_device_by_id(&id)?;
        Ok(AudioDeviceInfo {
            id,
            name,
            host: host_name,
        })
    }

    pub fn open_device_by_id(&self, id: &str) -> Result<(), String> {
        let result = {
            let tx = self.device_tx.lock().map_err(|e| e.to_string())?;
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(DeviceCommand::Open {
                id: id.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| "audio device thread stopped".to_string())?;
            reply_rx
                .recv()
                .map_err(|_| "audio device thread stopped".to_string())?
        };
        if let Ok(mut current) = self.device_id.lock() {
            // Failed open already dropped the previous stream.
            *current = result.is_ok().then(|| id.to_string());
        }
        result
    }

    pub fn current_device_id(&self) -> Option<String> {
        self.device_id.lock().ok().and_then(|g| g.clone())
    }

    pub fn list_devices() -> Result<Vec<AudioDeviceInfo>, String> {
        let _guard = lock_devices();
        let mut out = Vec::new();
        for (host_id, host) in hosts() {
            let host_name = host_id_name(host_id);
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
        remember_hidden_devices(&mut out);
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

    pub fn reset_track_mixer(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.reset_track_mixer();
        }
    }

    pub fn set_drum_track_id(&self, track_id: Option<u16>) {
        if let Ok(mut s) = self.shared.lock() {
            s.drum_track_id = track_id;
        }
    }

    pub fn set_master_volume(&self, volume: f32) {
        if let Ok(mut s) = self.shared.lock() {
            s.master_volume = clamp_midi_volume(volume);
        }
    }

    pub fn set_track_volume(&self, track_id: u16, volume: f32) {
        if let Ok(mut s) = self.shared.lock() {
            s.track_volume.insert(track_id, clamp_midi_volume(volume));
        }
    }

    pub fn set_track_mute(&self, track_id: u16, muted: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.set_track_mute(track_id, muted);
        }
    }

    pub fn set_track_solo(&self, track_id: u16, solo: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.set_track_solo(track_id, solo);
        }
    }

    pub fn set_player_volume(&self, volume: f32) {
        if let Ok(mut s) = self.shared.lock() {
            s.player_volume = clamp_midi_volume(volume);
            s.ensure_player_drum_channel();
        }
    }

    pub fn set_player_muted(&self, muted: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.player_muted = muted;
            if muted {
                let _ = s.synth.send_event(MidiEvent::AllNotesOff {
                    channel: PLAYER_DRUM_CHANNEL,
                });
            }
            s.ensure_player_drum_channel();
        }
    }

    pub fn set_click_muted(&self, muted: bool) {
        if let Ok(mut s) = self.shared.lock() {
            s.click_muted = muted;
            if muted {
                s.click_left = 0;
            }
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
        if !s.play_player_drums || s.player_muted || s.player_volume <= 0.0 {
            return;
        }
        let note = pad_to_gm_note(pad);
        let scaled = (f32::from(velocity.clamp(1, 127)) * volume_gain(s.player_volume))
            .round()
            .clamp(1.0, 127.0) as u8;
        let vel = scaled;
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
            s.click_volume = clamp_midi_volume(volume);
        }
    }

    /// Schedule a click at session timeline `when_ms` (same clock as notes).
    pub fn schedule_click(&self, when_ms: u64, index: u32) {
        let Ok(mut s) = self.shared.lock() else {
            return;
        };
        if !s.armed || s.click_muted || s.click_volume <= 0.0 {
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

        if !s.track_audible(note.track_id) {
            return;
        }

        let gain = s.track_gain(note.track_id);
        if gain <= 0.0 {
            return;
        }
        let velocity = (f32::from(note.velocity) * gain)
            .round()
            .clamp(0.0, 127.0) as u8;
        if velocity == 0 {
            return;
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
                velocity,
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
        // Calibration clicks ignore metronome mute.
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
}

/// Build and start an output stream. Must run on the `audio-device` thread.
fn open_stream(shared: &Arc<Mutex<SharedAudio>>, device_id: &str) -> Result<Stream, String> {
    let _guard = lock_devices();
    let (host_name, device_name) = split_device_id(device_id)?;
    let device = find_output_device(&host_name, &device_name)?;

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
        let mut s = shared.lock().map_err(|e| e.to_string())?;
        let rate = sample_rate.0 as f32;
        s.sample_rate = rate;
        s.synth.set_sample_rate(rate);
        s.sample_pos = 0;
        s.audio_origin_samples = 0;
        s.pending.clear();
        s.click_left = 0;
        s.ensure_player_drum_channel();
    }

    let shared = Arc::clone(shared);
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
                                *out = (sample.clamp(-1.0, 1.0) as f64 * i32::MAX as f64) as i32;
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
    Ok(stream)
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

/// One `cpal::Host` per id for the process lifetime.
///
/// Fresh `host_from_id` calls each get an empty ASIO "loaded driver" tracker,
/// while the SDK keeps the real driver in global state. Enumerating through a
/// second host then reloads the driver under a live stream and kills audio with
/// no error. Sharing hosts keeps `load_driver` aware of what is already open.
fn hosts() -> &'static [(cpal::HostId, cpal::Host)] {
    static HOSTS: OnceLock<Vec<(cpal::HostId, cpal::Host)>> = OnceLock::new();
    HOSTS.get_or_init(|| {
        cpal::available_hosts()
            .into_iter()
            .filter_map(|id| cpal::host_from_id(id).ok().map(|host| (id, host)))
            .collect()
    })
}

/// Serialises ASIO enumerate vs open (SDK driver slot is process-global).
fn lock_devices() -> std::sync::MutexGuard<'static, ()> {
    static DEVICE_LOCK: Mutex<()> = Mutex::new(());
    DEVICE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn host_by_id(id: cpal::HostId) -> Option<&'static cpal::Host> {
    hosts()
        .iter()
        .find(|(host_id, _)| *host_id == id)
        .map(|(_, host)| host)
}

/// ASIO can only enumerate the loaded driver; keep previously seen ASIO devices
/// in the picker while another one is playing.
fn remember_hidden_devices(devices: &mut Vec<AudioDeviceInfo>) {
    static SEEN: Mutex<Vec<AudioDeviceInfo>> = Mutex::new(Vec::new());
    let Ok(mut seen) = SEEN.lock() else {
        return;
    };
    for device in devices.iter().filter(|d| d.host == "asio") {
        if !seen.iter().any(|s| s.id == device.id) {
            seen.push(device.clone());
        }
    }
    for device in seen.iter() {
        if !devices.iter().any(|d| d.id == device.id) {
            devices.push(device.clone());
        }
    }
}

fn find_output_device(host_name: &str, device_name: &str) -> Result<Device, String> {
    for (host_id, host) in hosts() {
        if host_id_name(host_id) != host_name {
            continue;
        }
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
