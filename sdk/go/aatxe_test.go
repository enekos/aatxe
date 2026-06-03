package aatxe

import (
	"encoding/json"
	"math"
	"os"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestRunnerStringContainsRealGoVersion(t *testing.T) {
	s := NewSuite("svc")
	r := s.IntoReport()
	// We don't pin the value — Go upgrades shouldn't break the test — but the
	// hardcoded "1.x" placeholder is no longer acceptable.
	if r.Runner == "go 1.x" || r.Runner == "" {
		t.Fatalf("runner string still placeholder: %q", r.Runner)
	}
	// The current Go runtime version (minus the "go" prefix) must appear.
	expected := strings.TrimPrefix(runtime.Version(), "go")
	if !strings.Contains(r.Runner, expected) {
		t.Fatalf("runner %q does not contain real version %q", r.Runner, expected)
	}
}

func TestKeepDefeatsDCEAndReturnsValue(t *testing.T) {
	// Behavioural test: Keep is the identity at the value level and writes to
	// Sink. The "really defeats DCE" property is a compile-time concern that
	// can't be asserted from a test, but we can at least guarantee the
	// observable side-effect is present.
	Sink = nil
	got := Keep(1234)
	if got != 1234 {
		t.Fatalf("Keep should return its argument unchanged, got %v", got)
	}
	if v, ok := Sink.(int); !ok || v != 1234 {
		t.Fatalf("Keep should write to Sink, got %#v", Sink)
	}
	// Generic — works on any type.
	type pair struct{ a, b string }
	Keep(pair{"x", "y"})
	if p, ok := Sink.(pair); !ok || p.a != "x" || p.b != "y" {
		t.Fatalf("Keep should be generic, got %#v", Sink)
	}
}

func TestSummaryAgreesWithHandComputed(t *testing.T) {
	xs := []float64{1, 2, 3, 4, 5, 6, 7, 8, 9, 10}
	s := computeSummary(xs)
	if math.Abs(s.Mean-5.5) > 1e-9 {
		t.Fatalf("mean: %v", s.Mean)
	}
	if math.Abs(s.Median-5.5) > 1e-9 {
		t.Fatalf("median: %v", s.Median)
	}
	if s.Min != 1 || s.Max != 10 {
		t.Fatalf("min/max: %v %v", s.Min, s.Max)
	}
	if math.Abs(s.IQR-4.5) > 1e-9 {
		t.Fatalf("iqr: %v", s.IQR)
	}
}

func TestSuiteEmitsRunReport(t *testing.T) {
	s := NewSuite("svc")
	s.Bench("noop", func() { _ = 1 + 1 })
	r := s.IntoReport()
	if r.Language != "go" {
		t.Fatalf("expected language=go, got %s", r.Language)
	}
	if r.SchemaVersion != SchemaVersion {
		t.Fatalf("schema version: %d", r.SchemaVersion)
	}
	if len(r.Runs) != 1 {
		t.Fatalf("expected 1 run, got %d", len(r.Runs))
	}
	if r.Runs[0].Name != "noop" {
		t.Fatalf("name: %s", r.Runs[0].Name)
	}
	if r.Runs[0].Iterations < 30 {
		t.Fatalf("expected min 30 iterations, got %d", r.Runs[0].Iterations)
	}
	// JSON round-trip preserves keys aatxe-core depends on.
	buf, err := json.Marshal(r)
	if err != nil {
		t.Fatal(err)
	}
	var parsed map[string]any
	if err := json.Unmarshal(buf, &parsed); err != nil {
		t.Fatal(err)
	}
	for _, k := range []string{"schemaVersion", "language", "service", "ref", "runs"} {
		if _, ok := parsed[k]; !ok {
			t.Fatalf("missing key: %s", k)
		}
	}
}

func TestPercentileInterp(t *testing.T) {
	xs := []float64{1, 2, 3, 4, 5}
	if percentileSorted(xs, 0) != 1 {
		t.Fatal("p0")
	}
	if percentileSorted(xs, 50) != 3 {
		t.Fatal("p50")
	}
	if percentileSorted(xs, 100) != 5 {
		t.Fatal("p100")
	}
	if math.Abs(percentileSorted(xs, 12.5)-1.5) > 1e-9 {
		t.Fatal("p12.5")
	}
}

func TestDefaultOptions(t *testing.T) {
	o := DefaultOptions()
	if o.Warmup != 5 || o.MinIterations != 30 || o.MaxIterations != 200 {
		t.Fatalf("unexpected defaults: %+v", o)
	}
	if o.TimeBudget != time.Second {
		t.Fatalf("time budget: %v", o.TimeBudget)
	}
	if o.TargetCV != 0.02 {
		t.Fatalf("target cv: %v", o.TargetCV)
	}
}

func TestBenchWithFiveIterations(t *testing.T) {
	s := NewSuite("svc")
	// Fixed-shape opts let us pin the iteration count exactly. File is
	// overridden via Options.File rather than the now-removed positional
	// argument; an empty File would derive from runtime.Caller.
	opts := Options{
		Warmup:        0,
		MinIterations: 5,
		MaxIterations: 5,
		TimeBudget:    time.Second,
		TargetCV:      0,
		File:          "fixture.go",
	}
	s.BenchWith("pinned", opts, func() {
		_ = 1 + 1
	})
	r := s.IntoReport()
	if got := r.Runs[0].Iterations; got != 5 {
		t.Fatalf("expected 5 iterations, got %d", got)
	}
	if r.Runs[0].File != "fixture.go" {
		t.Fatalf("file: %s", r.Runs[0].File)
	}
}

func TestBenchDerivesFileFromCaller(t *testing.T) {
	s := NewSuite("svc")
	s.Bench("from-caller", func() { _ = 1 })
	r := s.IntoReport()
	// Should resolve to this test file's path (relative to CWD).
	if !strings.Contains(r.Runs[0].File, "aatxe_test.go") {
		t.Fatalf("expected file to contain aatxe_test.go, got %q", r.Runs[0].File)
	}
}

func TestEnvOverridesServiceAndRef(t *testing.T) {
	t.Setenv("AATXE_SERVICE", "from-env")
	t.Setenv("AATXE_REF", "ref-abc")
	s := NewSuite("ignored")
	r := s.IntoReport()
	if r.Service != "from-env" {
		t.Fatalf("service: %s", r.Service)
	}
	if r.Ref != "ref-abc" {
		t.Fatalf("ref: %s", r.Ref)
	}
}

func TestMultipleBenchesAccumulate(t *testing.T) {
	s := NewSuite("multi")
	s.Bench("one", func() { _ = 1 })
	s.Bench("two", func() { _ = 2 })
	s.Bench("three", func() { _ = 3 })
	r := s.IntoReport()
	if len(r.Runs) != 3 {
		t.Fatalf("expected 3 runs, got %d", len(r.Runs))
	}
	want := []string{"one", "two", "three"}
	for i, n := range want {
		if r.Runs[i].Name != n {
			t.Fatalf("run %d name: got %s, want %s", i, r.Runs[i].Name, n)
		}
	}
}

func TestComputeSummaryConstantSamples(t *testing.T) {
	// All-equal samples should give zero spread but a valid mean/median.
	xs := []float64{42, 42, 42, 42, 42}
	s := computeSummary(xs)
	if s.Mean != 42 || s.Median != 42 || s.P99 != 42 {
		t.Fatalf("constant samples: %+v", s)
	}
	if s.Stddev != 0 || s.CV != 0 || s.IQR != 0 || s.MAD != 0 {
		t.Fatalf("spread should be zero: %+v", s)
	}
}

func TestComputeSummaryEmptyDoesNotPanic(t *testing.T) {
	s := computeSummary(nil)
	if s.Mean != 0 || s.Median != 0 {
		t.Fatalf("empty summary should be zeroed: %+v", s)
	}
}

func TestRoundTripJSONMatchesCanonicalKeys(t *testing.T) {
	s := NewSuite("svc")
	s.Bench("noop", func() {})
	buf, err := json.Marshal(s.IntoReport())
	if err != nil {
		t.Fatal(err)
	}
	// Aatxe-core deserialises camelCase. Each must be present at the top level
	// and inside the bench run.
	for _, fragment := range []string{
		`"schemaVersion":`, `"language":`, `"startedAt":`, `"finishedAt":`,
		`"batchSize":`, `"elapsedNs":`, `"trimmedMean":`, `"p95":`,
	} {
		if !contains(string(buf), fragment) {
			t.Fatalf("missing key fragment %q in JSON: %s", fragment, buf)
		}
	}
}

func contains(haystack, needle string) bool {
	for i := 0; i+len(needle) <= len(haystack); i++ {
		if haystack[i:i+len(needle)] == needle {
			return true
		}
	}
	return false
}

// Compile-time check: os.Stdout is fine; we don't shadow it.
var _ = os.Stdout
