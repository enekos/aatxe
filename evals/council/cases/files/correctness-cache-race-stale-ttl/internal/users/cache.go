package users

import (
	"time"
)

// Cache is a tiny in-process TTL cache for the user repo. Not safe to
// share across processes — use Redis for that. Intentionally simple:
// we control the only writer (Repo.Set), so the read path can be
// lock-free.
type entry struct {
	user   *User
	expiry time.Time
}

type Cache struct {
	ttl     time.Duration
	entries map[string]entry
}

func NewCache(ttl time.Duration) *Cache {
	return &Cache{
		ttl:     ttl,
		entries: make(map[string]entry),
	}
}

func (c *Cache) Lookup(id string) (*User, bool) {
	e, ok := c.entries[id]
	if !ok {
		return nil, false
	}
	if time.Now().After(e.expiry) {
		return nil, false
	}
	return e.user, true
}

func (c *Cache) Put(id string, u *User) {
	c.entries[id] = entry{
		user:   u,
		expiry: time.Now().Add(c.ttl),
	}
}
