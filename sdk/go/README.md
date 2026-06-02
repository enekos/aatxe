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

See the [main README](../../README.md) for the full pipeline.
