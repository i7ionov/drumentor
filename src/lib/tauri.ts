import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AppInfo,
  AudioClickEvent,
  AudioDeviceInfo,
  ExpectedHit,
  HighlightEvent,
  HitCounts,
  IncomingHit,
  JudgementEvent,
  MidiInputPort,
  MidiNoteOnEvent,
  PadMapProfile,
  PositionEvent,
  SessionSummary,
  SongSummary,
  TransportState,
  TransportStatus,
} from "../domain/types";
import { useAppStore } from "../store/appStore";

export const PLAYBACK_SPEED_MIN = 0.1;
export const PLAYBACK_SPEED_MAX = 1;
export const PLAYBACK_SPEED_STEP = 0.05;

export async function loadAppInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("get_app_info");
}

/** Close splash once backend + frontend are both ready (no-op in browser). */
export async function signalSplashReady(): Promise<void> {
  try {
    await invoke("splash_frontend_ready");
  } catch {
    // Browser / non-Tauri preview — no splash window.
  }
}

export async function fetchExpectedHits(): Promise<void> {
  const { setExpectedHits, setError } = useAppStore.getState();
  try {
    const hits = await invoke<ExpectedHit[]>("get_expected_hits");
    setExpectedHits(hits);
  } catch (e) {
    setExpectedHits([]);
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function refreshMidiInputs(): Promise<void> {
  const { setMidiPorts, setError } = useAppStore.getState();
  try {
    const ports = await invoke<MidiInputPort[]>("list_midi_inputs");
    setMidiPorts(ports);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function selectMidiInput(portId: string | null): Promise<void> {
  const { setSelectedMidiPortId, setError } = useAppStore.getState();
  try {
    if (portId == null || portId === "") {
      await invoke("close_midi_input");
      setSelectedMidiPortId(null);
      return;
    }
    await invoke("open_midi_input", { portId });
    setSelectedMidiPortId(portId);
  } catch (e) {
    setSelectedMidiPortId(null);
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function refreshAudioDevices(): Promise<void> {
  const { setAudioDevices, setSelectedAudioDeviceId, setError } =
    useAppStore.getState();
  try {
    const [devices, currentId] = await Promise.all([
      invoke<AudioDeviceInfo[]>("list_audio_devices"),
      invoke<string | null>("get_audio_device"),
    ]);
    setAudioDevices(devices);
    setSelectedAudioDeviceId(currentId);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function selectAudioDevice(deviceId: string): Promise<void> {
  const { setSelectedAudioDeviceId, setError } = useAppStore.getState();
  try {
    await invoke("set_audio_device", { id: deviceId });
    setSelectedAudioDeviceId(deviceId);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function setMuteDrums(muted: boolean): Promise<void> {
  const { setError } = useAppStore.getState();
  try {
    await invoke("set_mute_drums", { muted });
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function setPlayPlayerDrums(enabled: boolean): Promise<void> {
  const { setPlayPlayerDrums: setLocal, setError } = useAppStore.getState();
  setLocal(enabled);
  try {
    await invoke("set_play_player_drums", { enabled });
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function setMetronomeEnabled(enabled: boolean): Promise<void> {
  const { setMetronomeEnabled: setLocal, setError } = useAppStore.getState();
  setLocal(enabled);
  try {
    await invoke("set_metronome_enabled", { enabled });
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function setMetronomeVolume(volume: number): Promise<void> {
  const { setMetronomeVolume: setLocal, setError } = useAppStore.getState();
  setLocal(volume);
  try {
    await invoke("set_metronome_volume", { volume });
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

/** Push persisted audio prefs into the native engine (call once on startup). */
export async function syncMetronomePrefs(): Promise<void> {
  const { metronomeEnabled, metronomeVolume, playPlayerDrums, setError } =
    useAppStore.getState();
  try {
    await invoke("set_metronome_volume", { volume: metronomeVolume });
    await invoke("set_metronome_enabled", { enabled: metronomeEnabled });
    await invoke("set_play_player_drums", { enabled: playPlayerDrums });
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function refreshPadMaps(): Promise<void> {
  const { setPadMapProfiles, setActivePadMap, setError } = useAppStore.getState();
  try {
    const [profiles, active] = await Promise.all([
      invoke<PadMapProfile[]>("list_pad_maps"),
      invoke<PadMapProfile | null>("get_active_pad_map"),
    ]);
    setPadMapProfiles(profiles);
    setActivePadMap(active);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function savePadMap(profile: PadMapProfile): Promise<PadMapProfile> {
  const saved = await invoke<PadMapProfile>("save_pad_map", { profile });
  await refreshPadMaps();
  return saved;
}

export async function deletePadMap(id: string): Promise<void> {
  await invoke("delete_pad_map", { id });
  await refreshPadMaps();
}

export async function setActivePadMap(id: string | null): Promise<void> {
  const { setActivePadMap: setActive, setError } = useAppStore.getState();
  try {
    const profile = await invoke<PadMapProfile | null>("set_active_pad_map", { id });
    setActive(profile);
    await refreshPadMaps();
    await fetchExpectedHits();
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

/** Subscribe to wizard capture; returns unsubscribe. */
export async function listenMidiNoteOn(
  handler: (event: MidiNoteOnEvent) => void,
): Promise<() => void> {
  return listen<MidiNoteOnEvent>("midi:noteOn", (event) => {
    handler(event.payload);
  });
}

export async function listenAudioClicks(
  handler: (event: AudioClickEvent) => void,
): Promise<() => void> {
  return listen<AudioClickEvent>("audio:click", (event) => {
    handler(event.payload);
  });
}

export async function startLatencyClickTrain(
  count: number,
  intervalMs: number,
  leadInMs: number,
): Promise<void> {
  await invoke("audio_start_click_train", {
    count,
    intervalMs,
    leadInMs,
  });
}

export async function stopLatencyClickTrain(): Promise<void> {
  await invoke("audio_stop_click_train");
}

export async function openMidiFile(): Promise<void> {
  const {
    setSong,
    setError,
    setSelectedDrumTrackId,
    setExpectedHits,
    resetLiveScore,
    setSessionSummary,
  } = useAppStore.getState();
  try {
    const selected = await open({
      multiple: false,
      filters: [{ name: "MIDI", extensions: ["mid", "midi"] }],
    });
    if (!selected || Array.isArray(selected)) {
      return;
    }
    const song = await invoke<SongSummary>("parse_midi", { path: selected });
    setSong(song);
    resetLiveScore();
    setSessionSummary(null);
    const trackId = song.suggestedDrumTrackId;
    if (trackId != null) {
      await invoke("set_drum_track", { trackId });
      setSelectedDrumTrackId(trackId);
      await fetchExpectedHits();
    } else {
      setExpectedHits([]);
    }
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export const LATENCY_OFFSET_MIN = -200;
export const LATENCY_OFFSET_MAX = 200;

export function clampLatencyOffsetMs(ms: number): number {
  if (!Number.isFinite(ms)) return 0;
  return Math.min(
    LATENCY_OFFSET_MAX,
    Math.max(LATENCY_OFFSET_MIN, Math.round(ms)),
  );
}

export async function loadLatencyOffset(): Promise<void> {
  const { setLatencyOffsetMs, setError } = useAppStore.getState();
  try {
    const ms = await invoke<number>("get_latency_offset_ms");
    setLatencyOffsetMs(clampLatencyOffsetMs(ms));
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

/** Persist latency offset (ms) to SQLite + live transport clock. */
export async function setLatencyOffset(ms: number): Promise<void> {
  const { setLatencyOffsetMs, setError } = useAppStore.getState();
  const clamped = clampLatencyOffsetMs(ms);
  // Optimistic UI so the slider stays snappy.
  setLatencyOffsetMs(clamped);
  try {
    await invoke("set_latency_offset_ms", { ms: clamped });
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function setDrumTrack(trackId: number): Promise<void> {
  const { setSelectedDrumTrackId, setError, setTransportStatus, setPositionMs } =
    useAppStore.getState();
  try {
    await invoke("set_drum_track", { trackId });
    setSelectedDrumTrackId(trackId);
    setTransportStatus("stopped");
    setPositionMs(0);
    await fetchExpectedHits();
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

function applyTransportSnapshot(state: {
  status: TransportStatus;
  positionMs: number;
  speed?: number;
}): void {
  const { setTransportStatus, setPositionMs, setPlaybackSpeed } =
    useAppStore.getState();
  setTransportStatus(state.status);
  setPositionMs(state.positionMs);
  if (state.speed != null && Number.isFinite(state.speed)) {
    setPlaybackSpeed(state.speed);
  }
}

export async function transportPlay(): Promise<void> {
  const {
    setError,
    transportStatus,
    resetLiveScore,
    setSessionSummary,
  } = useAppStore.getState();
  try {
    // Fresh run from stopped clears live counters (resume from pause keeps them).
    if (transportStatus === "stopped") {
      resetLiveScore();
      setSessionSummary(null);
    }
    const state = await invoke<TransportState>("transport_play");
    applyTransportSnapshot(state);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function transportPause(): Promise<void> {
  const { setError } = useAppStore.getState();
  try {
    const state = await invoke<TransportState>("transport_pause");
    applyTransportSnapshot(state);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function transportStop(): Promise<void> {
  const { setError } = useAppStore.getState();
  try {
    const state = await invoke<TransportState>("transport_stop");
    applyTransportSnapshot(state);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function transportSeek(positionMs: number): Promise<void> {
  const { setError } = useAppStore.getState();
  try {
    const state = await invoke<TransportState>("transport_seek", { positionMs });
    applyTransportSnapshot(state);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function transportSetSpeed(speed: number): Promise<void> {
  const { setError, setPlaybackSpeed } = useAppStore.getState();
  const clamped = Math.min(
    PLAYBACK_SPEED_MAX,
    Math.max(PLAYBACK_SPEED_MIN, Math.round(speed * 100) / 100),
  );

  // Optimistic UI so the control always responds.
  setPlaybackSpeed(clamped);

  try {
    const state = await invoke<TransportState>("transport_set_speed", {
      speed: clamped,
    });
    applyTransportSnapshot(state);
  } catch (e) {
    setError(e instanceof Error ? e.message : String(e));
  }
}

export async function startTransportListeners(): Promise<() => void> {
  const unlisteners: UnlistenFn[] = [];

  unlisteners.push(
    await listen<PositionEvent>("transport:position", (event) => {
      const { positionMs, status } = event.payload;
      // Do not sync speed from the high-frequency ticker — it races with
      // optimistic UI updates and can snap the control back to 100%.
      applyTransportSnapshot({ status, positionMs });
      const song = useAppStore.getState().song;
      if (song && event.payload.durationMs !== song.durationMs) {
        useAppStore.setState({
          song: { ...song, durationMs: event.payload.durationMs },
        });
      }
    }),
  );

  unlisteners.push(
    await listen<HighlightEvent>("transport:highlight", (event) => {
      useAppStore.getState().flashPad(event.payload.padId);
    }),
  );

  unlisteners.push(
    await listen<IncomingHit>("midi:incomingHit", (event) => {
      const { padId } = event.payload;
      if (!padId) return;
      // While playing, score:judgement owns the flash (colored).
      if (useAppStore.getState().transportStatus === "playing") return;
      useAppStore.getState().flashPad(padId);
    }),
  );

  unlisteners.push(
    await listen<JudgementEvent>("score:judgement", (event) => {
      const { padId, judgement } = event.payload;
      const store = useAppStore.getState();
      store.setLastJudgement(event.payload);
      store.flashJudgement(padId, judgement);
    }),
  );

  unlisteners.push(
    await listen<HitCounts>("score:liveCounts", (event) => {
      useAppStore.getState().setLiveHitCounts(event.payload);
    }),
  );

  unlisteners.push(
    await listen<SessionSummary>("score:sessionSummary", (event) => {
      useAppStore.getState().setSessionSummary(event.payload);
      useAppStore.getState().setLiveHitCounts(event.payload.hitCounts);
    }),
  );

  return () => {
    for (const unlisten of unlisteners) {
      unlisten();
    }
  };
}
