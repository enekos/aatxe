#!/usr/bin/env node
/**
 * `aatxe-ts-runner` — discover `*.bench.ts` files, drive the sampling loop,
 * emit a `RunReport` JSON on stdout.
 *
 * Invocation env:
 *   AATXE_SERVICE    service name to embed in the report (defaults to cwd)
 *   AATXE_REF        git ref (defaults to HEAD)
 *   AATXE_FILTER     regex applied to bench names
 *   AATXE_PATTERNS   colon-separated discovery globs (defaults to **\/*.bench.{ts,js})
 *   AATXE_BENCH_FILES  colon-separated explicit file list (skips discovery)
 *
 * Stdout: a single JSON document — the RunReport. Status/info goes to stderr.
 */

import { spawn } from 'node:child_process'
import { readdir, stat } from 'node:fs/promises'
import { resolve, sep } from 'node:path'
import { performance } from 'node:perf_hooks'
import { pathToFileURL } from 'node:url'
import { _internal, type RunReport } from './index.js'
import type {
  BenchFn, BenchRun, RegisteredBench, ResolvedBenchOptions,
} from './types.js'
import { summarizeSamples } from './stats.js'

const cwd = process.cwd()
const service = process.env['AATXE_SERVICE'] ?? cwd.split(sep).pop() ?? 'service'
const ref = process.env['AATXE_REF'] ?? (await detectShortSha()) ?? 'HEAD'
const filter = process.env['AATXE_FILTER'] ? new RegExp(process.env['AATXE_FILTER']!) : null

const patternsEnv = process.env['AATXE_PATTERNS']
const patterns = patternsEnv ? patternsEnv.split(':').filter(Boolean) : []
const benchFilesEnv = process.env['AATXE_BENCH_FILES']
const explicitFiles = benchFilesEnv
  ? benchFilesEnv.split(':').filter(Boolean).map(p => resolve(cwd, p))
  : null

const startedAt = new Date().toISOString()
const files = explicitFiles ?? (await discover(cwd, patterns))

for (const file of files) {
  _internal.setCurrentFile(file)
  await import(pathToFileURL(file).href)
}
_internal.setCurrentFile(null)

const allRuns: BenchRun[] = []
for (const b of _internal.list()) {
  if (b.options.skip) continue
  if (filter && !filter.test(b.name)) continue
  allRuns.push(await runOne(b))
}

const report: RunReport = {
  schemaVersion: _internal.SCHEMA_VERSION,
  language: 'ts',
  service,
  ref,
  runner: `node ${process.version}`,
  startedAt,
  finishedAt: new Date().toISOString(),
  runs: allRuns,
}

process.stdout.write(JSON.stringify(report, null, 2) + '\n')

async function runOne(b: RegisteredBench): Promise<BenchRun> {
  const o: ResolvedBenchOptions = b.options
  const fn = b.fn as BenchFn<unknown>
  const setup = o.setup
  const teardown = o.teardown

  // Decide once whether this bench can run on the sync hot loop. Wrapping a
  // sync fn in `async () => { … }` adds ~100-500ns per invocation from the
  // synthetic Promise + microtask hop, which swamps real sub-µs benches.
  const declaredAsync = (fn as { constructor?: { name?: string } }).constructor?.name === 'AsyncFunction'
  let probeAsync = false
  if (!setup && !teardown && !declaredAsync) {
    try {
      const probe = fn(undefined)
      if (probe instanceof Promise) {
        probeAsync = true
        await probe
      }
    } catch {
      // Probe failures fall through to the async path so the error surfaces
      // with the same try/finally semantics as a normal iteration.
      probeAsync = true
    }
  }
  const isAsync = declaredAsync || probeAsync || setup !== null || teardown !== null

  if (!isAsync) {
    return runSync(b, fn, o)
  }
  return runAsync(b, fn, setup, teardown, o)
}

function runSync(
  b: RegisteredBench,
  fn: BenchFn<unknown>,
  o: ResolvedBenchOptions,
): BenchRun {
  const fnSync = fn as (fixture: unknown) => void
  const batchSize = o.batchSize === 'auto' ? calibrateBatchSync(fnSync) : o.batchSize

  for (let i = 0; i < o.warmup; i++) {
    for (let j = 0; j < batchSize; j++) fnSync(undefined)
  }

  const samples = new Float64Array(o.maxIterations)
  let count = 0
  const startNs = bigintNow()
  let elapsedNs = 0
  for (let i = 0; i < o.maxIterations; i++) {
    if (o.gc) tryGc()
    const t0 = bigintNow()
    for (let j = 0; j < batchSize; j++) fnSync(undefined)
    const dt = Number(bigintNow() - t0)
    samples[count++] = dt / batchSize
    elapsedNs += dt
    if (count >= o.minIterations) {
      const view = samples.subarray(0, count) as unknown as readonly number[]
      const s = summarizeSamples(view)
      const budget = Number(bigintNow() - startNs) / 1e6 >= o.timeBudgetMs
      const cvDone = o.targetCv > 0 && s.cv > 0 && s.cv <= o.targetCv
      if (cvDone || budget) break
    }
  }
  return finalize(b, samples, count, batchSize, elapsedNs)
}

async function runAsync(
  b: RegisteredBench,
  fn: BenchFn<unknown>,
  setup: ResolvedBenchOptions['setup'],
  teardown: ResolvedBenchOptions['teardown'],
  o: ResolvedBenchOptions,
): Promise<BenchRun> {
  const callOnce = async (): Promise<void> => {
    let fixture: unknown = undefined
    if (setup) fixture = await setup()
    try {
      const ret = fn(fixture)
      if (ret instanceof Promise) await ret
    } finally {
      if (teardown) await teardown(fixture)
    }
  }

  const batchSize = o.batchSize === 'auto' ? await calibrateBatchAsync(callOnce) : o.batchSize

  for (let i = 0; i < o.warmup; i++) {
    for (let j = 0; j < batchSize; j++) await callOnce()
  }

  const samples = new Float64Array(o.maxIterations)
  let count = 0
  const startNs = bigintNow()
  let elapsedNs = 0
  for (let i = 0; i < o.maxIterations; i++) {
    if (o.gc) tryGc()
    const t0 = bigintNow()
    for (let j = 0; j < batchSize; j++) await callOnce()
    const dt = Number(bigintNow() - t0)
    samples[count++] = dt / batchSize
    elapsedNs += dt
    if (count >= o.minIterations) {
      const view = samples.subarray(0, count) as unknown as readonly number[]
      const s = summarizeSamples(view)
      const budget = Number(bigintNow() - startNs) / 1e6 >= o.timeBudgetMs
      const cvDone = o.targetCv > 0 && s.cv > 0 && s.cv <= o.targetCv
      if (cvDone || budget) break
    }
  }
  return finalize(b, samples, count, batchSize, elapsedNs)
}

function finalize(
  b: RegisteredBench,
  samples: Float64Array,
  count: number,
  batchSize: number,
  elapsedNs: number,
): BenchRun {
  const arr = Array.from(samples.subarray(0, count))
  const s = summarizeSamples(arr)
  return {
    name: b.name,
    file: b.file,
    iterations: count,
    batchSize,
    elapsedNs,
    samples: arr,
    ...s,
  }
}

function calibrateBatchSync(fnSync: (fixture: unknown) => void): number {
  const target = 50_000n // ns — amortise hrtime.bigint() overhead per reading.
  let n = 1
  while (n < 1_048_576) {
    const t0 = bigintNow()
    for (let i = 0; i < n; i++) fnSync(undefined)
    const dt = bigintNow() - t0
    if (dt >= target) return n
    n *= 2
  }
  return n
}

async function calibrateBatchAsync(callOnce: () => Promise<void>): Promise<number> {
  const target = 50_000n // ns
  let n = 1
  while (n < 1_048_576) {
    const t0 = bigintNow()
    for (let i = 0; i < n; i++) await callOnce()
    const dt = bigintNow() - t0
    if (dt >= target) return n
    n *= 2
  }
  return n
}

let gcWarned = false
function tryGc(): void {
  const gcFn = (globalThis as { gc?: () => void }).gc
  if (gcFn) {
    gcFn()
    return
  }
  if (!gcWarned) {
    gcWarned = true
    process.stderr.write(
      'aatxe-bench: WARNING — `gc: true` requested but globalThis.gc is unavailable. ' +
      'Re-launch node with --expose-gc to enable manual GC between samples.\n',
    )
  }
}

function bigintNow(): bigint {
  return process.hrtime.bigint()
}

async function discover(root: string, requested: string[]): Promise<string[]> {
  const matchers = (requested.length > 0 ? requested : ['**/*.bench.ts', '**/*.bench.js'])
    .map(toRegex)
  const excludes = ['node_modules', 'dist', 'build', '.git', 'target']
  const out: string[] = []
  const stack: string[] = [root]
  while (stack.length > 0) {
    const dir = stack.pop()!
    let entries
    try { entries = await readdir(dir, { withFileTypes: true }) }
    catch { continue }
    for (const e of entries) {
      if (excludes.includes(e.name)) continue
      const full = resolve(dir, e.name)
      if (e.isDirectory()) stack.push(full)
      else if (e.isFile() && matchers.some(rx => rx.test(full))) out.push(full)
    }
  }
  out.sort()
  return out
}

function toRegex(g: string): RegExp {
  let rx = '^'
  for (let i = 0; i < g.length; i++) {
    const c = g[i]!
    const next = g[i + 1]
    if (c === '*' && next === '*') {
      rx += '.*'
      i++
      if (g[i + 1] === '/') i++
    } else if (c === '*') rx += '[^/]*'
    else if (c === '?') rx += '[^/]'
    else if ('.+(){}[]^$|\\'.includes(c)) rx += '\\' + c
    else rx += c
  }
  return new RegExp(rx + '$')
}

async function detectShortSha(): Promise<string | null> {
  return new Promise(res => {
    const ch = spawn('git', ['rev-parse', '--short=10', 'HEAD'], { cwd, stdio: ['ignore', 'pipe', 'ignore'] })
    let buf = ''
    ch.stdout.on('data', (b: Buffer) => buf += b.toString('utf8'))
    ch.on('error', () => res(null))
    ch.on('close', code => res(code === 0 ? buf.trim() || null : null))
  })
}

// Silence unused-import in some bundlers.
void stat
void performance
