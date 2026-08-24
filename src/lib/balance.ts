/** (volume, balance) -> independent (left, right) gains, 0..1 each. */
export function toLeftRight(volume: number, balance: number): [number, number] {
  const left = volume * (1 - Math.max(balance, 0));
  const right = volume * (1 + Math.min(balance, 0));
  return [left, right];
}

/** Inverse of `toLeftRight` -- the louder channel is the "true" volume, and balance is how far
 * the quieter channel has been pulled down from it. Fully reversible: feeding the result back
 * through `toLeftRight` reproduces the exact (left, right) pair a user just set. */
export function fromLeftRight(left: number, right: number): [number, number] {
  if (right >= left) {
    const volume = right;
    const balance = volume > 0 ? 1 - left / volume : 0;
    return [volume, balance];
  }
  const volume = left;
  const balance = volume > 0 ? right / volume - 1 : 0;
  return [volume, balance];
}
