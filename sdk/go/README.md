# aatxe (Go SDK)

Authoring helpers + JSON emitter for aatxe-compatible Go benches.

```go
package svc

import (
    "testing"

    "github.com/enekos/aatxe/sdk/go"
)

func BenchmarkParseFoo(b *testing.B) {
    aatxe.Bench(b, "parseFoo", func() {
        _ = ParseFoo("hello")
    })
}
```

Or build a standalone runner:

```go
func main() {
    s := aatxe.NewSuite("my-svc")
    s.Bench("noop", func() { _ = 1 + 1 })
    s.EmitStdout()
}
```

Parameterize over input sizes with `BenchParam` — one `BenchRun` per
entry, named `name/param` (labelled via `fmt.Sprint`, so params must
print uniquely). A free function rather than a `Suite` method because Go
methods cannot declare their own type parameters:

```go
aatxe.BenchParam(s, "parse", []int{10, 1_000, 100_000}, func(n int) {
    aatxe.Keep(Parse(inputs[n]))
})
```

`BenchParamWith` is the full-control form taking `Options`.

See the [main README](../../README.md) for the full pipeline.
