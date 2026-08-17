/**
 * A keyed, content-sized virtual window.
 *
 * Heights start as an estimate because a layout-free renderer cannot measure
 * content. Rendered items report their real border-box height through a
 * ResizeObserver; the indexed prefix sums then keep the spacer and visible
 * range aligned with the content rather than with a CSS row contract.
 */

interface Fenwick {
  values: number[];
  add(index: number, delta: number): void;
  sum(endExclusive: number): number;
  lowerBound(offset: number): number;
}
function fenwick(size: number, initial: number): Fenwick {
  const values = Array<number>(size + 1).fill(0);
  const tree: Fenwick = {
    values,
    add(index, delta) {
      for (let cursor = index + 1; cursor < values.length; cursor += cursor & -cursor) {
        values[cursor] = (values[cursor] ?? 0) + delta;
      }
    },
    sum(endExclusive) {
      let total = 0;
      for (let cursor = Math.min(endExclusive, size); cursor > 0; cursor -= cursor & -cursor) {
        total += values[cursor] ?? 0;
      }
      return total;
    },
    lowerBound(offset) {
      // Return the item whose interval contains `offset`. An offset at the
      // exact end belongs to the last item, which is useful at scrollBottom.
      if (size === 0) return 0;
      const target = Math.max(0, Math.min(offset, tree.sum(size) - Number.EPSILON));
      let index = 0;
      let accumulated = 0;
      let step = 1;
      while (step < values.length) step <<= 1;
      for (; step !== 0; step >>= 1) {
        const next = index + step;
        const candidate = accumulated + (values[next] ?? 0);
        if (next < values.length && candidate <= target) {
          index = next;
          accumulated = candidate;
        }
      }
      return Math.min(size - 1, index);
    },
  };
  for (let index = 0; index < size; index += 1) tree.add(index, initial);
  return tree;
}

export interface WindowRange {
  start: number;
  end: number;
  top: number;
  total: number;
}

export interface MeasurementChange {
  index: number;
  top: number;
  delta: number;
}

export class MeasuredWindow {
  private keys: string[] = [];
  private known = new Map<string, number>();
  private tree = fenwick(0, 0);
  private estimate: number;

  constructor(estimate: number) {
    this.estimate = Math.max(1, estimate);
  }

  setKeys(keys: readonly string[]): boolean {
    const next = [...keys];
    const changed =
      next.length !== this.keys.length || next.some((key, index) => key !== this.keys[index]);
    if (!changed) return false;
    const previous = this.known;
    this.keys = next;
    this.known = new Map(next.flatMap((key) => {
      const height = previous.get(key);
      return height === undefined ? [] : [[key, height] as const];
    }));
    this.tree = fenwick(next.length, this.estimate);
    for (const [index, key] of next.entries()) {
      const measured = this.known.get(key);
      if (measured !== undefined) this.tree.add(index, measured - this.estimate);
    }
    return true;
  }

  measure(key: string, height: number): MeasurementChange | null {
    const index = this.keys.indexOf(key);
    if (index < 0 || !Number.isFinite(height) || height <= 0) return null;
    const next = Math.max(1, height);
    const previous = this.known.get(key) ?? this.estimate;
    if (Math.abs(previous - next) < 0.5) return null;
    const top = this.offsetOf(index);
    this.known.set(key, next);
    this.tree.add(index, next - previous);
    return { index, top, delta: next - previous };
  }

  offsetOf(index: number): number {
    return this.tree.sum(Math.max(0, Math.min(index, this.keys.length)));
  }

  totalHeight(): number {
    return this.tree.sum(this.keys.length);
  }

  range(scrollTop: number, viewport: number, overscanPixels: number): WindowRange {
    const total = this.totalHeight();
    if (!this.keys.length) return { start: 0, end: 0, top: 0, total: 0 };
    const overscan = Math.max(0, overscanPixels);
    const firstOffset = Math.max(0, scrollTop - overscan);
    const lastOffset = Math.min(total, scrollTop + Math.max(0, viewport) + overscan);
    const start = this.tree.lowerBound(firstOffset);
    // A value exactly on an item boundary belongs to the next interval. A
    // machine epsilon is not enough once the offset has been rounded (for
    // example, 120 - Number.EPSILON is still 120), so step back by a tiny
    // pixel-sized amount for the exclusive end boundary.
    const endOffset = Math.max(
      0,
      lastOffset - Math.max(1e-7, Math.abs(lastOffset) * Number.EPSILON * 4),
    );
    const end = Math.min(
      this.keys.length,
      this.tree.lowerBound(endOffset) + 1,
    );
    return { start, end: Math.max(start, end), top: this.offsetOf(start), total };
  }

  keyAt(index: number): string | undefined {
    return this.keys[index];
  }

  indexOf(key: string): number {
    return this.keys.indexOf(key);
  }
}
