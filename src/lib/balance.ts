/**
 * The backend stores exactly two numbers per session: `volume` (an overall gain) and `balance`
 * (-1 full left .. 0 centered .. 1 full right). Earlier, the L/R sliders displayed *absolute*
 * per-channel gain (`volume`-scaled) and, when dragged, recomputed *both* `volume` and `balance`
 * together via a reversible (left, right) <-> (volume, balance) mapping -- which meant moving the
 * master volume slider visibly moved the L/R sliders (they're volume-scaled), and dragging L or R
 * visibly moved the volume slider (whichever channel was louder became the new volume). Reported
 * live as confusing -- the three controls read as tangled together instead of independent.
 *
 * This is the fix: L/R now display and set balance's raw per-channel multiplier -- "what fraction
 * of the *current* volume is reaching this channel" -- never volume itself. Moving volume alone
 * can never move these (they don't depend on volume at all, so there's nothing to recompute);
 * dragging L or R alone can never move volume (only `balance` is derived from the drag). The one
 * inherent constraint this doesn't remove: `balance` is still a single scalar, so only one channel
 * can be attenuated below the other at a time -- dragging the reduced side back up, or dragging
 * the other side down, is what crosses over to reducing the opposite channel instead. That's just
 * what "balance" means on one shared knob, same as any stereo pan control; true independent L/R
 * gain would need the backend to track a second value it doesn't today.
 */

function clamp01(value: number): number {
  return Math.min(Math.max(value, 0), 1);
}

/** Balance's raw per-channel multiplier, 0..1 each -- what the L/R sliders display. */
export function balanceToChannels(balance: number): [left: number, right: number] {
  const left = 1 - Math.max(balance, 0);
  const right = 1 + Math.min(balance, 0);
  return [left, right];
}

/** New balance from the left slider being set directly to `leftFraction` (0..1). */
export function balanceFromLeftFraction(leftFraction: number): number {
  return 1 - clamp01(leftFraction);
}

/** New balance from the right slider being set directly to `rightFraction` (0..1). */
export function balanceFromRightFraction(rightFraction: number): number {
  return -(1 - clamp01(rightFraction));
}
