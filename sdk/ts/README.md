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
