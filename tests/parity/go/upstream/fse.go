package zstd

import "math/bits"

// bitReader is libzstd's BIT_DStream_t: a 64-bit container read backwards
// from the end of in. The container holds in[ptr:ptr+8].
type bitReader struct {
	in       []byte
	ptr      int
	bits     uint64
	consumed uint
}

// Results of reload, as BIT_DStream_status.
const (
	bitUnfinished = iota
	bitEndOfBuffer
	bitCompleted
	bitOverflow
)

// init ports BIT_initDStream.
func (br *bitReader) init(in []byte) bool {
	n := len(in)
	*br = bitReader{in: in}
	if n < 1 {
		return false
	}
	last := in[n-1]
	if last == 0 {
		return false
	}
	if n >= 8 {
		br.ptr = n - 8
		br.bits = le64(in[n-8:])
		br.consumed = 8 - highbit(uint32(last))
		return true
	}
	for i := n - 1; i >= 0; i-- {
		br.bits = br.bits<<8 | uint64(in[i])
	}
	br.consumed = 8 - highbit(uint32(last)) + uint(8-n)*8
	return true
}

// look is BIT_lookBits; garbage, not a panic, once past the end.
func (br *bitReader) look(n uint) uint64 {
	return br.bits >> ((64 - br.consumed - n) & 63) & (1<<n - 1)
}

func (br *bitReader) read(n uint) uint64 {
	v := br.look(n)
	br.consumed += n
	return v
}

// lookFast is BIT_lookBitsFast; n >= 1.
func (br *bitReader) lookFast(n uint) uint64 {
	return br.bits << (br.consumed & 63) >> ((64 - n) & 63)
}

// reload ports BIT_reloadDStream.
func (br *bitReader) reload() int {
	if br.consumed > 64 {
		return bitOverflow
	}
	if br.ptr >= 8 {
		br.ptr -= int(br.consumed >> 3)
		br.consumed &= 7
		br.bits = le64(br.in[br.ptr:])
		return bitUnfinished
	}
	if br.ptr == 0 {
		if br.consumed < 64 {
			return bitEndOfBuffer
		}
		return bitCompleted
	}
	nb := int(br.consumed >> 3)
	res := bitUnfinished
	if br.ptr-nb < 0 {
		nb = br.ptr
		res = bitEndOfBuffer
	}
	br.ptr -= nb
	br.consumed -= uint(nb) * 8
	br.bits = le64(br.in[br.ptr:])
	return res
}

// reloadFast ports BIT_reloadDStreamFast.
func (br *bitReader) reloadFast() int {
	if br.ptr < 8 {
		return bitOverflow
	}
	br.ptr -= int(br.consumed >> 3)
	br.consumed &= 7
	br.bits = le64(br.in[br.ptr:])
	return bitUnfinished
}

func (br *bitReader) finished() bool { return br.ptr == 0 && br.consumed == 64 }

// readNCount ports FSE_readNCount. It returns the bytes read.
func readNCount(norm []int16, maxSV *uint, tableLog *uint, hb []byte) (int, bool) {
	if len(hb) < 8 {
		var buf [8]byte
		copy(buf[:], hb)
		n, ok := readNCount(norm, maxSV, tableLog, buf[:])
		if !ok || n > len(hb) {
			return 0, false
		}
		return n, true
	}
	iend := len(hb)
	ip := 0
	maxSV1 := *maxSV + 1
	clear(norm[:maxSV1])
	bitStream := le32(hb)
	nbBits := int(bitStream&0xF) + 5
	if nbBits > 15 {
		return 0, false
	}
	bitStream >>= 4
	bitCount := 4
	*tableLog = uint(nbBits)
	remaining := 1<<nbBits + 1
	threshold := 1 << nbBits
	nbBits++
	charnum := uint(0)
	previous0 := false
	for {
		if previous0 {
			repeats := bits.TrailingZeros32(^bitStream|0x80000000) >> 1
			for repeats >= 12 {
				charnum += 3 * 12
				if ip <= iend-7 {
					ip += 3
				} else {
					bitCount -= 8 * (iend - 7 - ip)
					bitCount &= 31
					ip = iend - 4
				}
				bitStream = le32(hb[ip:]) >> bitCount
				repeats = bits.TrailingZeros32(^bitStream|0x80000000) >> 1
			}
			charnum += uint(3 * repeats)
			bitStream >>= 2 * repeats
			bitCount += 2 * repeats
			charnum += uint(bitStream & 3)
			bitCount += 2
			if charnum >= maxSV1 {
				break
			}
			if ip <= iend-7 || ip+bitCount>>3 <= iend-4 {
				ip += bitCount >> 3
				bitCount &= 7
			} else {
				bitCount -= 8 * (iend - 4 - ip)
				bitCount &= 31
				ip = iend - 4
			}
			bitStream = le32(hb[ip:]) >> bitCount
		}
		max := (2*threshold - 1) - remaining
		var count int
		if int(bitStream&uint32(threshold-1)) < max {
			count = int(bitStream & uint32(threshold-1))
			bitCount += nbBits - 1
		} else {
			count = int(bitStream & uint32(2*threshold-1))
			if count >= threshold {
				count -= max
			}
			bitCount += nbBits
		}
		count--
		if count >= 0 {
			remaining -= count
		} else {
			remaining += count
		}
		norm[charnum] = int16(count)
		charnum++
		previous0 = count == 0
		if remaining < threshold {
			if remaining <= 1 {
				break
			}
			nbBits = int(highbit(uint32(remaining))) + 1
			threshold = 1 << (nbBits - 1)
		}
		if charnum >= maxSV1 {
			break
		}
		if ip <= iend-7 || ip+bitCount>>3 <= iend-4 {
			ip += bitCount >> 3
			bitCount &= 7
		} else {
			bitCount -= 8 * (iend - 4 - ip)
			bitCount &= 31
			ip = iend - 4
		}
		bitStream = le32(hb[ip:]) >> bitCount
	}
	if remaining != 1 || charnum > maxSV1 || bitCount > 32 {
		return 0, false
	}
	*maxSV = charnum - 1
	ip += (bitCount + 7) >> 3
	return ip, true
}

// spreadSymbols places each symbol's cells as FSE_buildDTable does and
// returns the symbol of every cell in sym[:1<<tableLog].
func spreadSymbols(sym []uint8, norm []int16, maxSV, tableLog uint) bool {
	size := uint(1) << tableLog
	high := size - 1
	for s := uint(0); s <= maxSV; s++ {
		if norm[s] == -1 {
			sym[high] = uint8(s)
			high--
		}
	}
	mask := size - 1
	step := size>>1 + size>>3 + 3
	pos := uint(0)
	for s := uint(0); s <= maxSV; s++ {
		for i := 0; i < int(norm[s]); i++ {
			sym[pos] = uint8(s)
			pos = (pos + step) & mask
			for pos > high {
				pos = (pos + step) & mask
			}
		}
	}
	return pos == 0
}

// fseCell is one FSE_decode_t: symbol | nbBits<<8 | newState<<16.
type fseCell uint32

// buildFSE ports FSE_buildDTable_internal for Huffman weights.
func buildFSE(dt []fseCell, norm []int16, maxSV, tableLog uint) bool {
	var sym [1 << 6]uint8
	var next [256]uint16
	for s := uint(0); s <= maxSV; s++ {
		if norm[s] == -1 {
			next[s] = 1
		} else {
			next[s] = uint16(norm[s])
		}
	}
	if !spreadSymbols(sym[:], norm, maxSV, tableLog) {
		return false
	}
	size := uint(1) << tableLog
	for u := uint(0); u < size; u++ {
		s := sym[u]
		ns := uint32(next[s])
		next[s]++
		nb := tableLog - highbit(ns)
		dt[u] = fseCell(uint32(s) | uint32(nb)<<8 | (ns<<nb-uint32(size))<<16)
	}
	return true
}

// fseDecompressWeights ports FSE_decompress_wksp_body with maxLog 6 and
// libzstd's workspace size, whose limits it also enforces.
func fseDecompressWeights(dst []uint8, src []byte) (int, bool) {
	var norm [256]int16
	maxSV, tableLog := uint(255), uint(0)
	n, ok := readNCount(norm[:], &maxSV, &tableLog, src)
	if !ok || tableLog > 6 {
		return 0, false
	}
	// FSE_DECOMPRESS_WKSP_SIZE_U32(tableLog, maxSV) must fit
	// HUF_READ_STATS_WORKSPACE_SIZE_U32 = 219.
	if (1+1<<tableLog)+1+(2*(maxSV+1)+1<<tableLog+8+3)/4+128+1 > 219 {
		return 0, false
	}
	var dt [1 << 6]fseCell
	if !buildFSE(dt[:], norm[:], maxSV, tableLog) {
		return 0, false
	}
	src = src[n:]

	var br bitReader
	if !br.init(src) {
		return 0, false
	}
	s1 := uint(br.read(tableLog))
	br.reload()
	s2 := uint(br.read(tableLog))
	br.reload()
	if br.reload() == bitOverflow {
		return 0, false
	}
	decode := func(s *uint) uint8 {
		c := dt[*s]
		nb := uint(c>>8) & 0xFF
		*s = uint(c>>16) + uint(br.read(nb))
		return uint8(c)
	}
	omax := len(dst)
	op := 0
	for br.reload() == bitUnfinished && op < omax-3 {
		dst[op] = decode(&s1)
		dst[op+1] = decode(&s2)
		dst[op+2] = decode(&s1)
		dst[op+3] = decode(&s2)
		op += 4
	}
	for {
		if op > omax-2 {
			return 0, false
		}
		dst[op] = decode(&s1)
		op++
		if br.reload() == bitOverflow {
			dst[op] = decode(&s2)
			op++
			break
		}
		if op > omax-2 {
			return 0, false
		}
		dst[op] = decode(&s2)
		op++
		if br.reload() == bitOverflow {
			dst[op] = decode(&s1)
			op++
			break
		}
	}
	return op, true
}
