package upload

import (
	"context"
	"log"
	"time"
)

// cleanupOldUploads deletes uploads older than 24h from the store.
// Bails immediately if `ctx` is cancelled — the caller is responsible
// for handing in a context that lives long enough for the cleanup to
// finish. A typical scan touches ~10k rows and runs for ~5–30 seconds.
func cleanupOldUploads(ctx context.Context, store *Store) {
	if err := ctx.Err(); err != nil {
		// Fast path: caller already gave up. Treat this as a no-op so
		// we don't waste a goroutine slot.
		return
	}

	cutoff := time.Now().Add(-24 * time.Hour)
	rows, err := store.ListOlderThan(ctx, cutoff)
	if err != nil {
		log.Printf("cleanup: list older than %s: %v", cutoff, err)
		return
	}

	for _, id := range rows {
		if err := ctx.Err(); err != nil {
			// Context cancelled mid-loop — stop, leaving the rest for
			// the next call.
			return
		}
		if err := store.Delete(ctx, id); err != nil {
			log.Printf("cleanup: delete %s: %v", id, err)
		}
	}
}
