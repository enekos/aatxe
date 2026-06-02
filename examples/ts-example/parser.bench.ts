import { bench } from '@aatxe/bench'

// Cheap bench — the auto-batcher amortises sub-µs timings.
bench('parse: number', () => Number.parseInt('42', 10))

// Slightly heavier: JSON parse.
bench('parse: small json', () => JSON.parse('{"a":1,"b":[1,2,3]}'), {
  warmup: 3,
  minIterations: 50,
})
