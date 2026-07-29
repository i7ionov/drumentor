import { useEffect, useRef, useState } from "react";
import {
  clampLatencyOffsetMs,
  LATENCY_OFFSET_MAX,
  LATENCY_OFFSET_MIN,
  listenAudioClicks,
  listenMidiNoteOn,
  setLatencyOffset,
  startLatencyClickTrain,
  stopLatencyClickTrain,
} from "../lib/tauri";
import { useAppStore } from "../store/appStore";
import styles from "./LatencyCalWizard.module.css";

type Step = "intro" | "measure" | "result";

/** Warm-up clicks — not used for the offset. */
const PRACTICE_CLICK_COUNT = 16;
/** Counted clicks — used for median lag. */
const MEASURE_CLICK_COUNT = 16;
const TOTAL_CLICK_COUNT = PRACTICE_CLICK_COUNT + MEASURE_CLICK_COUNT;
/** ~120 BPM — faster than a slow practice metronome. */
const INTERVAL_MS = 500;
const LEAD_IN_MS = 550;
const MATCH_WINDOW_MS = 180;
const MIN_HITS = 8;

function formatOffset(ms: number): string {
  const sign = ms > 0 ? "+" : "";
  return `${sign}${ms} ms`;
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) return sorted[mid]!;
  return (sorted[mid - 1]! + sorted[mid]!) / 2;
}

/**
 * Latency calibration: 16 practice clicks, then 16 counted taps on the
 * metronome. Offset = −median(hit − click) so scoring matches what you hear.
 * Clicks play through the native audio engine (same path as song playback).
 */
export function LatencyCalWizard() {
  const open = useAppStore((s) => s.latencyWizardOpen);
  const setOpen = useAppStore((s) => s.setLatencyWizardOpen);
  const selectedMidiPortId = useAppStore((s) => s.selectedMidiPortId);
  const currentOffset = useAppStore((s) => s.latencyOffsetMs);
  const setError = useAppStore((s) => s.setError);

  const [step, setStep] = useState<Step>("intro");
  const [hitsCaptured, setHitsCaptured] = useState(0);
  const [clicksDone, setClicksDone] = useState(0);
  const [phase, setPhase] = useState<"practice" | "measure">("practice");
  const [pulseHot, setPulseHot] = useState(false);
  const [lastDeltaMs, setLastDeltaMs] = useState<number | null>(null);
  const [measuredLagMs, setMeasuredLagMs] = useState<number | null>(null);
  const [draftOffset, setDraftOffset] = useState(0);
  const [saving, setSaving] = useState(false);
  const [running, setRunning] = useState(false);

  const clicksRef = useRef<
    { wallMs: number; used: boolean; counted: boolean }[]
  >([]);
  const deltasRef = useRef<number[]>([]);
  const stopMeasureRef = useRef<(() => void) | null>(null);
  const finishTimerRef = useRef<number | null>(null);

  const resetMeasureState = () => {
    setHitsCaptured(0);
    setClicksDone(0);
    setPhase("practice");
    setPulseHot(false);
    setLastDeltaMs(null);
    setMeasuredLagMs(null);
    setRunning(false);
    clicksRef.current = [];
    deltasRef.current = [];
  };

  const teardownMeasure = () => {
    if (finishTimerRef.current != null) {
      window.clearTimeout(finishTimerRef.current);
      finishTimerRef.current = null;
    }
    stopMeasureRef.current?.();
    stopMeasureRef.current = null;
    void stopLatencyClickTrain();
  };

  useEffect(() => {
    if (!open) return;
    setStep("intro");
    setDraftOffset(currentOffset);
    resetMeasureState();
    return () => {
      teardownMeasure();
    };
    // Reset only when the wizard opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const finishMeasure = (deltas: number[]) => {
    teardownMeasure();
    setRunning(false);
    if (deltas.length < MIN_HITS) {
      setError(
        `Need at least ${MIN_HITS} on-beat hits (got ${deltas.length}). Try again.`,
      );
      setStep("intro");
      resetMeasureState();
      return;
    }
    const lagMs = median(deltas);
    const recommended = clampLatencyOffsetMs(-lagMs);
    setMeasuredLagMs(Math.round(lagMs));
    setDraftOffset(recommended);
    setStep("result");
  };

  const startMeasure = async () => {
    teardownMeasure();
    resetMeasureState();
    setStep("measure");
    setRunning(true);

    try {
      clicksRef.current = Array.from({ length: TOTAL_CLICK_COUNT }, (_, i) => ({
        wallMs: 0,
        used: false,
        counted: i >= PRACTICE_CLICK_COUNT,
      }));
      deltasRef.current = [];

      let disposed = false;
      let unlistenMidi: (() => void) | undefined;
      let unlistenClicks: (() => void) | undefined;
      let doneCount = 0;

      void listenMidiNoteOn((ev) => {
        if (disposed) return;
        const now = ev.wallMs ?? 0;
        if (!(now > 0)) return;
        let bestIdx = -1;
        let bestAbs = MATCH_WINDOW_MS;
        for (let i = 0; i < clicksRef.current.length; i++) {
          const click = clicksRef.current[i]!;
          if (click.used || !(click.wallMs > 0)) continue;
          const abs = Math.abs(now - click.wallMs);
          if (abs <= bestAbs) {
            bestAbs = abs;
            bestIdx = i;
          }
        }
        if (bestIdx < 0) return;
        const click = clicksRef.current[bestIdx]!;
        click.used = true;
        const delta = now - click.wallMs;
        setLastDeltaMs(Math.round(delta));
        if (!click.counted) return;
        deltasRef.current.push(delta);
        setHitsCaptured(deltasRef.current.length);
      }).then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlistenMidi = fn;
      });

      void listenAudioClicks((ev) => {
        if (disposed) return;
        const click = clicksRef.current[ev.index];
        if (!click) return;
        click.wallMs = ev.wallMs;
        doneCount += 1;
        setPulseHot(true);
        window.setTimeout(() => setPulseHot(false), 90);
        setClicksDone(doneCount);
        setPhase(doneCount <= PRACTICE_CLICK_COUNT ? "practice" : "measure");
      }).then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlistenClicks = fn;
      });

      stopMeasureRef.current = () => {
        disposed = true;
        unlistenMidi?.();
        unlistenClicks?.();
      };

      await startLatencyClickTrain(TOTAL_CLICK_COUNT, INTERVAL_MS, LEAD_IN_MS);

      const totalMs =
        LEAD_IN_MS +
        (TOTAL_CLICK_COUNT - 1) * INTERVAL_MS +
        MATCH_WINDOW_MS +
        80;
      finishTimerRef.current = window.setTimeout(() => {
        if (!disposed) {
          finishMeasure([...deltasRef.current]);
        }
      }, totalMs);
    } catch (e) {
      setRunning(false);
      setError(e instanceof Error ? e.message : String(e));
      setStep("intro");
    }
  };

  const close = () => {
    teardownMeasure();
    setOpen(false);
  };

  const onSave = async () => {
    setSaving(true);
    try {
      await setLatencyOffset(draftOffset);
      close();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  if (!open) return null;

  const stepLabel =
    step === "intro" ? "1 / 3" : step === "measure" ? "2 / 3" : "3 / 3";

  return (
    <div
      className={styles.backdrop}
      role="dialog"
      aria-modal
      aria-label="Latency calibration"
    >
      <div className={styles.panel}>
        <header className={styles.header}>
          <h2>Calibrate latency</h2>
          <button type="button" className={styles.close} onClick={close}>
            Cancel
          </button>
        </header>

        {step === "intro" && (
          <div className={styles.body}>
            <p className={styles.prompt}>Hit pads on the clicks</p>
            <p className={styles.meta}>
              Audio reaches your ears later than the scoring clock. First{" "}
              {PRACTICE_CLICK_COUNT} clicks are practice (not counted), then{" "}
              {MEASURE_CLICK_COUNT} counted hits measure how late your MIDI
              arrives — we set an offset so Perfect matches what you hear.
            </p>
            <p className={styles.meta}>
              Current offset:{" "}
              <strong className={styles.progress}>
                {formatOffset(currentOffset)}
              </strong>
              . Sign: negative usually fixes “I must hit early to score
              Perfect”.
            </p>
            {!selectedMidiPortId && (
              <p className={styles.warn}>
                Select a MIDI input port first, then start calibration.
              </p>
            )}
            <div className={styles.actions}>
              <button
                type="button"
                className={styles.primary}
                disabled={!selectedMidiPortId || running}
                onClick={() => void startMeasure()}
              >
                Start
              </button>
            </div>
          </div>
        )}

        {step === "measure" && (
          <div className={styles.body}>
            <p className={styles.prompt}>
              {phase === "practice"
                ? "Practice — get in the pocket"
                : "Measuring — keep hitting on the click"}
            </p>
            <p className={styles.meta}>
              {phase === "practice"
                ? `Warm-up ${Math.min(clicksDone, PRACTICE_CLICK_COUNT)} / ${PRACTICE_CLICK_COUNT}. These hits are not counted.`
                : `Counted hits ${hitsCaptured} / ${MEASURE_CLICK_COUNT}. Stay relaxed and hit with the sound.`}
            </p>
            <div className={styles.pulseWrap}>
              <div
                className={`${styles.pulse} ${pulseHot ? styles.pulseHot : ""}`}
                aria-hidden
              />
              <span className={styles.progress}>
                {phase === "practice" ? "Practice" : "Measure"} · Clicks{" "}
                {clicksDone} / {TOTAL_CLICK_COUNT}
                {phase === "measure" ? ` · Hits ${hitsCaptured}` : ""}
              </span>
              <span className={styles.lastHit}>
                {lastDeltaMs == null
                  ? "Waiting for hits…"
                  : `Last Δ ${lastDeltaMs > 0 ? "+" : ""}${lastDeltaMs} ms`}
              </span>
            </div>
            <div className={styles.actions}>
              <button
                type="button"
                className={styles.secondary}
                onClick={() => {
                  teardownMeasure();
                  resetMeasureState();
                  setStep("intro");
                }}
              >
                Stop
              </button>
            </div>
          </div>
        )}

        {step === "result" && (
          <div className={styles.body}>
            <p className={styles.prompt}>Recommended offset</p>
            <div className={styles.resultCard}>
              <span className={styles.resultValue}>
                {formatOffset(draftOffset)}
              </span>
              <span className={styles.meta}>
                {measuredLagMs == null
                  ? "Fine-tune below, then save."
                  : `Measured hit lag ≈ ${measuredLagMs > 0 ? "+" : ""}${measuredLagMs} ms vs click schedule → offset ${formatOffset(clampLatencyOffsetMs(-(measuredLagMs)))}.`}
              </span>
            </div>

            <label className={styles.sliderField}>
              <span className={styles.sliderLabel}>
                Fine-tune
                <span className={styles.sliderValue}>
                  {formatOffset(draftOffset)}
                </span>
              </span>
              <input
                className={styles.slider}
                type="range"
                min={LATENCY_OFFSET_MIN}
                max={LATENCY_OFFSET_MAX}
                step={1}
                value={draftOffset}
                aria-label="Latency offset"
                onChange={(e) => setDraftOffset(Number(e.target.value))}
              />
            </label>
            <p className={styles.meta}>
              Hits feel late in score → more negative. Hits feel early → more
              positive. Range {LATENCY_OFFSET_MIN}…{LATENCY_OFFSET_MAX} ms.
            </p>

            <div className={styles.actions}>
              <button
                type="button"
                className={styles.secondary}
                onClick={() => void startMeasure()}
              >
                Retake
              </button>
              <button
                type="button"
                className={styles.primary}
                disabled={saving}
                onClick={() => void onSave()}
              >
                {saving ? "Saving…" : "Save offset"}
              </button>
            </div>
          </div>
        )}

        <footer className={styles.footer}>Step {stepLabel}</footer>
      </div>
    </div>
  );
}
