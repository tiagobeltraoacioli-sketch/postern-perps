/**
 * Placement of the tags on the price axis (mark, index, last price, the user's levels) and the pinning of levels
 * outside the visible price range. Pure functions.
 *
 * Rules (tested in levels.test.ts):
 * 1. A tag sits at its price's y when it can; tags never overlap; their top-to-bottom order is the order of their
 *    prices (a higher price is never written below a lower one), ties kept in input order.
 * 2. A level whose price is above the visible range is PINNED at the top edge with an up arrow, below it at the
 *    bottom edge with a down arrow; the line itself is not drawn (it is not on screen) but the tag and its label
 *    are: a liquidation price is never out of sight.
 * 3. When there are more tags than fit in the axis height, the tags with the lowest priority are dropped first
 *    (liquidation has the highest priority, then position, mark, last, trigger, order, index); a dropped tag is
 *    reported so the accessible summary still lists it.
 */

export type TagKind = 'liquidation' | 'position' | 'mark' | 'last' | 'trigger' | 'order' | 'index';

export const TAG_PRIORITY: Readonly<Record<TagKind, number>> = {
  liquidation: 7,
  position: 6,
  mark: 5,
  last: 4,
  trigger: 3,
  order: 2,
  index: 1,
};

export type TagInput = {
  readonly id: string;
  readonly kind: TagKind;
  readonly price: number;
};

export type PlacedTag = {
  readonly id: string;
  readonly kind: TagKind;
  /** y of the price (may be outside [top, bottom] for a pinned tag). */
  readonly priceY: number;
  /** Centre y of the tag box. */
  readonly y: number;
  readonly pinned: 'above' | 'below' | null;
};

export type TagLayout = { readonly placed: PlacedTag[]; readonly dropped: string[] };

/**
 * Place tags of height `h` in [top, bottom] (the axis band), given each tag's price y from `toY`.
 * Tags with a non-finite price are not placed and not reported (nothing to draw).
 */
export function layoutTags(
  inputs: readonly TagInput[],
  toY: (price: number) => number,
  top: number,
  bottom: number,
  h: number,
): TagLayout {
  type Work = { id: string; kind: TagKind; priceY: number; want: number; pinned: 'above' | 'below' | null; order: number };
  const half = h / 2;
  const work: Work[] = [];
  inputs.forEach((t, order) => {
    if (!Number.isFinite(t.price)) return;
    const py = toY(t.price);
    if (!Number.isFinite(py)) return;
    const pinned = py < top ? 'above' : py > bottom ? 'below' : null;
    const want = Math.min(bottom - half, Math.max(top + half, py));
    work.push({ id: t.id, kind: t.kind, priceY: py, want, pinned, order });
  });
  // Drop the least important tags until they fit.
  const capacity = Math.max(0, Math.floor((bottom - top) / h));
  const dropped: string[] = [];
  if (work.length > capacity) {
    const byPriority = [...work].sort((a, b) => TAG_PRIORITY[a.kind] - TAG_PRIORITY[b.kind] || b.order - a.order);
    const remove = new Set(byPriority.slice(0, work.length - capacity));
    for (const w of remove) dropped.push(w.id);
    for (let i = work.length - 1; i >= 0; i--) if (remove.has(work[i])) work.splice(i, 1);
  }
  // Order by price y (top first); ties in input order.
  work.sort((a, b) => a.priceY - b.priceY || a.order - b.order);
  const ys = work.map((w) => w.want);
  // Forward pass: push down to remove overlaps; backward pass: push up from the bottom edge.
  for (let i = 1; i < ys.length; i++) if (ys[i] < ys[i - 1] + h) ys[i] = ys[i - 1] + h;
  if (ys.length && ys[ys.length - 1] > bottom - half) {
    ys[ys.length - 1] = bottom - half;
    for (let i = ys.length - 2; i >= 0; i--) if (ys[i] > ys[i + 1] - h) ys[i] = ys[i + 1] - h;
  }
  // Settle: a box pushed down that now has room above moves back toward its price (one relaxation pass).
  for (let i = 0; i < ys.length; i++) {
    const lo = i === 0 ? top + half : ys[i - 1] + h;
    if (ys[i] > work[i].want) ys[i] = Math.max(lo, work[i].want);
  }
  return {
    placed: work.map((w, i) => ({ id: w.id, kind: w.kind, priceY: w.priceY, y: ys[i], pinned: w.pinned })),
    dropped,
  };
}

/**
 * The shaded zone of a liquidation level: from the line toward the losing side (a long is liquidated when the
 * price falls: the zone is below the line; a short: above). Returns the y span clipped to [top, bottom], or null.
 */
export function liquidationZone(
  priceY: number,
  side: 'buy' | 'sell',
  top: number,
  bottom: number,
): [number, number] | null {
  const [a, b] = side === 'buy' ? [priceY, bottom] : [top, priceY];
  const y0 = Math.max(top, Math.min(bottom, a));
  const y1 = Math.max(top, Math.min(bottom, b));
  return y1 > y0 ? [y0, y1] : null;
}
