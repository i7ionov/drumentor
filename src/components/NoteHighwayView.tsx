import {
  useCallback,
  useMemo,
  useRef,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import {
  KIT_PADS,
  OPTIONAL_PAD_IDS,
  REQUIRED_PAD_IDS,
  WINDOW_GOOD_MS,
  WINDOW_OK_MS,
  WINDOW_PERFECT_MS,
  type ExpectedHit,
  type Judgement,
  type PadId,
} from "../domain/types";
import { useAppStore } from "../store/appStore";
import styles from "./NoteHighwayView.module.css";

/** How long a note is visible before reaching the hit-line (ms). */
const APPROACH_MS = 2000;
/** How long a note stays visible after crossing the hit-line (ms). */
const FADE_MS = 220;
/** Within this window notes grow/brighten toward the hit-line. */
const APPROACH_GLOW_MS = 280;

const PAD_LABEL: Record<PadId, string> = Object.fromEntries(
  KIT_PADS.map((p) => [p.id, p.label]),
) as Record<PadId, string>;

function noteTopPercent(atMs: number, now: number, hitLineY: number): number {
  const progress = (atMs - now) / APPROACH_MS;
  return (hitLineY - progress * hitLineY) * 100;
}

/** Convert a timing offset (ms) into stage height % above/below the hit-line. */
function msToStagePercent(ms: number, hitLineY: number): number {
  return (ms / APPROACH_MS) * hitLineY * 100;
}

function visibleHits(
  hits: ExpectedHit[],
  now: number,
): ExpectedHit[] {
  const lo = now - FADE_MS;
  const hi = now + APPROACH_MS;
  return hits.filter((h) => h.timeMs > lo && h.timeMs <= hi);
}

function laneIdsForHits(hits: ExpectedHit[]): PadId[] {
  const present = new Set(hits.map((h) => h.padId));
  const optionalExtra = OPTIONAL_PAD_IDS.filter((id) => present.has(id));
  return [...REQUIRED_PAD_IDS, ...optionalExtra];
}

function receptorClass(
  padId: PadId,
  activePads: Set<PadId>,
  padJudgements: Map<PadId, Judgement>,
): string {
  const judgement = padJudgements.get(padId);
  if (judgement === "perfect") return `${styles.receptor} ${styles.receptorPerfect}`;
  if (judgement === "good") return `${styles.receptor} ${styles.receptorGood}`;
  if (judgement === "ok") return `${styles.receptor} ${styles.receptorOk}`;
  if (judgement === "miss" || judgement === "wrong") {
    return `${styles.receptor} ${styles.receptorMiss}`;
  }
  if (activePads.has(padId)) return `${styles.receptor} ${styles.receptorLit}`;
  return styles.receptor;
}

export function NoteHighwayView() {
  const song = useAppStore((s) => s.song);
  const expectedHits = useAppStore((s) => s.expectedHits);
  const positionMs = useAppStore((s) => s.positionMs);
  const hitLineY = useAppStore((s) => s.hitLineY);
  const setHitLineY = useAppStore((s) => s.setHitLineY);
  const activePads = useAppStore((s) => s.activePads);
  const padJudgements = useAppStore((s) => s.padJudgements);

  const stageRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  const lanes = useMemo(() => laneIdsForHits(expectedHits), [expectedHits]);
  const notes = useMemo(
    () => visibleHits(expectedHits, positionMs),
    [expectedHits, positionMs],
  );

  const windowBand = useMemo(() => {
    const okH = msToStagePercent(WINDOW_OK_MS, hitLineY) * 2;
    const goodH = msToStagePercent(WINDOW_GOOD_MS, hitLineY) * 2;
    const perfectH = msToStagePercent(WINDOW_PERFECT_MS, hitLineY) * 2;
    return { okH, goodH, perfectH };
  }, [hitLineY]);

  const updateHitLineFromPointer = useCallback(
    (clientY: number) => {
      const el = stageRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      if (rect.height <= 0) return;
      setHitLineY((clientY - rect.top) / rect.height);
    },
    [setHitLineY],
  );

  const onPointerDown = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      e.preventDefault();
      dragging.current = true;
      e.currentTarget.setPointerCapture(e.pointerId);
      updateHitLineFromPointer(e.clientY);
    },
    [updateHitLineFromPointer],
  );

  const onPointerMove = useCallback(
    (e: PointerEvent<HTMLDivElement>) => {
      if (!dragging.current) return;
      updateHitLineFromPointer(e.clientY);
    },
    [updateHitLineFromPointer],
  );

  const onPointerUp = useCallback((e: PointerEvent<HTMLDivElement>) => {
    dragging.current = false;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }, []);

  const onHitLineKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      // ArrowUp/Down control playback speed globally; use [ ] for hit-line.
      if (e.key === "[" || e.key === "PageUp") {
        e.preventDefault();
        e.stopPropagation();
        setHitLineY(hitLineY - 0.02);
      } else if (e.key === "]" || e.key === "PageDown") {
        e.preventDefault();
        e.stopPropagation();
        setHitLineY(hitLineY + 0.02);
      }
    },
    [hitLineY, setHitLineY],
  );

  const emptyMessage = !song
    ? "Open a MIDI file to see the note highway"
    : expectedHits.length === 0
      ? "Select a drum track to load notes"
      : null;

  return (
    <section className={styles.panel} aria-label="Note highway">
      <header className={styles.header}>
        <h2 className={styles.title}>Highway</h2>
        <span className={styles.meta}>
          {expectedHits.length > 0
            ? `${expectedHits.length} hits · strike at the line`
            : "awaiting notes"}
        </span>
      </header>

      <div className={styles.stage} ref={stageRef}>
        {emptyMessage ? (
          <p className={styles.empty}>{emptyMessage}</p>
        ) : (
          <>
            <div
              className={styles.lanes}
              style={{ "--lane-count": lanes.length } as CSSProperties}
            >
              {lanes.map((padId) => (
                <div key={padId} className={styles.lane} data-pad={padId}>
                  <span className={styles.laneLabel}>{PAD_LABEL[padId]}</span>
                </div>
              ))}
            </div>

            <div className={styles.hitZone} aria-hidden>
              <div
                className={styles.windowOk}
                style={{
                  top: `calc(${hitLineY * 100}% - ${windowBand.okH / 2}%)`,
                  height: `${windowBand.okH}%`,
                }}
              />
              <div
                className={styles.windowGood}
                style={{
                  top: `calc(${hitLineY * 100}% - ${windowBand.goodH / 2}%)`,
                  height: `${windowBand.goodH}%`,
                }}
              />
              <div
                className={styles.windowPerfect}
                style={{
                  top: `calc(${hitLineY * 100}% - ${windowBand.perfectH / 2}%)`,
                  height: `${windowBand.perfectH}%`,
                }}
              />
            </div>

            <div
              className={styles.receptors}
              style={
                {
                  top: `${hitLineY * 100}%`,
                  "--lane-count": lanes.length,
                } as CSSProperties
              }
              aria-hidden
            >
              {lanes.map((padId) => (
                <div key={padId} className={styles.receptorSlot}>
                  <span
                    className={receptorClass(padId, activePads, padJudgements)}
                    data-pad={padId}
                  />
                </div>
              ))}
            </div>

            <div className={styles.notesLayer} aria-hidden>
              {notes.map((hit) => {
                const laneIndex = lanes.indexOf(hit.padId);
                if (laneIndex < 0) return null;
                const top = noteTopPercent(hit.timeMs, positionMs, hitLineY);
                const delta = hit.timeMs - positionMs;
                const opacity =
                  delta < 0
                    ? Math.max(0, 1 - (-delta) / FADE_MS)
                    : 1;
                const approach =
                  delta > 0 && delta < APPROACH_GLOW_MS
                    ? 1 - delta / APPROACH_GLOW_MS
                    : delta <= 0
                      ? 1
                      : 0;
                const size = 0.55 + (hit.velocity / 127) * 0.45 + approach * 0.35;
                return (
                  <div
                    key={hit.uid}
                    className={
                      approach > 0.35
                        ? `${styles.note} ${styles.noteHot}`
                        : styles.note
                    }
                    data-pad={hit.padId}
                    style={{
                      top: `${top}%`,
                      left: `calc((100% / ${lanes.length}) * ${laneIndex} + (100% / ${lanes.length}) / 2)`,
                      opacity,
                      transform: `translate(-50%, -50%) scale(${size})`,
                    }}
                  />
                );
              })}
            </div>

            <div
              className={styles.hitLine}
              style={{ top: `${hitLineY * 100}%` }}
              role="slider"
              aria-label="Hit line"
              aria-valuemin={55}
              aria-valuemax={95}
              aria-valuenow={Math.round(hitLineY * 100)}
              aria-orientation="vertical"
              tabIndex={0}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
              onKeyDown={onHitLineKeyDown}
            >
              <span className={styles.hitLineHandle} />
            </div>
          </>
        )}
      </div>
    </section>
  );
}
