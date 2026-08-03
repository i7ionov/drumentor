export type PadId =
  | "kick"
  | "snare"
  | "hiHatClosed"
  | "hiHatOpen"
  | "hiHatPedal"
  | "tomHigh"
  | "tomMid"
  | "floorTom"
  | "crash"
  | "ride"
  | "rideBell"
  | "splash";

/** Required for pad-map wizard + kit highlight (MVP). Mirrors docs/DOMAIN.md */
export const REQUIRED_PAD_IDS = [
  "kick",
  "snare",
  "hiHatClosed",
  "hiHatOpen",
  "tomHigh",
  "tomMid",
  "floorTom",
  "crash",
  "ride",
] as const satisfies readonly PadId[];

/** Skippable in wizard; still in GM map / enum */
export const OPTIONAL_PAD_IDS = [
  "hiHatPedal",
  "rideBell",
  "splash",
] as const satisfies readonly PadId[];

export type RequiredPadId = (typeof REQUIRED_PAD_IDS)[number];
export type OptionalPadId = (typeof OPTIONAL_PAD_IDS)[number];

export const KIT_PADS: { id: PadId; label: string; required: boolean }[] = [
  { id: "kick", label: "Kick", required: true },
  { id: "snare", label: "Snare", required: true },
  { id: "hiHatClosed", label: "HH Closed", required: true },
  { id: "hiHatOpen", label: "HH Open", required: true },
  { id: "tomHigh", label: "Tom High", required: true },
  { id: "tomMid", label: "Tom Mid", required: true },
  { id: "floorTom", label: "Floor Tom", required: true },
  { id: "crash", label: "Crash", required: true },
  { id: "ride", label: "Ride", required: true },
  { id: "hiHatPedal", label: "HH Pedal", required: false },
  { id: "rideBell", label: "Ride Bell", required: false },
  { id: "splash", label: "Splash", required: false },
];

/** Device pad map: many MIDI notes may bind to the same PadId */
export interface PadBinding {
  padId: PadId;
  midiNote: number;
  channel?: number;
}

export interface PadMapProfile {
  id: string;
  name: string;
  deviceNameHint?: string;
  schemaVersion: 1;
  bindings: PadBinding[];
  createdAt: string;
  updatedAt: string;
}

export interface IncomingHit {
  padId?: PadId;
  rawNote: number;
  rawChannel: number;
  velocity: number;
  timeMs: number;
}

export interface MidiNoteOnEvent {
  note: number;
  channel: number;
  velocity: number;
  timeMs: number;
  /** Monotonic ms from native audio engine epoch (latency calibration). */
  wallMs?: number;
}

export interface AudioDeviceInfo {
  id: string;
  name: string;
  host: string;
}

export interface AudioClickEvent {
  index: number;
  wallMs: number;
}

export interface TrackInfo {
  id: number;
  name: string;
  noteCount: number;
  isDrumCandidate: boolean;
  drumScore: number;
  /** Initial Main Volume (CC7) from the MIDI file, 0–127. */
  volume: number;
}

export interface SongSummary {
  path: string;
  trackCount: number;
  durationMs: number;
  barBoundariesMs: number[];
  tracks: TrackInfo[];
  suggestedDrumTrackId: number | null;
}

export interface MidiInputPort {
  id: string;
  name: string;
}

export interface AppInfo {
  name: string;
  version: string;
  phase: string;
}

export type TransportStatus = "stopped" | "playing" | "paused";

export interface LoopRegion {
  startMs: number;
  endMs: number;
}

export interface ExpectedHit {
  uid: number;
  padId: PadId;
  timeMs: number;
  velocity: number;
}

export interface PositionEvent {
  positionMs: number;
  status: TransportStatus;
  durationMs: number;
  speed: number;
  repeatEnabled: boolean;
  loopRegion?: LoopRegion;
}

export interface TransportState {
  status: TransportStatus;
  positionMs: number;
  durationMs: number;
  speed: number;
  repeatEnabled: boolean;
  loopRegion?: LoopRegion;
}

export interface HighlightEvent {
  padId: PadId;
  atMs: number;
  velocity: number;
  uid: number;
}

export type NoteRole = "drum" | "backing";

export interface ScheduleNote {
  uid: number;
  trackId: number;
  role: NoteRole;
  channel: number;
  program: number;
  note: number;
  velocity: number;
  whenMs: number;
  durationMs: number;
}

/** Scoring windows (ms) — docs/SCORING.md */
export const WINDOW_PERFECT_MS = 20;
export const WINDOW_GOOD_MS = 50;
export const WINDOW_OK_MS = 80;

export type Judgement =
  | "perfect"
  | "good"
  | "ok"
  | "miss"
  | "wrong"
  | "extra";

export interface JudgementEvent {
  expectedUid?: number;
  padId: PadId;
  judgement: Judgement;
  deltaMs?: number;
}

export interface HitCounts {
  perfect: number;
  good: number;
  ok: number;
  miss: number;
  wrong: number;
  extra: number;
}

export interface SessionSummary {
  id: string;
  songPath?: string;
  drumTrackId?: number;
  startedAt: string;
  endedAt: string;
  totalExpected: number;
  hitCounts: HitCounts;
  noteAccuracy: number;
  timingMeanMs?: number;
  timingAbsMeanMs?: number;
  scorePercent: number;
}

export const EMPTY_HIT_COUNTS: HitCounts = {
  perfect: 0,
  good: 0,
  ok: 0,
  miss: 0,
  wrong: 0,
  extra: 0,
};
