import { strict as assert } from 'node:assert'
import { test } from 'node:test'
import { _internal, bench, keep, summarizeSamples } from './index.js'

test('bench registers under current file', () => {
  _internal.clear()
  _internal.setCurrentFile('/x/foo.bench.ts')
  bench('a', () => undefined)
  bench('b', async () => undefined, { warmup: 2, minIterations: 10, maxIterations: 10 })
  _internal.setCurrentFile(null)

  const list = _internal.list()
  assert.equal(list.length, 2)
  assert.equal(list[0]!.name, 'a')
  assert.equal(list[0]!.file, '/x/foo.bench.ts')
  assert.equal(list[1]!.options.warmup, 2)
  assert.equal(list[1]!.options.minIterations, 10)
})

test('bench refuses duplicate names', () => {
  _internal.clear()
  bench('x', () => undefined)
  assert.throws(() => bench('x', () => undefined), /duplicate bench name/)
})

test('iterations pin both bounds', () => {
  _internal.clear()
  bench('pinned', () => undefined, { iterations: 50 })
  const b = _internal.list()[0]!
  assert.equal(b.options.minIterations, 50)
  assert.equal(b.options.maxIterations, 50)
  assert.equal(b.options.targetCv, 0)
})

test('summarizeSamples agrees with hand-computed values', () => {
  // Mean of 1..10 is 5.5; median is 5.5.
  const s = summarizeSamples([1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
  assert.equal(s.mean, 5.5)
  assert.equal(s.median, 5.5)
  assert.equal(s.min, 1)
  assert.equal(s.max, 10)
})

test('summarizeSamples returns zeros on empty input', () => {
  const s = summarizeSamples([])
  // The shape exists, but every field is 0 — no NaN leaks.
  assert.equal(s.mean, 0)
  assert.equal(s.median, 0)
  assert.equal(s.cv, 0)
  assert.equal(s.p95, 0)
  assert.equal(s.iqr, 0)
})

test('summarizeSamples on a single element zeroes the spread', () => {
  const s = summarizeSamples([42])
  assert.equal(s.mean, 42)
  assert.equal(s.median, 42)
  assert.equal(s.stddev, 0)
  assert.equal(s.cv, 0)
  // Bessel-corrected variance is undefined for n=1 ⇒ we return 0 (not NaN).
  assert.equal(Number.isFinite(s.cv), true)
})

test('summarizeSamples on all-zero samples does not produce NaN', () => {
  const s = summarizeSamples(new Array(20).fill(0))
  assert.equal(s.mean, 0)
  assert.equal(s.cv, 0, 'cv must coerce to 0 when mean=0')
  assert.equal(Number.isFinite(s.p95), true)
})

test('summarizeSamples on constant samples reports zero spread', () => {
  const s = summarizeSamples(new Array(30).fill(100))
  assert.equal(s.mean, 100)
  assert.equal(s.median, 100)
  assert.equal(s.stddev, 0)
  assert.equal(s.iqr, 0)
  assert.equal(s.mad, 0)
  assert.equal(s.p99, 100)
})

test('bench respects skip option', () => {
  _internal.clear()
  bench('alive', () => undefined)
  bench('skipped', () => undefined, { skip: true })
  const list = _internal.list()
  // Both stay registered — `skip` is honoured by the runner, not the registrar.
  assert.equal(list.length, 2)
  assert.equal(list.find(b => b.name === 'skipped')!.options.skip, true)
  assert.equal(list.find(b => b.name === 'alive')!.options.skip, false)
})

test('integration option is preserved on the resolved bench', () => {
  _internal.clear()
  bench('db-call', () => undefined, { integration: true })
  assert.equal(_internal.list()[0]!.options.integration, true)
})

test('minIterations greater than maxIterations is rejected', () => {
  _internal.clear()
  assert.throws(
    () => bench('bad', () => undefined, { minIterations: 100, maxIterations: 10 }),
    /minIterations .* > maxIterations/,
  )
})

test('numeric batchSize overrides auto', () => {
  _internal.clear()
  bench('fixed', () => undefined, { batchSize: 64 })
  assert.equal(_internal.list()[0]!.options.batchSize, 64)
})

test('default batchSize is auto', () => {
  _internal.clear()
  bench('default', () => undefined)
  assert.equal(_internal.list()[0]!.options.batchSize, 'auto')
})

test('keep returns its argument unchanged', () => {
  const obj = { x: 1 }
  assert.equal(keep(obj), obj)
  assert.equal(keep(42), 42)
  assert.equal(keep('hello'), 'hello')
})

test('keep writes to a globalThis sink so V8 cannot prove the write is dead', () => {
  keep('aatxe-sink-marker-' + Date.now())
  const g = globalThis as Record<string, unknown>
  assert.ok('__aatxe_sink__' in g, 'keep() should publish on globalThis under __aatxe_sink__')
  assert.equal(typeof g['__aatxe_sink__'], 'string')
})

test('summarizeSamples computes p99 distinct from p95 on a long tail', () => {
  // Tail-heavy: 95 small + 5 large.
  const xs = [...new Array(95).fill(100), ...new Array(5).fill(10_000)]
  const s = summarizeSamples(xs)
  assert.ok(s.p95 < s.p99, `p95=${s.p95} not less than p99=${s.p99}`)
  assert.ok(s.median < s.p95, `median should be << p95 on this tail`)
})
