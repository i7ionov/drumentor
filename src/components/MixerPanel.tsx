import { useEffect, useRef } from "react";
import styles from "./MixerPanel.module.css";
import {
  setClickMuted,
  setMasterVolume,
  setMetronomeVolume,
  setPlayerMuted,
  setPlayerVolume,
  setTrackMute,
  setTrackSolo,
  setTrackVolume,
} from "../lib/tauri";
import { useAppStore, VOLUME_MAX, VOLUME_MIN, VOLUME_UNITY } from "../store/appStore";

function pct(v: number): string {
  return `${Math.round(v)}%`;
}

/** Prefer the part before "(…)" so long MIDI names fit the strip. */
function shortTrackLabel(name: string, isDrum: boolean): string {
  const paren = name.indexOf("(");
  const base = paren > 1 ? name.slice(0, paren).trim() : name.trim();
  return isDrum ? `★ ${base}` : base;
}

export function MixerPanel() {
  const mixerExpanded = useAppStore((s) => s.mixerExpanded);
  const setMixerExpanded = useAppStore((s) => s.setMixerExpanded);
  const song = useAppStore((s) => s.song);
  const selectedDrumTrackId = useAppStore((s) => s.selectedDrumTrackId);
  const masterVolume = useAppStore((s) => s.masterVolume);
  const trackMixer = useAppStore((s) => s.trackMixer);
  const metronomeVolume = useAppStore((s) => s.metronomeVolume);
  const metronomeEnabled = useAppStore((s) => s.metronomeEnabled);
  const clickMuted = useAppStore((s) => s.clickMuted);
  const playerVolume = useAppStore((s) => s.playerVolume);
  const playPlayerDrums = useAppStore((s) => s.playPlayerDrums);
  const playerMuted = useAppStore((s) => s.playerMuted);

  const masterTimer = useRef<number | null>(null);
  const trackTimers = useRef<Map<number, number>>(new Map());
  const clickTimer = useRef<number | null>(null);
  const playerTimer = useRef<number | null>(null);

  useEffect(() => {
    if (!mixerExpanded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMixerExpanded(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mixerExpanded, setMixerExpanded]);

  const tracks = (song?.tracks ?? []).filter((t) => t.noteCount > 0);
  const anySolo = Object.values(trackMixer).some((c) => c.solo);

  const onMaster = (value: number) => {
    useAppStore.getState().setMasterVolume(value);
    if (masterTimer.current != null) window.clearTimeout(masterTimer.current);
    masterTimer.current = window.setTimeout(() => {
      void setMasterVolume(value);
    }, 80);
  };

  const onTrackVol = (trackId: number, value: number) => {
    useAppStore.getState().setTrackVolume(trackId, value);
    const prev = trackTimers.current.get(trackId);
    if (prev != null) window.clearTimeout(prev);
    trackTimers.current.set(
      trackId,
      window.setTimeout(() => {
        void setTrackVolume(trackId, value);
      }, 80),
    );
  };

  const onClickVol = (value: number) => {
    useAppStore.getState().setMetronomeVolume(value);
    if (clickTimer.current != null) window.clearTimeout(clickTimer.current);
    clickTimer.current = window.setTimeout(() => {
      void setMetronomeVolume(value);
    }, 80);
  };

  const onPlayerVol = (value: number) => {
    useAppStore.getState().setPlayerVolume(value);
    if (playerTimer.current != null) window.clearTimeout(playerTimer.current);
    playerTimer.current = window.setTimeout(() => {
      void setPlayerVolume(value);
    }, 80);
  };

  return (
    <section className={styles.shell} aria-label="Mixer">
      <div
        className={
          mixerExpanded ? `${styles.panel} ${styles.expanded}` : styles.panel
        }
      >
        <button
          type="button"
          className={
            mixerExpanded
              ? `${styles.expandTab} ${styles.expandTabActive}`
              : styles.expandTab
          }
          aria-expanded={mixerExpanded}
          aria-label={mixerExpanded ? "Collapse mixer" : "Expand mixer"}
          title={mixerExpanded ? "Collapse mixer" : "Expand mixer"}
          onClick={() => setMixerExpanded(!mixerExpanded)}
        >
          <span className={styles.expandChevron} aria-hidden>
            {mixerExpanded ? "⌄" : "⌃"}
          </span>
        </button>

        <div className={styles.body}>
        {mixerExpanded && (
          <div className={styles.stickyLeft}>
            <div className={styles.strip}>
              <span className={styles.stripName} title="Master">
                Master
              </span>
              <input
                className={styles.fader}
                type="range"
                min={VOLUME_MIN}
                max={VOLUME_MAX}
                step={1}
                value={masterVolume}
                aria-label="Master volume"
                onChange={(e) => onMaster(Number(e.target.value))}
              />
              <span className={styles.level}>{pct(masterVolume)}</span>
            </div>
          </div>
        )}

        <div className={styles.scroll}>
          {tracks.length === 0 && (
            <span className={styles.empty}>Load a MIDI file to mix tracks</span>
          )}
          {tracks.map((t) => {
            const ch = trackMixer[t.id] ?? {
              volume: VOLUME_UNITY,
              muted: false,
              solo: false,
            };
            const isDrum = selectedDrumTrackId === t.id;
            const dim = anySolo && !ch.solo;
            return (
              <div
                key={t.id}
                className={dim ? `${styles.strip} ${styles.stripDim}` : styles.strip}
              >
                <span
                  className={
                    isDrum
                      ? `${styles.stripName} ${styles.stripDrum}`
                      : styles.stripName
                  }
                  title={t.name}
                >
                  {shortTrackLabel(t.name, isDrum)}
                </span>
                {mixerExpanded && (
                  <>
                    <input
                      className={styles.fader}
                      type="range"
                      min={VOLUME_MIN}
                      max={VOLUME_MAX}
                      step={1}
                      value={ch.volume}
                      aria-label={`${t.name} volume`}
                      onChange={(e) => onTrackVol(t.id, Number(e.target.value))}
                    />
                    <span className={styles.level}>{pct(ch.volume)}</span>
                  </>
                )}
                <div className={styles.btns}>
                  <button
                    type="button"
                    className={
                      ch.muted
                        ? `${styles.ms} ${styles.msMuteActive}`
                        : styles.ms
                    }
                    title="Mute"
                    aria-pressed={ch.muted}
                    onClick={() => void setTrackMute(t.id, !ch.muted)}
                  >
                    M
                  </button>
                  <button
                    type="button"
                    className={ch.solo ? styles.msActive : styles.ms}
                    title="Solo"
                    aria-pressed={ch.solo}
                    onClick={() => void setTrackSolo(t.id, !ch.solo)}
                  >
                    S
                  </button>
                </div>
              </div>
            );
          })}
        </div>

        <div className={styles.stickyRight}>
          <div className={styles.strip}>
            <span className={styles.stripName} title="Metronome click">
              Click
            </span>
            {mixerExpanded && (
              <>
                <input
                  className={styles.fader}
                  type="range"
                  min={VOLUME_MIN}
                  max={VOLUME_MAX}
                  step={1}
                  value={metronomeVolume}
                  disabled={!metronomeEnabled}
                  aria-label="Click volume"
                  onChange={(e) => onClickVol(Number(e.target.value))}
                />
                <span className={styles.level}>{pct(metronomeVolume)}</span>
              </>
            )}
            <div className={styles.btns}>
              <button
                type="button"
                className={
                  clickMuted
                    ? `${styles.ms} ${styles.msMuteActive}`
                    : styles.ms
                }
                title="Mute click"
                aria-pressed={clickMuted}
                disabled={!metronomeEnabled}
                onClick={() => void setClickMuted(!clickMuted)}
              >
                M
              </button>
            </div>
          </div>

          <div className={styles.strip}>
            <span className={styles.stripName} title="Player kit feedback">
              Player
            </span>
            {mixerExpanded && (
              <>
                <input
                  className={styles.fader}
                  type="range"
                  min={VOLUME_MIN}
                  max={VOLUME_MAX}
                  step={1}
                  value={playerVolume}
                  disabled={!playPlayerDrums}
                  aria-label="Player kit volume"
                  onChange={(e) => onPlayerVol(Number(e.target.value))}
                />
                <span className={styles.level}>{pct(playerVolume)}</span>
              </>
            )}
            <div className={styles.btns}>
              <button
                type="button"
                className={
                  playerMuted
                    ? `${styles.ms} ${styles.msMuteActive}`
                    : styles.ms
                }
                title="Mute player kit"
                aria-pressed={playerMuted}
                disabled={!playPlayerDrums}
                onClick={() => void setPlayerMuted(!playerMuted)}
              >
                M
              </button>
            </div>
          </div>
        </div>
      </div>
      </div>
    </section>
  );
}
