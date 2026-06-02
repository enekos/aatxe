/**
 * Same stats engine as aatxe-core, ported to TypeScript. Kept in this package
 * so a TS bench runner can compute the derived `BenchRun` fields without
 * shelling out to the Rust binary.
 *
 * Aatxe-core normalises any missing fields anyway, but emitting them
 * up-front means consumers (PR-comment renderers, dashboards) that read the
 * JSON directly see complete data.
 */

export interface Summary {
  mean: number
  median: number
  trimmedMean: number
  stddev: number
  cv: number
  mad: number
  iqr: number
  min: number
  max: number
  p50: number
  p95: number
  p99: number
}

const empty: Summary = {
  mean: 0, median: 0, trimmedMean: 0, stddev: 0, cv: 0, mad: 0, iqr: 0,
  min: 0, max: 0, p50: 0, p95: 0, p99: 0,
}

function percentileSorted(sorted: Float64Array, p: number): number {
  if (sorted.length === 0) return 0
  const rank = (p / 100) * (sorted.length - 1)
  const lo = Math.floor(rank)
  const hi = Math.ceil(rank)
  if (lo === hi) return sorted[lo]!
  const frac = rank - lo
  return sorted[lo]! * (1 - frac) + sorted[hi]! * frac
}

export function summarizeSamples(samples: readonly number[]): Summary {
  const n = samples.length
  if (n === 0) return { ...empty }

  let mean = 0, m2 = 0, min = samples[0]!, max = samples[0]!
  for (let i = 0; i < n; i++) {
    const x = samples[i]!
    if (x < min) min = x
    if (x > max) max = x
    const delta = x - mean
    mean += delta / (i + 1)
    const delta2 = x - mean
    m2 += delta * delta2
  }
  const variance = n < 2 ? 0 : m2 / (n - 1)
  const stddev = Math.sqrt(variance)
  const cv = mean === 0 ? 0 : stddev / mean

  const sorted = new Float64Array(samples).sort()
  const median = percentileSorted(sorted, 50)
  const p95 = percentileSorted(sorted, 95)
  const p99 = percentileSorted(sorted, 99)
  const iqr = percentileSorted(sorted, 75) - percentileSorted(sorted, 25)

  const cut = Math.floor(n * 0.05)
  let trimSum = 0, trimCount = 0
  for (let i = cut; i < n - cut; i++) {
    trimSum += sorted[i]!
    trimCount++
  }
  const trimmedMean = trimCount > 0 ? trimSum / trimCount : mean

  const mad = madFromSorted(sorted, median)

  return { mean, median, trimmedMean, stddev, cv, mad, iqr, min, max, p50: median, p95, p99 }
}

function madFromSorted(sorted: Float64Array, med: number): number {
  const n = sorted.length
  if (n === 0) return 0
  let left = 0
  while (left < n && sorted[left]! < med) left++
  let right = n - 1
  while (right >= 0 && sorted[right]! > med) right--
  const eqCount = right - left + 1
  const merged = new Array<number>(n)
  let idx = 0
  for (let i = 0; i < eqCount; i++) merged[idx++] = 0
  let li = left - 1
  let ri = right + 1
  while (li >= 0 && ri < n) {
    const lv = med - sorted[li]!
    const rv = sorted[ri]! - med
    if (lv <= rv) { merged[idx++] = lv; li-- }
    else { merged[idx++] = rv; ri++ }
  }
  while (li >= 0) merged[idx++] = med - sorted[li--]!
  while (ri < n) merged[idx++] = sorted[ri++]! - med
  return percentileSorted(new Float64Array(merged), 50)
}
