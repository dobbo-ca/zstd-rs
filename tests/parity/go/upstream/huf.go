package zstd

import "math/bits"

// hufTable is the Huffman decoding table of a frame, kept for treeless
// literals. X1 cells are byte<<8 | nbBits (HUF_DEltX1); X2 cells are
// seq | nbBits<<16 | len<<24 (HUF_DEltX2).
type hufTable struct {
	x1     [1 << 12]uint16
	x2     [1 << 12]uint32
	log    uint // HUF DTableDesc.tableLog
	double bool // X2 table

	weight  [256]uint8
	rankVal [13]uint32
}

const hufFastLog = 11 // HUF_DECODER_FAST_TABLELOG

// readStats ports HUF_readStats. It fills t.weight and t.rankVal.
func (t *hufTable) readStats(src []byte) (iSize int, nbSymbols, tableLog uint, ok bool) {
	if len(src) == 0 {
		return 0, 0, 0, false
	}
	iSize = int(src[0])
	var oSize int
	if iSize >= 128 {
		oSize = iSize - 127
		iSize = (oSize + 1) / 2
		if iSize+1 > len(src) || oSize >= 256 {
			return 0, 0, 0, false
		}
		for n := 0; n < oSize; n += 2 {
			b := src[1+n/2]
			t.weight[n] = b >> 4
			t.weight[n+1] = b & 15
		}
	} else {
		if iSize+1 > len(src) {
			return 0, 0, 0, false
		}
		oSize, ok = fseDecompressWeights(t.weight[:255], src[1:1+iSize])
		if !ok {
			return 0, 0, 0, false
		}
	}
	t.rankVal = [13]uint32{}
	total := uint32(0)
	for _, w := range t.weight[:oSize] {
		if w > 12 {
			return 0, 0, 0, false
		}
		t.rankVal[w]++
		total += 1 << w >> 1
	}
	if total == 0 {
		return 0, 0, 0, false
	}
	tableLog = highbit(total) + 1
	if tableLog > 12 {
		return 0, 0, 0, false
	}
	rest := uint32(1)<<tableLog - total
	if 1<<highbit(rest) != rest {
		return 0, 0, 0, false
	}
	last := highbit(rest) + 1
	t.weight[oSize] = uint8(last)
	t.rankVal[last]++
	if t.rankVal[1] < 2 || t.rankVal[1]&1 != 0 {
		return 0, 0, 0, false
	}
	return iSize + 1, uint(oSize + 1), tableLog, true
}

// readX1 ports HUF_readDTableX1_wksp, including the rescale to log 11.
func (t *hufTable) readX1(src []byte) (int, bool) {
	iSize, nbSymbols, tableLog, ok := t.readStats(src)
	if !ok {
		return 0, false
	}
	if tableLog < hufFastLog {
		scale := hufFastLog - tableLog
		for s := range t.weight[:nbSymbols] {
			if t.weight[s] != 0 {
				t.weight[s] += uint8(scale)
			}
		}
		for s := uint(hufFastLog); s > scale; s-- {
			t.rankVal[s] = t.rankVal[s-scale]
		}
		for s := scale; s > 0; s-- {
			t.rankVal[s] = 0
		}
		tableLog = hufFastLog
	}
	t.log, t.double = tableLog, false

	var rankStart [13]uint32
	next := uint32(0)
	for n := uint(0); n <= tableLog; n++ {
		rankStart[n] = next
		next += t.rankVal[n]
	}
	var symbols [256]uint8
	for n := range t.weight[:nbSymbols] {
		w := t.weight[n]
		symbols[rankStart[w]] = uint8(n)
		rankStart[w]++
	}
	symbol := t.rankVal[0]
	u := 0
	for w := uint(1); w <= tableLog; w++ {
		length := 1 << w >> 1
		nb := uint16(tableLog + 1 - w)
		for _, s := range symbols[symbol : symbol+t.rankVal[w]] {
			d := uint16(s)<<8 | nb
			cells := t.x1[u : u+length]
			for i := range cells {
				cells[i] = d
			}
			u += length
		}
		symbol += t.rankVal[w]
	}
	return iSize, true
}

// readX2 ports HUF_readDTableX2_wksp with maxTableLog 12.
func (t *hufTable) readX2(src []byte) (int, bool) {
	iSize, nbSymbols, tableLog, ok := t.readStats(src)
	if !ok {
		return 0, false
	}
	maxTableLog := uint(12)
	if tableLog <= hufFastLog {
		maxTableLog = hufFastLog
	}
	maxW := tableLog
	for t.rankVal[maxW] == 0 {
		maxW--
	}
	// rankStart0[w] .. rankStart0[w+1] holds the symbols of weight w.
	var rankStart0 [15]uint32
	rankStart := rankStart0[1:]
	next := uint32(0)
	for w := uint(1); w <= maxW; w++ {
		rankStart[w] = next
		next += t.rankVal[w]
	}
	rankStart[0] = next
	rankStart[maxW+1] = next
	var sorted [256]uint8
	for s := range t.weight[:nbSymbols] {
		w := t.weight[s]
		sorted[rankStart[w]] = uint8(s)
		rankStart[w]++
	}
	rankStart[0] = 0

	var rankVal [12][13]uint32
	rescale := int(maxTableLog-tableLog) - 1
	nextRank := uint32(0)
	for w := uint(1); w <= maxW; w++ {
		rankVal[0][w] = nextRank
		nextRank += t.rankVal[w] << (int(w) + rescale)
	}
	minBits := tableLog + 1 - maxW
	for consumed := minBits; consumed < maxTableLog-minBits+1; consumed++ {
		for w := uint(1); w <= maxW; w++ {
			rankVal[consumed][w] = rankVal[0][w] >> consumed
		}
	}
	t.fillX2(maxTableLog, sorted[:], rankStart0[:], &rankVal, maxW, tableLog+1)
	t.log, t.double = maxTableLog, true
	return iSize, true
}

func x2Cell(symbol, nbBits, baseSeq uint32, level uint32) uint32 {
	seq := symbol
	if level == 2 {
		seq = baseSeq + symbol<<8
	}
	return seq | nbBits<<16 | level<<24
}

// fillX2ForWeight ports HUF_fillDTableX2ForWeight.
func fillX2ForWeight(dt []uint32, syms []uint8, nbBits, tableLog, baseSeq, level uint32) {
	length := 1 << (tableLog - nbBits)
	u := 0
	for _, s := range syms {
		c := x2Cell(uint32(s), nbBits, baseSeq, level)
		cells := dt[u : u+length]
		for i := range cells {
			cells[i] = c
		}
		u += length
	}
}

// fillX2 ports HUF_fillDTableX2 and HUF_fillDTableX2Level2.
func (t *hufTable) fillX2(targetLog uint, sorted []uint8, rankStart []uint32, rankValOrigin *[12][13]uint32, maxWeight, nbBitsBaseline uint) {
	rankVal := &rankValOrigin[0]
	scaleLog := int(nbBitsBaseline) - int(targetLog)
	minBits := nbBitsBaseline - maxWeight
	wEnd := maxWeight + 1
	for w := uint(1); w < wEnd; w++ {
		syms := sorted[rankStart[w]:rankStart[w+1]]
		nbBits := nbBitsBaseline - w
		if targetLog-nbBits < minBits {
			fillX2ForWeight(t.x2[rankVal[w]:], syms, uint32(nbBits), uint32(targetLog), 0, 1)
			continue
		}
		start := rankVal[w]
		length := uint32(1) << (targetLog - nbBits)
		minWeight := max(int(nbBits)+scaleLog, 1)
		level2 := &rankValOrigin[nbBits]
		for _, base := range syms {
			dt := t.x2[start:]
			if minWeight > 1 {
				c := x2Cell(uint32(base), uint32(nbBits), 0, 1)
				skip := dt[:level2[minWeight]]
				for i := range skip {
					skip[i] = c
				}
			}
			for w2 := uint(minWeight); w2 < wEnd; w2++ {
				total := nbBitsBaseline - w2 + nbBits
				fillX2ForWeight(dt[level2[w2]:], sorted[rankStart[w2]:rankStart[w2+1]], uint32(total), uint32(targetLog), uint32(base), 2)
			}
			start += length
		}
	}
}

type algoTime struct{ table, decode256 uint32 }

var algoTimes = [16][2]algoTime{
	{{0, 0}, {1, 1}},
	{{0, 0}, {1, 1}},
	{{150, 216}, {381, 119}},
	{{170, 205}, {514, 112}},
	{{177, 199}, {539, 110}},
	{{197, 194}, {644, 107}},
	{{221, 192}, {735, 107}},
	{{256, 189}, {881, 106}},
	{{359, 188}, {1167, 109}},
	{{582, 187}, {1570, 114}},
	{{688, 187}, {1712, 122}},
	{{825, 186}, {1965, 136}},
	{{976, 185}, {2131, 150}},
	{{1180, 186}, {2070, 175}},
	{{1377, 185}, {1731, 202}},
	{{1412, 185}, {1695, 202}},
}

// selectX2 ports HUF_selectDecoder.
func selectX2(dstSize, cSrcSize int) bool {
	algo := &algoTimes
	q := 15
	if cSrcSize < dstSize {
		q = cSrcSize * 16 / dstSize
	}
	d256 := uint32(dstSize >> 8)
	t0 := algo[q][0].table + algo[q][0].decode256*d256
	t1 := algo[q][1].table + algo[q][1].decode256*d256
	t1 += t1 >> 5
	return t1 < t0
}

// decodeStreamX1 ports HUF_decodeStreamX1 into dst[p:end].
func (t *hufTable) decodeStreamX1(dst []byte, p, end int, br *bitReader) {
	dt := &t.x1
	log := t.log
	sym := func() {
		c := dt[br.lookFast(log)]
		dst[p] = byte(c >> 8)
		br.consumed += uint(c & 0xFF)
		p++
	}
	if end-p > 3 {
		for br.reload() == bitUnfinished && p < end-3 {
			sym()
			sym()
			sym()
			sym()
		}
	} else {
		br.reload()
	}
	for p < end {
		sym()
	}
}

// decodeStreamX2 ports HUF_decodeStreamX2 into dst[p:end].
func (t *hufTable) decodeStreamX2(dst []byte, p, end int, br *bitReader) int {
	dt := &t.x2
	log := t.log
	sym := func() {
		c := dt[br.lookFast(log)]
		dst[p] = byte(c)
		dst[p+1] = byte(c >> 8)
		br.consumed += uint(c>>16) & 0xFF
		p += int(c >> 24)
	}
	if end-p >= 8 {
		if log <= 11 {
			for br.reload() == bitUnfinished && p < end-9 {
				sym()
				sym()
				sym()
				sym()
				sym()
			}
		} else {
			for br.reload() == bitUnfinished && p < end-7 {
				sym()
				sym()
				sym()
				sym()
			}
		}
	} else {
		br.reload()
	}
	if end-p >= 2 {
		for br.reload() == bitUnfinished && p <= end-2 {
			sym()
		}
		for p <= end-2 {
			sym()
		}
	}
	if p < end {
		c := dt[br.lookFast(log)]
		dst[p] = byte(c)
		if c>>24 == 1 {
			br.consumed += uint(c>>16) & 0xFF
		} else if br.consumed < 64 {
			br.consumed += uint(c>>16) & 0xFF
			if br.consumed > 64 {
				br.consumed = 64
			}
		}
		p++
	}
	return p
}

// decode1X ports HUF_decompress1X{1,2}_usingDTable_internal.
func (t *hufTable) decode1X(dst []byte, src []byte) bool {
	var br bitReader
	if !br.init(src) {
		return false
	}
	if t.double {
		t.decodeStreamX2(dst, 0, len(dst), &br)
	} else {
		t.decodeStreamX1(dst, 0, len(dst), &br)
	}
	return br.finished()
}

// decode4X ports HUF_decompress4X{1,2}_usingDTable_internal: the fast
// loop when it applies, else the reference loop.
func (t *hufTable) decode4X(dst []byte, src []byte) bool {
	if ret := t.decode4XFast(dst, src); ret != 0 {
		return ret > 0
	}
	return t.decode4XSlow(dst, src)
}

// hufArgs ports HUF_DecompressFastArgs; indexes are into src and dst.
type hufArgs struct {
	ip   [4]int
	op   [4]int
	bits [4]uint64
	iend [4]int
	oend int
}

// decode4XFast returns 1 on success, 0 to fall back, -1 on corruption.
func (t *hufTable) decode4XFast(dst []byte, src []byte) int {
	var a hufArgs
	// HUF_DecompressFastArgs_init
	if len(dst) == 0 {
		return 0
	}
	if len(src) < 10 {
		return -1
	}
	if t.log != hufFastLog {
		return 0
	}
	l1 := int(src[0]) | int(src[1])<<8
	l2 := int(src[2]) | int(src[3])<<8
	l3 := int(src[4]) | int(src[5])<<8
	l4 := len(src) - (l1 + l2 + l3 + 6)
	a.iend = [4]int{6, 6 + l1, 6 + l1 + l2, 6 + l1 + l2 + l3}
	if l1 < 8 || l2 < 8 || l3 < 8 || (l4 >= 0 && l4 < 8) {
		return 0
	}
	if l4 < 0 {
		return -1
	}
	a.ip = [4]int{a.iend[1] - 8, a.iend[2] - 8, a.iend[3] - 8, len(src) - 8}
	seg := (len(dst) + 3) / 4
	a.op = [4]int{0, seg, 2 * seg, 3 * seg}
	a.oend = len(dst)
	if a.op[3] >= a.oend {
		return 0
	}
	for i := range 4 {
		last := src[a.ip[i]+7]
		c := uint(0)
		if last != 0 {
			c = 8 - highbit(uint32(last))
		}
		a.bits[i] = (le64(src[a.ip[i]:]) | 1) << c
	}

	if t.double {
		huf4X2Loop(&a, src, dst, &t.x2)
	} else {
		huf4X1Loop(&a, src, dst, &t.x1)
	}

	seg = (len(dst) + 3) / 4
	segEnd := 0
	for i := range 4 {
		if seg <= len(dst)-segEnd {
			segEnd += seg
		} else {
			segEnd = len(dst)
		}
		// HUF_initRemainingDStream
		if a.op[i] > segEnd || a.ip[i] < a.iend[i]-8 {
			return -1
		}
		br := bitReader{in: src, ptr: a.ip[i], bits: le64(src[a.ip[i]:])}
		br.consumed = uint(bits.TrailingZeros64(a.bits[i]))
		if t.double {
			a.op[i] = t.decodeStreamX2(dst, a.op[i], segEnd, &br)
		} else {
			t.decodeStreamX1(dst, a.op[i], segEnd, &br)
			a.op[i] = segEnd
		}
		if a.op[i] != segEnd {
			return -1
		}
	}
	return 1
}

// decode4XSlow ports HUF_decompress4X{1,2}_usingDTable_internal_body.
func (t *hufTable) decode4XSlow(dst []byte, src []byte) bool {
	if len(src) < 10 || len(dst) < 6 {
		return false
	}
	l1 := int(src[0]) | int(src[1])<<8
	l2 := int(src[2]) | int(src[3])<<8
	l3 := int(src[4]) | int(src[5])<<8
	l4 := len(src) - (l1 + l2 + l3 + 6)
	if l4 < 0 {
		return false
	}
	seg := (len(dst) + 3) / 4
	start := [4]int{0, seg, 2 * seg, 3 * seg}
	oend := len(dst)
	if start[3] > oend {
		return false
	}
	var br [4]bitReader
	in := src[6:]
	for i, l := range [4]int{l1, l2, l3, l4} {
		if !br[i].init(in[:l]) {
			return false
		}
		in = in[l:]
	}
	op := start
	if t.double {
		if oend-op[3] >= 8 {
			olimit := oend - 7
			dt := &t.x2
			log := t.log
			for end := true; end && op[3] < olimit; {
				for k := 0; k < 4; k++ {
					for i := range 4 {
						c := dt[br[i].lookFast(log)]
						dst[op[i]] = byte(c)
						dst[op[i]+1] = byte(c >> 8)
						br[i].consumed += uint(c>>16) & 0xFF
						op[i] += int(c >> 24)
					}
				}
				for i := range 4 {
					end = br[i].reloadFast() == bitUnfinished && end
				}
			}
		}
	} else if oend-op[3] >= 8 {
		olimit := oend - 3
		dt := &t.x1
		log := t.log
		for end := true; end && op[3] < olimit; {
			for k := 0; k < 4; k++ {
				for i := range 4 {
					c := dt[br[i].lookFast(log)]
					dst[op[i]] = byte(c >> 8)
					br[i].consumed += uint(c & 0xFF)
					op[i]++
				}
			}
			for i := range 4 {
				end = br[i].reloadFast() == bitUnfinished && end
			}
		}
	}
	if op[0] > start[1] || op[1] > start[2] || op[2] > start[3] {
		return false
	}
	ends := [4]int{start[1], start[2], start[3], oend}
	for i := range 4 {
		if t.double {
			t.decodeStreamX2(dst, op[i], ends[i], &br[i])
		} else {
			t.decodeStreamX1(dst, op[i], ends[i], &br[i])
		}
	}
	return br[0].finished() && br[1].finished() && br[2].finished() && br[3].finished()
}
