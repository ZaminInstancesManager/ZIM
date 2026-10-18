/**
 * The metrics chart model (M30): the pure geometry between the
 * engine's derived series and the canvas the metrics page draws.
 *
 * No DOM here — every function is pure so the frontend unit layer
 * (node:test) can pin the contract: value → pixel mapping, the
 * "unknown is a gap, never zero" rule, and the axis bounds a chart
 * labels itself with. The page (`metrics.ts`) owns the canvases.
 */
/** Round a non-negative maximum up to the nearest "nice" bound —
 * 1/2/5 × 10^k — so chart ceilings read as clean numbers. Zero
 * stays zero (an all-zero series maps against a zero axis: every
 * known point sits on the baseline, honestly flat).
 */
export function niceCeiling(value) {
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
export function buildChartModel(values, width, height, padding, yMaxOverride) {
    const yMax = yMaxOverride ?? niceCeiling(maxOf(values));
    const innerWidth = Math.max(width - padding * 2, 1);
    const innerHeight = Math.max(height - padding * 2, 1);
    const step = values.length > 1 ? innerWidth / (values.length - 1) : 0;
    const vertices = values.map((value, index) => {
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
function maxOf(values) {
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
export function drawableRuns(vertices) {
    const runs = [];
    let current = [];
    for (let index = 0; index < vertices.length; index += 1) {
        if (vertices[index].y !== null) {
            current.push(index);
        }
        else {
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
export function gridlineValues(yMax, count) {
    if (count < 2 || yMax <= 0 || !Number.isFinite(yMax)) {
        return [yMax];
    }
    const values = [];
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
export function liveCpuPercent(before, after) {
    const workload = counterDelta(before.cpu_usage_total_ns, after.cpu_usage_total_ns);
    const host = counterDelta(before.system_cpu_total_ns, after.system_cpu_total_ns);
    if (workload === null || host === null || host === 0) {
        return null;
    }
    return Math.min((workload / host) * 100, 100);
}
/** A cumulative counter's delta, null when unknown or reset. */
function counterDelta(before, after) {
    if (typeof before !== "number" ||
        typeof after !== "number" ||
        !Number.isFinite(before) ||
        !Number.isFinite(after) ||
        after < before) {
        return null;
    }
    return after - before;
}
