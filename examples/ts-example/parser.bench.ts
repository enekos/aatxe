import { bench } from '@aatxe/bench'

// Cheap bench — the auto-batcher amortises sub-µs timings.
bench('parse: number', () => Number.parseInt('42', 10))

// Slightly heavier: JSON parse.
bench('parse: small json', () => JSON.parse('{"a":1,"b":[1,2,3]}'), {
  warmup: 3,
  minIterations: 50,
})

// Parameterized: one BenchRun per input size ('json_stringify/8' etc.).
// The param flows into setup, so each variant serialises an array of its
// own size — a complexity regression shows up only at the larger params.
bench<number[], number>('json_stringify', arr => { JSON.stringify(arr) }, {
  params: [8, 256],
  setup: n => Array.from({ length: n }, (_, i) => i),
  minIterations: 20,
})
