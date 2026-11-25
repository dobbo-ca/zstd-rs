package czi

// interleaveGo writes lo[i], hi[i] as the little-endian word dst[2i:2i+2].
func interleaveGo(dst, lo, hi []byte) {
	dst = dst[:2*len(lo)]
	hi = hi[:len(lo)]
	for i, l := range lo {
		dst[2*i] = l
		dst[2*i+1] = hi[i]
	}
}
