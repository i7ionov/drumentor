import { useEffect, useRef, useState, type PointerEvent } from "react";
import styles from "./TransportBar.module.css";
import {
  LATENCY_OFFSET_MAX,
  LATENCY_OFFSET_MIN,
  openMidiFile,
  PLAYBACK_SPEED_MAX,
  PLAYBACK_SPEED_MIN,
  PLAYBACK_SPEED_STEP,
  refreshAudioDevices,
  refreshMidiInputs,
  selectAudioDevice,
  selectMidiInput,
  setActivePadMap,
  setDrumTrack,
  setLatencyOffset,
  setMasterVolume,
  setMuteDrums,
  setPlayPlayerDrums,
  setMetronomeEnabled,
  transportPause,
  transportPlay,
  transportSeek,
  transportSetLoopRegion,
  transportSetRepeat,
  transportSetSpeed,
  transportStop,
} from "../lib/tauri";
import type { LoopRegion } from "../domain/types";
import { useAppStore, VOLUME_MAX, VOLUME_MIN } from "../store/appStore";

const SEEK_STEP_MS = 1000;

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    target.isContentEditable
  );
}

function formatMs(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

function formatSpeedPercent(speed: number): string {
  return `${Math.round(speed * 100)}%`;
}

function formatOffset(ms: number): string {
  const sign = ms > 0 ? "+" : "";
  return `${sign}${ms} ms`;
}

function liveAccuracy(counts: {
  perfect: number;
  good: number;
  ok: number;
  miss: number;
}): string | null {
  const total = counts.perfect + counts.good + counts.ok + counts.miss;
  if (total === 0) return null;
  const hit = counts.perfect + counts.good + counts.ok;
  return `${Math.round((100 * hit) / total)}%`;
}

export function TransportBar() {
  const song = useAppStore((s) => s.song);
  const transportStatus = useAppStore((s) => s.transportStatus);
  const positionMs = useAppStore((s) => s.positionMs);
  const playbackSpeed = useAppStore((s) => s.playbackSpeed);
  const repeatEnabled = useAppStore((s) => s.repeatEnabled);
  const loopRegion = useAppStore((s) => s.loopRegion);
  const muteDrums = useAppStore((s) => s.muteDrums);
  const playPlayerDrums = useAppStore((s) => s.playPlayerDrums);
  const metronomeEnabled = useAppStore((s) => s.metronomeEnabled);
  const masterVolume = useAppStore((s) => s.masterVolume);
  const midiPorts = useAppStore((s) => s.midiPorts);
  const selectedMidiPortId = useAppStore((s) => s.selectedMidiPortId);
  const audioDevices = useAppStore((s) => s.audioDevices);
  const selectedAudioDeviceId = useAppStore((s) => s.selectedAudioDeviceId);
  const selectedDrumTrackId = useAppStore((s) => s.selectedDrumTrackId);
  const padMapProfiles = useAppStore((s) => s.padMapProfiles);
  const activePadMap = useAppStore((s) => s.activePadMap);
  const liveHitCounts = useAppStore((s) => s.liveHitCounts);
  const latencyOffsetMs = useAppStore((s) => s.latencyOffsetMs);
  const wizardOpen = useAppStore((s) => s.wizardOpen);
  const latencyWizardOpen = useAppStore((s) => s.latencyWizardOpen);
  const statsOpen = useAppStore((s) => s.statsOpen);
  const setWizardOpen = useAppStore((s) => s.setWizardOpen);
  const setLatencyWizardOpen = useAppStore((s) => s.setLatencyWizardOpen);
  const setStatsOpen = useAppStore((s) => s.setStatsOpen);

  const [scrubbing, setScrubbing] = useState(false);
  const [scrubMs, setScrubMs] = useState(0);
  const [draftRegion, setDraftRegion] = useState<LoopRegion | null>(null);
  const seekTimer = useRef<number | null>(null);
  const latencyTimer = useRef<number | null>(null);
  const masterVolTimer = useRef<number | null>(null);
  const rulerRef = useRef<HTMLDivElement>(null);
  const dragKind = useRef<"create" | "start" | "end" | null>(null);
  const dragAnchor = useRef(0);
  const draftRegionRef = useRef<LoopRegion | null>(null);

  const duration = song?.durationMs ?? 0;
  const canPlay = Boolean(song) && selectedDrumTrackId != null;
  const displayMs = scrubbing ? scrubMs : positionMs;
  const accuracy = liveAccuracy(liveHitCounts);
  const judged =
    liveHitCounts.perfect +
    liveHitCounts.good +
    liveHitCounts.ok +
    liveHitCounts.miss;
  const canDecreaseSpeed = playbackSpeed > PLAYBACK_SPEED_MIN + 1e-9;
  const canIncreaseSpeed = playbackSpeed < PLAYBACK_SPEED_MAX - 1e-9;
  const modalOpen = wizardOpen || latencyWizardOpen || statsOpen;
  const bars = song?.barBoundariesMs ?? [];
  const shownRegion = draftRegion ?? loopRegion;

  useEffect(() => {
    setDraftRegion(null);
    draftRegionRef.current = null;
  }, [loopRegion?.startMs, loopRegion?.endMs]);

  const nearestBarBoundary = (clientX: number): number => {
    const ruler = rulerRef.current;
    if (!ruler || bars.length === 0) return 0;
    const rect = ruler.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
    const raw = ratio * duration;
    return bars.reduce((best, boundary) =>
      Math.abs(boundary - raw) < Math.abs(best - raw) ? boundary : best,
    );
  };

  const setDraft = (region: LoopRegion | null) => {
    draftRegionRef.current = region;
    setDraftRegion(region);
  };

  const updateRegionDrag = (clientX: number) => {
    const boundary = nearestBarBoundary(clientX);
    const kind = dragKind.current;
    if (!kind || bars.length < 2) return;
    if (kind === "create") {
      const a = dragAnchor.current;
      if (a === boundary) {
        const index = Math.max(0, bars.indexOf(a));
        const end = bars[Math.min(index + 1, bars.length - 1)];
        const start = end === a ? bars[Math.max(0, index - 1)] : a;
        setDraft(start < end ? { startMs: start, endMs: end } : null);
      } else {
        setDraft({
          startMs: Math.min(a, boundary),
          endMs: Math.max(a, boundary),
        });
      }
      return;
    }
    const current = draftRegionRef.current ?? loopRegion;
    if (!current) return;
    if (kind === "start" && boundary < current.endMs) {
      setDraft({ ...current, startMs: boundary });
    } else if (kind === "end" && boundary > current.startMs) {
      setDraft({ ...current, endMs: boundary });
    }
  };

  const beginRegionDrag = (
    kind: "create" | "start" | "end",
    event: PointerEvent<HTMLElement>,
  ) => {
    if (!song || bars.length < 2) return;
    event.preventDefault();
    event.stopPropagation();
    dragKind.current = kind;
    event.currentTarget.setPointerCapture(event.pointerId);
    if (kind === "create") {
      dragAnchor.current = nearestBarBoundary(event.clientX);
    } else {
      setDraft(loopRegion);
    }
    updateRegionDrag(event.clientX);
  };

  const finishRegionDrag = (event: PointerEvent<HTMLElement>) => {
    if (!dragKind.current) return;
    updateRegionDrag(event.clientX);
    dragKind.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    const next = draftRegionRef.current;
    if (next) void transportSetLoopRegion(next);
  };

  const nudgeLoopBoundary = (edge: "start" | "end", delta: number) => {
    if (!loopRegion || bars.length < 2) return;
    const value = edge === "start" ? loopRegion.startMs : loopRegion.endMs;
    const index = Math.max(0, bars.indexOf(value));
    const nextIndex = Math.min(bars.length - 1, Math.max(0, index + delta));
    const boundary = bars[nextIndex];
    const next =
      edge === "start"
        ? boundary < loopRegion.endMs
          ? { ...loopRegion, startMs: boundary }
          : null
        : boundary > loopRegion.startMs
          ? { ...loopRegion, endMs: boundary }
          : null;
    if (next) void transportSetLoopRegion(next);
  };

  const onSeekInput = (value: number) => {
    setScrubbing(true);
    setScrubMs(value);
    if (seekTimer.current != null) {
      window.clearTimeout(seekTimer.current);
    }
    seekTimer.current = window.setTimeout(() => {
      void transportSeek(value).finally(() => {
        setScrubbing(false);
      });
    }, 80);
  };

  const onLatencyInput = (value: number) => {
    useAppStore.getState().setLatencyOffsetMs(value);
    if (latencyTimer.current != null) {
      window.clearTimeout(latencyTimer.current);
    }
    latencyTimer.current = window.setTimeout(() => {
      void setLatencyOffset(value);
    }, 120);
  };

  const onMasterVolumeInput = (value: number) => {
    useAppStore.getState().setMasterVolume(value);
    if (masterVolTimer.current != null) {
      window.clearTimeout(masterVolTimer.current);
    }
    masterVolTimer.current = window.setTimeout(() => {
      void setMasterVolume(value);
    }, 80);
  };

  const nudgeSpeed = (delta: number) => {
    const current = useAppStore.getState().playbackSpeed;
    const next = Math.round((current + delta) * 100) / 100;
    void transportSetSpeed(next);
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.repeat || e.metaKey || e.ctrlKey || e.altKey) return;
      if (modalOpen || isTypingTarget(e.target)) return;

      if (e.code === "Space") {
        if (!canPlay) return;
        e.preventDefault();
        const { transportStatus } = useAppStore.getState();
        void (transportStatus === "playing" ? transportPause() : transportPlay());
        return;
      }

      if (e.code === "ArrowUp" || e.code === "ArrowDown") {
        e.preventDefault();
        const delta =
          e.code === "ArrowUp" ? PLAYBACK_SPEED_STEP : -PLAYBACK_SPEED_STEP;
        const current = useAppStore.getState().playbackSpeed;
        const next = Math.round((current + delta) * 100) / 100;
        void transportSetSpeed(next);
        return;
      }

      if (e.code === "ArrowLeft" || e.code === "ArrowRight") {
        const { song: currentSong, positionMs: pos } = useAppStore.getState();
        if (!currentSong || currentSong.durationMs <= 0) return;
        e.preventDefault();
        const delta = e.code === "ArrowRight" ? SEEK_STEP_MS : -SEEK_STEP_MS;
        const next = Math.min(
          currentSong.durationMs,
          Math.max(0, pos + delta),
        );
        void transportSeek(next);
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [canPlay, modalOpen]);

  useEffect(() => {
    return () => {
      if (latencyTimer.current != null) {
        window.clearTimeout(latencyTimer.current);
      }
      if (masterVolTimer.current != null) {
        window.clearTimeout(masterVolTimer.current);
      }
    };
  }, []);

  return (
    <header className={styles.bar}>
      <div className={styles.brand}>
        <span className={styles.logo}>Drumentor</span>
        <span className={styles.tag}>learn · play · measure</span>
      </div>

      <div className={styles.controls}>
        <button type="button" className={styles.btn} onClick={() => void openMidiFile()}>
          Open MIDI
        </button>
        <button
          type="button"
          className={styles.btn}
          disabled={!canPlay}
          onClick={() =>
            void (transportStatus === "playing" ? transportPause() : transportPlay())
          }
        >
          {transportStatus === "playing" ? "Pause" : "Play"}
        </button>
        <button
          type="button"
          className={styles.btn}
          disabled={!song}
          onClick={() => void transportStop()}
        >
          Stop
        </button>
        <button
          type="button"
          className={repeatEnabled ? styles.btnActive : styles.btn}
          disabled={!song}
          aria-pressed={repeatEnabled}
          title={loopRegion ? "Repeat selected bars" : "Repeat whole song"}
          onClick={() => void transportSetRepeat(!repeatEnabled)}
        >
          Repeat
        </button>
        <div className={styles.speed} role="group" aria-label="Playback speed">
          <button
            type="button"
            className={styles.speedBtn}
            disabled={!canIncreaseSpeed}
            aria-label="Increase playback speed"
            title="Faster (Arrow Up)"
            onClick={() => nudgeSpeed(PLAYBACK_SPEED_STEP)}
          >
            ▲
          </button>
          <span className={styles.speedValue} title="Playback speed">
            {formatSpeedPercent(playbackSpeed)}
          </span>
          <button
            type="button"
            className={styles.speedBtn}
            disabled={!canDecreaseSpeed}
            aria-label="Decrease playback speed"
            title="Slower (Arrow Down)"
            onClick={() => nudgeSpeed(-PLAYBACK_SPEED_STEP)}
          >
            ▼
          </button>
        </div>
        <button
          type="button"
          className={muteDrums ? styles.btnActive : styles.btn}
          disabled={selectedDrumTrackId == null}
          title="Mute the selected drum track"
          onClick={() => void setMuteDrums(!muteDrums)}
        >
          Mute Drums
        </button>
        <button
          type="button"
          className={playPlayerDrums ? styles.btnActive : styles.btn}
          title="Play your mapped pad hits through SoundFont"
          onClick={() => void setPlayPlayerDrums(!playPlayerDrums)}
        >
          Player Kit
        </button>
        <button
          type="button"
          className={metronomeEnabled ? styles.btnActive : styles.btn}
          title="Click on quarter notes (follows MIDI tempo)"
          onClick={() => void setMetronomeEnabled(!metronomeEnabled)}
        >
          Metronome
        </button>
        <label className={styles.metroVol} title="Master volume (0–127, unity 100)">
          <span className={styles.metroVolLabel}>Master</span>
          <input
            className={styles.metroVolSlider}
            type="range"
            min={VOLUME_MIN}
            max={VOLUME_MAX}
            step={1}
            value={masterVolume}
            aria-label="Master volume"
            onChange={(e) => onMasterVolumeInput(Number(e.target.value))}
          />
          <span className={styles.metroVolValue}>
            {Math.round(masterVolume)}%
          </span>
        </label>
      </div>

      <div className={styles.meta}>
        <span className={styles.time}>
          {formatMs(displayMs)} / {formatMs(duration)}
        </span>
        {!repeatEnabled && accuracy != null && (
          <button
            type="button"
            className={styles.liveScore}
            title="Open score details"
            onClick={() => setStatsOpen(true)}
          >
            {accuracy}
            <span className={styles.liveDetail}>
              {" "}
              · {liveHitCounts.perfect}P {liveHitCounts.good}G {liveHitCounts.ok}O{" "}
              {liveHitCounts.miss}M
              {liveHitCounts.wrong > 0 ? ` ${liveHitCounts.wrong}W` : ""}
              {judged > 0 ? ` / ${judged}` : ""}
            </span>
          </button>
        )}
        <span className={styles.song}>{song ? song.path : "No file loaded"}</span>
      </div>

      <div className={styles.seekRow}>
        <div className={styles.timeline}>
          <div className={styles.loopMeta}>
            <span>
              {shownRegion
                ? `Bars ${Math.max(1, bars.indexOf(shownRegion.startMs) + 1)}–${Math.max(
                    1,
                    bars.indexOf(shownRegion.endMs),
                  )}`
                : "Whole song"}
            </span>
            {loopRegion && (
              <button
                type="button"
                className={styles.clearLoop}
                onClick={() => void transportSetLoopRegion(null)}
              >
                Clear region
              </button>
            )}
          </div>
          <div
            ref={rulerRef}
            className={styles.ruler}
            aria-label="Bar loop selection"
            onPointerDown={(event) => beginRegionDrag("create", event)}
            onPointerMove={(event) => {
              if (dragKind.current) updateRegionDrag(event.clientX);
            }}
            onPointerUp={finishRegionDrag}
            onPointerCancel={finishRegionDrag}
          >
            {bars.slice(1, -1).map((boundary) => (
              <span
                key={boundary}
                className={styles.barTick}
                style={{ left: `${(boundary / Math.max(duration, 1)) * 100}%` }}
                aria-hidden
              />
            ))}
            {shownRegion && (
              <>
                <span
                  className={styles.loopSelection}
                  style={{
                    left: `${(shownRegion.startMs / Math.max(duration, 1)) * 100}%`,
                    width: `${
                      ((shownRegion.endMs - shownRegion.startMs) /
                        Math.max(duration, 1)) *
                      100
                    }%`,
                  }}
                  aria-hidden
                />
                <div
                  className={styles.loopHandle}
                  style={{
                    left: `${(shownRegion.startMs / Math.max(duration, 1)) * 100}%`,
                  }}
                  role="slider"
                  tabIndex={0}
                  aria-label="Loop start"
                  aria-valuemin={0}
                  aria-valuemax={shownRegion.endMs}
                  aria-valuenow={shownRegion.startMs}
                  aria-valuetext={`Bar ${Math.max(
                    1,
                    bars.indexOf(shownRegion.startMs) + 1,
                  )}`}
                  onPointerDown={(event) => beginRegionDrag("start", event)}
                  onPointerMove={(event) => {
                    if (dragKind.current === "start") updateRegionDrag(event.clientX);
                  }}
                  onPointerUp={finishRegionDrag}
                  onPointerCancel={finishRegionDrag}
                  onKeyDown={(event) => {
                    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                      event.preventDefault();
                      event.stopPropagation();
                      nudgeLoopBoundary("start", event.key === "ArrowLeft" ? -1 : 1);
                    }
                  }}
                />
                <div
                  className={styles.loopHandle}
                  style={{
                    left: `${(shownRegion.endMs / Math.max(duration, 1)) * 100}%`,
                  }}
                  role="slider"
                  tabIndex={0}
                  aria-label="Loop end"
                  aria-valuemin={shownRegion.startMs}
                  aria-valuemax={duration}
                  aria-valuenow={shownRegion.endMs}
                  aria-valuetext={`Bar ${Math.max(
                    1,
                    bars.indexOf(shownRegion.endMs),
                  )}`}
                  onPointerDown={(event) => beginRegionDrag("end", event)}
                  onPointerMove={(event) => {
                    if (dragKind.current === "end") updateRegionDrag(event.clientX);
                  }}
                  onPointerUp={finishRegionDrag}
                  onPointerCancel={finishRegionDrag}
                  onKeyDown={(event) => {
                    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                      event.preventDefault();
                      event.stopPropagation();
                      nudgeLoopBoundary("end", event.key === "ArrowLeft" ? -1 : 1);
                    }
                  }}
                />
              </>
            )}
          </div>
        </div>
        <input
          className={styles.seek}
          type="range"
          min={0}
          max={Math.max(duration, 1)}
          step={1}
          value={Math.min(displayMs, duration)}
          disabled={!song || duration <= 0}
          aria-label="Seek"
          onChange={(e) => onSeekInput(Number(e.target.value))}
          onPointerUp={() => {
            if (scrubbing) {
              if (seekTimer.current != null) {
                window.clearTimeout(seekTimer.current);
                seekTimer.current = null;
              }
              void transportSeek(scrubMs).finally(() => setScrubbing(false));
            }
          }}
        />
      </div>

      <div className={styles.side}>
        {song && (
          <label className={styles.field}>
            Drum track
            <select
              value={selectedDrumTrackId ?? ""}
              onChange={(e) => {
                const id = e.target.value === "" ? null : Number(e.target.value);
                if (id != null) {
                  void setDrumTrack(id);
                }
              }}
            >
              {song.tracks.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                  {t.isDrumCandidate ? " ★" : ""} ({t.noteCount})
                </option>
              ))}
            </select>
          </label>
        )}
        <label className={styles.field}>
          MIDI in
          <select
            value={selectedMidiPortId ?? ""}
            onChange={(e) =>
              void selectMidiInput(e.target.value === "" ? null : e.target.value)
            }
            onFocus={() => void refreshMidiInputs()}
          >
            <option value="">Not connected</option>
            {midiPorts.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Audio out
          <select
            value={selectedAudioDeviceId ?? ""}
            onChange={(e) => {
              if (e.target.value) void selectAudioDevice(e.target.value);
            }}
            onFocus={() => void refreshAudioDevices()}
          >
            {audioDevices.length === 0 && (
              <option value="">No devices</option>
            )}
            {audioDevices.map((d) => (
              <option key={d.id} value={d.id}>
                [{d.host}] {d.name}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Pad map
          <select
            value={activePadMap?.id ?? ""}
            onChange={(e) =>
              void setActivePadMap(e.target.value === "" ? null : e.target.value)
            }
          >
            <option value="">None</option>
            {padMapProfiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
        <label
          className={styles.latencyField}
          title="Compensates audio/MIDI lag for scoring. Negative if you must hit early to get Perfect."
        >
          Latency
          <div className={styles.latencyRow}>
            <input
              className={styles.latencySlider}
              type="range"
              min={LATENCY_OFFSET_MIN}
              max={LATENCY_OFFSET_MAX}
              step={1}
              value={latencyOffsetMs}
              aria-label="Latency offset"
              onChange={(e) => onLatencyInput(Number(e.target.value))}
            />
            <span className={styles.latencyValue}>{formatOffset(latencyOffsetMs)}</span>
          </div>
        </label>
        <button
          type="button"
          className={styles.btnGhost}
          onClick={() => {
            void refreshMidiInputs();
            void refreshAudioDevices();
          }}
        >
          Refresh
        </button>
        <button
          type="button"
          className={styles.btn}
          onClick={() => setLatencyWizardOpen(true)}
        >
          Calibrate
        </button>
        {!repeatEnabled && (
          <button
            type="button"
            className={styles.btn}
            disabled={!song}
            title={
              song
                ? "Open score details"
                : "Load a MIDI file to view score stats"
            }
            onClick={() => setStatsOpen(true)}
          >
            Stats
          </button>
        )}
        <button
          type="button"
          className={styles.btn}
          onClick={() => setWizardOpen(true)}
        >
          Map pads
        </button>
      </div>
    </header>
  );
}
