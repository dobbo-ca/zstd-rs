// Package czi decodes CZI subblock payloads compressed with zstd0 (mode 5)
// or zstd1 (mode 6), as libCZI does.
package czi

import (
	"context"
	"errors"
	"fmt"
	"sync"

	zstd "github.com/dobbo-ca/zstd-go"
	"github.com/dobbo-ca/zstd-go/internal/hook"
)

// Compression modes, as stored in the subblock.
const (
	Zstd0 = 5
	Zstd1 = 6
)

// Pixel types, as stored in the subblock. Hi/lo packing needs 16-bit words.
const (
	Gray16 = 1
	Bgr48  = 4
)

var (
	errMode     = errors.New("czi: compression mode is not zstd0 or zstd1")
	errHeader   = fmt.Errorf("%w: bad zstd1 header", zstd.ErrCorrupt)
	errHiLo     = fmt.Errorf("%w: hi/lo packing needs Gray16 or Bgr48 and an even size", zstd.ErrCorrupt)
	errNoSize   = fmt.Errorf("%w: frame has no content size", zstd.ErrCorrupt)
	errSizeLeft = fmt.Errorf("%w: frames decode to more than the first frame's content size", zstd.ErrCorrupt)
)

// Decoder is reusable and not safe for concurrent use.
type Decoder struct {
	z    *zstd.Decoder
	blob []byte // decoded planes when they cannot go straight to dst

	// Hi/lo state for the block hook.
	dst  []byte
	half int // words in each plane
	done int // words packed so far
}

// NewDecoder returns a Decoder.
func NewDecoder() *Decoder {
	d := &Decoder{z: zstd.NewDecoder()}
	hook.SetBlockDone(d.z, d.packed)
	return d
}

// Decode decodes src, a subblock payload of the given compression mode, into
// dst. len(dst) must be the bitmap size at minimal stride (width * height *
// bytes per pixel). pixelType is only used to check hi/lo packing. As
// libCZI's default does, a frame content size smaller than len(dst) leaves
// the rest of dst zero, and a larger one is cut to len(dst). On error the
// contents of dst are undefined.
func (d *Decoder) Decode(ctx context.Context, dst, src []byte, mode, pixelType int) error {
	hilo := false
	switch mode {
	case Zstd0:
	case Zstd1:
		n, h, err := parseHeader(src)
		if err != nil {
			return err
		}
		// 16-bit pixels make an even bitmap size.
		if h && (pixelType != Gray16 && pixelType != Bgr48 || len(dst)%2 != 0) {
			return errHiLo
		}
		src, hilo = src[n:], h
	default:
		return errMode
	}
	fcs, ok, err := zstd.ContentSize(src)
	if err != nil {
		return err
	}
	if !ok {
		return errNoSize
	}
	if hilo {
		return d.decodeHiLo(ctx, dst, src, fcs)
	}
	if fcs <= uint64(len(dst)) {
		out, err := d.z.DecodeAllContext(ctx, dst[:0:len(dst)], src)
		if err != nil {
			return err
		}
		if uint64(len(out)) != fcs {
			return errSizeLeft
		}
		clear(dst[fcs:])
		return nil
	}
	out, err := d.decodeBlob(ctx, src, fcs)
	if err != nil {
		return err
	}
	copy(dst, out)
	return nil
}

// decodeBlob decodes src into d.blob and checks the total size.
func (d *Decoder) decodeBlob(ctx context.Context, src []byte, fcs uint64) ([]byte, error) {
	out, err := d.z.DecodeAllContext(ctx, d.blob[:0], src)
	if cap(out) > cap(d.blob) {
		d.blob = out[:0]
	}
	if err != nil {
		return nil, err
	}
	if uint64(len(out)) != fcs {
		return nil, errSizeLeft
	}
	return out, nil
}

// decodeHiLo decodes the low and high byte planes and interleaves each
// block's words into dst while the block is still in cache.
func (d *Decoder) decodeHiLo(ctx context.Context, dst, src []byte, fcs uint64) error {
	d.dst, d.half, d.done = dst, int(fcs/2), 0
	_, err := d.decodeBlob(ctx, src, fcs)
	d.dst = nil
	if err != nil {
		return err
	}
	clear(dst[2*min(d.half, len(dst)/2):])
	return nil
}

// packed interleaves the words whose high byte out now holds.
func (d *Decoder) packed(out []byte) {
	if d.dst == nil {
		return
	}
	n := min(len(out)-d.half, d.half, len(d.dst)/2)
	if n <= d.done {
		return
	}
	interleave(d.dst[2*d.done:2*n], out[d.done:n], out[d.half+d.done:d.half+n])
	d.done = n
}

// parseHeader parses a zstd1 header: {01}, or {03 01 xx} where bit 0 of xx
// is the hi/lo flag. It reports the header size.
func parseHeader(src []byte) (n int, hilo bool, err error) {
	switch {
	case len(src) > 1 && src[0] == 1:
		return 1, false, nil
	case len(src) > 3 && src[0] == 3 && src[1] == 1:
		return 3, src[2]&1 != 0, nil
	}
	return 0, false, errHeader
}

var pool = sync.Pool{New: func() any { return NewDecoder() }}

// Decode is Decoder.Decode with a pooled Decoder.
func Decode(ctx context.Context, dst, src []byte, mode, pixelType int) error {
	d := pool.Get().(*Decoder)
	err := d.Decode(ctx, dst, src, mode, pixelType)
	pool.Put(d)
	return err
}
