// Package zstd decodes Zstandard (RFC 8878) frames.
//
// It is decode only: no dictionaries, no legacy formats and no streaming
// reader. Output matches libzstd byte for byte on every valid frame.
package zstd

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"unsafe"

	"github.com/dobbo-ca/zstd-go/internal/hook"
)

// Errors returned by the decoder wrap one of these, so errors.Is works.
var (
	ErrCorrupt        = errors.New("zstd: corrupt input")
	ErrChecksum       = errors.New("zstd: checksum mismatch")
	ErrDictionary     = errors.New("zstd: dictionaries are not supported")
	ErrMemoryLimit    = errors.New("zstd: memory limit exceeded")
	ErrWindowTooLarge = errors.New("zstd: window too large")
)

// Pre-wrapped so the error paths stay allocation free.
var (
	errTruncated = fmt.Errorf("%w: truncated input", ErrCorrupt)
	errMagic     = fmt.Errorf("%w: unknown magic number", ErrCorrupt)
	errHeader    = fmt.Errorf("%w: bad frame header", ErrCorrupt)
	errBlock     = fmt.Errorf("%w: bad block", ErrCorrupt)
	errLiterals  = fmt.Errorf("%w: bad literals section", ErrCorrupt)
	errHuffman   = fmt.Errorf("%w: bad huffman stream", ErrCorrupt)
	errFSE       = fmt.Errorf("%w: bad FSE table", ErrCorrupt)
	errSequences = fmt.Errorf("%w: bad sequences section", ErrCorrupt)
	errOffset    = fmt.Errorf("%w: match offset out of range", ErrCorrupt)
	errSize      = fmt.Errorf("%w: content size mismatch", ErrCorrupt)
	errOverlap   = errors.New("zstd: src overlaps the spare capacity of dst")
)

const (
	blockSizeMax = 128 << 10
	// ZSTD_decompressStream's default limit, for frames without a size.
	maxWindow = 1<<27 + 1
	// wildcopy may write this far past the current output position.
	wildcopySlack = 32
)

// Decoder is reusable and not safe for concurrent use. Zero allocations per
// DecodeAll when dst has room for the content.
type Decoder struct {
	// Literals regenerated for the current block, plus wildcopy read slack.
	lit [blockSizeMax + wildcopySlack]byte

	huf     hufTable
	seq     seqTables
	rep     [3]uint64
	litOK   bool // a Huffman table exists for treeless literals
	fseOK   bool // sequence tables exist for repeat mode
	reserve int64

	blockDone func(out []byte) // set by package czi
}

func init() {
	hook.SetBlockDone = func(d any, fn func([]byte)) { d.(*Decoder).blockDone = fn }
}

// NewDecoder returns a Decoder.
func NewDecoder() *Decoder { return new(Decoder) }

// DecodeAll appends the decoded frames in src to dst. A frame with a
// content size decodes as libzstd's one-shot ZSTD_decompress does; one
// without follows ZSTD_decompressStream's limits and may use the spare
// capacity of dst past the returned length as scratch.
func (d *Decoder) DecodeAll(dst, src []byte) ([]byte, error) {
	return d.DecodeAllContext(context.Background(), dst, src)
}

// DecodeAllContext is DecodeAll with ctx checked between blocks.
func (d *Decoder) DecodeAllContext(ctx context.Context, dst, src []byte) ([]byte, error) {
	if overlaps(src, dst[len(dst):cap(dst)]) {
		return dst, errOverlap
	}
	defer d.releaseAll()
	start := len(dst)
	for len(src) >= 5 {
		magic := le32(src)
		if magic&0xFFFFFFF0 == 0x184D2A50 {
			n, err := skippableSize(src)
			if err != nil {
				return dst[:start], err
			}
			src = src[n:]
			continue
		}
		out, n, err := d.decodeFrame(ctx, dst, src)
		if err != nil {
			return dst[:start], err
		}
		dst, src = out, src[n:]
	}
	if len(src) != 0 {
		return dst[:start], errTruncated
	}
	return dst, nil
}

var pool = sync.Pool{New: func() any { return NewDecoder() }}

// DecodeAll appends the decoded frames in src to dst using a pooled Decoder.
func DecodeAll(dst, src []byte) ([]byte, error) {
	return DecodeAllContext(context.Background(), dst, src)
}

// DecodeAllContext is DecodeAll with ctx checked between blocks.
func DecodeAllContext(ctx context.Context, dst, src []byte) ([]byte, error) {
	d := pool.Get().(*Decoder)
	out, err := d.DecodeAllContext(ctx, dst, src)
	pool.Put(d)
	return out, err
}

// ContentSize reports the first frame's content size, ok=false if absent.
// A skippable frame decodes to nothing, so it reports 0.
func ContentSize(src []byte) (n uint64, ok bool, err error) {
	if len(src) >= 4 && le32(src)&0xFFFFFFF0 == 0x184D2A50 {
		if len(src) < 8 {
			return 0, false, errTruncated
		}
		return 0, true, nil
	}
	h, err := parseFrameHeader(src)
	if err != nil {
		return 0, false, err
	}
	return h.fcs, h.hasFCS, nil
}

func skippableSize(src []byte) (int, error) {
	if len(src) < 8 {
		return 0, errTruncated
	}
	n := uint64(le32(src[4:])) + 8
	if n > uint64(len(src)) {
		return 0, errTruncated
	}
	return int(n), nil
}

// overlaps reports whether a and b share memory.
func overlaps(a, b []byte) bool {
	if len(a) == 0 || len(b) == 0 {
		return false
	}
	a0 := uintptr(unsafe.Pointer(unsafe.SliceData(a)))
	b0 := uintptr(unsafe.Pointer(unsafe.SliceData(b)))
	return a0 < b0+uintptr(len(b)) && b0 < a0+uintptr(len(a))
}
