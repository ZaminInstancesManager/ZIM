/**
 * The metrics chart model (M30): the pure geometry between the
 * engine's derived series and the canvas the metrics page draws.
 *
 * No DOM here — every function is pure so the frontend unit layer
 * (node:test) can pin the contract: value → pixel mapping, the
 * "unknown is a gap, never zero" rule, and the axis bounds a chart
 * labels itself with. The page (`metrics.ts`) owns the canvases.
 */

/** One drawable vertex: canvas-space x/y plus whether the series
 * was known at this point (an unknown value breaks the line — it is
 * never drawn as zero, which would read "idle"). */
export interface ChartVertex {
  /** Horizontal position in CSS pixels. */
  x: number;
  /** Vertical position in CSS pixels (null when the value is
   * unknown: the vertex is a gap marker, not a point). */
  y: number | null;
}

/** The drawable form of one series: the vertices plus the y-axis
 * bound the caller should label (a "nice" ceiling, not the raw
 * maximum, so the top gridline reads well). */
export interface ChartModel {
  /** The series' vertices, one per input value, left to right. */
  vertices: ChartVertex[];
  /** The y-axis ceiling the series is mapped against (the raw
   * maximum rounded up to a human bound). */
  yMax: number;
}

/** Round a non-negative maximum up to the nearest "nice" bound —
 * 1/2/5 × 10^k — so chart ceilings read as clean numbers. Zero
 * stays zero (an all-zero series maps against a zero axis: every
 * known point sits on the baseline, honestly flat).
 */
export function niceCeiling(value: number): number {
  if (!Number.isFinite(value) || value <= 0) {
    return 0;
  }
  const exponent = Math.floor(Math.log10(value));
  const base = Math.pow(10, exponent);
  const scaled = value / base;
  const nice = scaled <= 1 ? 1 : scaled <= 2 ? 2 : scaled <= 5 ? 5 : 10;
  return nice * base;
}

/** Map one series onto a chart's geometry.
 *
 * `values` are the series samples (null = unknown), `width`/`height`
 * the canvas's CSS-pixel drawing area, and `padding` the inner
 * margin the axes occupy. Points spread evenly across the width;
 * values map linearly onto `[padding, height - padding]` against
 * `yMax` (zero at the bottom). An all-null series still returns
 * vertices (all gap markers) — the page renders the honest
 * "nothing reported" state from it.
 */
export function buildChartModel(
  values: ReadonlyArray<number | null>,
  width: number,
  height: number,
  padding: number,
  yMaxOverride?: number,
): ChartModel {
  const yMax = yMaxOverride ?? niceCeiling(maxOf(values));
  const innerWidth = Math.max(width - padding * 2, 1);
  const innerHeight = Math.max(height - padding * 2, 1);
  const step = values.length > 1 ? innerWidth / (values.length - 1) : 0;
  const vertices: ChartVertex[] = values.map((value, index) => {
    const x = padding + step * index;
    // Unknown (or malformed) values are gap markers, never points.
    if (value === null || !Number.isFinite(value)) {
      return { x, y: null };
    }
    // A zero axis (an all-zero series): every known point sits on
    // the flat baseline — honestly zero, not "unknown".
    if (yMax <= 0) {
      return { x, y: padding + innerHeight };
    }
    const clamped = Math.min(Math.max(value, 0), yMax);
    return { x, y: padding + innerHeight * (1 - clamped / yMax) };
  });
  return { vertices, yMax };
}

/** The largest known value in a series (0 when nothing is known). */
function maxOf(values: ReadonlyArray<number | null>): number {
  let max = 0;
  for (const value of values) {
    if (value !== null && Number.isFinite(value) && value > max) {
      max = value;
    }
  }
  return max;
}

/** Break one series' vertices into contiguous drawable runs — a
 * `null` vertex (or an unknown stretch) splits the polyline so the
 * canvas never draws a bridge across missing data. Single-vertex
 * runs are dropped (a line needs two points).
 */
export function drawableRuns(vertices: ReadonlyArray<ChartVertex>): number[][] {
  const runs: number[][] = [];
  let current: number[] = [];
  for (let index = 0; index < vertices.length; index += 1) {
    if (vertices[index].y !== null) {
      current.push(index);
    } else {
      if (current.length > 1) {
        runs.push(current);
      }
      current = [];
    }
  }
  if (current.length > 1) {
    runs.push(current);
  }
  return runs;
}

/** The evenly-spaced gridline values for one y-axis (the caller
 * draws the labels): `count` values from zero to `yMax`, always
 * including both ends.
 */
export function gridlineValues(yMax: number, count: number): number[] {
  if (count < 2 || yMax <= 0 || !Number.isFinite(yMax)) {
    return [yMax];
  }
  const values: number[] = [];
  for (let index = 0; index < count; index += 1) {
    values.push((yMax * index) / (count - 1));
  }
  return values;
}

/** The live CPU-percent derivation over two consecutive stream
 * samples (M30's honest live strip): the workload-CPU-over-host-CPU
 * share, or null when either counter is missing or the workload
 * counter went backwards (a restart reset — unknown, not negative).
 */
export function liveCpuPercent(
  before: { cpu_usage_total_ns?: number | null; system_cpu_total_ns?: number | null },
  after: { cpu_usage_total_ns?: number | null; system_cpu_total_ns?: number | null },
): number | null {
  const workload = counterDelta(before.cpu_usage_total_ns, after.cpu_usage_total_ns);
  const host = counterDelta(before.system_cpu_total_ns, after.system_cpu_total_ns);
  if (workload === null || host === null || host === 0) {
    return null;
  }
  return Math.min((workload / host) * 100, 100);
}

/** A cumulative counter's delta, null when unknown or reset. */
function counterDelta(
  before: number | null | undefined,
  after: number | null | undefined,
): number | null {
  if (
    typeof before !== "number" ||
    typeof after !== "number" ||
    !Number.isFinite(before) ||
    !Number.isFinite(after) ||
    after < before
  ) {
    return null;
  }
  return after - before;
}
