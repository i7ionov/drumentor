import { useEffect, useMemo, useRef, useState } from "react";
import {
  KIT_PADS,
  REQUIRED_PAD_IDS,
  type PadBinding,
  type PadId,
  type PadMapProfile,
} from "../domain/types";
import {
  fetchExpectedHits,
  listenMidiNoteOn,
  savePadMap,
  setActivePadMap,
} from "../lib/tauri";
import { useAppStore } from "../store/appStore";
import styles from "./PadMapWizard.module.css";

type WizardStep =
  | { kind: "name" }
  | { kind: "pad"; padId: PadId; required: boolean }
  | { kind: "review" };

/**
 * Wizard order: map HH pedal before closed/open so we can ignore pedal
 * noteOns while capturing closed (and open) hat hits.
 */
const WIZARD_PAD_ORDER: { padId: PadId; required: boolean }[] = [
  { padId: "kick", required: true },
  { padId: "snare", required: true },
  { padId: "hiHatPedal", required: false },
  { padId: "hiHatClosed", required: true },
  { padId: "hiHatOpen", required: true },
  { padId: "tomHigh", required: true },
  { padId: "tomMid", required: true },
  { padId: "floorTom", required: true },
  { padId: "crash", required: true },
  { padId: "ride", required: true },
  { padId: "rideBell", required: false },
  { padId: "splash", required: false },
];

const PAD_STEPS: WizardStep[] = WIZARD_PAD_ORDER.map(({ padId, required }) => ({
  kind: "pad",
  padId,
  required,
}));

/** Steps where pedal noteOns must not steal the binding. */
const IGNORE_PEDAL_ON: ReadonlySet<PadId> = new Set([
  "hiHatClosed",
  "hiHatOpen",
]);

/** Optional pad → stand-in when skipped / no dedicated cymbal. */
const STAND_IN_FOR: Partial<Record<PadId, PadId>> = {
  splash: "crash",
  rideBell: "ride",
};

function padLabel(padId: PadId): string {
  return KIT_PADS.find((p) => p.id === padId)?.label ?? padId;
}

function isPedalNote(bindings: PadBinding[], note: number, channel: number): boolean {
  return bindings.some(
    (b) =>
      b.padId === "hiHatPedal" &&
      b.midiNote === note &&
      (b.channel == null || b.channel === channel),
  );
}

function padPrompt(padId: PadId): { title: string; hint: string } {
  switch (padId) {
    case "hiHatPedal":
      return {
        title: "Press the hi-hat pedal (chick)",
        hint: "Stomp the pedal only — do not hit the cymbal. Skip if your kit has no chick.",
      };
    case "hiHatClosed":
      return {
        title: "Hit closed hi-hat",
        hint: "Hold the pedal down, then strike the hi-hat. Pedal notes are ignored. Hit every key you want, then Next.",
      };
    case "hiHatOpen":
      return {
        title: "Hit open hi-hat",
        hint: "Release the pedal, then strike the hi-hat. Pedal notes are ignored. Hit every key you want, then Next.",
      };
    case "crash":
      return {
        title: "Hit Crash (and stand-ins)",
        hint: "Map every physical pad that should count as Crash — crash, splash, ride edge, etc. Each hit adds a key. Press Next when done.",
      };
    case "ride":
      return {
        title: "Hit Ride",
        hint: "Map ride bow / edge keys that should count as Ride. Extra keys that already go to Crash can stay on Crash. Press Next when done.",
      };
    case "splash":
      return {
        title: "Splash (optional)",
        hint: "No splash? Skip — song splash notes will count as Crash. Or hit a dedicated splash pad. Hitting Crash again also means Skip.",
      };
    case "rideBell":
      return {
        title: "Ride Bell (optional)",
        hint: "No ride bell? Skip — song ride-bell notes will count as Ride. Hitting Ride again also means Skip.",
      };
    default:
      return {
        title: `Hit ${padLabel(padId)}`,
        hint: "You can hit several keys for this pad, then press Next.",
      };
  }
}

function emptyProfile(deviceHint?: string): PadMapProfile {
  return {
    id: crypto.randomUUID(),
    name: deviceHint ? `${deviceHint} map` : "My kit",
    deviceNameHint: deviceHint,
    schemaVersion: 1,
    bindings: [],
    createdAt: "",
    updatedAt: "",
  };
}

function isComplete(bindings: PadBinding[]): boolean {
  return REQUIRED_PAD_IDS.every((padId) =>
    bindings.some((b) => b.padId === padId),
  );
}

export function PadMapWizard() {
  const wizardOpen = useAppStore((s) => s.wizardOpen);
  const setWizardOpen = useAppStore((s) => s.setWizardOpen);
  const setWizardTargetPad = useAppStore((s) => s.setWizardTargetPad);
  const flashPad = useAppStore((s) => s.flashPad);
  const midiPorts = useAppStore((s) => s.midiPorts);
  const selectedMidiPortId = useAppStore((s) => s.selectedMidiPortId);
  const setError = useAppStore((s) => s.setError);

  const deviceHint = useMemo(() => {
    if (!selectedMidiPortId) return undefined;
    return midiPorts.find((p) => p.id === selectedMidiPortId)?.name;
  }, [midiPorts, selectedMidiPortId]);

  const [profileName, setProfileName] = useState("My kit");
  const [bindings, setBindings] = useState<PadBinding[]>([]);
  const [stepIndex, setStepIndex] = useState(0);
  const [conflict, setConflict] = useState<string | null>(null);
  const [pendingNote, setPendingNote] = useState<{
    note: number;
    channel: number;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const [profileId, setProfileId] = useState(() => crypto.randomUUID());

  const steps: WizardStep[] = useMemo(
    () => [{ kind: "name" }, ...PAD_STEPS, { kind: "review" }],
    [],
  );
  const step = steps[stepIndex] ?? { kind: "review" as const };

  const bindingsRef = useRef(bindings);
  const stepRef = useRef(step);
  bindingsRef.current = bindings;
  stepRef.current = step;

  useEffect(() => {
    if (!wizardOpen) return;
    setProfileName(deviceHint ? `${deviceHint} map` : "My kit");
    setProfileId(crypto.randomUUID());
    setBindings([]);
    setStepIndex(0);
    setConflict(null);
    setPendingNote(null);
  }, [wizardOpen, deviceHint]);

  useEffect(() => {
    if (!wizardOpen) {
      setWizardTargetPad(null);
      return;
    }
    if (step.kind === "pad") {
      setWizardTargetPad(step.padId);
    } else {
      setWizardTargetPad(null);
    }
  }, [wizardOpen, step, setWizardTargetPad]);

  const goNext = () => {
    setConflict(null);
    setPendingNote(null);
    setStepIndex((i) => Math.min(i + 1, steps.length - 1));
  };

  const commitBinding = (
    padId: PadId,
    midiNote: number,
    channel: number,
    overwrite: boolean,
  ) => {
    setBindings((prev) => {
      let next = overwrite
        ? prev.filter(
            (b) =>
              !(
                b.midiNote === midiNote &&
                (b.channel == null || b.channel === channel)
              ),
          )
        : prev;
      const duplicate = next.some(
        (b) =>
          b.padId === padId &&
          b.midiNote === midiNote &&
          (b.channel == null || b.channel === channel),
      );
      if (!duplicate) {
        next = [...next, { padId, midiNote }];
      }
      return next;
    });
    flashPad(padId);
    setConflict(null);
    setPendingNote(null);
    // Stay on step so multiple keys can map to one pad; user presses Next.
  };

  useEffect(() => {
    if (!wizardOpen) return;

    let disposed = false;
    let unlisten: (() => void) | undefined;

    void listenMidiNoteOn((ev) => {
      if (disposed) return;
      const current = stepRef.current;
      if (current.kind !== "pad") return;

      // Closed/open hat: ignore already-mapped pedal noteOns (user holds pedal first).
      if (
        IGNORE_PEDAL_ON.has(current.padId) &&
        isPedalNote(bindingsRef.current, ev.note, ev.channel)
      ) {
        return;
      }

      const existing = bindingsRef.current.find(
        (b) =>
          b.midiNote === ev.note &&
          (b.channel == null || b.channel === ev.channel),
      );

      // Optional splash/rideBell already covered by Crash/Ride stand-in → skip step.
      const standIn = STAND_IN_FOR[current.padId];
      if (standIn && existing?.padId === standIn) {
        flashPad(standIn);
        goNext();
        return;
      }

      if (existing && existing.padId !== current.padId) {
        setPendingNote({ note: ev.note, channel: ev.channel });
        setConflict(
          `Note ${ev.note} is already mapped to ${padLabel(existing.padId)}. Overwrite?`,
        );
        return;
      }
      commitBinding(current.padId, ev.note, ev.channel, false);
    }).then((fn) => {
      if (disposed) {
        fn();
        return;
      }
      unlisten = fn;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
    // Stable subscription for the lifetime of the open wizard.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wizardOpen]);

  const close = () => {
    setWizardOpen(false);
  };

  const onSave = async () => {
    if (!isComplete(bindings)) return;
    setSaving(true);
    try {
      const base = emptyProfile(deviceHint);
      const profile: PadMapProfile = {
        ...base,
        id: profileId,
        name: profileName.trim() || base.name,
        bindings,
      };
      const saved = await savePadMap(profile);
      await setActivePadMap(saved.id);
      await fetchExpectedHits();
      close();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  if (!wizardOpen) return null;

  const bindingsForCurrent =
    step.kind === "pad"
      ? bindings.filter((b) => b.padId === step.padId)
      : [];

  const canNextRequired =
    step.kind !== "pad" ||
    !step.required ||
    bindingsForCurrent.length > 0;

  return (
    <div className={styles.backdrop} role="dialog" aria-modal aria-label="Pad mapping wizard">
      <div className={styles.panel}>
        <header className={styles.header}>
          <h2>Map pads</h2>
          <button type="button" className={styles.close} onClick={close}>
            Cancel
          </button>
        </header>

        {!selectedMidiPortId && (
          <p className={styles.warn}>
            Select a MIDI input port first, then hit pads when prompted.
          </p>
        )}

        {step.kind === "name" && (
          <div className={styles.body}>
            <p className={styles.prompt}>Name this mapping profile.</p>
            <label className={styles.field}>
              Profile name
              <input
                value={profileName}
                onChange={(e) => setProfileName(e.target.value)}
                autoFocus
              />
            </label>
            {deviceHint && <p className={styles.meta}>Device: {deviceHint}</p>}
            <div className={styles.actions}>
              <button type="button" className={styles.primary} onClick={goNext}>
                Next
              </button>
            </div>
          </div>
        )}

        {step.kind === "pad" && (
          <div className={styles.body}>
            {(() => {
              const prompt = padPrompt(step.padId);
              return (
                <>
                  <p className={styles.prompt}>{prompt.title}</p>
                  {prompt.hint && <p className={styles.meta}>{prompt.hint}</p>}
                </>
              );
            })()}
            <p className={styles.meta}>
              {step.required ? "Required" : "Optional — you can skip"}
              {bindingsForCurrent.length > 0 &&
                ` · mapped notes: ${bindingsForCurrent.map((b) => b.midiNote).join(", ")}`}
            </p>

            {conflict && pendingNote && (
              <div className={styles.conflict}>
                <span>{conflict}</span>
                <div className={styles.conflictActions}>
                  <button
                    type="button"
                    className={styles.primary}
                    onClick={() =>
                      commitBinding(
                        step.padId,
                        pendingNote.note,
                        pendingNote.channel,
                        true,
                      )
                    }
                  >
                    Overwrite
                  </button>
                  <button
                    type="button"
                    className={styles.secondary}
                    onClick={() => {
                      setConflict(null);
                      setPendingNote(null);
                    }}
                  >
                    Keep existing
                  </button>
                </div>
              </div>
            )}

            <div className={styles.actions}>
              <button
                type="button"
                className={styles.secondary}
                onClick={() => setStepIndex((i) => Math.max(0, i - 1))}
              >
                Back
              </button>
              {!step.required && (
                <button type="button" className={styles.secondary} onClick={goNext}>
                  Skip
                </button>
              )}
              <button
                type="button"
                className={styles.primary}
                disabled={!canNextRequired}
                onClick={goNext}
              >
                Next
              </button>
            </div>
          </div>
        )}

        {step.kind === "review" && (
          <div className={styles.body}>
            <p className={styles.prompt}>Review bindings</p>
            <ul className={styles.list}>
              {KIT_PADS.map((pad) => {
                const notes = bindings
                  .filter((b) => b.padId === pad.id)
                  .map((b) => b.midiNote);
                const standIn = STAND_IN_FOR[pad.id];
                const coveredByStandIn =
                  notes.length === 0 &&
                  standIn &&
                  bindings.some((b) => b.padId === standIn);
                return (
                  <li key={pad.id}>
                    <span>{pad.label}</span>
                    <span>
                      {notes.length > 0
                        ? notes.join(", ")
                        : coveredByStandIn
                          ? `→ ${padLabel(standIn)}`
                          : pad.required
                            ? "missing"
                            : "—"}
                    </span>
                  </li>
                );
              })}
            </ul>
            {!isComplete(bindings) && (
              <p className={styles.warn}>
                Map all required pads before saving.
              </p>
            )}
            <div className={styles.actions}>
              <button
                type="button"
                className={styles.secondary}
                onClick={() => setStepIndex((i) => Math.max(0, i - 1))}
              >
                Back
              </button>
              <button
                type="button"
                className={styles.primary}
                disabled={!isComplete(bindings) || saving}
                onClick={() => void onSave()}
              >
                {saving ? "Saving…" : "Save & activate"}
              </button>
            </div>
          </div>
        )}

        <footer className={styles.footer}>
          Step {stepIndex + 1} / {steps.length}
        </footer>
      </div>
    </div>
  );
}
