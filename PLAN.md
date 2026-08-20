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
