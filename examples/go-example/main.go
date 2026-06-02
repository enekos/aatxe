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

	aatxe "github.com/enekosarasola/aatxe/sdk/go"
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
	s.EmitStdout()
}
