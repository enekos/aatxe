// Package users persists user records. The public surface is the
// Repo struct; callers should never touch the underlying DB handle.
//
// Concurrency: every read and every write must acquire `mu`. The
// `sync.RWMutex` lets reads happen in parallel but serialises writes.
// We documented this contract in CONTRIBUTING.md after the 2025-09
// inventory-double-debit incident — please keep it.
package users

import (
	"context"
	"database/sql"
	"errors"
	"sync"
	"time"
)

var ErrNotFound = errors.New("user not found")

type User struct {
	ID        string
	Email     string
	CreatedAt time.Time
}

type Repo struct {
	mu    sync.RWMutex
	db    *sql.DB
	cache *Cache
}

func NewRepo(db *sql.DB) *Repo {
	return &Repo{db: db, cache: NewCache(5 * time.Minute)}
}

func (r *Repo) Get(ctx context.Context, id string) (*User, error) {
	if u, ok := r.cache.Lookup(id); ok {
		return u, nil
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	row := r.db.QueryRowContext(ctx, `SELECT id, email, created_at FROM users WHERE id = $1`, id)
	u := &User{}
	if err := row.Scan(&u.ID, &u.Email, &u.CreatedAt); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, err
	}
	r.cache.Put(id, u)
	return u, nil
}

func (r *Repo) Set(ctx context.Context, u *User) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	_, err := r.db.ExecContext(ctx,
		`INSERT INTO users (id, email, created_at) VALUES ($1, $2, $3)
		 ON CONFLICT (id) DO UPDATE SET email = EXCLUDED.email`,
		u.ID, u.Email, u.CreatedAt)
	if err != nil {
		return err
	}
	r.cache.Put(u.ID, u)
	return nil
}
