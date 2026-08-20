import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, Volume2 } from "lucide-react";
import { useMixerStore } from "@/stores/mixer-store";
import { useSessionListWithFadeOut } from "@/hooks/useSessionListWithFadeOut";
import { SessionRow } from "@/components/SessionRow";

const FADE_HOLD_MS = 1500;

function App() {
  const sessions = useMixerStore((state) => state.sessions);
  const isLoaded = useMixerStore((state) => state.isLoaded);
  const init = useMixerStore((state) => state.init);
  const setVolume = useMixerStore((state) => state.setVolume);
  const setMuted = useMixerStore((state) => state.setMuted);

  const [inactiveExpanded, setInactiveExpanded] = useState(false);

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
    <main className="bg-background text-foreground flex h-full min-h-[120px] flex-col overflow-y-auto p-2">
      {showEmptyState && (
        <div className="text-muted-foreground flex flex-1 flex-col items-center justify-center gap-2 py-8 text-sm">
          <Volume2 className="size-6 opacity-60" />
          <p>No apps are currently playing audio.</p>
        </div>
      )}

      <div className="flex flex-col gap-1">
        {activeSessions.map((session) => (
          <SessionRow
            key={session.id}
            session={session}
            onVolumeChange={setVolume}
            onMuteToggle={setMuted}
          />
        ))}
      </div>

      {inactiveSessions.length > 0 && (
        <div className="mt-1">
          <button
            type="button"
            onClick={() => setInactiveExpanded((expanded) => !expanded)}
            className="text-muted-foreground hover:text-foreground flex w-full items-center gap-1 rounded-md px-3 py-1.5 text-xs font-medium"
          >
            {inactiveExpanded ? (
              <ChevronDown className="size-3.5" />
            ) : (
              <ChevronRight className="size-3.5" />
            )}
            Inactive ({inactiveSessions.length})
          </button>

          {inactiveExpanded && (
            <div className="flex flex-col gap-1">
              {inactiveSessions.map((session) => (
                <SessionRow
                  key={session.id}
                  session={session}
                  onVolumeChange={setVolume}
                  onMuteToggle={setMuted}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </main>
  );
}

export default App;
