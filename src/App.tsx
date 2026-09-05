import { useEffect, useState } from "react";
import { ChevronRight, Settings, Volume2 } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import { useMixerStore } from "@/stores/mixer-store";
import { useSessionListWithFadeOut } from "@/hooks/useSessionListWithFadeOut";
import { SessionRow } from "@/components/SessionRow";
import { SettingsView } from "@/components/SettingsView";
import { PermissionNeededView } from "@/components/PermissionNeededView";
import { Wordmark } from "@/components/Wordmark";
import { Button } from "@/components/ui/button";
import { isMac } from "@/lib/platform";
import { beginWindowDrag } from "@/lib/tauri";
import icon from "@/assets/icon.svg";

const FADE_HOLD_MS = 1500;

function App() {
  const sessions = useMixerStore((state) => state.sessions);
  const isLoaded = useMixerStore((state) => state.isLoaded);
  const needsPermission = useMixerStore((state) => state.needsPermission);
  const init = useMixerStore((state) => state.init);
  const setVolume = useMixerStore((state) => state.setVolume);
  const setMuted = useMixerStore((state) => state.setMuted);
  const setBalance = useMixerStore((state) => state.setBalance);
  const maxVolumePercent = useMixerStore((state) => state.maxVolumePercent);
  const outputRoutingSupported = useMixerStore((state) => state.outputRoutingSupported);
  const outputDevices = useMixerStore((state) => state.outputDevices);
  const setSessionOutputDevice = useMixerStore((state) => state.setSessionOutputDevice);

  const [inactiveExpanded, setInactiveExpanded] = useState(false);
  const [showSettings, setShowSettings] = useState(false);

  useEffect(() => {
    init();
  }, [init]);

  const rendered = useSessionListWithFadeOut(sessions, FADE_HOLD_MS);
  // A fading-out session keeps its last known `isActive` value, so it stays
  // in whichever section it was already in while it fades rather than
  // jumping sections on its way out.
  const activeSessions = rendered.filter((session) => session.isActive);
  const inactiveSessions = rendered.filter((session) => !session.isActive);

  const showEmptyState = isLoaded && rendered.length === 0;

  return (
    <AnimatePresence mode="wait" initial={false}>
      {showSettings ? (
        <motion.main
          key="settings"
          initial={{ opacity: 0, x: 12 }}
          animate={{ opacity: 1, x: 0 }}
          exit={{ opacity: 0, x: 12 }}
          transition={{ duration: 0.18, ease: "easeOut" }}
          className="bg-background text-foreground flex h-full min-h-[120px] flex-col"
        >
          <SettingsView onBack={() => setShowSettings(false)} />
        </motion.main>
      ) : (
        <motion.main
          key="mixer"
          initial={{ opacity: 0, x: -12 }}
          animate={{ opacity: 1, x: 0 }}
          exit={{ opacity: 0, x: -12 }}
          transition={{ duration: 0.18, ease: "easeOut" }}
          className="bg-background text-foreground flex h-full min-h-[120px] flex-col overflow-y-auto p-2"
        >
          <div
            className={`mb-1 flex items-center justify-between px-1 ${
              isMac ? "" : "cursor-grab active:cursor-grabbing"
            }`}
            // Windows/Linux have no native title bar to drag from (decorations: false) and no
            // tray-anchored menu-bar convention users already know, unlike macOS -- letting the
            // header itself start a native window drag is how ytaudiobar (a sibling project)
            // solved the same "window feels stuck" complaint there. Left out on macOS, where the
            // window deliberately re-anchors under the menu-bar icon every time it opens instead
            // of being manually positioned, matching Control Center/menu-bar-extra behavior.
            onMouseDown={(event) => {
              if (!isMac && event.button === 0) {
                void beginWindowDrag();
              }
            }}
          >
            <div className="flex items-center gap-1.5">
              <img src={icon} alt="" className="size-4 rounded-[4px]" />
              <Wordmark className="text-muted-foreground text-xs" />
            </div>
            <Button
              variant="ghost"
              size="icon"
              className="size-6"
              onClick={() => setShowSettings(true)}
              onMouseDown={(event) => event.stopPropagation()}
              aria-label="Settings"
            >
              <Settings className="size-3.5" />
            </Button>
          </div>

          {needsPermission ? (
            <PermissionNeededView />
          ) : (
            showEmptyState && (
              <div className="text-muted-foreground flex flex-1 flex-col items-center justify-center gap-2 py-8 text-sm">
                <Volume2 className="size-6 opacity-60" />
                <p>No apps are currently playing audio.</p>
              </div>
            )
          )}

          {/* `mode="popLayout"`: an exiting row is taken out of normal flow immediately (instead
              of holding its layout space until its own exit animation finishes), so the rows
              below it can slide up to fill the gap right away rather than waiting. Each
              `SessionRow` handles its own opacity animation (mount fade-in, `removing` fade-out,
              active/inactive dim) and `layout="position"` (smooth reordering) -- see its own
              comment for why that's position-only, not full layout tracking. */}
          <div className="flex flex-col gap-2">
            <AnimatePresence mode="popLayout" initial={false}>
              {activeSessions.map((session) => (
                <SessionRow
                  key={session.id}
                  session={session}
                  onVolumeChange={setVolume}
                  onMuteToggle={setMuted}
                  onBalanceChange={setBalance}
                  maxVolumePercent={maxVolumePercent}
                  outputRoutingSupported={outputRoutingSupported}
                  outputDevices={outputDevices}
                  onOutputDeviceChange={setSessionOutputDevice}
                />
              ))}
            </AnimatePresence>
          </div>

          {inactiveSessions.length > 0 && (
            <div className="mt-1">
              <button
                type="button"
                onClick={() => setInactiveExpanded((expanded) => !expanded)}
                className="text-muted-foreground hover:text-foreground flex w-full items-center gap-1 rounded-md px-3 py-1.5 text-xs font-medium"
              >
                <motion.span
                  animate={{ rotate: inactiveExpanded ? 90 : 0 }}
                  transition={{ duration: 0.15, ease: "easeOut" }}
                  className="inline-flex"
                >
                  <ChevronRight className="size-3.5" />
                </motion.span>
                Inactive ({inactiveSessions.length})
              </button>

              <AnimatePresence initial={false}>
                {inactiveExpanded && (
                  <motion.div
                    key="inactive-list"
                    initial={{ height: 0, opacity: 0 }}
                    animate={{ height: "auto", opacity: 1 }}
                    exit={{ height: 0, opacity: 0 }}
                    transition={{ duration: 0.2, ease: "easeOut" }}
                    className="overflow-hidden"
                  >
                    <div className="flex flex-col gap-2 pt-2">
                      <AnimatePresence mode="popLayout" initial={false}>
                        {inactiveSessions.map((session) => (
                          <SessionRow
                            key={session.id}
                            session={session}
                            onVolumeChange={setVolume}
                            onMuteToggle={setMuted}
                            onBalanceChange={setBalance}
                            maxVolumePercent={maxVolumePercent}
                            outputRoutingSupported={outputRoutingSupported}
                            outputDevices={outputDevices}
                            onOutputDeviceChange={setSessionOutputDevice}
                          />
                        ))}
                      </AnimatePresence>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
          )}
        </motion.main>
      )}
    </AnimatePresence>
  );
}

export default App;
