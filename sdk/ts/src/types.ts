/**
 * On-disk JSON shape produced by the TS runner. Mirrors aatxe-core's
 * `RunReport` exactly so the Rust CLI can deserialise without translation.
 */

export interface BenchOptions<T = void> {
  setup?: () => T | Promise<T>
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
  setup: (() => unknown | Promise<unknown>) | null
  teardown: ((fixture: unknown) => void | Promise<void>) | null
}

export type BenchFn<T = void> = (fixture: T) => void | Promise<void>

export interface RegisteredBench {
  name: string
  file: string
  options: ResolvedBenchOptions
  fn: BenchFn<unknown>
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
