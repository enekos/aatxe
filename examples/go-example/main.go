// Minimal example of an aatxe-driven Go bench runner.
//
// Build & run:
//
//	go run ./examples/go-example > aatxe.json
//
// Wire it into `aatxe run --lang go` with:
//
//	AATXE_GO_RUNNER="go run ./examples/go-example" aatxe run --lang go
package main

import (
	"strings"

	aatxe "github.com/enekos/aatxe/sdk/go"
)

func main() {
	s := aatxe.NewSuite("example-go")
	s.Bench("string_concat", func() {
		_ = "hello" + "/" + "world"
	})
	s.Bench("strings_builder", func() {
		var b strings.Builder
		b.WriteString("hello")
		b.WriteString("/")
		b.WriteString("world")
		_ = b.String()
	})
	// Parameterized: one BenchRun per repeat count ("strings_repeat/8" etc.).
	aatxe.BenchParam(s, "strings_repeat", []int{8, 256}, func(n int) {
		aatxe.Keep(strings.Repeat("x", n))
	})
	s.EmitStdout()
}
