use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use midir::{MidiInput, MidiInputConnection};
use tauri::{AppHandle, Emitter, Manager};

use crate::domain::{IncomingHit, MidiNoteOnEvent, PadMapProfile};
use crate::transport::{self, AppSession};

const EVENT_NOTE_ON: &str = "midi:noteOn";
const EVENT_INCOMING_HIT: &str = "midi:incomingHit";

pub struct MidiInputState {
    connection: Option<MidiInputConnection<()>>,
    active_port_id: Option<String>,
    active_profile: Option<PadMapProfile>,
}

impl Default for MidiInputState {
    fn default() -> Self {
        Self {
            connection: None,
            active_port_id: None,
            active_profile: None,
        }
    }
}

pub type MidiInputHandle = Arc<Mutex<MidiInputState>>;

pub fn new_handle() -> MidiInputHandle {
    Arc::new(Mutex::new(MidiInputState::default()))
}

fn host_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn parse_note_on(message: &[u8]) -> Option<(u8, u8, u8)> {
    if message.len() < 3 {
        return None;
    }
    let status = message[0];
    let kind = status & 0xF0;
    let channel = status & 0x0F;
    let note = message[1];
    let velocity = message[2];
    // NoteOn with velocity > 0. Velocity 0 is note-off.
    if kind == 0x90 && velocity > 0 {
        Some((channel, note, velocity))
    } else {
        None
    }
}

pub fn set_active_profile(
    handle: &MidiInputHandle,
    profile: Option<PadMapProfile>,
) -> Result<(), String> {
    let mut state = handle
        .lock()
        .map_err(|e| format!("midi input lock: {e}"))?;
    state.active_profile = profile;
    Ok(())
}

pub fn get_active_profile(handle: &MidiInputHandle) -> Result<Option<PadMapProfile>, String> {
    let state = handle
        .lock()
        .map_err(|e| format!("midi input lock: {e}"))?;
    Ok(state.active_profile.clone())
}

pub fn close_midi_input(handle: &MidiInputHandle) -> Result<(), String> {
    let mut state = handle
        .lock()
        .map_err(|e| format!("midi input lock: {e}"))?;
    state.connection = None;
    state.active_port_id = None;
    Ok(())
}

pub fn open_midi_input(
    app: AppHandle,
    handle: &MidiInputHandle,
    port_id: &str,
) -> Result<(), String> {
    // Drop existing connection first (midir allows only one connection per MidiInput).
    {
        let mut state = handle
            .lock()
            .map_err(|e| format!("midi input lock: {e}"))?;
        state.connection = None;
        state.active_port_id = None;
    }

    let midi_in =
        MidiInput::new("drumentor-input").map_err(|e| format!("midi input open: {e}"))?;
    let ports = midi_in.ports();
    let index: usize = port_id
        .parse()
        .map_err(|_| format!("invalid MIDI port id: {port_id}"))?;
    let port = ports
        .get(index)
        .ok_or_else(|| format!("MIDI port not found: {port_id}"))?;

    let profile_handle = Arc::clone(handle);
    let app_for_cb = app.clone();

    let connection = midi_in
        .connect(
            port,
            "drumentor-input-port",
            move |_stamp, message, _| {
                let Some((channel, note, velocity)) = parse_note_on(message) else {
                    return;
                };
                let time_ms = host_time_ms();
                let wall_ms = app_for_cb
                    .try_state::<crate::audio_engine::AudioEngineHandle>()
                    .map(|audio| audio.wall_ms())
                    .unwrap_or(0.0);

                let raw = MidiNoteOnEvent {
                    note,
                    channel,
                    velocity,
                    time_ms,
                    wall_ms,
                };
                let _ = app_for_cb.emit(EVENT_NOTE_ON, &raw);

                let pad_id = profile_handle
                    .lock()
                    .ok()
                    .and_then(|state| {
                        state
                            .active_profile
                            .as_ref()
                            .and_then(|p| p.map_note(note, channel))
                    });

                match pad_id {
                    Some(pad_id) => {
                        let session = app_for_cb.state::<AppSession>();
                        transport::handle_incoming_hit(
                            &app_for_cb,
                            &session,
                            pad_id,
                            velocity,
                            note,
                            channel,
                        );
                    }
                    None => {
                        let hit = IncomingHit {
                            pad_id: None,
                            raw_note: note,
                            raw_channel: channel,
                            velocity,
                            time_ms,
                        };
                        let _ = app_for_cb.emit(EVENT_INCOMING_HIT, &hit);
                    }
                }
            },
            (),
        )
        .map_err(|e| format!("midi connect failed: {e}"))?;

    let mut state = handle
        .lock()
        .map_err(|e| format!("midi input lock: {e}"))?;
    state.connection = Some(connection);
    state.active_port_id = Some(port_id.to_string());
    Ok(())
}

#[allow(dead_code)]
pub fn active_port_id(handle: &MidiInputHandle) -> Result<Option<String>, String> {
    let state = handle
        .lock()
        .map_err(|e| format!("midi input lock: {e}"))?;
    Ok(state.active_port_id.clone())
}
