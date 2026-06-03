package ingest

import (
	"context"
	"sync"
)

// Indexer indexes events into the search backend. Drains an Event
// channel until the channel is closed (NOT until ctx is cancelled —
// the contract on StreamEvents is that the producer closes when done,
// see producer.go:18).
type Indexer struct {
	backend SearchBackend
}

type SearchBackend interface {
	Index(ctx context.Context, id string, payload []byte) error
}

func (i *Indexer) Run(ctx context.Context, events <-chan Event) error {
	var wg sync.WaitGroup
	const workers = 4
	errCh := make(chan error, workers)

	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for ev := range events {
				if err := i.backend.Index(ctx, ev.ID, ev.Payload); err != nil {
					errCh <- err
					return
				}
			}
		}()
	}

	wg.Wait()
	close(errCh)
	for err := range errCh {
		if err != nil {
			return err
		}
	}
	return nil
}
