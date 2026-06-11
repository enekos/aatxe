# @aatxe/bench

TypeScript SDK + runner for [aatxe](https://github.com/enekos/aatxe).

Write benches as plain TS:

```ts
import { bench } from '@aatxe/bench'

bench('parse: happy path', () => parsePhone('+34 612 345 678'))

bench('parse: cold path', async () => {
  await parsePdfBuffer(buffer)
}, { warmup: 2, minIterations: 10, timeBudgetMs: 5000 })
```

**Async benches must use `async () => { ... }` syntax** so the runner can
route them to the async hot loop. A sync-shaped function that returns a
Promise (e.g. `() => fetch(...)`) will be timed as sync, producing
nonsense — wrap it: `async () => fetch(...)`.

Parameterize over input sizes with `params` — one `BenchRun` per entry,
named `name/param`. The param arrives as the fn's second argument and as
`setup`'s first:

```ts
bench<number[], number>('serialise', arr => { keep(JSON.stringify(arr)) }, {
  params: [10, 1e3, 1e5],
  setup: n => makeInput(n),
})
```

Params must stringify uniquely (`String(param)` becomes the run-name
suffix), so prefer numbers and strings over objects.

Run them locally:

```bash
npx aatxe-ts-runner > aatxe.json
```

…or via the aatxe CLI:

```bash
aatxe run --lang ts --out aatxe.json
```

In CI, `aatxe run` produces `aatxe.json` on both base and head, then
`aatxe compare` produces the verdict and `aatxe comment` posts the sticky
PR comment.

See the [main README](../../README.md) for the full pipeline.
