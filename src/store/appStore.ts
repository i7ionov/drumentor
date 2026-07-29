import { create } from "zustand";
import type {
  ExpectedHit,
  HitCounts,
  Judgement,
  JudgementEvent,
  MidiInputPort,
  AudioDeviceInfo,
  PadId,
  PadMapProfile,
  SessionSummary,
  SongSummary,
  TransportStatus,
} from "../domain/types";
import { EMPTY_HIT_COUNTS } from "../domain/types";

const HIT_LINE_STORAGE_KEY = "drumentor.hitLineY";
const METRONOME_ENABLED_KEY = "drumentor.metronomeEnabled";
const METRONOME_VOLUME_KEY = "drumentor.metronomeVolume";
const PLAY_PLAYER_DRUMS_KEY = "drumentor.playPlayerDrums";
const DEFAULT_HIT_LINE_Y = 0.85;
const HIT_LINE_MIN = 0.55;
const HIT_LINE_MAX = 0.95;
const DEFAULT_METRONOME_VOLUME = 0.7;

function loadHitLineY(): number {
  try {
    const raw = localStorage.getItem(HIT_LINE_STORAGE_KEY);
    if (raw == null) return DEFAULT_HIT_LINE_Y;
    const value = Number(raw);
    if (!Number.isFinite(value)) return DEFAULT_HIT_LINE_Y;
    return Math.min(HIT_LINE_MAX, Math.max(HIT_LINE_MIN, value));
  } catch {
    return DEFAULT_HIT_LINE_Y;
  }
}

function loadMetronomeEnabled(): boolean {
  try {
    return localStorage.getItem(METRONOME_ENABLED_KEY) === "1";
  } catch {
    return false;
  }
}

function loadMetronomeVolume(): number {
  try {
    const raw = localStorage.getItem(METRONOME_VOLUME_KEY);
    if (raw == null) return DEFAULT_METRONOME_VOLUME;
    const value = Number(raw);
    if (!Number.isFinite(value)) return DEFAULT_METRONOME_VOLUME;
    return Math.min(1, Math.max(0, value));
  } catch {
    return DEFAULT_METRONOME_VOLUME;
  }
}

function loadPlayPlayerDrums(): boolean {
  try {
    const raw = localStorage.getItem(PLAY_PLAYER_DRUMS_KEY);
    if (raw == null) return true;
    return raw === "1";
  } catch {
    return true;
  }
}

export function clampHitLineY(y: number): number {
  return Math.min(HIT_LINE_MAX, Math.max(HIT_LINE_MIN, y));
}

interface AppState {
  song: SongSummary | null;
  selectedDrumTrackId: number | null;
  transportStatus: TransportStatus;
  positionMs: number;
  muteDrums: boolean;
  playPlayerDrums: boolean;
  metronomeEnabled: boolean;
  metronomeVolume: number;
  activePads: Set<PadId>;
  padJudgements: Map<PadId, Judgement>;
  expectedHits: ExpectedHit[];
  hitLineY: number;
  midiPorts: MidiInputPort[];
  selectedMidiPortId: string | null;
  audioDevices: AudioDeviceInfo[];
  selectedAudioDeviceId: string | null;
  padMapProfiles: PadMapProfile[];
  activePadMap: PadMapProfile | null;
  wizardOpen: boolean;
  wizardTargetPad: PadId | null;
  latencyWizardOpen: boolean;
  statsOpen: boolean;
  liveHitCounts: HitCounts;
  lastJudgement: JudgementEvent | null;
  sessionSummary: SessionSummary | null;
  latencyOffsetMs: number;
  playbackSpeed: number;
  error: string | null;
  setSong: (song: SongSummary | null) => void;
  setSelectedDrumTrackId: (id: number | null) => void;
  setTransportStatus: (status: TransportStatus) => void;
  setPositionMs: (ms: number) => void;
  setExpectedHits: (hits: ExpectedHit[]) => void;
  setHitLineY: (y: number) => void;
  toggleMuteDrums: () => void;
  setPlayPlayerDrums: (enabled: boolean) => void;
  setMetronomeEnabled: (enabled: boolean) => void;
  setMetronomeVolume: (volume: number) => void;
  flashPad: (pad: PadId) => void;
  flashJudgement: (pad: PadId, judgement: Judgement) => void;
  setMidiPorts: (ports: MidiInputPort[]) => void;
  setSelectedMidiPortId: (id: string | null) => void;
  setAudioDevices: (devices: AudioDeviceInfo[]) => void;
  setSelectedAudioDeviceId: (id: string | null) => void;
  setPadMapProfiles: (profiles: PadMapProfile[]) => void;
  setActivePadMap: (profile: PadMapProfile | null) => void;
  setWizardOpen: (open: boolean) => void;
  setWizardTargetPad: (pad: PadId | null) => void;
  setLatencyWizardOpen: (open: boolean) => void;
  setStatsOpen: (open: boolean) => void;
  setLiveHitCounts: (counts: HitCounts) => void;
  setLastJudgement: (event: JudgementEvent | null) => void;
  setSessionSummary: (summary: SessionSummary | null) => void;
  setLatencyOffsetMs: (ms: number) => void;
  setPlaybackSpeed: (speed: number) => void;
  resetLiveScore: () => void;
  setError: (error: string | null) => void;
}

export const useAppStore = create<AppState>((set, get) => ({
  song: null,
  selectedDrumTrackId: null,
  transportStatus: "stopped",
  positionMs: 0,
  muteDrums: false,
  playPlayerDrums: loadPlayPlayerDrums(),
  metronomeEnabled: loadMetronomeEnabled(),
  metronomeVolume: loadMetronomeVolume(),
  activePads: new Set(),
  padJudgements: new Map(),
  expectedHits: [],
  hitLineY: loadHitLineY(),
  midiPorts: [],
  selectedMidiPortId: null,
  audioDevices: [],
  selectedAudioDeviceId: null,
  padMapProfiles: [],
  activePadMap: null,
  wizardOpen: false,
  wizardTargetPad: null,
  latencyWizardOpen: false,
  statsOpen: false,
  liveHitCounts: { ...EMPTY_HIT_COUNTS },
  lastJudgement: null,
  sessionSummary: null,
  latencyOffsetMs: 0,
  playbackSpeed: 1,
  error: null,
  setSong: (song) =>
    set({
      song,
      selectedDrumTrackId: song?.suggestedDrumTrackId ?? null,
      transportStatus: "stopped",
      positionMs: 0,
      expectedHits: [],
      liveHitCounts: { ...EMPTY_HIT_COUNTS },
      lastJudgement: null,
      sessionSummary: null,
      statsOpen: false,
      error: null,
    }),
  setSelectedDrumTrackId: (id) => set({ selectedDrumTrackId: id }),
  setTransportStatus: (status) => set({ transportStatus: status }),
  setPositionMs: (ms) => set({ positionMs: ms }),
  setExpectedHits: (hits) => set({ expectedHits: hits }),
  setHitLineY: (y) => {
    const hitLineY = clampHitLineY(y);
    try {
      localStorage.setItem(HIT_LINE_STORAGE_KEY, String(hitLineY));
    } catch {
      /* ignore quota / private mode */
    }
    set({ hitLineY });
  },
  toggleMuteDrums: () => set({ muteDrums: !get().muteDrums }),
  setPlayPlayerDrums: (enabled) => {
    try {
      localStorage.setItem(PLAY_PLAYER_DRUMS_KEY, enabled ? "1" : "0");
    } catch {
      /* ignore quota / private mode */
    }
    set({ playPlayerDrums: enabled });
  },
  setMetronomeEnabled: (enabled) => {
    try {
      localStorage.setItem(METRONOME_ENABLED_KEY, enabled ? "1" : "0");
    } catch {
      /* ignore quota / private mode */
    }
    set({ metronomeEnabled: enabled });
  },
  setMetronomeVolume: (volume) => {
    const metronomeVolume = Math.min(1, Math.max(0, volume));
    try {
      localStorage.setItem(METRONOME_VOLUME_KEY, String(metronomeVolume));
    } catch {
      /* ignore quota / private mode */
    }
    set({ metronomeVolume });
  },
  flashPad: (pad) => {
    const next = new Set(get().activePads);
    next.add(pad);
    set({ activePads: next });
    window.setTimeout(() => {
      const cleared = new Set(get().activePads);
      cleared.delete(pad);
      set({ activePads: cleared });
    }, 160);
  },
  flashJudgement: (pad, judgement) => {
    const nextPads = new Set(get().activePads);
    nextPads.add(pad);
    const nextJudgements = new Map(get().padJudgements);
    nextJudgements.set(pad, judgement);
    set({ activePads: nextPads, padJudgements: nextJudgements });
    window.setTimeout(() => {
      const clearedPads = new Set(get().activePads);
      clearedPads.delete(pad);
      const clearedJudgements = new Map(get().padJudgements);
      clearedJudgements.delete(pad);
      set({ activePads: clearedPads, padJudgements: clearedJudgements });
    }, 220);
  },
  setMidiPorts: (ports) => set({ midiPorts: ports }),
  setSelectedMidiPortId: (id) => set({ selectedMidiPortId: id }),
  setAudioDevices: (devices) => set({ audioDevices: devices }),
  setSelectedAudioDeviceId: (id) => set({ selectedAudioDeviceId: id }),
  setPadMapProfiles: (profiles) => set({ padMapProfiles: profiles }),
  setActivePadMap: (profile) => set({ activePadMap: profile }),
  setWizardOpen: (open) =>
    set({
      wizardOpen: open,
      wizardTargetPad: open ? get().wizardTargetPad : null,
      // One modal at a time.
      latencyWizardOpen: open ? false : get().latencyWizardOpen,
      statsOpen: open ? false : get().statsOpen,
    }),
  setWizardTargetPad: (pad) => set({ wizardTargetPad: pad }),
  setLatencyWizardOpen: (open) =>
    set({
      latencyWizardOpen: open,
      wizardOpen: open ? false : get().wizardOpen,
      wizardTargetPad: open ? null : get().wizardTargetPad,
      statsOpen: open ? false : get().statsOpen,
    }),
  setStatsOpen: (open) =>
    set({
      statsOpen: open,
      wizardOpen: open ? false : get().wizardOpen,
      wizardTargetPad: open ? null : get().wizardTargetPad,
      latencyWizardOpen: open ? false : get().latencyWizardOpen,
    }),
  setLiveHitCounts: (counts) => set({ liveHitCounts: counts }),
  setLastJudgement: (event) => set({ lastJudgement: event }),
  setSessionSummary: (summary) =>
    set({
      sessionSummary: summary,
      ...(summary != null ? { statsOpen: true } : {}),
    }),
  setLatencyOffsetMs: (ms) => set({ latencyOffsetMs: ms }),
  setPlaybackSpeed: (speed) => set({ playbackSpeed: speed }),
  resetLiveScore: () =>
    set({
      liveHitCounts: { ...EMPTY_HIT_COUNTS },
      lastJudgement: null,
    }),
  setError: (error) => set({ error }),
}));
