# Domain — Drumentor

Domain model: drum kit, GM mapping, drum track detection, hit matching at the entity level. Scoring rules live in [SCORING.md](SCORING.md).

## Core entities

| Entity | Description |
|--------|-------------|
| `Song` | MIDI file + metadata (path, hash, duration, tempo map) |
| `Track` | MIDI track: id, name, channel(s), notes |
| `DrumTrackRef` | Selected track (or subset) as the source of expected hits |
| `PadId` | Logical kit element |
| `ExpectedHit` | Expected hit: pad, timeMs, velocity, uid |
| `IncomingHit` | Hit from the device: pad, timeMs, velocity |
| `PadMapProfile` | MIDI note → PadId bindings for a device |
| `Session` | Practice: song + config + judgements + summary |

## PadId (MVP kit)

Internal identifiers are a stable contract across UI ↔ core ↔ scoring.

| PadId | Role | Description | Typical GM notes |
|-------|------|-------------|------------------|
| `Kick` | required | Bass drum | 36 (Bass Drum 1), also 35 |
| `Snare` | required | Snare | 38 (Acoustic Snare), also 40 |
| `HiHatClosed` | required | Closed hi-hat | 42 |
| `HiHatOpen` | required | Open hi-hat | 46 |
| `TomHigh` | required | Tom 1 (high) | 50, 48 |
| `TomMid` | required | Tom 2 (mid) | 47, 45 |
| `FloorTom` | required | Tom 3 (floor) | 43, 41 |
| `Crash` | required | Crash | 49, 57 |
| `Ride` | required | Ride | 51, 59 |
| `HiHatPedal` | optional | Hi-hat pedal (chick) | 44 |
| `RideBell` | optional | Ride bell | 53 |
| `Splash` | optional | Splash / extra crash | 55 |

### Required minimum

The mapping wizard **must** cover these 9 `PadId` values — practice cannot start without them:

`Kick`, `Snare`, `HiHatClosed`, `HiHatOpen`, `TomHigh`, `TomMid`, `FloorTom`, `Crash`, `Ride`.

These same pads are the highlight zones on the kit SVG in MVP.

### Optional

`HiHatPedal`, `RideBell`, `Splash` — present in GM → PadId and in the enum; the wizard offers them as skippable steps.  
A second crash / splash on the device defaults to binding to the same `Crash` (see multi-key below), not to a separate required pad.

### Multi-key → one PadId

Mapping is **many-to-one**: several MIDI notes (and/or channels) may point to a single `PadId`.

Examples from drumming practice:

- ride edge + crash → both `Crash` (or both `Ride`) if the player treats the hits as interchangeable;
- rimshot + snare head → both `Snare`;
- GM 35 and 36 → both `Kick`.

Rules:

1. `PadMapProfile.bindings` may contain multiple entries with the same `padId` and different `midiNote` values.
2. One MIDI note in a profile → at most one `padId` (conflict → overwrite with a wizard warning).
3. Scoring and matching work only by `PadId`: which key arrived does not matter if both resolve to the same pad.
4. The file-side GM map is also many-to-one (several GM notes → one `PadId`).

The expected timeline stores `PadId` already, not the raw MIDI note (the raw note is kept for debugging).

## General MIDI Percussion

- Standard drum channel: **channel 10** (1-based) / **index 9** (0-based).
- Percussion note range: **35–81** (extensions possible).
- Drumentor uses GM as the **default** file→pad map when building expected hits from a drum track.
- The user **device pad map** is independent: e-drums often send their own note layout.

```text
MIDI file note (GM) --fileMap--> PadId <--deviceMap-- MIDI input note
```

## Drum track detection

### Signals (heuristic)

Each track is assigned a score:

1. **Channel 10** — strong bonus.
2. **Share of notes in range 35–81** — high % → bonus.
3. **Track name** contains `drum`, `drums`, `kit`, `percussion`, `удар` (case-insensitive) — bonus.
4. **Program change** drum bank / standard drum kit — bonus (if present).
5. **Melodic penalty** — many notes outside the percussion range and not channel 10 — penalty.

### Result

- Candidate list sorted by score.
- `suggestedTrackId` = top candidate if score ≥ threshold; otherwise `null` and mandatory manual selection.
- The user can always override.

### Edge cases

| Case | Behavior |
|------|----------|
| Format 0, everything in one track | Filter note events on channel 10 / percussion range 35–81 as a logical drum track |
| Multiple drum tracks | UI multi-select later (v1+); MVP — one track, others ignored for expected |
| Non-GM map in file | Manual pad remap file-side (v1+); MVP — GM assumption + warning |

## ExpectedHit timeline

After selecting a drum track:

1. Collect all noteOn (velocity &gt; 0) → map to `PadId` via the GM file map.
2. Convert tick → `timeMs` via the tempo map.
3. Filter unmapped notes (or bucket into `Other` — MVP: skip for scoring, optional debug).
4. Assign a stable `uid` (index or hash of time+pad+ordinal).

On **speed change** (v1): either scale `timeMs`, or store musical ticks and compute time on the fly. Preferred implementation: store ticks + tempo map, compute `timeMs` with speed applied.

## PadMapProfile

```text
PadMapProfile {
  id
  name                 // "Roland TD-17"
  deviceNameHint       // for auto-suggestion
  schemaVersion        // 1
  bindings: [{ padId, midiNote, channel? }]  // many notes → one padId OK
  createdAt, updatedAt
}
```

- One active profile per session.
- Channel optional: if set — stricter match; if not — any channel.
- Wizard: required pads are mandatory; optional — skippable.
- A profile is valid for practice if every required `PadId` has ≥ 1 binding.
- Multiple bindings for one `padId` are fine (multi-key); duplicate `(midiNote, channel)` — error / overwrite.

## IncomingHit

```text
IncomingHit {
  padId        // after PadMapper; None if unmapped
  rawNote
  rawChannel
  velocity
  timeMs       // session time (after latency offset)
}
```

Unmapped → does not participate in HitMatcher (not Miss, not Wrong); UI may flash “unmapped”.

## Hit matching (entities)

The matcher holds:

- `expected[]` — not yet matched and not expired
- for each `IncomingHit` with a `padId`:
  - find the nearest expected with the same `padId` within ±`okWindowMs`
  - if found → Judgement by |delta| (SCORING), mark matched
  - if not found → `Wrong` (or ignore if there is no nearby expected at all — see SCORING policy)
- expected with no match by `time + okWindow` → `Miss`

Window and aggregate details live only in SCORING; here we fix that matching is **1:1** (one incoming closes one expected).

## Visual model: kit and highway

### Practice screen layout

- Two vertical columns: **left** `DrumKitView`, **right** note highway.
- Transport sits above/below the main split, not inside the columns.

### Kit (SVG)

- Each interactive SVG zone has `data-pad-id={PadId}`.
- Zone states: `idle | preview | hit | judgementPerfect | judgementMiss | ...`.
- `preview` — lookahead expected; `hit` — confirmed incoming or playback ghost.

### Note highway

- One vertical **lane** per displayed `PadId` (lane order is fixed by the UI; unused pads may be hidden).
- A note appears at the top of the lane and moves down at a constant (or speed-scaled) approach rate.
- **Hit-line** — horizontal line near the bottom of the highway; when `note.timeMs == session.now` the note center aligns with the hit-line.
- Hit-line is **draggable on Y**; persisted in settings (`hitLineY` / normalized height fraction).
- Changing `hitLineY` only changes approach length (and, at the same `approachMs`, visual speed in px/s), **not** musical timing.
- Lookahead window for the highway ≥ approach time (how many ms the note is visible before the hit-line); UI requests/uses `upcomingHits` in that window.

### Kit ↔ highway link

- One `ExpectedHit` → simultaneously a lane note and (optionally) kit preview highlight.
- Judgement feedback may appear on both kit and highway (P1).

## Domain versioning

- `PadId` and pad map schema are versioned (`schemaVersion: 1`).
- New pads in v2 are added without removing old ids.
- SQLite migrations are documented when the code appears.
