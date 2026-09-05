import { useEffect, useMemo, useState } from "react";

/**
 * Converts a raw PNG byte array (as received from the Tauri `AppSession.iconPng` field) into a
 * `blob:` object URL suitable for an `<img src>`, properly revoked when no longer needed.
 *
 * Deliberately *not* a `data:image/png;base64,...` URI, which is what this used to be (see git
 * history / `iconUrl.ts`, now removed). Confirmed live, via a macOS `sample` profile taken during
 * an active slider drag, that the data URI was a real, significant cost: WebKit computes a
 * hit-test result on *every* mouse-move event (not just clicks or hovers directly over an image),
 * and that hit-test resolves `absoluteImageURL()` for the nearest `<img>` -- which means parsing
 * the *entire* URL string from scratch, every single mouse move, for as long as it was live. The
 * app icon is resolved at 128x128px backend-side (displayed at 32px), so its base64 text ran to
 * several KB -- real, repeated URL-parsing work, dozens of times a second while dragging, that
 * had nothing to do with the drag itself.
 *
 * A `blob:` URL is a short, fixed-length string (a UUID) regardless of the underlying image's
 * size, so that same per-mouse-move parse becomes trivially cheap no matter how large the icon
 * is. Creating the `Blob` is also cheaper than the byte-loop + `btoa` base64 encoding it
 * replaces, since there's no text-encoding step at all -- but object URLs are a real browser
 * resource that must be explicitly revoked, which is why this is a hook (owning the URL's
 * lifecycle via an effect) rather than the plain, referentially-transparent function it used to
 * be.
 */
export function useIconObjectUrl(rawIconPng: number[] | null): string | null {
  const [url, setUrl] = useState<string | null>(null);

  // Re-keyed by content, not `rawIconPng`'s own array *reference* -- a caller is free to pass a
  // freshly-allocated array with identical bytes on every render (e.g. straight off a JSON
  // deserialize). Depending on the reference directly below would re-run the effect (and, since
  // it calls `setUrl`, re-render) every single time regardless of whether the bytes actually
  // changed -- which re-invokes the caller, which can produce yet another fresh reference,
  // forever: a genuine infinite render loop, reproduced live, not a hypothetical one. `useMemo`
  // gives every render with the same bytes back the exact same array reference, so the effect
  // below can depend on `iconPng` itself and still only fire when the content really changes.
  const iconKey = rawIconPng && rawIconPng.length > 0 ? rawIconPng.join(",") : null;
  // eslint-disable-next-line react-hooks/exhaustive-deps -- deliberately keyed by content (`iconKey`), not `rawIconPng`'s own reference; see the comment above.
  const iconPng = useMemo(() => rawIconPng, [iconKey]);

  useEffect(() => {
    if (!iconPng || iconPng.length === 0) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- synchronizing with a real external system (the browser's object-URL registry) is the documented, intended use of an effect's setState, not the derived-state anti-pattern this rule exists to catch.
      setUrl(null);
      return;
    }
    const blob = new Blob([Uint8Array.from(iconPng)], { type: "image/png" });
    const objectUrl = URL.createObjectURL(blob);
    setUrl(objectUrl);
    return () => {
      URL.revokeObjectURL(objectUrl);
    };
  }, [iconPng]);

  return url;
}
