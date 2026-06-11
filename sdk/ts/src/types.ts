/**
 * On-disk JSON shape produced by the TS runner. Mirrors aatxe-core's
 * `RunReport` exactly so the Rust CLI can deserialise without translation.
 */

export interface BenchOptions<T = void, P = unknown> {
  /**
   * Build the per-call fixture. With `params` set, receives the current
   * param so sized inputs can be generated per variant; without `params`
   * the argument is `undefined` and zero-arg setups keep working unchanged.
   */
  setup?: (param: P) => T | Promise<T>
  teardown?: (fixture: T) => void | Promise<void>
  warmup?: number
  minIterations?: number
  maxIterations?: number
  timeBudgetMs?: number
  targetCv?: number
  batchSize?: number | 'auto'
  gc?: boolean
  iterations?: number
  integration?: boolean
  skip?: boolean
  /**
   * When set on at least one bench in a run, the runner skips every bench
   * not marked `only: true`. Mirrors vitest/jest semantics — useful for
   * locally iterating on a single flaky bench without commenting out the rest.
   */
  only?: boolean
  /**
   * Parameterize the bench: one registration (and one `BenchRun`) per
   * entry, named `name/String(param)`. The param flows into the bench fn
   * as its second argument and into `setup` as its first. Params must
   * stringify uniquely — `[10, 1e3, 1e5]` works, two distinct objects
   * (both `[object Object]`) don't. A regression that appears only at
   * large sizes then shows up as `parse/100000` regressing while
   * `parse/10` holds — a complexity change, not a constant-factor one.
   */
  params?: readonly P[]
}

export interface ResolvedBenchOptions {
  warmup: number
  minIterations: number
  maxIterations: number
  timeBudgetMs: number
  targetCv: number
  batchSize: number | 'auto'
  gc: boolean
  integration: boolean
  skip: boolean
  only: boolean
  setup: (() => unknown | Promise<unknown>) | null
  teardown: ((fixture: unknown) => void | Promise<void>) | null
}

export type BenchFn<T = void, P = unknown> = (fixture: T, param: P) => void | Promise<void>

export interface RegisteredBench {
  name: string
  file: string
  options: ResolvedBenchOptions
  fn: BenchFn<unknown>
}

/**
 * A non-time metric attached to a {@link BenchRun}. See `Metric` in
 * `aatxe-core::types` for the canonical contract.
 *
 * Adding metrics does **not** bump the schema version — they're a forward-
 * compatible extension point for throughput, allocations, custom counters.
 */
export interface Metric {
  name: string
  value: number
  unit: string
  lowerIsBetter?: boolean
}

export interface BenchRun {
  name: string
  file: string
  iterations: number
  batchSize: number
  elapsedNs: number
  samples: number[]
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
  /** Optional non-time metrics. Serialised only when non-empty. */
  metrics?: Metric[]
  /** Optional free-form tags for filtering / grouping. */
  tags?: string[]
}

export interface AffectedScope {
  base: string
  changedFiles: string[]
  benchFiles: string[]
  skippedBenchFiles: string[]
}

export interface RunReport {
  schemaVersion: number
  language: 'ts'
  service: string
  ref: string
  runner: string
  startedAt: string
  finishedAt: string
  runs: BenchRun[]
  affectedScope?: AffectedScope
}
