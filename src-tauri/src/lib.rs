mod audio_engine;
mod db;
mod domain;
mod hit_matcher;
mod midi;
mod midi_input;
mod transport;

use audio_engine::{AudioDeviceInfo, AudioEngine, AudioEngineHandle};
use db::Database;
use domain::{AppInfo, ExpectedHit, MidiInputPort, PadMapProfile, SongSummary, TransportState};
use midi::list_midi_input_ports;
use midi_input::MidiInputHandle;
use transport::AppSession;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

pub struct DbState(pub Database);

struct SetupState {
    frontend_ready: bool,
    backend_ready: bool,
}

struct SplashGate {
    state: Mutex<SetupState>,
    ready: Condvar,
}

fn reveal_main_window(app: &AppHandle) {
    if let Some(splash) = app.get_webview_window("splashscreen") {
        let _ = splash.close();
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.set_focus();
    }
}

fn maybe_reveal_main(app: &AppHandle, state: &SetupState) {
    if state.frontend_ready && state.backend_ready {
        reveal_main_window(app);
    }
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple UTC timestamp sufficient for local profiles (ISO-8601-ish).
    format!("{secs}")
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    AppInfo {
        name: "Drumentor".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        phase: "mvp-native-audio".into(),
    }
}

/// Called once the main webview has painted. Blocks until backend init finishes
/// so the UI can safely invoke audio/MIDI commands afterward.
#[tauri::command]
fn splash_frontend_ready(app: AppHandle, gate: State<'_, Arc<SplashGate>>) -> Result<(), String> {
    {
        let mut s = gate.state.lock().map_err(|e| e.to_string())?;
        s.frontend_ready = true;
        maybe_reveal_main(&app, &s);
        if s.backend_ready {
            return Ok(());
        }
    }

    let (guard, timed_out) = {
        let s = gate.state.lock().map_err(|e| e.to_string())?;
        let (guard, wait) = gate
            .ready
            .wait_timeout_while(s, Duration::from_secs(60), |s| !s.backend_ready)
            .map_err(|e| e.to_string())?;
        (guard, wait.timed_out())
    };

    maybe_reveal_main(&app, &guard);
    if timed_out && !guard.backend_ready {
        reveal_main_window(&app);
        return Err("backend startup timed out".into());
    }
    Ok(())
}

#[tauri::command]
fn parse_midi(
    path: String,
    session: State<'_, AppSession>,
    audio: State<'_, AudioEngineHandle>,
) -> Result<SongSummary, String> {
    audio.cancel_all();
    audio.reset_track_mixer();
    let summary = transport::load_midi(&session, &path)?;
    for track in &summary.tracks {
        audio.set_track_volume(track.id, f32::from(track.volume));
    }
    Ok(summary)
}

#[tauri::command]
fn set_drum_track(
    track_id: u16,
    session: State<'_, AppSession>,
    midi: State<'_, MidiInputHandle>,
    audio: State<'_, AudioEngineHandle>,
) -> Result<(), String> {
    audio.cancel_all();
    let profile = midi_input::get_active_profile(&midi)?;
    transport::set_drum_track(&session, track_id, profile.as_ref())?;
    audio.set_drum_track_id(Some(track_id));
    Ok(())
}

#[tauri::command]
fn get_expected_hits(session: State<'_, AppSession>) -> Result<Vec<ExpectedHit>, String> {
    transport::get_expected_hits(&session)
}

#[tauri::command]
fn list_midi_inputs() -> Result<Vec<MidiInputPort>, String> {
    list_midi_input_ports().map_err(|e| e.to_string())
}

#[tauri::command]
fn open_midi_input(
    port_id: String,
    app: AppHandle,
    midi: State<'_, MidiInputHandle>,
) -> Result<(), String> {
    midi_input::open_midi_input(app, &midi, &port_id)
}

#[tauri::command]
fn close_midi_input(midi: State<'_, MidiInputHandle>) -> Result<(), String> {
    midi_input::close_midi_input(&midi)
}

#[tauri::command]
fn list_pad_maps(db: State<'_, DbState>) -> Result<Vec<PadMapProfile>, String> {
    db.0.list_pad_maps().map_err(|e| e.to_string())
}

#[tauri::command]
fn get_pad_map(id: String, db: State<'_, DbState>) -> Result<Option<PadMapProfile>, String> {
    db.0.get_pad_map(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_pad_map(
    mut profile: PadMapProfile,
    db: State<'_, DbState>,
    midi: State<'_, MidiInputHandle>,
    session: State<'_, AppSession>,
) -> Result<PadMapProfile, String> {
    if profile.id.is_empty() {
        profile.id = uuid::Uuid::new_v4().to_string();
    }
    if profile.schema_version == 0 {
        profile.schema_version = 1;
    }
    let stamp = now_rfc3339();
    if profile.created_at.is_empty() {
        profile.created_at = stamp.clone();
    }
    profile.updated_at = stamp;
    db.0.save_pad_map(&profile).map_err(|e| e.to_string())?;
    // If this is the active profile, refresh in-memory mapper + expected fold.
    if let Ok(Some(active_id)) = db.0.get_active_pad_map_id() {
        if active_id == profile.id {
            midi_input::set_active_profile(&midi, Some(profile.clone()))?;
            transport::apply_pad_map_fold(&session, Some(&profile))?;
        }
    }
    Ok(profile)
}

#[tauri::command]
fn delete_pad_map(
    id: String,
    db: State<'_, DbState>,
    midi: State<'_, MidiInputHandle>,
    session: State<'_, AppSession>,
) -> Result<(), String> {
    let was_active = db
        .0
        .get_active_pad_map_id()
        .map_err(|e| e.to_string())?
        .as_deref()
        == Some(id.as_str());
    db.0.delete_pad_map(&id).map_err(|e| e.to_string())?;
    if was_active {
        midi_input::set_active_profile(&midi, None)?;
        transport::apply_pad_map_fold(&session, None)?;
    }
    Ok(())
}

#[tauri::command]
fn set_active_pad_map(
    id: Option<String>,
    db: State<'_, DbState>,
    midi: State<'_, MidiInputHandle>,
    session: State<'_, AppSession>,
) -> Result<Option<PadMapProfile>, String> {
    match id {
        Some(id) => {
            let profile = db
                .0
                .get_pad_map(&id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("pad map not found: {id}"))?;
            db.0.set_active_pad_map_id(Some(&id))
                .map_err(|e| e.to_string())?;
            midi_input::set_active_profile(&midi, Some(profile.clone()))?;
            transport::apply_pad_map_fold(&session, Some(&profile))?;
            Ok(Some(profile))
        }
        None => {
            db.0.set_active_pad_map_id(None)
                .map_err(|e| e.to_string())?;
            midi_input::set_active_profile(&midi, None)?;
            transport::apply_pad_map_fold(&session, None)?;
            Ok(None)
        }
    }
}

#[tauri::command]
fn get_active_pad_map(
    db: State<'_, DbState>,
    midi: State<'_, MidiInputHandle>,
) -> Result<Option<PadMapProfile>, String> {
    if let Some(profile) = midi_input::get_active_profile(&midi)? {
        return Ok(Some(profile));
    }
    let Some(id) = db.0.get_active_pad_map_id().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let profile = db.0.get_pad_map(&id).map_err(|e| e.to_string())?;
    if let Some(ref p) = profile {
        midi_input::set_active_profile(&midi, Some(p.clone()))?;
    }
    Ok(profile)
}

#[tauri::command]
fn get_latency_offset_ms(
    db: State<'_, DbState>,
    session: State<'_, AppSession>,
) -> Result<i64, String> {
    let ms = db.0.get_latency_offset_ms().map_err(|e| e.to_string())?;
    transport::set_latency_offset(&session, ms)?;
    Ok(ms)
}

#[tauri::command]
fn set_latency_offset_ms(
    ms: i64,
    db: State<'_, DbState>,
    session: State<'_, AppSession>,
) -> Result<(), String> {
    db.0.set_latency_offset_ms(ms)
        .map_err(|e| e.to_string())?;
    transport::set_latency_offset(&session, ms)
}

#[tauri::command]
fn list_audio_devices() -> Result<Vec<AudioDeviceInfo>, String> {
    AudioEngine::list_devices()
}

#[tauri::command]
fn get_audio_device(
    audio: State<'_, AudioEngineHandle>,
    db: State<'_, DbState>,
) -> Result<Option<String>, String> {
    if let Some(id) = audio.current_device_id() {
        return Ok(Some(id));
    }
    db.0.get_audio_device_id().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_audio_device(
    id: String,
    audio: State<'_, AudioEngineHandle>,
    db: State<'_, DbState>,
) -> Result<(), String> {
    audio.open_device_by_id(&id)?;
    db.0.set_audio_device_id(Some(&id))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn set_mute_drums(muted: bool, audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.set_mute_drums(muted);
    Ok(())
}

#[tauri::command]
fn set_master_volume(volume: f64, audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.set_master_volume(volume as f32);
    Ok(())
}

#[tauri::command]
fn set_track_volume(
    track_id: u16,
    volume: f64,
    audio: State<'_, AudioEngineHandle>,
) -> Result<(), String> {
    audio.set_track_volume(track_id, volume as f32);
    Ok(())
}

#[tauri::command]
fn set_track_mute(
    track_id: u16,
    muted: bool,
    audio: State<'_, AudioEngineHandle>,
) -> Result<(), String> {
    audio.set_track_mute(track_id, muted);
    Ok(())
}

#[tauri::command]
fn set_track_solo(
    track_id: u16,
    solo: bool,
    audio: State<'_, AudioEngineHandle>,
) -> Result<(), String> {
    audio.set_track_solo(track_id, solo);
    Ok(())
}

#[tauri::command]
fn set_player_volume(volume: f64, audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.set_player_volume(volume as f32);
    Ok(())
}

#[tauri::command]
fn set_player_muted(muted: bool, audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.set_player_muted(muted);
    Ok(())
}

#[tauri::command]
fn set_click_muted(muted: bool, audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.set_click_muted(muted);
    Ok(())
}

#[tauri::command]
fn set_play_player_drums(enabled: bool, audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.set_play_player_drums(enabled);
    Ok(())
}

#[tauri::command]
fn set_metronome_enabled(
    enabled: bool,
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<(), String> {
    transport::set_metronome_enabled(&app, &session, enabled)
}

#[tauri::command]
fn set_metronome_volume(volume: f64, app: AppHandle) -> Result<(), String> {
    transport::set_metronome_volume(&app, volume)
}

#[tauri::command]
fn audio_play_click(audio: State<'_, AudioEngineHandle>) -> Result<f64, String> {
    audio.play_click()
}

#[tauri::command]
fn audio_start_click_train(
    count: u32,
    interval_ms: u64,
    lead_in_ms: u64,
    audio: State<'_, AudioEngineHandle>,
) -> Result<(), String> {
    audio.start_click_train(count, interval_ms, lead_in_ms)
}

#[tauri::command]
fn audio_stop_click_train(audio: State<'_, AudioEngineHandle>) -> Result<(), String> {
    audio.stop_click_train();
    Ok(())
}

#[tauri::command]
fn get_transport_state(session: State<'_, AppSession>) -> Result<TransportState, String> {
    transport::get_state(&session)
}

#[tauri::command]
fn transport_play(
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::play(app, &session)
}

#[tauri::command]
fn transport_pause(
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::pause(app, &session)
}

#[tauri::command]
fn transport_stop(
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::stop(app, &session)
}

#[tauri::command]
fn transport_seek(
    position_ms: u64,
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::seek(app, &session, position_ms)
}

#[tauri::command]
fn transport_set_speed(
    speed: f64,
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::set_speed(app, &session, speed)
}

#[tauri::command]
fn transport_set_repeat(
    enabled: bool,
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::set_repeat_enabled(app, &session, enabled)
}

#[tauri::command]
fn transport_set_loop_region(
    region: Option<domain::LoopRegion>,
    app: AppHandle,
    session: State<'_, AppSession>,
) -> Result<TransportState, String> {
    transport::set_loop_region(app, &session, region)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppSession(Mutex::new(transport::SessionState::default())))
        .manage(midi_input::new_handle())
        .manage(Arc::new(SplashGate {
            state: Mutex::new(SetupState {
                frontend_ready: false,
                backend_ready: false,
            }),
            ready: Condvar::new(),
        }))
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app data dir: {e}"))?;
            let database = Database::open(&data_dir).map_err(|e| e.to_string())?;

            // Warm active pad map into midi mapper (fast; before splash paints).
            if let Ok(Some(id)) = database.get_active_pad_map_id() {
                if let Ok(Some(profile)) = database.get_pad_map(&id) {
                    let midi = app.state::<MidiInputHandle>();
                    let _ = midi_input::set_active_profile(&midi, Some(profile));
                }
            }
            // Warm latency offset into session clock.
            if let Ok(ms) = database.get_latency_offset_ms() {
                let session = app.state::<AppSession>();
                let _ = transport::set_latency_offset(&session, ms);
            }

            let preferred_device = database.get_audio_device_id().ok().flatten();
            app.manage(DbState(database));

            // Heavy SoundFont / audio device init off the main thread so the
            // splash window can paint instead of looking frozen.
            let resource_dir = app.path().resource_dir().ok();
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("audio-init".into())
                .spawn(move || {
                    let init = (|| -> Result<AudioEngine, String> {
                        let sf_path = audio_engine::resolve_soundfont_path(resource_dir)?;
                        let engine = AudioEngine::create(sf_path)?;
                        engine.set_app_handle(handle.clone());

                        let opened = if let Some(id) = preferred_device {
                            match engine.open_device_by_id(&id) {
                                Ok(()) => Some(id),
                                Err(err) => {
                                    eprintln!(
                                        "audio device restore failed ({err}); using default"
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        if opened.is_none() {
                            let info = engine.open_default()?;
                            if let Some(db) = handle.try_state::<DbState>() {
                                let _ = db.0.set_audio_device_id(Some(&info.id));
                            }
                        }
                        Ok(engine)
                    })();

                    match init {
                        Ok(engine) => {
                            handle.manage(AudioEngineHandle(Arc::new(engine)));
                        }
                        Err(err) => {
                            eprintln!("audio init failed: {err}");
                            handle.exit(1);
                            return;
                        }
                    }

                    if let Some(gate) = handle.try_state::<Arc<SplashGate>>() {
                        if let Ok(mut s) = gate.state.lock() {
                            s.backend_ready = true;
                            maybe_reveal_main(&handle, &s);
                        }
                        gate.ready.notify_all();
                    }
                })
                .map_err(|e| format!("spawn audio-init: {e}"))?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            splash_frontend_ready,
            parse_midi,
            set_drum_track,
            get_expected_hits,
            list_midi_inputs,
            open_midi_input,
            close_midi_input,
            list_pad_maps,
            get_pad_map,
            save_pad_map,
            delete_pad_map,
            set_active_pad_map,
            get_active_pad_map,
            get_latency_offset_ms,
            set_latency_offset_ms,
            list_audio_devices,
            get_audio_device,
            set_audio_device,
            set_mute_drums,
            set_master_volume,
            set_track_volume,
            set_track_mute,
            set_track_solo,
            set_player_volume,
            set_player_muted,
            set_click_muted,
            set_play_player_drums,
            set_metronome_enabled,
            set_metronome_volume,
            audio_play_click,
            audio_start_click_train,
            audio_stop_click_train,
            get_transport_state,
            transport_play,
            transport_pause,
            transport_stop,
            transport_seek,
            transport_set_speed,
            transport_set_repeat,
            transport_set_loop_region,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
