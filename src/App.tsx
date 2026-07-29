import { useEffect, useState } from "react";
import { DrumKitView } from "./components/DrumKitView";
import { NoteHighwayView } from "./components/NoteHighwayView";
import { LatencyCalWizard } from "./components/LatencyCalWizard";
import { MixerPanel } from "./components/MixerPanel";
import { PadMapWizard } from "./components/PadMapWizard";
import { SessionSummaryPanel } from "./components/SessionSummaryPanel";
import { TransportBar } from "./components/TransportBar";
import {
  loadAppInfo,
  loadLatencyOffset,
  refreshAudioDevices,
  refreshMidiInputs,
  refreshPadMaps,
  signalSplashReady,
  startTransportListeners,
  syncMetronomePrefs,
} from "./lib/tauri";
import { useAppStore } from "./store/appStore";
import styles from "./App.module.css";

export default function App() {
  const error = useAppStore((s) => s.error);
  const setError = useAppStore((s) => s.setError);
  const [version, setVersion] = useState("…");

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    let cancelled = false;

    void (async () => {
      // Wait until splash can close (backend audio init + this paint).
      await signalSplashReady();
      if (cancelled) return;

      try {
        const info = await loadAppInfo();
        if (!cancelled) setVersion(info.version);
      } catch {
        if (!cancelled) setVersion("dev");
      }

      void refreshMidiInputs();
      void refreshAudioDevices();
      void refreshPadMaps();
      void loadLatencyOffset();
      void syncMetronomePrefs();

      try {
        cleanup = await startTransportListeners();
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
        }
      }
    })();

    return () => {
      cancelled = true;
      cleanup?.();
    };
  }, [setError]);

  return (
    <div className={styles.app}>
      <div className={styles.atmosphere} aria-hidden />
      <TransportBar />
      <main className={styles.main}>
        <DrumKitView />
        <NoteHighwayView />
      </main>
      <MixerPanel />
      <footer className={styles.footer}>
        <span>Drumentor v{version}</span>
      </footer>
      <PadMapWizard />
      <LatencyCalWizard />
      <SessionSummaryPanel />
      {error && (
        <div className={styles.error} role="alert">
          <span>{error}</span>
          <button type="button" onClick={() => setError(null)}>
            Dismiss
          </button>
        </div>
      )}
    </div>
  );
}
