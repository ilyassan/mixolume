// The Rust backend sends `iconPng` as a plain array of bytes (serde's default
// JSON encoding for `Vec<u8>`), not as a data URL or base64 string. This is a
// pure, framework-free helper so it can be unit tested in isolation from any
// component/store wiring.

// btoa/String.fromCharCode choke on very large arguments in some engines, so
// the bytes are base64-encoded in fixed-size chunks rather than all at once.
const CHUNK_SIZE = 0x8000;

/**
 * Converts a raw PNG byte array (as received from the Tauri `AppSession.iconPng`
 * field) into a `data:image/png;base64,...` URL suitable for an `<img src>`.
 *
 * Returns `null` when there is no icon, so callers can fall back to a generic
 * icon instead of rendering a broken `<img>`.
 */
export function iconPngToDataUrl(
  iconPng: number[] | null | undefined,
): string | null {
  if (!iconPng || iconPng.length === 0) {
    return null;
  }

  const bytes = Uint8Array.from(iconPng);
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += CHUNK_SIZE) {
    const chunk = bytes.subarray(offset, offset + CHUNK_SIZE);
    binary += String.fromCharCode(...chunk);
  }

  return `data:image/png;base64,${btoa(binary)}`;
}
