export const MOBILE_PAGER_INTENT_PX = 10;
export const MOBILE_PAGER_SETTLE_PX = 60;
export const MOBILE_PAGER_EDGE_RESISTANCE = 0.3;

export type MobilePagerAxis = "pending" | "horizontal" | "vertical";

export interface MobilePagerGesture {
  startX: number;
  startY: number;
  axis: MobilePagerAxis;
}

export interface MobilePagerMove {
  gesture: MobilePagerGesture;
  offset: number;
  claimed: boolean;
}

export function beginMobilePagerGesture(
  x: number,
  y: number,
): MobilePagerGesture {
  return { startX: x, startY: y, axis: "pending" };
}

/**
 * Arbitrate a touch without stealing vertical scrolling or terminal selection.
 * The session bar and dot strip opt into `forceHorizontal`, making them the
 * reliable fallback when terminal content itself is too ambiguous to claim.
 */
export function moveMobilePagerGesture(
  gesture: MobilePagerGesture,
  x: number,
  y: number,
  index: number,
  count: number,
  forceHorizontal = false,
): MobilePagerMove {
  const dx = x - gesture.startX;
  const dy = y - gesture.startY;
  let axis = gesture.axis;

  if (axis === "pending") {
    if (Math.abs(dx) < MOBILE_PAGER_INTENT_PX) {
      return { gesture, offset: 0, claimed: false };
    }
    if (!forceHorizontal && Math.abs(dx) <= Math.abs(dy)) {
      axis = "vertical";
    } else {
      axis = "horizontal";
    }
  }

  const next = axis === gesture.axis ? gesture : { ...gesture, axis };
  if (axis !== "horizontal") {
    return { gesture: next, offset: 0, claimed: false };
  }

  const pastStart = index <= 0 && dx > 0;
  const pastEnd = index >= count - 1 && dx < 0;
  return {
    gesture: next,
    offset:
      pastStart || pastEnd ? dx * MOBILE_PAGER_EDGE_RESISTANCE : dx,
    claimed: true,
  };
}

export function adjacentMobilePagerIndex(
  index: number,
  count: number,
  offset: number,
): number | null {
  if (offset === 0) return null;
  const candidate = index + (offset < 0 ? 1 : -1);
  return candidate >= 0 && candidate < count ? candidate : null;
}

export function settleMobilePagerIndex(
  index: number,
  count: number,
  offset: number,
  threshold = MOBILE_PAGER_SETTLE_PX,
): number | null {
  if (Math.abs(offset) < threshold) return null;
  return adjacentMobilePagerIndex(index, count, offset);
}
