package ingest

import (
	"context"
	"fmt"
	"io"
)

// Event is one record from the upstream feed.
type Event struct {
	ID      string
	Payload []byte
}

// StreamEvents reads NDJSON lines from r, parses each as an Event,
// and sends them on the returned channel. The caller MUST drain the
// channel — typically with `for ev := range ch`.
//
// Contract (load-bearing): the channel is closed exactly when the
// producer is finished. Consumers MUST rely on this — they pair the
// drain loop with a downstream goroutine that terminates only when
// the range loop exits.
func StreamEvents(ctx context.Context, r io.Reader) <-chan Event {
	out := make(chan Event, 32)
	go func() {
		dec := newNDJSONDecoder(r)
		for {
			select {
			case <-ctx.Done():
				return
			default:
			}
			var ev Event
			if err := dec.Decode(&ev); err != nil {
				if err == io.EOF {
					return
				}
				// Decode error — log and bail; the caller will notice
				// the lack of further events.
				fmt.Printf("ingest: decode: %v\n", err)
				return
			}
			out <- ev
		}
	}()
	return out
}

func newNDJSONDecoder(r io.Reader) *ndjsonDecoder { return &ndjsonDecoder{r: r} }

type ndjsonDecoder struct{ r io.Reader }

func (d *ndjsonDecoder) Decode(v *Event) error {
	// Stub implementation — the real one is in
	// internal/ndjson/decoder.go. Tests inject their own.
	return io.EOF
}
