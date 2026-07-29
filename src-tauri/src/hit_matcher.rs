use crate::domain::{
    HitCounts, Judgement, JudgementEvent, PadId, SessionSummary, WINDOW_OK_MS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Open,
    Matched,
    Missed,
    Skipped,
}

#[derive(Debug, Clone)]
struct ExpectedSlot {
    uid: u64,
    pad_id: PadId,
    time_ms: u64,
    state: SlotState,
}

/// Matches incoming pad hits against expected timeline (docs/SCORING.md).
#[derive(Debug)]
pub struct HitMatcher {
    slots: Vec<ExpectedSlot>,
    counts: HitCounts,
    timing_deltas: Vec<i64>,
    started_at: String,
    song_path: Option<String>,
    drum_track_id: Option<u16>,
    active: bool,
}

impl HitMatcher {
    pub fn start(
        expected: &[crate::domain::ExpectedHit],
        position_ms: u64,
        started_at: String,
        song_path: Option<String>,
        drum_track_id: Option<u16>,
    ) -> Self {
        let slots = expected
            .iter()
            .map(|h| ExpectedSlot {
                uid: h.uid,
                pad_id: h.pad_id,
                time_ms: h.time_ms,
                state: if h.time_ms < position_ms {
                    SlotState::Skipped
                } else {
                    SlotState::Open
                },
            })
            .collect();
        Self {
            slots,
            counts: HitCounts::default(),
            timing_deltas: Vec::new(),
            started_at,
            song_path,
            drum_track_id,
            active: true,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn live_counts(&self) -> HitCounts {
        self.counts.clone()
    }

    /// Seek: keep past judgements; re-open/skip relative to new position.
    pub fn reset_open_from(&mut self, position_ms: u64) {
        for slot in &mut self.slots {
            match slot.state {
                SlotState::Matched | SlotState::Missed => {}
                SlotState::Open | SlotState::Skipped => {
                    if slot.time_ms < position_ms {
                        slot.state = SlotState::Skipped;
                    } else {
                        slot.state = SlotState::Open;
                    }
                }
            }
        }
    }

    pub fn on_incoming(&mut self, pad_id: PadId, session_time_ms: u64) -> JudgementEvent {
        let t_in = session_time_ms as i64;
        let window = WINDOW_OK_MS;

        let mut best: Option<(usize, i64)> = None;
        for (idx, slot) in self.slots.iter().enumerate() {
            if slot.state != SlotState::Open || slot.pad_id != pad_id {
                continue;
            }
            let delta = t_in - slot.time_ms as i64;
            let abs = delta.abs();
            if abs > window {
                continue;
            }
            match best {
                None => best = Some((idx, delta)),
                Some((best_idx, best_delta)) => {
                    let best_abs = best_delta.abs();
                    if abs < best_abs
                        || (abs == best_abs && slot.time_ms < self.slots[best_idx].time_ms)
                    {
                        best = Some((idx, delta));
                    }
                }
            }
        }

        if let Some((idx, delta)) = best {
            let abs = delta.abs();
            let judgement = Judgement::from_abs_delta(abs).unwrap_or(Judgement::Ok);
            let uid = self.slots[idx].uid;
            self.slots[idx].state = SlotState::Matched;
            self.bump(judgement);
            self.timing_deltas.push(delta);
            return JudgementEvent {
                expected_uid: Some(uid),
                pad_id,
                judgement,
                delta_ms: Some(delta),
            };
        }

        // Wrong if another pad's open expected is nearby; else Extra.
        let nearby_other = self.slots.iter().any(|slot| {
            slot.state == SlotState::Open
                && slot.pad_id != pad_id
                && (t_in - slot.time_ms as i64).abs() <= window
        });

        if nearby_other {
            self.bump(Judgement::Wrong);
            JudgementEvent {
                expected_uid: None,
                pad_id,
                judgement: Judgement::Wrong,
                delta_ms: None,
            }
        } else {
            self.bump(Judgement::Extra);
            JudgementEvent {
                expected_uid: None,
                pad_id,
                judgement: Judgement::Extra,
                delta_ms: None,
            }
        }
    }

    pub fn expire_misses(&mut self, now_ms: u64) -> Vec<JudgementEvent> {
        let mut events = Vec::new();
        let now = now_ms as i64;
        for slot in &mut self.slots {
            if slot.state != SlotState::Open {
                continue;
            }
            if now > slot.time_ms as i64 + WINDOW_OK_MS {
                slot.state = SlotState::Missed;
                self.counts.miss += 1;
                events.push(JudgementEvent {
                    expected_uid: Some(slot.uid),
                    pad_id: slot.pad_id,
                    judgement: Judgement::Miss,
                    delta_ms: None,
                });
            }
        }
        events
    }

    /// Expire remaining open as Miss and build summary. Deactivates matcher.
    pub fn finalize(&mut self, ended_at: String) -> SessionSummary {
        let end_ms = self
            .slots
            .iter()
            .map(|s| s.time_ms)
            .max()
            .unwrap_or(0)
            .saturating_add(WINDOW_OK_MS as u64 + 1);
        let _ = self.expire_misses(end_ms);

        let total_expected =
            self.counts.perfect + self.counts.good + self.counts.ok + self.counts.miss;

        let note_accuracy = if total_expected == 0 {
            0.0
        } else {
            (self.counts.perfect + self.counts.good + self.counts.ok) as f64
                / total_expected as f64
        };

        let (timing_mean_ms, timing_abs_mean_ms) = if self.timing_deltas.is_empty() {
            (None, None)
        } else {
            let n = self.timing_deltas.len() as f64;
            let sum: i64 = self.timing_deltas.iter().sum();
            let abs_sum: i64 = self.timing_deltas.iter().map(|d| d.abs()).sum();
            (Some(sum as f64 / n), Some(abs_sum as f64 / n))
        };

        let points = self.counts.perfect as i64 * 100
            + self.counts.good as i64 * 70
            + self.counts.ok as i64 * 40
            + self.counts.miss as i64 * 0
            + self.counts.wrong as i64 * -20;
        let max_points = total_expected as i64 * 100;
        let score_percent = if max_points <= 0 {
            0
        } else {
            let pct = (100.0 * points as f64 / max_points as f64).round() as i64;
            pct.clamp(0, 100) as u32
        };

        self.active = false;

        SessionSummary {
            id: uuid::Uuid::new_v4().to_string(),
            song_path: self.song_path.clone(),
            drum_track_id: self.drum_track_id,
            started_at: self.started_at.clone(),
            ended_at,
            total_expected,
            hit_counts: self.counts.clone(),
            note_accuracy,
            timing_mean_ms,
            timing_abs_mean_ms,
            score_percent,
        }
    }

    fn bump(&mut self, judgement: Judgement) {
        match judgement {
            Judgement::Perfect => self.counts.perfect += 1,
            Judgement::Good => self.counts.good += 1,
            Judgement::Ok => self.counts.ok += 1,
            Judgement::Miss => self.counts.miss += 1,
            Judgement::Wrong => self.counts.wrong += 1,
            Judgement::Extra => self.counts.extra += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ExpectedHit;

    fn hit(uid: u64, pad: PadId, time_ms: u64) -> ExpectedHit {
        ExpectedHit {
            uid,
            pad_id: pad,
            time_ms,
            velocity: 100,
        }
    }

    fn matcher_at(hits: Vec<ExpectedHit>, pos: u64) -> HitMatcher {
        HitMatcher::start(&hits, pos, "0".into(), None, Some(0))
    }

    #[test]
    fn exact_pad_delta_0_perfect() {
        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        let ev = m.on_incoming(PadId::Snare, 1000);
        assert_eq!(ev.judgement, Judgement::Perfect);
        assert_eq!(ev.delta_ms, Some(0));
    }

    #[test]
    fn delta_20_perfect_21_good() {
        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        assert_eq!(
            m.on_incoming(PadId::Snare, 1020).judgement,
            Judgement::Perfect
        );

        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        assert_eq!(m.on_incoming(PadId::Snare, 1021).judgement, Judgement::Good);
    }

    #[test]
    fn delta_80_ok_81_no_match() {
        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        assert_eq!(m.on_incoming(PadId::Snare, 1080).judgement, Judgement::Ok);

        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        // Outside window with no nearby other → Extra
        assert_eq!(m.on_incoming(PadId::Snare, 1081).judgement, Judgement::Extra);
        // Original still open → Miss after expire
        let misses = m.expire_misses(1081);
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].judgement, Judgement::Miss);
    }

    #[test]
    fn two_expected_nearest() {
        let mut m = matcher_at(
            vec![hit(1, PadId::Snare, 1000), hit(2, PadId::Snare, 1100)],
            0,
        );
        let e1 = m.on_incoming(PadId::Snare, 1040);
        assert_eq!(e1.expected_uid, Some(1));
        let e2 = m.on_incoming(PadId::Snare, 1090);
        assert_eq!(e2.expected_uid, Some(2));
    }

    #[test]
    fn extra_in_gap_not_miss() {
        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        let ev = m.on_incoming(PadId::Snare, 500);
        assert_eq!(ev.judgement, Judgement::Extra);
        assert_eq!(m.counts.extra, 1);
        assert_eq!(m.counts.miss, 0);
    }

    #[test]
    fn wrong_does_not_close_expected() {
        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        let ev = m.on_incoming(PadId::Kick, 1005);
        assert_eq!(ev.judgement, Judgement::Wrong);
        assert_eq!(m.counts.wrong, 1);
        // Snare still open — can still hit
        let hit_ev = m.on_incoming(PadId::Snare, 1010);
        assert_eq!(hit_ev.judgement, Judgement::Perfect);
        assert_eq!(hit_ev.expected_uid, Some(1));
    }

    #[test]
    fn wrong_then_miss_if_never_hit() {
        let mut m = matcher_at(vec![hit(1, PadId::Snare, 1000)], 0);
        let _ = m.on_incoming(PadId::Kick, 1005);
        let misses = m.expire_misses(1081);
        assert_eq!(misses.len(), 1);
        assert_eq!(m.counts.miss, 1);
        assert_eq!(m.counts.wrong, 1);
    }

    #[test]
    fn seek_skips_without_miss() {
        let mut m = matcher_at(
            vec![hit(1, PadId::Snare, 1000), hit(2, PadId::Kick, 2000)],
            0,
        );
        m.reset_open_from(1500);
        let summary = m.finalize("1".into());
        // First skipped, second expired as Miss
        assert_eq!(summary.hit_counts.miss, 1);
        assert_eq!(summary.total_expected, 1);
    }

    #[test]
    fn score_percent_weights() {
        let mut m = matcher_at(
            vec![
                hit(1, PadId::Snare, 1000),
                hit(2, PadId::Snare, 2000),
            ],
            0,
        );
        let _ = m.on_incoming(PadId::Snare, 1000); // Perfect
        let _ = m.on_incoming(PadId::Snare, 2050); // Good (50ms)
        let summary = m.finalize("1".into());
        // points = 100 + 70 = 170; max = 200; pct = 85
        assert_eq!(summary.score_percent, 85);
        assert!((summary.note_accuracy - 1.0).abs() < f64::EPSILON);
    }
}
