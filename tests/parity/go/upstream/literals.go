package zstd

// decodeLiterals ports ZSTD_decodeLiteralsBlock. It returns the literals
// followed by at least wildcopySlack readable bytes, the literal count, and
// the section size. room is the output space left in the frame.
func (d *Decoder) decodeLiterals(src []byte, bsm, room int) (lits []byte, litSize, n int, err error) {
	if len(src) < 2 {
		return nil, 0, 0, errLiterals
	}
	litType := src[0] & 3
	lhl := (src[0] >> 2) & 3
	switch litType {
	case 0, 1: // raw, RLE
		var lhSize int
		switch lhl {
		case 0, 2:
			lhSize, litSize = 1, int(src[0]>>3)
		case 1:
			if litType == 1 && len(src) < 3 {
				return nil, 0, 0, errLiterals
			}
			lhSize, litSize = 2, int(src[0])>>4|int(src[1])<<4
		case 3:
			if len(src) < 3+int(litType) {
				return nil, 0, 0, errLiterals
			}
			lhSize, litSize = 3, int(src[0])>>4|int(src[1])<<4|int(src[2])<<12
		}
		if litSize > min(bsm, room) {
			return nil, 0, 0, errLiterals
		}
		if litType == 1 {
			b := d.lit[:litSize]
			for i := range b {
				b[i] = src[lhSize]
			}
			return d.lit[:], litSize, lhSize + 1, nil
		}
		if lhSize+litSize+wildcopySlack > len(src) {
			if lhSize+litSize > len(src) {
				return nil, 0, 0, errLiterals
			}
			copy(d.lit[:], src[lhSize:lhSize+litSize])
			return d.lit[:], litSize, lhSize + litSize, nil
		}
		// In place: the block has wildcopySlack bytes after the literals.
		return src[lhSize:], litSize, lhSize + litSize, nil
	}

	// Huffman compressed (2) or treeless (3).
	if litType == 3 && !d.litOK {
		return nil, 0, 0, errLiterals
	}
	if len(src) < 5 {
		return nil, 0, 0, errLiterals
	}
	lhc := uint32(le32(src))
	var lhSize, cSize int
	single := false
	switch lhl {
	case 0, 1:
		single = lhl == 0
		lhSize = 3
		litSize = int(lhc>>4) & 0x3FF
		cSize = int(lhc>>14) & 0x3FF
	case 2:
		lhSize = 4
		litSize = int(lhc>>4) & 0x3FFF
		cSize = int(lhc >> 18)
	case 3:
		lhSize = 5
		litSize = int(lhc>>4) & 0x3FFFF
		cSize = int(lhc>>22) + int(src[4])<<10
	}
	if litSize > min(bsm, room) || (!single && litSize < 6) || cSize+lhSize > len(src) {
		return nil, 0, 0, errLiterals
	}
	in := src[lhSize : lhSize+cSize]
	out := d.lit[:litSize]
	t := &d.huf
	ok := false
	switch {
	case litType == 3 && single:
		ok = t.decode1X(out, in)
	case litType == 3:
		ok = t.decode4X(out, in)
	case single:
		// HUF_decompress1X1_DCtx_wksp: always an X1 table.
		h, rok := t.readX1(in)
		ok = rok && h < len(in) && t.decode1X(out, in[h:])
	default:
		// HUF_decompress4X_hufOnly_wksp
		if len(in) == 0 {
			break
		}
		var h int
		var rok bool
		if selectX2(litSize, len(in)) {
			h, rok = t.readX2(in)
		} else {
			h, rok = t.readX1(in)
		}
		ok = rok && h < len(in) && t.decode4X(out, in[h:])
	}
	if !ok {
		return nil, 0, 0, errHuffman
	}
	d.litOK = true
	return d.lit[:], litSize, lhSize + cSize, nil
}
