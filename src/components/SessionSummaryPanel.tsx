import type { HitCounts, SessionSummary } from "../domain/types";
import styles from "./SessionSummaryPanel.module.css";
import { useAppStore } from "../store/appStore";

function pct(n: number): string {
  return `${Math.round(n * 100)}%`;
}

function fmtMs(n: number | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const sign = n > 0 ? "+" : "";
  return `${sign}${n.toFixed(1)} ms`;
}

function noteAccuracyFromCounts(counts: HitCounts): number | null {
  const total = counts.perfect + counts.good + counts.ok + counts.miss;
  if (total === 0) return null;
  return (counts.perfect + counts.good + counts.ok) / total;
}

function displayFromSummary(summary: SessionSummary) {
  return {
    title: "Session score",
    scoreLabel: `${summary.scorePercent}%`,
    noteAccuracy: pct(summary.noteAccuracy),
    expected: String(summary.totalExpected),
    timingMean: fmtMs(summary.timingMeanMs),
    timingAbs: fmtMs(summary.timingAbsMeanMs),
    counts: summary.hitCounts,
    live: false,
  };
}

function displayFromLive(counts: HitCounts) {
  const accuracy = noteAccuracyFromCounts(counts);
  return {
    title: "Live score",
    scoreLabel: accuracy == null ? "—" : pct(accuracy),
    noteAccuracy: accuracy == null ? "—" : pct(accuracy),
    expected: "in progress",
    timingMean: "—",
    timingAbs: "—",
    counts,
    live: true,
  };
}

export function SessionSummaryPanel() {
  const statsOpen = useAppStore((s) => s.statsOpen);
  const summary = useAppStore((s) => s.sessionSummary);
  const liveHitCounts = useAppStore((s) => s.liveHitCounts);
  const setStatsOpen = useAppStore((s) => s.setStatsOpen);

  if (!statsOpen) return null;

  const view =
    summary != null
      ? displayFromSummary(summary)
      : displayFromLive(liveHitCounts);
  const c = view.counts;

  return (
    <div className={styles.backdrop} role="dialog" aria-labelledby="session-summary-title">
      <div className={styles.panel}>
        <header className={styles.header}>
          <h2 id="session-summary-title" className={styles.title}>
            {view.title}
          </h2>
          <p className={styles.score}>{view.scoreLabel}</p>
        </header>

        {view.live && (
          <p className={styles.hint}>
            Mid-run snapshot — final score and timing finalize when you Stop.
          </p>
        )}

        <dl className={styles.stats}>
          <div>
            <dt>Note accuracy</dt>
            <dd>{view.noteAccuracy}</dd>
          </div>
          <div>
            <dt>Expected</dt>
            <dd>{view.expected}</dd>
          </div>
          <div>
            <dt>Timing mean</dt>
            <dd>{view.timingMean}</dd>
          </div>
          <div>
            <dt>Abs timing</dt>
            <dd>{view.timingAbs}</dd>
          </div>
        </dl>

        <ul className={styles.breakdown}>
          <li className={styles.perfect}>
            <span>Perfect</span>
            <strong>{c.perfect}</strong>
          </li>
          <li className={styles.good}>
            <span>Good</span>
            <strong>{c.good}</strong>
          </li>
          <li className={styles.ok}>
            <span>Ok</span>
            <strong>{c.ok}</strong>
          </li>
          <li className={styles.miss}>
            <span>Miss</span>
            <strong>{c.miss}</strong>
          </li>
          <li className={styles.wrong}>
            <span>Wrong</span>
            <strong>{c.wrong}</strong>
          </li>
          {c.extra > 0 && (
            <li className={styles.extra}>
              <span>Extra</span>
              <strong>{c.extra}</strong>
            </li>
          )}
        </ul>

        <button
          type="button"
          className={styles.dismiss}
          onClick={() => setStatsOpen(false)}
        >
          Dismiss
        </button>
      </div>
    </div>
  );
}
