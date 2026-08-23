# Mixolume — Cross-Platform Per-App Volume Mixer
## Project Brief & Build Plan (read this whole document before writing any code)

You are starting a brand-new desktop application from an empty folder. This document is your
complete context — there is no prior conversation, no existing code, no assumed knowledge beyond
what's written here. Read it fully before doing anything.

---

## 1. What we're building

**One sentence:** A small cross-platform desktop utility that lists every application currently
producing sound and gives the user an independent volume slider for each one — instead of a
single system-wide volume control.

**Why it matters:** Windows has this natively (its own Volume Mixer, plus the well-known
third-party app EarTrumpet). macOS and Linux users have no equally good, actively-maintained,
*free*, cross-platform option. Nothing currently ships one unified app that does this well on
Windows, macOS, **and** Linux from a single codebase with a consistent UI. That's the gap this
project fills.

**Product name:** Mixolume. Use this consistently: repo name `mixolume`, product name
"Mixolume", Windows/macOS bundle identifier `com.mixolume.app`, Linux package name `mixolume`.

### Existing competitors (know this landscape before designing anything — don't reinvent worse versions of these)

- **Windows:** EarTrumpet (dominant, free, open source, very polished — the bar to beat),
  BetterTrumpet, Volume2, SoundSplit, ModernFlyouts, "Modern Volume Mixer" (MS Store).
- **macOS:** SoundSource (paid, most polished), Background Music (free, open source, a bit
  dated), FineTune (free, open source), AppVolume, VolumeHub, Fader (`pantafive/fader`, a newer
  free menu-bar mixer — closest direct analog to what we're building, Mac-only today).
- **Linux:** `pavucontrol` (ships with most distros, does this already at the OS level),
  `volctl` (tray-icon version of the same idea).
- **Cross-platform, one app, all three OSes:** nothing polished exists. This is the actual gap.

Do not copy any of these apps' names, logos, or exact visual identity. Do look at EarTrumpet and
SoundSource for interaction-design inspiration (they're the most refined UX in this space).

---

## 2. The core technical reality (read this before planning anything else)

Per-app volume works completely differently on each OS, and the difficulty is **wildly
asymmetric**. This shapes everything about how this project should be sequenced.

### Windows — native OS support, straightforward
Windows Core Audio (WASAPI) exposes audio **sessions** — roughly one per app producing sound —
through documented, public COM interfaces:
- `IMMDeviceEnumerator` → get the default audio render endpoint.
- `IAudioSessionManager2` → `GetSessionEnumerator()` → `IAudioSessionEnumerator` → enumerate
  `IAudioSessionControl` objects (one per active session).
- QueryInterface each session control for `IAudioSessionControl2` to get its **process ID**
  (`GetProcessId()`) and session state.
- QueryInterface the same session for `ISimpleAudioVolume` to **get/set that session's volume**
  and mute state — this is the actual per-app volume control.
- To show a friendly name + icon: use the PID to open the process (`OpenProcess`), get its exe
  path (`QueryFullProcessImageNameW`), then pull the file's `FileDescription` (version info) as
  the display name and extract its icon (`SHGetFileInfo` or `ExtractIconEx`) for the UI.

No elevated privileges needed. No driver. This is genuinely the easy platform here.

**Rust crate:** the official `windows` crate (windows-rs) has bindings for all of the above under
`windows::Win32::Media::Audio` and `windows::Win32::Media::Audio::Endpoints`. Verify current
exact module paths against the crate's docs.rs page when you start implementing — COM interface
paths in windows-rs shift between versions.

### Linux — native OS support via the sound server, straightforward
PulseAudio (and PipeWire, which almost all modern distros now run, via its PulseAudio-compatible
shim `pipewire-pulse`) models audio the same way: as **sink inputs**, roughly one per app
producing sound, each with its own volume you can get/set independently. This is exactly what
`pactl list sink-inputs` and `pavucontrol` already expose.

Two implementation options, in increasing order of quality/complexity:
1. **MVP/fastest path:** shell out to `pactl` (parse `pactl list sink-inputs` output, and set
   volume via `pactl set-sink-input-volume <id> <value>`). Crude but works immediately and needs
   zero FFI.
2. **Proper path:** bind directly to libpulse via the `libpulse-binding` crate (event-driven,
   no polling, no text parsing) — do this once the MVP proves the UX out. `pipewire-rs` is the
   native PipeWire alternative if you want to skip the Pulse-compat shim entirely, but the Pulse
   API path is more universally compatible across older PulseAudio-only systems too.

No elevated privileges needed. No driver.

### macOS — no OS API exists at all. This is the hard one.
There is **no public API** for per-application output volume on macOS. Every tool that does this
(SoundSource, Background Music, FineTune, Fader) uses the same underlying trick:

1. Install a **Core Audio HAL plugin** — a small native driver bundle (not Rust, not something
   Tauri gives you; written against Apple's Core Audio HAL C API) — into
   `/Library/Audio/Plug-Ins/HAL/`.
2. This plugin publishes a **virtual audio output device**.
3. The app sets that virtual device as the **system default output**, so Core Audio routes *all*
   app audio into it.
4. The plugin (or a companion process) identifies which process each audio stream belongs to,
   applies a per-app gain, mixes everything, and forwards the final mix to the real physical
   output device (speakers/headphones).

This requires: admin privileges to install the plugin, restarting `coreaudiod` (or a logout) to
pick it up, and code-signing/notarization for a system-level audio driver — a stricter trust bar
than a normal signed `.app`.

**Do not write this HAL plugin from scratch.** `kyleneideck/BackgroundMusic` on GitHub is open
source and solves exactly this problem — its `BGMDriver` component *is* this HAL plugin, already
built, tested, and battle-proven. The realistic path is to build on top of (or directly adapt)
that driver rather than reinventing Core Audio HAL programming, which is a narrow, gnarly
specialty on its own.

**Before writing a single line of macOS code, do this:**
- Read `kyleneideck/BackgroundMusic`'s actual `LICENSE` file and confirm exactly what it permits
  (redistribution, modification, commercial use, source-disclosure obligations). Do not assume
  a license — check it directly and follow it. If it's copyleft (e.g. GPL-family) and that's
  incompatible with your plans for this project, that changes the plan; flag it back rather than
  proceeding on an assumption.
- Read `BackgroundMusic/DEVELOPING.md` in that repo for the actual driver architecture before
  designing the macOS integration layer.

---

## 3. Tech stack

Use **Tauri v2** (Rust backend + a React/TypeScript frontend). Rationale: one shared UI codebase
across all three OSes, a real systems-programming language (Rust) for the platform-specific
backend work described above, and a small resulting binary/footprint compared to Electron.

- **Backend:** Rust, Tauri v2.
- **Frontend:** React + TypeScript + Tailwind CSS. Use Zustand for any client-side state that
  needs to persist across renders (mirroring common Tauri-app conventions — pick whatever state
  approach is simplest for what's actually a fairly simple UI; don't over-architect this part).
- **Testing:** `cargo test` for Rust (unit-test anything with real logic — session diffing,
  volume math, name/icon resolution fallbacks — mock the OS-level calls behind a trait so pure
  logic is testable without a real audio session running). Vitest + React Testing Library for
  the frontend.
- **CI:** GitHub Actions, a matrix build across `windows-latest`, `macos-latest`,
  `ubuntu-latest` (or `ubuntu-22.04` for wider glibc compatibility), running `cargo test`,
  `cargo check`, `npx tsc --noEmit`, and the frontend test suite on every PR.

---

## 4. Cross-platform architecture

One Tauri app, one repo — **not** separate codebases per OS. The React frontend is 100% shared
and has zero knowledge of which OS it's running on. The Rust backend defines one shared interface
and has three platform-specific implementations behind `#[cfg(target_os = "...")]`:

```rust
// Illustrative shape -- refine as you actually implement each backend.
trait AudioMixerBackend: Send + Sync {
    /// Every app currently known to be producing (or recently produced) sound.
    fn list_sessions(&self) -> Vec<AppSession>;
    /// 0.0 (silent) to 1.0 (full) or higher if the platform allows boosting past 100%.
    fn set_volume(&self, session_id: &str, volume: f32) -> Result<(), MixerError>;
    fn set_muted(&self, session_id: &str, muted: bool) -> Result<(), MixerError>;
}

struct AppSession {
    id: String,           // stable per-app-instance identifier (platform-specific meaning)
    display_name: String,
    icon: Option<Vec<u8>>, // PNG bytes, or a path/URI depending on what's cheapest per-OS
    volume: f32,
    muted: bool,
    is_active: bool,       // producing sound right now vs. present-but-silent
}
```

```
src-tauri/src/
  mixer/
    mod.rs              // trait definition + shared types (AppSession, MixerError)
    windows.rs           // #[cfg(target_os = "windows")] — WASAPI implementation
    linux.rs             // #[cfg(target_os = "linux")]   — PulseAudio/PipeWire implementation
    macos.rs             // #[cfg(target_os = "macos")]   — talks to the HAL driver
  main.rs
src-tauri/macos-driver/   // the native Core Audio HAL plugin sub-project (NOT Rust/Tauri code)
  README.md              // explains it's adapted from kyleneideck/BackgroundMusic's BGMDriver,
                          // with the license terms actually checked and documented here
src/                      // React frontend, shared across all platforms
```

The macOS driver is a genuinely separate native sub-project living in the same repo (much like
how a Windows-only NSIS installer script or a Linux-only `.desktop` file would live alongside
shared code) — it is not Rust, and the Tauri app talks to it via local IPC (a Unix domain socket
or XPC service is the idiomatic macOS choice — research the actual mechanism BackgroundMusic uses
between BGMDriver and BGMApp before choosing your own).

---

## 5. UI/UX spec (v1)

Keep this simple — this is a utility, not a media player. A single small window (or a
menu-bar/tray popover, matching the pattern EarTrumpet and SoundSource both use):

- One row per app currently in the session list: app icon, app name, a volume slider (0–100%),
  a mute toggle.
- Apps that stop producing sound should either fade out of the list after a short delay, or move
  to a collapsed "inactive" section rather than disappearing instantly — abrupt list reflow while
  the user's mouse is on a slider is a real, easily-avoidable UX bug.
- Live-updating: the list and each slider's displayed value should reflect external changes
  (e.g. the user changed volume from the OS's own native mixer) without requiring a manual
  refresh.
- A system tray/menu-bar icon as the primary entry point; clicking it toggles the window, similar
  to how YTAudioBar's own tray-icon-summons-a-popover pattern works, if you want a concrete
  reference behavior to replicate.
- No onboarding wizard, no accounts, no cloud sync. This is a local utility.

---

## 6. Recommended build phases

This is a recommendation, not a mandate — reorder if there's a good reason, but sequencing
Windows and Linux before macOS is deliberate: they share almost no OS-specific complexity with
each other (both are "call a documented API"), let you validate the shared UI/IPC contract fast,
and de-risk the project before committing to the much larger macOS driver effort.

1. **Phase 0 — scaffolding.** `npm create tauri-app`, repo structure above, CI skeleton, empty
   `AudioMixerBackend` trait with a fake/mock implementation so the UI can be built against real
   data shapes before any real OS integration exists.
2. **Phase 1 — Windows backend.** Real WASAPI session enumeration + volume control, wired to the
   real UI. This is your first fully-working platform.
3. **Phase 2 — Linux backend.** PulseAudio/PipeWire sink-input implementation. Reuse everything
   from Phase 1 except the backend module.
4. **Phase 3 — macOS backend.** The HAL driver integration. Budget significantly more time here
   than the other two combined — this is real systems/audio-driver work, not "another cfg
   block."
5. **Phase 4 — polish & release.** Icon/branding, auto-updater, installers per platform (NSIS for
   Windows, a signed `.app`/`.dmg` for macOS — remember `TAURI_BUNDLER_DMG_IGNORE_CI=true` needs
   to be set on the CI build step, or the dmg's custom background/icon layout gets silently
   skipped because Tauri auto-detects `CI=true` and disables the AppleScript styling step; verify
   this is still accurate for whatever Tauri version you're on by checking
   `tauri-apps/tauri-action#740` and `tauri-apps/tauri#592` at build time), `.deb`/`.rpm`/AppImage
   for Linux.

---

## 7. Engineering process (mirror what's already proven to work, not just a suggestion)

Set this up from day one, not as an afterthought:

- **Branching:** `main` = stable, protected (PR + passing CI required, enforced even for
  admins, no direct pushes, no force-push, no branch deletion). Feature branches per change,
  auto-deleted on merge. If/when a feature (e.g. the macOS driver) needs real-world testing
  before it's trusted, use a long-lived `beta` branch with the same protection, and only merge
  into `main` once verified — don't let unverified, risky work land directly on `main`.
- **Commit style:** conventional-ish prefixes (`fix:`, `feat:`, `chore:`, `docs:`), messages that
  explain *why*, not just what.
- **CI required checks:** structure them so a check that's skipped (e.g. backend tests on a
  frontend-only PR, via a paths-filter job) is still treated as passing, not blocking — a naive
  matrix-job required-check setup can permanently block unrelated PRs if you're not careful (this
  exact mistake is easy to make with matrix builds — give the whole matrix job a single
  fan-in "status" job with a stable name and require *that*, not the matrix's per-leg names,
  which only exist when that leg actually ran).
- **Docs to have from the start:** `CONTRIBUTING.md`, `SECURITY.md` (with a private
  vulnerability-reporting path, not a public issue), `CODE_OF_CONDUCT.md`, PR/issue templates,
  `CHANGELOG.md`, a whats-new-style in-app changelog dialog for user-facing updates.
- **Dependabot:** enable it for npm, cargo, and github-actions ecosystems from the start, plus
  GitHub's vulnerability alerts + automated security fixes.

---

## 8. Privacy & analytics

If you add any analytics (recommended: minimal, anonymous, opt-nothing-personal), follow this
stance rather than reinventing one:
- An anonymous per-install ID (randomly generated, stored locally, never tied to any account or
  identity) plus OS and app version on each event — nothing else identifying.
- Explicitly document in the README exactly what is and isn't collected, and hold to it.
- Do not add any mechanism that tries to detect or report app uninstallation — it's not
  reliably possible cross-platform (Windows/Linux package managers could support an uninstall
  hook; macOS categorically cannot, since removing a `.app` is a pure file-delete with no hook
  point at all), and even where it's technically possible, treat it as a low-value, PII-adjacent
  addition to skip in favor of inferring inactivity from the absence of regular pings over
  time — the only method that's actually consistent across all three platforms.

---

## 9. How you (the AI agent) should operate on this project

- Verify OS-API claims before implementing them, especially anything above about exact
  interface/method names, crate module paths, or the macOS driver's IPC mechanism — this
  document was researched via web search at planning time, but library/API surfaces shift; check
  current docs before writing code against them, don't just trust this document as gospel forever.
- Ask when genuinely blocked — a decision only the human maintainer can make (scope,
  licensing tradeoffs, whether to pursue macOS at all given its cost) — rather than guessing and
  proceeding.
- Write tests for real logic, not for trivial pass-throughs. Mock the OS layer behind the
  `AudioMixerBackend` trait so session-list diffing, volume clamping, name/icon-fallback logic,
  etc. are unit-testable without a real audio session.
- Don't scope-creep. This is a focused utility. No feature not described above should get
  added without it being a deliberate, explicit decision — not something that quietly appears
  because it seemed easy while implementing something else.
- Commit incrementally, in the logical units described in the phases above, with clear
  messages — not one giant commit per phase.
- Cite sources for any external technical claim (API docs, license terms, licensing
  interpretation) rather than asserting from memory alone, the same way this document does.

---

## 10. Notes accumulated during the initial autonomous build session (2026-08-20)

- **macOS approach (superseded once, see below):** the first pass built the macOS backend against
  an independently-installed `kyleneideck/BackgroundMusic` (GPLv2) `BGMDriver`, reasoning that
  talking to its public HAL property (`kAudioDeviceCustomPropertyAppVolumes`, fourCC `'apvs'`) from
  outside was GPL §2 "mere aggregation," not a derivative work. That design is **no longer used**
  — kept here only as build history. It also left unresolved whether Mixolume needed the user to
  separately run BGMApp (which owned default-device switching + passthrough audio) or had to
  reimplement that role itself.
- **Current macOS approach:** Apple's own **Core Audio Process Tap API**
  (`CATapDescription`/`AudioHardwareCreateProcessTap`, macOS 14.2+, matured by 14.4) replaces the
  BackgroundMusic dependency entirely — confirmed against two real, working MIT-licensed reference
  implementations (`altuzar/sonicflow`, `insidegui/AudioCap`), not guessed from memory. This is a
  public Apple framework: no GPL exposure, no third-party driver install, no admin privileges, no
  `coreaudiod` restart, and the audio-routing question is fully solved (a private capture aggregate
  taps each app with a real mute of its normal output path, bridged via a lock-free ring buffer to
  a second IOProc on the real output device) rather than left open. Trade-off: macOS 14.2+ only,
  no fallback for older systems. See `src-tauri/macos-audio/README.md` for the full architecture,
  citations, and what a Mac-equipped contributor needs to verify first — `macos.rs` is real,
  carefully-written code but has never been compiled; no Mac was available in this build session.
- Reference project `C:\Users\ianida\personal\ytaudiobar` (same author, Tauri v2 + React 19 +
  Tailwind v4 + Zustand) supplied the tray-icon-toggle pattern, the CI paths-filter fan-in-job
  pattern (to avoid the exact required-check trap this document warns about above), and general
  stack versions — reused deliberately rather than reinvented.

## 11. macOS bugs found and fixed on real hardware (2026-08-21)

A Mac became available this session; the two items below were only discoverable by actually
running the compiled code, not by reading it.

- **Crash on every launch, real audio never worked:** `AudioDeviceCreateIOProcIDWithBlock`'s block
  argument was built as `&block as *const _ as *mut _` (a pointer to the `RcBlock<F>` wrapper
  struct on the Rust stack, not to the heap-allocated `Block<F>`/Objective-C block object it
  wraps) — Core Audio then dereferenced garbage and the process aborted with an ObjC "Attempt to
  use unknown class" fatal, every time. Fixed with `RcBlock::as_ptr(&block)`, block2's own
  documented accessor for the real pointer; this also surfaced a second latent bug (the IOProc
  closure signatures used raw `*mut` pointers where the real generated binding expects
  `NonNull<..>`) that the old unchecked `as` cast had been silently papering over. Verified
  end-to-end afterward: real audio detected, mixed, and played back correctly.
- **One system permission dialog per already-open app, and total audio silence while running:**
  `TapEngine::new` tapped every already-audible process in one tight synchronous loop on first
  launch; each authorization-triggering `AudioHardwareCreateProcessTap` call fired before the
  previous one's TCC decision had propagated, producing N queued dialogs instead of one. Worse,
  each attempt that failed on a still-unauthorized process destroyed every tap already created in
  that attempt and retried from scratch on the next 700ms poll tick — so for the whole window a
  user was clicking through N dialogs, taps were being created and destroyed in a loop, each
  `mutedWhenTapped`-muting its process with no stable reinjection path ever formed, which is why
  *all* system audio went silent until the app quit. Fixed by gating all tap creation behind a
  single `CGPreflightScreenCaptureAccess`/`CGRequestScreenCaptureAccess` check (Apple's public API
  for this exact "Screen & System Audio Recording" TCC category) before `TapEngine::new` does
  anything else.
- **"Permission doesn't persist" / black window -- multi-copy confusion was one real cause, not
  the only one.** First pass: a single clean install (one copy in `/Applications`, fresh TCC
  state, installed via `cp -R`+`xattr -cr`) showed exactly one dialog and rendered correctly,
  which looked like the whole story -- multiple same-bundle-identifier copies on disk (an
  artifact of this session's own rebuild/retest cycle) confusing TCC. That's real and still worth
  avoiding. But it was an *incomplete* diagnosis: `cp -R`+`xattr -cr` never gives the app a
  `com.apple.quarantine` attribute in the first place, which means that test never actually
  exercised **Gatekeeper App Translocation** -- the real mechanism a genuine user hits every time
  they mount the DMG and drag `Mixolume.app` to `/Applications` via Finder (confirmed live: even a
  proper Finder-mediated copy carries quarantine, and `ps` showed the running process executing
  from a randomized `/private/var/folders/.../AppTranslocation/<uuid>/d/mixolume.app` path, not
  `/Applications/mixolume.app`). Contrary to some folklore, moving an app out of a mounted DMG via
  Finder does **not** disable translocation -- that only holds for same-volume moves (e.g.
  `~/Downloads` to `/Applications`); a DMG-to-`/Applications` copy is inherently cross-volume, so
  translocation triggers on every fresh install regardless of *how* the user drags it in. Once
  translocated, the app's on-disk path is different (and re-randomized) on every single launch,
  which is why permission looked like it "didn't persist" and the WKWebView reliably rendered
  black -- both are downstream of running from that constantly-shifting sandboxed path. Confirmed
  fix: stripping quarantine (`xattr -cr`) **before the app's first launch** keeps it running from
  its real installed path and both symptoms disappear. This is a direct consequence of shipping
  unsigned/ad-hoc-signed builds (no Apple Developer ID configured in this repo yet) -- proper code
  signing + notarization removes translocation entirely, which is the real, durable fix. Until
  that's set up, users need to manually strip quarantine after every fresh install; release notes
  document this, though it's a genuinely poor first-run experience for anyone not comfortable with
  Terminal.
- **A real, distinct bug found the same session: Mixolume tapped itself.** `is_hidden_system_bundle`
  filtering was only applied when building the UI's session list, never before
  `Self::reconcile_engine`, which decides what to actually tap. Once `PlaybackTap` genuinely wrote
  real audio into the output device, Core Audio reported Mixolume's own process as
  `is_running_output = true`, so the engine tapped itself too -- confirmed live via a temporary
  debug build (`[playback-debug]`/`[probe2]` instrumentation, since removed) showing the engine
  destroying and rebuilding itself roughly once per second, indefinitely, which never gave a
  freshly-created playback IOProc a stable window to run even once. Fixed by filtering
  `list_audio_processes()`'s result by `std::process::id()` in `list_sessions` before it ever
  reaches `reconcile_engine`, not just before display. Verified fixed: with the filter in place,
  the engine builds exactly once and stays stable for a real, continuously-playing app
  (`YTAudioBar`) rather than churning.
- **Still open after the above two fixes: the playback IOProc can fail to ever receive a single
  callback, even with a stable (non-churning) engine, capture actively writing to the ring buffer,
  and `AudioDeviceCreateIOProcIDWithBlock`/`AudioDeviceStart` both reporting success (status 0) on
  a verified-correct device (confirmed real `BuiltInSpeakerDevice`, sane 512-frame buffer / 44100Hz
  sample rate -- not a stale/reused `AudioObjectID`).** Ruled out: stale `coreaudiod` state (a
  fresh restart, confirmed via new PID, didn't fix it), orphaned aggregate devices from earlier
  killed test runs (a standalone Swift diagnostic enumerating `kAudioHardwarePropertyDevices`
  found none), and a sequencing/architecture mismatch against the reference implementation (a
  side-by-side fetch of sonicflow's actual `AggregateOutputDevice.swift`/`PlaybackDevice.swift`/
  `AudioGainController.swift` from GitHub confirmed our Rust port matches its structure and
  call-ordering closely). The one concrete lead: `log show --predicate 'process == "coreaudiod"'`
  during a real Mixolume session showed `com.apple.audioanalytics:carc` overload-diagnostic
  messages with `"cause": ClientTimeout`, `"overload_type": ClientTimeoutStart`, and
  `"num_continuous_silent_io_cycles": 63999` for the real output device specifically (not the
  private aggregate, which never showed this) -- i.e. Core Audio's own realtime-scheduling watchdog
  considers our registered client on the *real hardware device* unable to meet its deadline and
  is silencing it, despite every API call we make reporting success. This smells like a realtime
  thread / `AudioWorkgroup` participation issue specific to registering a direct IOProc on a
  shared physical device (as opposed to a private aggregate device the app fully owns, which
  works reliably) -- worth investigating `kAudioDevicePropertyIOThreadOSWorkgroup` /
  `os_workgroup_join` next, though it's unconfirmed whether that's actually required for
  block-based (`AudioDeviceCreateIOProcIDWithBlock`) registration or only for the raw
  function-pointer API. Also unconfirmed: whether this is stable, reproducible OS/hardware
  behavior or itself an artifact of this same session's ~10+ aggregate device create/destroy
  cycles (a full reboot, not just a `coreaudiod` restart, was the next step to rule that out, but
  wasn't done this session). A Mac-equipped contributor picking this up should start with a clean
  reboot before assuming the workgroup theory is the real fix.

## 12. Menu-bar-native UI pass, and a real memory leak found along the way (2026-08-22)

- **Session list now shows real app names/icons, not raw bundle ids.** `resolve_app_info` (in
  `macos.rs`) resolves both via `NSRunningApplication` -- `.localizedName()` for the name,
  `.icon()` re-encoded to PNG via `NSImage::TIFFRepresentation` -> `NSBitmapImageRep` ->
  `representationUsingType_properties(.PNG)` for the icon (there's no direct "give me PNG bytes"
  call on `NSImage`). The frontend (`SessionIcon.tsx`/`iconUrl.ts`) already had full support for
  `iconPng` waiting unused -- it just needed the backend to populate it.
- **Real bug found via this feature, not hypothetically: re-resolving name+icon on every single
  700ms poll tick, for every active session, leaked memory catastrophically** -- observed 15GB+
  RSS within a few minutes on real hardware, discovered while chasing an unrelated "window shows
  black" report. `NSRunningApplication`/`NSImage`/`NSBitmapImageRep` calls create autoreleased
  temporary objects, and the poll loop runs on a plain `tauri::async_runtime::spawn` (tokio)
  background thread that never pushes an autorelease pool to drain them. Fixed by caching results
  by pid in `Inner::app_info_cache` -- a process's name/icon don't change during its lifetime, so
  there's no reason to re-resolve every tick -- with stale entries pruned once their pid exits.
  The underlying "no autorelease pool on this thread" gap is still there in principle; caching
  just makes the call frequency low enough (once per process, ever) that it's negligible. A more
  thorough fix would wrap the AppKit calls in an actual `@autoreleasepool`-equivalent.
- **Dock hidden via `AppHandle::set_activation_policy(ActivationPolicy::Accessory)`** in `setup()`
  -- confirmed via `osascript`/System Events (`background only` process query), not just assumed
  from the API docs.
- **Tray-anchored window positioning + hide-on-blur.** Clicking the tray icon positions the
  window directly under it (via `TrayIconEvent::Click`'s `rect` / `TrayIcon::rect()`, a
  `tauri::Rect` with DPI-aware `Position`/`Size` -- converted through the window's own
  `scale_factor()`, not used as raw physical pixels). `on_window_event` +
  `WindowEvent::Focused(false)` hides the window on focus loss, matching Tauri's own documented
  example for exactly this "native popover" pattern.
- **Fully undecorated window (no title bar / traffic-light buttons): now shipped, resolved
  2026-08-22.** Originally documented above as an open gap after `decorations: false` +
  `transparent: true` repeatedly produced a solid black WKWebView content area with several fixes
  attempted (adding `transparent: true`, an explicit `backgroundColor`, pinning
  `tauri`/`tauri-runtime-wry`/`wry` to a sibling app's exact versions -- none of it helped, and the
  version-pinning attempt broke the build outright via a cross-crate trait incompatibility).
  - **Real root cause: unrelated to `decorations`/`transparent` at all.** A `window.open_devtools()`
    call added in `setup()` (to get real WebKit console output while diagnosing this exact issue)
    was itself breaking the WKWebView's initial content paint in the *bundled* release build --
    opening devtools before/during the page's first load left the content layer never composited,
    while the window's own CSS background (`#root`'s `bg-background`) still painted normally. That
    made every earlier black-screen screenshot, taken with devtools auto-opening, look identical
    regardless of the `decorations`/`transparent` values being tested -- the real variable under
    test was masked by the diagnostic tool itself.
  - Confirmed via pixel-sampling screenshots (`screencapture -x -l<windowID>` targeting the app's
    own window specifically, bypassing the devtools panel entirely): with `open_devtools()` still
    present, the content area was *exactly* one flat color end to end (proven by
    `PIL.Image.getcolors()` returning a single distinct RGB value) even for an unconditional,
    inline-styled debug `<p>` element -- meaning nothing was compositing above the base background,
    not even literally-red text. Removing the `open_devtools()` call alone (keeping
    `decorations:false`+`transparent:true`) immediately fixed it -- content, icons, sliders all
    rendered correctly on the next rebuild.
  - Shipped config: `decorations: false`, `transparent: true`, `shadow: false` (`tauri.conf.json`),
    matching the CSS in `global.css` that already assumed a transparent `html`/`body` with an
    opaque `#root` -- that design intent is now actually realized.
  - Lesson for future diagnosis in this app: don't add `tauri`'s `devtools` feature /
    `open_devtools()` back as a first move on a bundled-build-only rendering bug -- it's a plausible
    cause of exactly this symptom, not just a neutral diagnostic. If devtools is needed again,
    open it manually after the window is confirmed showing content, or gate it behind a delay/user
    action rather than calling it unconditionally in `setup()`.

## 13. Follow-up fixes after shipping the native window look (2026-08-22)

- **Real window transparency requires `macos-private-api`, not just `tauri.conf.json`'s
  `"transparent": true`.** Section 12 shipped with a real bug hiding in it: the rounded corners
  looked right everywhere except the corner cutouts, which sampled as fully opaque white
  (alpha=255) instead of transparent. Root cause, confirmed by reading `tauri-runtime-wry`
  2.11.4's own `Cargo.toml`: `wry`'s entire transparency implementation (disabling WKWebView's
  private `drawsBackground` key, setting `NSWindow.isOpaque = false`) is gated behind wry's own
  `transparent` Cargo feature, which is *only* pulled in via tauri's `macos-private-api` feature
  (bundled with `wry/fullscreen`) -- `tauri.conf.json`'s `"transparent": true` value alone doesn't
  enable it. Fixed by adding `"macos-private-api"` to the `tauri` dependency's features in
  `Cargo.toml` and `"macOSPrivateApi": true` under `app` in `tauri.conf.json`. Also rounded `body`
  (not just `#root`) with matching `border-radius`/`overflow:hidden` in `global.css`, matching the
  working reference app (`ytaudiobar-tauri`) -- rounding `#root` alone left a gap between the
  window's rectangular edge and `#root`'s rounded edge where `body`'s untrimmed rectangle showed
  through.
- **App names for helper subprocesses (e.g. Chromium/Brave's `*.helper` renderer processes)** now
  resolve by walking up the parent-process chain (`libproc::proc_pid::pidinfo::<BSDInfo>`'s
  `pbi_ppid`, since `libc` doesn't expose Darwin's private `kinfo_proc` layout) until an ancestor
  with a real `NSRunningApplication.localizedName` is found, instead of showing the raw bundle id
  (`com.brave.Browser.helper`). See `resolve_named_running_app` in `macos.rs`.
- **Tray-click show/hide was intermittently flaky: sometimes a black/empty window, sometimes
  wrong position, sometimes the whole app silently gone.** Root cause turned out to be three
  separate, compounding issues, only found by actually reading Console logs and process state
  instead of guessing from screenshots alone:
  1. **macOS Automatic Termination was silently killing the app.** Confirmed live: Console showed
     `AutomaticTermination: No windows open yet` followed, a short while later, by a clean
     voluntary exit (code 0, no crash, no signal) with zero user action. An accessory-policy app
     that deliberately keeps its window hidden between tray clicks looks exactly like an idle
     background process macOS is allowed to reap. Fixed by calling
     `NSProcessInfo.processInfo().disableAutomaticTermination(reason:)` in `setup()` (needs
     objc2-foundation's `NSProcessInfo` feature).
  2. **Two instances running at once.** A second copy launched directly from
     `target/release/bundle/macos/mixolume.app` (outside `/Applications`) was running alongside
     the installed one -- two tray icons, two windows, both racing for the same Core Audio taps.
     Always launch/test only the `/Applications` copy; never run the build-output binary directly
     while another instance may already be running.
  3. **An intermittent WKWebView repaint bug on the hide/show cycle itself**, separate from the
     open_devtools issue in section 12 -- a transparent window's content doesn't always
     recomposite reliably after `orderOut`/`orderFront` (tauri-apps/wry#1524 describes the same
     class of bug, though its repro is GTK-specific). Worked around with a 1-point
     resize-and-restore right after `show()` (`nudge_repaint` in `lib.rs`), guarded by a short
     grace period on the hide-on-blur handler (`POST_SHOW_BLUR_GUARD`) so the nudge's own resize
     doesn't trigger a spurious focus-loss blur that immediately hides the window it just showed.
     Also cached the last successfully-computed tray-anchored position (`WindowShowState
     ::last_tray_position`) as a fallback for the (occasional) click where `TrayIconEvent::Click`'s
     `rect` comes back `None`, instead of leaving the window at whatever position it last had.
- **Process isn't literally required to fix "not working" reports by editing code.** More than
  once during this investigation, the user reported the app broken, and it started working again
  on its own after enough real time passed -- without a rebuild or restart -- purely because
  enough poll cycles / Core Audio state settling had happened in the meantime. Before assuming a
  code change fixed something, check whether the same already-running process would have recovered
  anyway; the reliable diagnostic signal is Console logs and process/window state at the exact
  moment of the report, not "I changed something and now it's fine."

## 14. The real root cause of all the "sometimes works, sometimes doesn't" flakiness (2026-08-22/23)

- **Ad-hoc code signing has no stable identity across rebuilds, so macOS's TCC (Screen & System
  Audio Recording permission) treated every single rebuild as a brand-new, never-approved app.**
  This was the actual root cause behind nearly every "app isn't detecting sounds" / "still shows
  black" / "keeps asking for permission even though I already granted it" report across sections
  12-13, not any of the code fixes attempted along the way. Confirmed directly: `codesign -dv
  /Applications/mixolume.app` showed `Identifier=mixolume-bfb4442fc4fac866` -- a hash derived from
  the binary's own contents, not `com.mixolume.app` (the configured bundle id). Ad-hoc signing (no
  `signingIdentity` configured, which is Tauri's default when no certificate is set) has no stable
  "designated requirement", so macOS can't tell that today's rebuild is "the same app" as
  yesterday's even though the bundle id, name, and behavior are identical -- confirmed against
  Apple's own documented behavior (see e.g. https://developer.apple.com/forums/thread/795739). One
  concrete symptom that proved this conclusively: `tccutil reset ScreenCapture com.mixolume.app`
  printed "Successfully reset" **six times** -- six separate stale permission entries had
  accumulated from a single day's worth of rebuilds.
- **Fixed with a local self-signed code-signing certificate**, giving every rebuild the same
  stable identity without needing a paid Apple Developer Program membership:
  1. Generated a self-signed cert: `openssl req -x509 -newkey rsa:2048 -days 3650 -keyout
     dev.key -out dev.crt -nodes -subj "/CN=Mixolume Dev" -addext
     "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=codeSigning"`, converted to
     `.p12` with `openssl pkcs12 -export -legacy` (the `-legacy` flag is required -- without it the
     Keychain import silently fails), imported via `security import ... -T /usr/bin/codesign`.
  2. Trusted it for code signing in Keychain Access (Trust section -> Code Signing -> Always
     Trust) -- this specific step needs a human: it's a system-trust change the permission system
     correctly refuses to automate.
  3. Set `signingIdentity: "Mixolume Dev"` under `bundle.macOS` in `tauri.conf.json`. `tauri
     build` now signs with this identity automatically every time -- confirmed via `codesign -dv`
     showing `Identifier=com.mixolume.app` (the real bundle id) instead of a content hash.
  - **This certificate is local to this machine only** (self-signed, not trusted anywhere else --
    exactly like the reference article this technique came from warns). On a fresh machine or
    reinstall, this whole setup needs to be redone, and `tauri.conf.json`'s `signingIdentity`
    referencing a non-existent certificate will make `tauri build` fail signing until either the
    cert is recreated or the config value is removed/changed. For real distribution to other
    users, this still needs an actual Apple Developer ID + notarization -- this only solves *this
    machine's development-loop* stale-permission pain.
- **Fixed the missing-permission UX properly instead of chasing the symptom further.** The
  frontend was silently swallowing the "waiting for screen & system audio recording permission"
  backend error (just a `console.error`, invisible without devtools open), which is what made every
  one of these incidents look like an unexplained black/empty screen instead of what it actually
  was. Added: `isPermissionError()` in `lib/tauri.ts` (matches the error substring), a
  `needsPermission` flag in the mixer store, and a dedicated `PermissionNeededView` with an "Open
  System Settings" button (`x-apple.systempreferences:com.apple.preference.security?
  Privacy_ScreenCapture`, requiring an explicit scoped `opener:allow-open-url` capability entry
  since that URL scheme isn't in the opener plugin's default allowed schemes). Also **do not
  permanently latch `needsPermission` and stop retrying** -- whether a mid-session grant takes
  effect without a full relaunch turned out to be inconsistent in live testing (sometimes a retry
  did pick it up), so the store keeps checking on an interval and clears the flag the moment a
  check actually succeeds, rather than assuming the worst case is always true and getting stuck
  showing "permission needed" even after the user has genuinely granted it.
- **A real Cargo.toml formatting bug, unrelated to any of the above**: an `Edit` call removing the
  `devtools` feature merged a trailing doc-comment line directly onto the `tauri = {...}`
  dependency line with no newline between them, silently commenting out the entire dependency
  declaration. Symptom was a confusing, unrelated-looking build failure ("missing `cargo:dev`
  instruction, please update tauri to latest") since `tauri` then only resolved transitively
  rather than as a direct dependency. Lesson: when an `Edit`'s `old_string` ends mid-line right
  before a comment block, double check the resulting file for merged lines rather than trusting
  the diff summary alone.
- **Other real fixes shipped alongside the permission work**: `com.apple.systemsoundserverd`
  (macOS's system alert/UI sound player) added to `SYSTEM_BUNDLE_PREFIXES_TO_HIDE` in `macos.rs`
  -- it was showing up in the Inactive list as if it were a real user app. A `[profile.release]`
  with `strip = true` only (aggressive settings tried once -- `opt-level="s"`, `lto = true`,
  `codegen-units = 1`, `panic = "abort"` -- broke live audio session detection immediately and
  were reverted without fully root-causing it; this codebase's `unsafe`-heavy Core Audio FFI in
  `macos.rs` is exactly the kind of code where aggressive cross-crate optimization can surface
  latent bugs that don't show up under normal compilation, so it's not worth the risk for an
  already-small ~3MB binary). A Settings page (`SettingsView.tsx`) with a "Launch at Login" toggle
  via `tauri-plugin-autostart` (real macOS Launch Agent, not an AppleScript hack). Branding: after
  several rounds of a custom V/X hybrid glyph (real "V" text + an SVG overlay of two extending
  lines, tuned interactively via a set of throwaway HTML/JS editors in `/tmp` rather than
  round-tripping full app rebuilds for each tweak), the final decision was to drop the custom
  glyph entirely and just ship plain "MiXolume" text (`Wordmark.tsx`).
