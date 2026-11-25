package zstd

import (
	"context"
	"encoding/binary"
	"math"
	"math/bits"
)

const frameMagic = 0xFD2FB528

type frameHeader struct {
	size     int // header bytes
	fcs      uint64
	hasFCS   bool
	window   uint64
	checksum bool
	dictID   uint32
}

// parseFrameHeader ports ZSTD_getFrameHeader_advanced.
func parseFrameHeader(src []byte) (h frameHeader, err error) {
	if len(src) < 5 {
		if len(src) >= 4 && le32(src) != frameMagic {
			return h, errMagic
		}
		return h, errTruncated
	}
	if le32(src) != frameMagic {
		return h, errMagic
	}
	fhd := src[4]
	single := fhd&0x20 != 0
	dictSize := [4]int{0, 1, 2, 4}[fhd&3]
	fcsSize := [4]int{0, 2, 4, 8}[fhd>>6]
	h.size = 5 + dictSize + fcsSize
	if !single {
		h.size++
	} else if fhd>>6 == 0 {
		h.size++
	}
	if len(src) < h.size {
		return h, errTruncated
	}
	if fhd&0x08 != 0 {
		return h, errHeader
	}
	pos := 5
	if !single {
		wd := src[pos]
		pos++
		wlog := uint(wd>>3) + 10
		if wlog > 31 {
			return h, ErrWindowTooLarge
		}
		h.window = 1 << wlog
		h.window += (h.window >> 3) * uint64(wd&7)
	}
	switch dictSize {
	case 1:
		h.dictID = uint32(src[pos])
	case 2:
		h.dictID = uint32(binary.LittleEndian.Uint16(src[pos:]))
	case 4:
		h.dictID = le32(src[pos:])
	}
	pos += dictSize
	h.hasFCS = true
	switch fhd >> 6 {
	case 0:
		if single {
			h.fcs = uint64(src[pos])
		} else {
			h.hasFCS = false
		}
	case 1:
		h.fcs = uint64(binary.LittleEndian.Uint16(src[pos:])) + 256
	case 2:
		h.fcs = uint64(le32(src[pos:]))
	case 3:
		h.fcs = le64(src[pos:])
	}
	if single {
		h.window = h.fcs
	}
	h.checksum = fhd&0x04 != 0
	return h, nil
}

// decodeFrame appends one frame from src to dst and reports the bytes read.
func (d *Decoder) decodeFrame(ctx context.Context, dst, src []byte) ([]byte, int, error) {
	if len(src) < 6+3 {
		return nil, 0, errTruncated
	}
	h, err := parseFrameHeader(src)
	if err != nil {
		return nil, 0, err
	}
	if len(src) < h.size+3 {
		return nil, 0, errTruncated
	}
	if h.dictID != 0 {
		return nil, 0, ErrDictionary
	}
	// Without a content size, follow ZSTD_decompressStream's checks.
	stream := !h.hasFCS
	if stream && h.window > maxWindow {
		return nil, 0, ErrWindowTooLarge
	}
	bsm := int(min(h.window, blockSizeMax))
	maxOut := math.MaxInt // decoded bytes per block
	if stream {
		maxOut = bsm
	}
	ip := h.size

	// out[base:] is this frame and len(out) the write limit. A frame that
	// does not fit dst grows as it decodes, so a lying header costs only
	// the output it really produces.
	base := len(dst)
	limit := math.MaxInt
	if h.hasFCS {
		// Each 4 input bytes yield at most one block, so a larger FCS is a lie.
		if h.fcs > uint64(len(src)/4+1)*blockSizeMax {
			return nil, 0, errSize
		}
		limit = base + int(h.fcs)
	}
	out := dst[:min(cap(dst), limit)]
	// ponytail: first growth assumes ratio <= 32; doubling covers the rest.
	hint := base + 32*len(src)

	d.rep = [3]uint64{1, 4, 8}
	d.litOK, d.fseOK = false, false
	op := base
	for {
		if ctx != nil {
			if err := ctx.Err(); err != nil {
				return nil, 0, err
			}
		}
		if len(src)-ip < 3 {
			return nil, 0, errTruncated
		}
		bh := uint32(src[ip]) | uint32(src[ip+1])<<8 | uint32(src[ip+2])<<16
		ip += 3
		last := bh&1 != 0
		btype := (bh >> 1) & 3
		size := int(bh >> 3)
		rsize := size // regenerated size bound for raw and RLE
		if btype == 1 {
			size = 1
		}
		// One-shot ZSTD_decompress checks raw and RLE sizes only against
		// the output space.
		if btype == 3 || btype == 2 && size > bsm || stream && rsize > bsm {
			return nil, 0, errBlock
		}
		if size > len(src)-ip {
			return nil, 0, errTruncated
		}
		room := rsize
		if btype == 2 {
			room = bsm // a compressed block's size is known only after decoding
		}
		if len(out)-op < room && len(out) < limit {
			if out, err = d.growOut(out, op, room, limit, hint); err != nil {
				return nil, 0, err
			}
		}
		switch btype {
		case 0:
			if size > len(out)-op {
				return nil, 0, errSize
			}
			op += copy(out[op:], src[ip:ip+size])
		case 1:
			if rsize > len(out)-op {
				return nil, 0, errSize
			}
			if rsize > 0 {
				// Doubling copies run at memmove speed.
				b := out[op : op+rsize]
				b[0] = src[ip]
				for n := 1; n < len(b); n *= 2 {
					copy(b[n:], b[:n])
				}
			}
			op += rsize
		case 2:
			n, err := d.decodeBlock(out, op, base, bsm, maxOut, src[ip:ip+size])
			if err != nil {
				return nil, 0, err
			}
			op += n
		}
		ip += size
		if d.blockDone != nil {
			d.blockDone(out[:op])
		}
		if last {
			break
		}
	}
	if h.hasFCS && uint64(op-base) != h.fcs {
		return nil, 0, errSize
	}
	if h.checksum {
		if len(src)-ip < 4 {
			return nil, 0, errTruncated
		}
		if uint32(xxh64(out[base:op])) != le32(src[ip:]) {
			return nil, 0, ErrChecksum
		}
		ip += 4
	}
	return out[:op], ip, nil
}

// growOut grows out so room more bytes fit after op, up to limit.
func (d *Decoder) growOut(out []byte, op, room, limit, hint int) ([]byte, error) {
	n := min(limit, max(2*cap(out), op+room+wildcopySlack, 64<<10, hint))
	if err := d.grow(n); err != nil {
		return nil, err
	}
	b := make([]byte, n)
	copy(b, out[:op])
	return b, nil
}

func le32(b []byte) uint32 { return binary.LittleEndian.Uint32(b) }
func le64(b []byte) uint64 { return binary.LittleEndian.Uint64(b) }

// highbit returns the index of the top set bit of x > 0.
func highbit(x uint32) uint { return uint(bits.Len32(x)) - 1 }
