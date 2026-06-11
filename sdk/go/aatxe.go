// Package aatxe is the Go SDK for the aatxe microbenchmark + regression tool.
//
// Build a small `main` that uses [Suite] / [Suite.Bench] and writes the
// finished report to stdout with [Suite.EmitStdout]. Wire it into
// `aatxe run --lang go` by setting `AATXE_GO_RUNNER` to invoke your binary.
//
//	func main() {
//	    s := aatxe.NewSuite("my-svc")
//	    s.Bench("noop", func() { _ = 1 + 1 })
//	    s.EmitStdout()
//	}
//
// The resulting `RunReport` has the same shape as the Rust and TS reports,
// so the aatxe CLI compares them uniformly.
//
// # Defeating dead-code elimination
//
// Go's compiler will happily elide pure expressions whose results are
// discarded. A benchmark of `parseFoo("x")` whose return value flows to `_`
// can be optimised down to nothing, and you measure the cost of an empty
// loop. Use [Keep] to mark the value as observed:
//
//	aatxe.Keep(parseFoo("x"))
//
// `Keep` writes to a package-level sink (also exposed as [Sink] for
// authors who prefer the explicit form `aatxe.Sink = parseFoo("x")`),
// preventing the compiler from proving the call is pure.
package aatxe

import (
	"encoding/json"
	"fmt"
	"math"
	"os"
	"runtime"
	"sort"
	"time"
)

// Sink is an exported observation point used by [Keep] to defeat
// dead-code elimination inside benchmarks. Writes are deliberately not
// synchronised — benches are expected to run on the main goroutine.
//
//nolint:gochecknoglobals
var Sink any

// Keep prevents the compiler from optimising away the production of v.
// Returns v so callers can chain the call inline:
//
//	result := aatxe.Keep(parseFoo("x"))
//
//go:noinline
func Keep[T any](v T) T {
	Sink = v
	return v
}

// SchemaVersion is the on-disk format version aatxe-core understands.
const SchemaVersion = 2

// Options controls the sampling loop. Defaults mirror aatxe-core and the
// TS/Rust SDKs so a service can switch languages without re-tuning gates.
type Options struct {
	Warmup        int
	MinIterations int
	MaxIterations int
	TimeBudget    time.Duration
	TargetCV      float64
	// File overrides the source-file tag recorded for the bench. When empty,
	// the SDK derives it from runtime.Caller at the BenchWith call site.
	File string
}

// DefaultOptions returns the standard aatxe defaults.
func DefaultOptions() Options {
	return Options{
		Warmup:        5,
		MinIterations: 30,
		MaxIterations: 200,
		TimeBudget:    time.Second,
		TargetCV:      0.02,
	}
}

// Metric is a non-time metric attached to a BenchRun. See the Rust
// aatxe-core::Metric type for the canonical contract. Adding metrics does
// not bump the schema version — they're a forward-compatible extension
// point for throughput, allocations, custom counters.
type Metric struct {
	Name          string  `json:"name"`
	Value         float64 `json:"value"`
	Unit          string  `json:"unit"`
	LowerIsBetter *bool   `json:"lowerIsBetter,omitempty"`
}

// BenchRun mirrors aatxe-core's BenchRun. Field tags are in lower-camelCase
// to match the canonical JSON format.
type BenchRun struct {
	Name        string    `json:"name"`
	File        string    `json:"file"`
	Iterations  int       `json:"iterations"`
	BatchSize   int       `json:"batchSize"`
	ElapsedNs   float64   `json:"elapsedNs"`
	Samples     []float64 `json:"samples"`
	Mean        float64   `json:"mean"`
	Median      float64   `json:"median"`
	TrimmedMean float64   `json:"trimmedMean"`
	Stddev      float64   `json:"stddev"`
	CV          float64   `json:"cv"`
	MAD         float64   `json:"mad"`
	IQR         float64   `json:"iqr"`
	Min         float64   `json:"min"`
	Max         float64   `json:"max"`
	P50         float64   `json:"p50"`
	P95         float64   `json:"p95"`
	P99         float64   `json:"p99"`
	// Optional non-time metrics. Serialised only when non-empty.
	Metrics []Metric `json:"metrics,omitempty"`
	// Optional free-form tags for filtering / grouping.
	Tags []string `json:"tags,omitempty"`
}

// RunReport is the top-level on-disk structure produced by the SDK.
type RunReport struct {
	SchemaVersion int             `json:"schemaVersion"`
	Language      string          `json:"language"`
	Service       string          `json:"service"`
	Ref           string          `json:"ref"`
	Runner        string          `json:"runner"`
	StartedAt     string          `json:"startedAt"`
	FinishedAt    string          `json:"finishedAt"`
	Runs          []BenchRun      `json:"runs"`
	AffectedScope *AffectedScope  `json:"affectedScope,omitempty"`
}

// AffectedScope mirrors the Rust struct of the same name.
type AffectedScope struct {
	Base              string   `json:"base"`
	ChangedFiles      []string `json:"changedFiles"`
	BenchFiles        []string `json:"benchFiles"`
	SkippedBenchFiles []string `json:"skippedBenchFiles"`
}

// Suite holds bench results until they are emitted as a single RunReport.
type Suite struct {
	service   string
	ref       string
	runner    string
	startedAt time.Time
	runs      []BenchRun
}

// NewSuite creates a fresh Suite. Service name falls back to AATXE_SERVICE
// then the supplied argument so CI can override without code changes.
func NewSuite(service string) *Suite {
	if v := os.Getenv("AATXE_SERVICE"); v != "" {
		service = v
	}
	ref := os.Getenv("AATXE_REF")
	if ref == "" {
		ref = "HEAD"
	}
	return &Suite{
		service:   service,
		ref:       ref,
		runner:    "go " + goVersion(),
		startedAt: time.Now().UTC(),
	}
}

// Bench measures fn under [DefaultOptions] and accumulates the result.
// The source-file tag is derived from runtime.Caller.
func (s *Suite) Bench(name string, fn func()) {
	file := callerFile(2)
	s.benchInternal(name, file, DefaultOptions(), fn)
}

// BenchWith is the full-control form. Source-file tag is taken from
// opts.File when set, otherwise derived from runtime.Caller.
func (s *Suite) BenchWith(name string, opts Options, fn func()) {
	file := opts.File
	if file == "" {
		file = callerFile(2)
	}
	s.benchInternal(name, file, opts, fn)
}

func (s *Suite) benchInternal(name, file string, opts Options, fn func()) {
	samples, batchSize, elapsedNs := runLoop(opts, fn)
	s.runs = append(s.runs, summarise(name, file, samples, batchSize, elapsedNs))
}

// BenchParam measures fn once per parameter under [DefaultOptions],
// recording one BenchRun named "name/param" per entry (labelled via
// fmt.Sprint). A regression that appears only at large params reads as a
// complexity change rather than a constant-factor one:
//
//	aatxe.BenchParam(s, "parse", []int{10, 1_000, 100_000}, func(n int) {
//	    aatxe.Keep(parse(inputs[n]))
//	})
//
// Free function rather than a Suite method because Go methods cannot
// declare their own type parameters.
func BenchParam[P any](s *Suite, name string, params []P, fn func(p P)) {
	benchParam(s, name, callerFile(2), DefaultOptions(), params, fn)
}

// BenchParamWith is the full-control form of [BenchParam]. The source-file
// tag is taken from opts.File when set, otherwise derived from
// runtime.Caller.
func BenchParamWith[P any](s *Suite, name string, opts Options, params []P, fn func(p P)) {
	file := opts.File
	if file == "" {
		file = callerFile(2)
	}
	benchParam(s, name, file, opts, params, fn)
}

// benchParam panics on registration mistakes (empty params, params whose
// fmt.Sprint forms collide) — failing loud at bench-author time beats
// emitting a report whose run names silently shadow each other.
func benchParam[P any](s *Suite, name, file string, opts Options, params []P, fn func(p P)) {
	if len(params) == 0 {
		panic(fmt.Sprintf("aatxe: BenchParam %q: empty params", name))
	}
	seen := make(map[string]bool, len(params))
	for _, p := range params {
		label := fmt.Sprint(p)
		if seen[label] {
			panic(fmt.Sprintf(
				"aatxe: BenchParam %q: params print to duplicate label %q — use values with unique fmt.Sprint forms",
				name, label,
			))
		}
		seen[label] = true
		s.benchInternal(name+"/"+label, file, opts, func() { fn(p) })
	}
}

// callerFile returns a repo-relative path to the source file `skip` frames
// up the stack. Falls back to "<inline>" if runtime.Caller fails.
func callerFile(skip int) string {
	_, file, _, ok := runtime.Caller(skip)
	if !ok || file == "" {
		return "<inline>"
	}
	// Trim to repo-relative when CWD is a prefix; otherwise return as-is.
	if cwd, err := os.Getwd(); err == nil && len(file) > len(cwd) && file[:len(cwd)] == cwd {
		// +1 to drop the leading separator.
		return file[len(cwd)+1:]
	}
	return file
}

// IntoReport finalises the suite into a RunReport without emitting it.
// Useful for tests.
func (s *Suite) IntoReport() RunReport {
	return RunReport{
		SchemaVersion: SchemaVersion,
		Language:      "go",
		Service:       s.service,
		Ref:           s.ref,
		Runner:        s.runner,
		StartedAt:     s.startedAt.Format(time.RFC3339Nano),
		FinishedAt:    time.Now().UTC().Format(time.RFC3339Nano),
		Runs:          s.runs,
	}
}

// EmitStdout writes the finished report as JSON to stdout. This is the
// integration point with `aatxe run --lang go`.
func (s *Suite) EmitStdout() {
	r := s.IntoReport()
	buf, err := json.MarshalIndent(r, "", "  ")
	if err != nil {
		fmt.Fprintln(os.Stderr, "aatxe: emit:", err)
		os.Exit(1)
	}
	fmt.Println(string(buf))
}

// runLoop drives the adaptive sampling loop and returns per-sample timings
// (in nanoseconds), the resolved batch size, and the total measured time.
func runLoop(opts Options, fn func()) ([]float64, int, float64) {
	batchSize := calibrateBatch(fn)

	for i := 0; i < opts.Warmup; i++ {
		runBatch(fn, batchSize)
	}

	samples := make([]float64, 0, opts.MinIterations)
	start := time.Now()
	totalNs := 0.0
	for i := 0; i < opts.MaxIterations; i++ {
		t0 := time.Now()
		runBatch(fn, batchSize)
		dt := time.Since(t0)
		ns := float64(dt.Nanoseconds()) / float64(batchSize)
		samples = append(samples, ns)
		totalNs += float64(dt.Nanoseconds())
		if i+1 >= opts.MinIterations {
			budget := time.Since(start) >= opts.TimeBudget
			cv := coefficientOfVariation(samples)
			cvDone := opts.TargetCV > 0 && cv > 0 && cv <= opts.TargetCV
			if cvDone || budget {
				break
			}
		}
	}
	return samples, batchSize, totalNs
}

func runBatch(fn func(), n int) {
	for i := 0; i < n; i++ {
		fn()
	}
}

func calibrateBatch(fn func()) int {
	n := 1
	for n < 1<<20 {
		t0 := time.Now()
		runBatch(fn, n)
		if time.Since(t0) >= 50*time.Microsecond {
			return n
		}
		n *= 2
	}
	return n
}

func summarise(name, file string, samples []float64, batchSize int, elapsedNs float64) BenchRun {
	s := computeSummary(samples)
	return BenchRun{
		Name:        name,
		File:        file,
		Iterations:  len(samples),
		BatchSize:   batchSize,
		ElapsedNs:   elapsedNs,
		Samples:     samples,
		Mean:        s.Mean,
		Median:      s.Median,
		TrimmedMean: s.TrimmedMean,
		Stddev:      s.Stddev,
		CV:          s.CV,
		MAD:         s.MAD,
		IQR:         s.IQR,
		Min:         s.Min,
		Max:         s.Max,
		P50:         s.Median,
		P95:         s.P95,
		P99:         s.P99,
	}
}

type summary struct {
	Mean, Median, TrimmedMean, Stddev, CV, MAD, IQR, Min, Max, P50, P95, P99 float64
}

func computeSummary(xs []float64) summary {
	if len(xs) == 0 {
		return summary{}
	}
	var s summary
	min, max := xs[0], xs[0]
	mean, m2 := 0.0, 0.0
	for i, x := range xs {
		if x < min {
			min = x
		}
		if x > max {
			max = x
		}
		delta := x - mean
		mean += delta / float64(i+1)
		delta2 := x - mean
		m2 += delta * delta2
	}
	stddev := 0.0
	if len(xs) >= 2 {
		stddev = math.Sqrt(m2 / float64(len(xs)-1))
	}
	cv := 0.0
	if mean != 0 {
		cv = stddev / mean
	}
	sorted := make([]float64, len(xs))
	copy(sorted, xs)
	sort.Float64s(sorted)
	median := percentileSorted(sorted, 50)
	iqr := percentileSorted(sorted, 75) - percentileSorted(sorted, 25)
	p95 := percentileSorted(sorted, 95)
	p99 := percentileSorted(sorted, 99)
	mad := madFromSorted(sorted, median)
	cut := int(float64(len(xs)) * 0.05)
	trimmed := sorted[cut : len(sorted)-cut]
	tm := 0.0
	if len(trimmed) > 0 {
		sum := 0.0
		for _, v := range trimmed {
			sum += v
		}
		tm = sum / float64(len(trimmed))
	} else {
		tm = mean
	}
	s.Mean, s.Median, s.TrimmedMean = mean, median, tm
	s.Stddev, s.CV, s.MAD, s.IQR = stddev, cv, mad, iqr
	s.Min, s.Max = min, max
	s.P50, s.P95, s.P99 = median, p95, p99
	return s
}

func percentileSorted(sorted []float64, p float64) float64 {
	if len(sorted) == 0 {
		return 0
	}
	rank := (p / 100) * float64(len(sorted)-1)
	lo := int(math.Floor(rank))
	hi := int(math.Ceil(rank))
	if lo == hi {
		return sorted[lo]
	}
	frac := rank - float64(lo)
	return sorted[lo]*(1-frac) + sorted[hi]*frac
}

func madFromSorted(sorted []float64, med float64) float64 {
	devs := make([]float64, len(sorted))
	for i, v := range sorted {
		devs[i] = math.Abs(v - med)
	}
	sort.Float64s(devs)
	return percentileSorted(devs, 50)
}

func coefficientOfVariation(xs []float64) float64 {
	if len(xs) < 2 {
		return 0
	}
	mean, m2 := 0.0, 0.0
	for i, x := range xs {
		delta := x - mean
		mean += delta / float64(i+1)
		delta2 := x - mean
		m2 += delta * delta2
	}
	if mean == 0 {
		return 0
	}
	return math.Sqrt(m2/float64(len(xs)-1)) / mean
}

// goVersion returns the Go toolchain version that built the running binary
// (e.g. "go1.22.3"). Used purely as a label in the RunReport — aatxe never
// gates on this string, so tests that pin specific values are inappropriate.
func goVersion() string {
	v := runtime.Version()
	// runtime.Version() already starts with "go", trim it so the runner field
	// reads "go 1.22.3" after the prefix added at the call site.
	if len(v) > 2 && v[:2] == "go" {
		return v[2:]
	}
	return v
}
