//! The literals section of a block (ports literals.go, which ports `ZSTD_decodeLiteralsBlock`).
#![forbid(unsafe_code)]

use crate::error::Error;
use crate::huf::select_x2;
use crate::xxhash::le32;
use crate::{Decoder, WILDCOPY_SLACK};

/// Where the literals of a block are: in the decoder's buffer, or in place in the block at
/// `blk[off..]`. Either way at least `WILDCOPY_SLACK` readable bytes follow them.
pub(crate) enum LitSrc {
    Buf,
    Blk(usize),
}

impl Decoder {
    /// Ports `ZSTD_decodeLiteralsBlock`. It returns the literals' location, their count and the
    /// section size. `room` is the output space left in the frame.
    pub(crate) fn decode_literals(
        &mut self,
        src: &[u8],
        bsm: usize,
        room: usize,
    ) -> Result<(LitSrc, usize, usize), Error> {
        const ERR: Error = Error::Corrupt("bad literals section");
        if src.len() < 2 {
            return Err(ERR);
        }
        let lit_type = src[0] & 3;
        let lhl = (src[0] >> 2) & 3;
        let lit_size;
        if lit_type < 2 {
            // raw, RLE
            let lh_size;
            match lhl {
                0 | 2 => {
                    lh_size = 1;
                    lit_size = usize::from(src[0] >> 3);
                }
                1 => {
                    if lit_type == 1 && src.len() < 3 {
                        return Err(ERR);
                    }
                    lh_size = 2;
                    lit_size = (usize::from(src[0]) >> 4) | (usize::from(src[1]) << 4);
                }
                _ => {
                    if src.len() < 3 + usize::from(lit_type) {
                        return Err(ERR);
                    }
                    lh_size = 3;
                    lit_size = (usize::from(src[0]) >> 4)
                        | (usize::from(src[1]) << 4)
                        | (usize::from(src[2]) << 12);
                }
            }
            if lit_size > bsm.min(room) {
                return Err(ERR);
            }
            if lit_type == 1 {
                self.lit[..lit_size].fill(src[lh_size]);
                return Ok((LitSrc::Buf, lit_size, lh_size + 1));
            }
            if lh_size + lit_size + WILDCOPY_SLACK > src.len() {
                if lh_size + lit_size > src.len() {
                    return Err(ERR);
                }
                self.lit[..lit_size].copy_from_slice(&src[lh_size..lh_size + lit_size]);
                return Ok((LitSrc::Buf, lit_size, lh_size + lit_size));
            }
            // In place: the block has WILDCOPY_SLACK bytes after the literals.
            return Ok((LitSrc::Blk(lh_size), lit_size, lh_size + lit_size));
        }

        // Huffman compressed (2) or treeless (3).
        if lit_type == 3 && !self.lit_ok {
            return Err(ERR);
        }
        if src.len() < 5 {
            return Err(ERR);
        }
        let lhc = le32(src) as usize;
        let lh_size;
        let c_size;
        let mut single = false;
        match lhl {
            0 | 1 => {
                single = lhl == 0;
                lh_size = 3;
                lit_size = (lhc >> 4) & 0x3FF;
                c_size = (lhc >> 14) & 0x3FF;
            }
            2 => {
                lh_size = 4;
                lit_size = (lhc >> 4) & 0x3FFF;
                c_size = lhc >> 18;
            }
            _ => {
                lh_size = 5;
                lit_size = (lhc >> 4) & 0x3FFFF;
                c_size = (lhc >> 22) + (usize::from(src[4]) << 10);
            }
        }
        if lit_size > bsm.min(room) || (!single && lit_size < 6) || c_size + lh_size > src.len() {
            return Err(ERR);
        }
        let inp = &src[lh_size..lh_size + c_size];
        let out = &mut self.lit[..lit_size];
        let t = &mut self.huf;
        let ok = if lit_type == 3 {
            if single {
                t.decode_1x(out, inp)
            } else {
                t.decode_4x(out, inp)
            }
        } else if single {
            // HUF_decompress1X1_DCtx_wksp: always an X1 table.
            match t.read_x1(inp) {
                Some(h) => h < inp.len() && t.decode_1x(out, &inp[h..]),
                None => false,
            }
        } else if inp.is_empty() {
            // HUF_decompress4X_hufOnly_wksp
            false
        } else {
            let h = if select_x2(lit_size, inp.len()) {
                t.read_x2(inp)
            } else {
                t.read_x1(inp)
            };
            match h {
                Some(h) => h < inp.len() && t.decode_4x(out, &inp[h..]),
                None => false,
            }
        };
        if !ok {
            return Err(Error::Corrupt("bad huffman stream"));
        }
        self.lit_ok = true;
        Ok((LitSrc::Buf, lit_size, lh_size + c_size))
    }
}
