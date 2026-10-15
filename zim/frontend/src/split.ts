/**
 * ZIM default frontend — the split-view model (M23).
 *
 * Pure, DOM-free math for the §8 split view: one tab may host two
 * panes (the primary surface plus a secondary surface), separated
 * by a draggable divider. The layout is a single `ratio` in the
 * [0.2, 0.8] window — the honest serialization of "the divider sits
 * this far along" — with Chrome's snap points (halves and thirds)
 * applied during a drag, per the §12 "Chrome 126+ split-tab
 * interaction" reference row.
 *
 * The model deliberately knows nothing about iframes or streams:
 * `app.ts` owns the panes (each with its own subscription, §8's
 * "closing one pane only unsubscribes that pane"), and this module
 * answers layout questions the divider drag asks.
 */

/** The narrowest a pane may become (Chrome's split keeps both sides
 * usable; 20% of the content width is the floor). */
export const SPLIT_MIN_RATIO = 0.2;

/** Divider snap points: Chrome's split snaps at halves and thirds. */
export const SPLIT_SNAP_POINTS: readonly number[] = [1 / 3, 1 / 2, 2 / 3];

/** How close (in ratio units) the divider must come before it
 * snaps. At a 1200 px content width this is ~36 px — felt, not
 * magnetic. */
export const SPLIT_SNAP_TOLERANCE = 0.03;

/** Clamp a ratio into the legal pane window. */
export function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) {
    return 1 / 2;
  }
  return Math.min(Math.max(ratio, SPLIT_MIN_RATIO), 1 - SPLIT_MIN_RATIO);
}

/** Snap a clamped ratio to the nearest snap point when within
 * tolerance; otherwise return it unchanged (already clamped). */
export function snapRatio(ratio: number): number {
  const clamped = clampRatio(ratio);
  for (const point of SPLIT_SNAP_POINTS) {
    if (Math.abs(clamped - point) <= SPLIT_SNAP_TOLERANCE) {
      return point;
    }
  }
  return clamped;
}

/** The ratio a pointer position implies, given the container's
 * content-box rect: how far along the divider track the pointer
 * sits. Degenerate (zero-width) containers yield the centered
 * fallback rather than NaN. */
export function ratioFromPointer(pointerX: number, rect: DOMRect): number {
  const width = rect.width;
  if (!(width > 0)) {
    return 1 / 2;
  }
  return clampRatio((pointerX - rect.left) / width);
}

/** Whether a split's secondary path is the same as its tab's primary
 * path (Chrome's split copies the tab when first torn in two). */
export function isMirroredSplit(primary: string, secondary: string): boolean {
  return primary === secondary;
}
