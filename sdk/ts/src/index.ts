/**
 * `@aatxe/bench` — authoring API and statistics for aatxe-compatible
 * TypeScript / JavaScript microbenchmarks.
 *
 * Devs call {@link bench} at module top level inside `*.bench.ts` files:
 *
 * ```ts
 * import { bench } from '@aatxe/bench'
 * bench('parseFoo: cold path', () => parseFoo(input))
 * ```
 *
 * A separate runner (see `runner.ts` / `aatxe-ts-runner`) loads the bench
 * files, drives the adaptive sampling loop, and emits a `RunReport` JSON on
 * stdout — the on-disk shape `aatxe run --lang ts` ingests.
 *
 * Keeping this file IO-free (no `fs`, no `process.exit`) lets us unit-test
 * the authoring API without spinning up a runner.
 */

import { summarizeSamples } from './stats.js'
import type { BenchFn, BenchOptions, RegisteredBench, ResolvedBenchOptions } from './types.js'

export type { BenchFn, BenchOptions, BenchRun, RunReport } from './types.js'
export { summarizeSamples } from './stats.js'

/**
 * Defeat V8 dead-code elimination on a benched expression. Stores `v` on a
 * `globalThis` property so the optimiser must treat the write as observable
 * by other modules — this prevents pure calls whose result flows to `_` from
 * being optimised down to nothing.
 *
 * ```ts
 * bench('parseFoo', () => { keep(parseFoo('x')) })
 * ```
 *
 * Returns `v` so it can be chained inline.
 */
const SINK_KEY = '__aatxe_sink__'
export function keep<T>(v: T): T {
  ;(globalThis as Record<string, unknown>)[SINK_KEY] = v
  return v
}

const SCHEMA_VERSION = 2

const DEFAULTS: ResolvedBenchOptions = {
  warmup: 5,
  minIterations: 30,
  maxIterations: 200,
  timeBudgetMs: 1000,
  targetCv: 0.02,
  batchSize: 'auto',
  gc: false,
  integration: false,
  skip: false,
  setup: null,
  teardown: null,
}

const STATE_KEY = Symbol.for('@aatxe/bench.harness')

interface HarnessState {
  registry: RegisteredBench[]
  currentFile: string | null
}

function getState(): HarnessState {
  const g = globalThis as unknown as { [STATE_KEY]?: HarnessState }
  if (!g[STATE_KEY]) g[STATE_KEY] = { registry: [], currentFile: null }
  return g[STATE_KEY]!
}

/**
 * Register a benchmark.
 *
 * Call at module top level inside a `*.bench.ts` file. Names must be unique
 * inside a single run. The harness sees the registration when the runner
 * loads the file; the actual measurement happens later, in the runner.
 */
export function bench<T = void>(
  name: string,
  fn: BenchFn<T>,
  options: BenchOptions<T> = {},
): void {
  const state = getState()
  if (state.registry.some(b => b.name === name)) {
    throw new Error(`aatxe: duplicate bench name "${name}"`)
  }
  state.registry.push({
    name,
    file: state.currentFile ?? '<unknown>',
    options: resolveOptions(options),
    fn: fn as BenchFn<unknown>,
  })
}

function resolveOptions<T>(opts: BenchOptions<T>): ResolvedBenchOptions {
  const resolved: ResolvedBenchOptions = {
    ...DEFAULTS,
    ...(opts.warmup != null ? { warmup: opts.warmup } : {}),
    ...(opts.minIterations != null ? { minIterations: opts.minIterations } : {}),
    ...(opts.maxIterations != null ? { maxIterations: opts.maxIterations } : {}),
    ...(opts.timeBudgetMs != null ? { timeBudgetMs: opts.timeBudgetMs } : {}),
    ...(opts.targetCv != null ? { targetCv: opts.targetCv } : {}),
    ...(opts.batchSize != null ? { batchSize: opts.batchSize } : {}),
    ...(opts.gc != null ? { gc: opts.gc } : {}),
    ...(opts.integration != null ? { integration: opts.integration } : {}),
    ...(opts.skip != null ? { skip: opts.skip } : {}),
    ...(opts.setup != null ? { setup: opts.setup as () => unknown } : {}),
    ...(opts.teardown != null ? { teardown: opts.teardown as (f: unknown) => void } : {}),
  }
  if (opts.iterations != null) {
    resolved.minIterations = opts.iterations
    resolved.maxIterations = opts.iterations
    resolved.timeBudgetMs = Number.POSITIVE_INFINITY
    resolved.targetCv = 0
  }
  if (resolved.minIterations > resolved.maxIterations) {
    throw new Error(
      `aatxe: minIterations (${resolved.minIterations}) > maxIterations (${resolved.maxIterations})`,
    )
  }
  return resolved
}

/** Internal hooks used by the runner. Not for bench authors. */
export const _internal = {
  setCurrentFile(file: string | null): void {
    getState().currentFile = file
  },
  list(): readonly RegisteredBench[] {
    return getState().registry
  },
  clear(): void {
    getState().registry = []
  },
  SCHEMA_VERSION,
  summarizeSamples,
}
