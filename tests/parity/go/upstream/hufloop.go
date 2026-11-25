package zstd

import "math/bits"

// Go versions of the Huffman 4-stream fast loops; asm replaces these later.
// Both port huf_decompress.c's *_fast_c_loop: dt must be a log-11 table, and
// the loop leaves the last symbols of each stream to the careful decoder.

// huf4X1LoopGo ports HUF_decompress4X1_usingDTable_internal_fast_c_loop.
func huf4X1LoopGo(a *hufArgs, src, dst []byte, dt *[1 << 12]uint16) {
	ip, op, b := a.ip, a.op, a.bits
	for {
		// Five symbols of at most 11 bits use under 7 input bytes.
		iters := min((a.oend-op[3])/5, ip[0]/7)
		olimit := op[3] + iters*5
		if op[3] == olimit {
			break
		}
		if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
			break
		}
		for op[3] < olimit {
			for s := range 4 {
				o := dst[op[s] : op[s]+5]
				bs := b[s]
				for k := range 5 {
					c := dt[bs>>53]
					bs <<= c & 0x3F
					o[k] = byte(c >> 8)
				}
				ctz := uint(bits.TrailingZeros64(bs))
				op[s] += 5
				ip[s] -= int(ctz >> 3)
				b[s] = (le64(src[ip[s]:]) | 1) << (ctz & 7)
			}
		}
	}
	a.ip, a.op, a.bits = ip, op, b
}

// huf4X2LoopGo ports HUF_decompress4X2_usingDTable_internal_fast_c_loop.
func huf4X2LoopGo(a *hufArgs, src, dst []byte, dt *[1 << 12]uint32) {
	ip, op, b := a.ip, a.op, a.bits
	oend := [4]int{op[1], op[2], op[3], a.oend}
	for {
		// Five lookups write at most 10 bytes per stream.
		iters := ip[0] / 7
		for s := range 4 {
			iters = min(iters, (oend[s]-op[s])/10)
		}
		olimit := op[3] + iters*5
		if op[3] == olimit {
			break
		}
		if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
			break
		}
		for op[3] < olimit {
			for s := range 4 {
				o := op[s]
				bs := b[s]
				for range 5 {
					c := dt[bs>>53]
					dst[o] = byte(c)
					dst[o+1] = byte(c >> 8)
					bs <<= (c >> 16) & 0x3F
					o += int(c >> 24)
				}
				op[s] = o
				ctz := uint(bits.TrailingZeros64(bs))
				ip[s] -= int(ctz >> 3)
				b[s] = (le64(src[ip[s]:]) | 1) << (ctz & 7)
			}
		}
	}
	a.ip, a.op, a.bits = ip, op, b
}
