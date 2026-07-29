# Scoring — Drumentor

Practice scoring rules. Hit entities — see [DOMAIN.md](DOMAIN.md).

## Goal

Give the drummer clear and fair feedback:

- whether they hit the **correct** kit element;
- how accurate they were on **timing** relative to the part;
- where systematic misses occur (specific pads, early vs late).

## Judgement

Each expected hit ends up with one of:

| Judgement | Condition |
|-----------|-----------|
| `Perfect` | Correct `PadId`, \|Δt\| ≤ 20 ms |
| `Good` | Correct `PadId`, \|Δt\| ≤ 50 ms |
| `Ok` | Correct `PadId`, \|Δt\| ≤ 80 ms |
| `Miss` | No correct hit within the ±80 ms window |
| `Wrong` | There was a hit, but on the wrong pad (see policy below) |

`Δt = incoming.timeMs - expected.timeMs`  
negative = early, positive = late.

### Windows (initial constants)

| Window | ms | Constant |
|--------|-----|----------|
| Perfect | 20 | `WINDOW_PERFECT_MS` |
| Good | 50 | `WINDOW_GOOD_MS` |
| Ok | 80 | `WINDOW_OK_MS` |

Windows will be tunable in settings later; in MVP they are constants in core.  
When slowing playback (v1), windows are **not stretched** in ms (musical strictness is preserved); a separate “forgiving mode” is an optional v1+ setting.

## Matching algorithm

1. Expected hits are ordered by time.
2. An incoming hit with a known `PadId` looks among **open** expected hits with the same `PadId` where `|t_in - t_exp| ≤ WINDOW_OK_MS`.
3. If there are multiple candidates — pick the **nearest** by \|Δt\|; on a tie — the earlier expected.
4. A matched expected no longer participates.
5. After `t_exp + WINDOW_OK_MS`, an unmatched expected → `Miss`.
6. One incoming closes exactly one expected.

### Wrong policy

- If an incoming finds no expected with its `PadId` in the window, but within ±`WINDOW_OK_MS` there is an unmatched expected for a **different** pad → `Wrong` (penalty); the expected is **not** closed by this hit (it can still be hit with the correct pad or become Miss).
- If there is no nearby expected at all (empty space in the part) → `Extra` (not Wrong): in MVP it may be omitted from the summary or counted separately without affecting note accuracy; it does **not** increase Miss.

Recommended MVP summary:

- count in accuracy only expected-oriented judgements: Perfect/Good/Ok/Miss (+ Wrong as a separate penalty counter);
- Extra — debug / secondary metric.

## Session metrics

### Required (MVP)

| Metric | Formula / meaning |
|--------|-------------------|
| `totalExpected` | Number of expected hits in the session (or in the loop region) |
| `hitCounts` | Count of Perfect / Good / Ok / Miss / Wrong |
| `noteAccuracy` | `(Perfect + Good + Ok) / totalExpected` |
| `timingMeanMs` | Mean Δt over Perfect+Good+Ok (signed) |
| `timingAbsMeanMs` | Mean \|Δt\| over hits |
| `scorePercent` | Weighted percent (below) |

### Weighted score (MVP)

```text
points = Perfect*100 + Good*70 + Ok*40 + Miss*0 + Wrong*(-20)
maxPoints = totalExpected * 100
scorePercent = clamp(0, 100, round(100 * points / maxPoints))
```

Wrong’s negative contribution is floored at zero for the final % (we do not show a negative score in the UI).

### v1+

| Metric | Meaning |
|--------|---------|
| `timingStdDevMs` | Timing spread |
| `streakBest` | Best consecutive Perfect/Good streak |
| `perPadBreakdown` | accuracy and mean Δt per PadId |
| `earlyLateHist` | Δt histogram (e.g. 10 ms bins from -80..+80) |
| `trend` | Compare scorePercent with previous sessions of the same song |

## Live feedback

During practice the UI receives events:

```text
JudgementEvent {
  expectedUid?
  padId
  judgement
  deltaMs?
}
```

- Highlight the pad with the judgement color.
- Short floating label `Early` / `Late` / `Perfect` (no spam: throttle per pad).
- Combo/streak — P1.

## Latency offset

`settings.latencyOffsetMs` is added to the incoming timestamp **before** matching:

```text
adjustedTime = rawSessionTime + latencyOffsetMs
```

Sign is calibrated by the v1 wizard (“hits feel early → shift the offset”).  
Document the chosen sign in UI settings (“device latency compensation”).

Audio/visual playback offset is a separate parameter; do not mix it with input latency in one number if there are two calibrations; in MVP a single global offset with an explanation is acceptable.

The **hit-line** position on the note highway (`hitLineY`) is a UI preference: it only changes note approach geometry and is **not** a latency offset and does not affect matching timestamps.

## Session boundaries

- **Start:** transport play in Practice mode (or explicit Start Practice).
- **End:** stop / end of song / end of loop-run count (v1).
- Seek backward in MVP: reset open expected relative to the new position; already emitted Miss/Hit before the seek are not rescored (simple rule). v1 may refine “rescore region”.

## What is not scored

- Hits before session start / after session end.
- Unmapped MIDI notes.
- Playback ghost hits (part highlight) — not Judgement.
- Non-drum tracks.

## Example

Expected: Snare @ 1000 ms.  
Incoming: Snare @ 1025 ms → Δt = +25 → `Good`.  
Incoming: Kick @ 1005 ms with expected Snare → `Wrong`, Snare still open.  
If Snare is never hit by 1080 ms → additional `Miss` on Snare.

## Scoring tests (for future unit tests)

- Exact pad, Δt 0 → Perfect.
- Δt 20 → Perfect; 21 → Good.
- Δt 80 → Ok; 81 → Miss (if no hit) / no match.
- Two consecutive expected on one pad — two incoming close by nearest.
- Extra hit in a rest — not Miss.
- Speed 0.5 (v1) — same ms windows relative to the scaled timeline.
