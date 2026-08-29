import { useEffect, useState } from "react";

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
export function useIconObjectUrl(iconPng: number[] | null): string | null {
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!iconPng || iconPng.length === 0) {
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
