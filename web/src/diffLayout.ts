// Responsive layout for the Monaco diff (#240). A side-by-side diff needs two
// full columns of code; below this width there is not room for them and the
// diff is rendered inline (unified) instead so it stays readable on a phone.
export const DIFF_SIDE_BY_SIDE_MIN_WIDTH = 900;

/** Whether a diff of the given host width should render side-by-side. At and
 *  above the threshold the two columns fit; narrower than that, render inline. */
export function shouldRenderSideBySide(width: number): boolean {
  return width >= DIFF_SIDE_BY_SIDE_MIN_WIDTH;
}
