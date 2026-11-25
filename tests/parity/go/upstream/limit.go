package zstd

import (
	"math"
	"runtime/debug"
	"sync"
)

// One process-wide byte budget for buffers a decode allocates.
var budget struct {
	sync.Mutex
	limit    int64 // <= 0: no limit
	explicit bool
	used     int64
}

// SetMemoryLimit caps the bytes all decodes in flight may allocate for
// output growth. n <= 0 means no limit. It returns the previous limit. The
// default is half of GOMEMLIMIT if that is set, else no limit.
func SetMemoryLimit(n int64) int64 {
	budget.Lock()
	defer budget.Unlock()
	prev := limitLocked()
	if n < 0 {
		n = 0
	}
	budget.limit, budget.explicit = n, true
	return prev
}

func limitLocked() int64 {
	if budget.explicit {
		return budget.limit
	}
	if g := debug.SetMemoryLimit(-1); g != math.MaxInt64 {
		return g / 2
	}
	return 0
}

// grow reserves n bytes for this decode, or fails at once.
func (d *Decoder) grow(n int) error {
	budget.Lock()
	defer budget.Unlock()
	if l := limitLocked(); l > 0 && budget.used+int64(n) > l {
		return ErrMemoryLimit
	}
	budget.used += int64(n)
	d.reserve += int64(n)
	return nil
}

// releaseAll returns this decode's reservation.
func (d *Decoder) releaseAll() {
	if d.reserve == 0 {
		return
	}
	budget.Lock()
	budget.used -= d.reserve
	budget.Unlock()
	d.reserve = 0
}
