import styles from "./DrumKitView.module.css";
import {
  KIT_PADS,
  WINDOW_GOOD_MS,
  WINDOW_OK_MS,
  type ExpectedHit,
  type Judgement,
  type PadId,
} from "../domain/types";
import { useAppStore } from "../store/appStore";

const LAYOUT: Record<string, { cx: number; cy: number; r: number }> = {
  crash: { cx: 160, cy: 90, r: 42 },
  ride: { cx: 440, cy: 100, r: 48 },
  hiHatClosed: { cx: 110, cy: 180, r: 34 },
  hiHatOpen: { cx: 110, cy: 180, r: 34 },
  tomHigh: { cx: 250, cy: 160, r: 36 },
  tomMid: { cx: 340, cy: 155, r: 38 },
  snare: { cx: 220, cy: 260, r: 42 },
  floorTom: { cx: 400, cy: 270, r: 46 },
  kick: { cx: 300, cy: 340, r: 70 },
};

/** How long before a hit the approach circle appears (ms). */
const KIT_APPROACH_MS = 800;
/** How long after a hit the circle keeps shrinking inside the pad (ms). */
const KIT_APPROACH_FADE_MS = WINDOW_OK_MS;
/** Outer radius = pad.r * this scale at approach start. */
const APPROACH_START_SCALE = 2.2;

const VISIBLE_PADS = KIT_PADS.filter((p) => p.required && p.id !== "hiHatOpen");

function judgementClass(
  judgement: Judgement | undefined,
  stylesMap: typeof styles,
): string | null {
  switch (judgement) {
    case "perfect":
      return stylesMap.padPerfect;
    case "good":
      return stylesMap.padGood;
    case "ok":
      return stylesMap.padOk;
    case "miss":
      return stylesMap.padMiss;
    case "wrong":
      return stylesMap.padWrong;
    case "extra":
      return stylesMap.padExtra;
    default:
      return null;
  }
}

function resolveJudgement(
  padId: PadId,
  padJudgements: Map<PadId, Judgement>,
): Judgement | undefined {
  if (padId === "hiHatClosed") {
    return (
      padJudgements.get("hiHatClosed") ??
      padJudgements.get("hiHatOpen") ??
      padJudgements.get("hiHatPedal")
    );
  }
  return padJudgements.get(padId);
}

/** Fold hi-hat open/pedal onto the shared closed pad for kit visuals. */
function foldPadId(padId: PadId): PadId {
  if (padId === "hiHatOpen" || padId === "hiHatPedal") return "hiHatClosed";
  return padId;
}

function approachStartR(padR: number): number {
  return padR * APPROACH_START_SCALE;
}

function goodRingStroke(padR: number): number {
  const travel = approachStartR(padR) - padR;
  return 2 * (WINDOW_GOOD_MS / KIT_APPROACH_MS) * travel;
}

function approachRadius(padR: number, deltaMs: number): number {
  const startR = approachStartR(padR);
  return padR + (deltaMs / KIT_APPROACH_MS) * (startR - padR);
}

function visibleApproachHits(hits: ExpectedHit[], now: number): ExpectedHit[] {
  const lo = now - KIT_APPROACH_FADE_MS;
  const hi = now + KIT_APPROACH_MS;
  return hits.filter((h) => h.timeMs > lo && h.timeMs <= hi);
}

export function DrumKitView() {
  const activePads = useAppStore((s) => s.activePads);
  const padJudgements = useAppStore((s) => s.padJudgements);
  const wizardTargetPad = useAppStore((s) => s.wizardTargetPad);
  const flashPad = useAppStore((s) => s.flashPad);
  const expectedHits = useAppStore((s) => s.expectedHits);
  const positionMs = useAppStore((s) => s.positionMs);
  const transportStatus = useAppStore((s) => s.transportStatus);

  const showTiming =
    transportStatus !== "stopped" && expectedHits.length > 0;
  const approachHits = showTiming
    ? visibleApproachHits(expectedHits, positionMs)
    : [];

  return (
    <section className={styles.stage} aria-label="Drum kit">
      <svg
        className={styles.svg}
        viewBox="0 0 600 420"
        role="img"
        aria-label="Interactive drum kit diagram"
      >
        <defs>
          <radialGradient id="kitGlow" cx="50%" cy="40%" r="60%">
            <stop offset="0%" stopColor="rgba(232, 168, 74, 0.18)" />
            <stop offset="100%" stopColor="rgba(0, 0, 0, 0)" />
          </radialGradient>
        </defs>
        <rect width="600" height="420" fill="url(#kitGlow)" />
        {VISIBLE_PADS.map((pad) => {
          const geo = LAYOUT[pad.id];
          if (!geo) return null;
          const active =
            pad.id === "hiHatClosed"
              ? activePads.has("hiHatClosed") ||
                activePads.has("hiHatOpen") ||
                activePads.has("hiHatPedal")
              : activePads.has(pad.id);
          const targeted =
            wizardTargetPad === pad.id ||
            (pad.id === "hiHatClosed" &&
              (wizardTargetPad === "hiHatOpen" ||
                wizardTargetPad === "hiHatPedal"));
          const jClass = judgementClass(resolveJudgement(pad.id, padJudgements), styles);
          const className = jClass
            ? jClass
            : active
              ? styles.padActive
              : targeted
                ? styles.padTarget
                : styles.pad;
          const padApproaches = approachHits.filter(
            (h) => foldPadId(h.padId) === pad.id,
          );
          const ringStroke = goodRingStroke(geo.r);

          return (
            <g key={pad.id}>
              {padApproaches.map((hit) => {
                const delta = hit.timeMs - positionMs;
                const r = approachRadius(geo.r, delta);
                if (r <= 0) return null;
                const opacity =
                  delta < 0
                    ? Math.max(0, 1 - -delta / KIT_APPROACH_FADE_MS)
                    : 1;
                return (
                  <circle
                    key={hit.uid}
                    className={styles.approach}
                    cx={geo.cx}
                    cy={geo.cy}
                    r={r}
                    opacity={opacity}
                  />
                );
              })}
              {showTiming ? (
                <circle
                  className={styles.toleranceRing}
                  cx={geo.cx}
                  cy={geo.cy}
                  r={geo.r}
                  strokeWidth={ringStroke}
                />
              ) : null}
              <circle
                className={className}
                cx={geo.cx}
                cy={geo.cy}
                r={geo.r}
                onClick={() => flashPad(pad.id)}
              />
              <text
                className={styles.label}
                x={geo.cx}
                y={geo.cy + 4}
                textAnchor="middle"
              >
                {pad.label}
              </text>
            </g>
          );
        })}
      </svg>
      <p className={styles.hint}>
        Click a pad to preview highlight. Hit your e-drums to flash mapped pads;
        during playback, pads follow the MIDI schedule and score judgements.
      </p>
    </section>
  );
}
