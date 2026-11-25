//! libzstd's bit stream reader and the FSE table reader (ports fse.go).
#![forbid(unsafe_code)]

use crate::xxhash::{le32, le64};

/// The index of the top set bit of `x > 0`.
pub(crate) fn highbit(x: u32) -> usize {
    31u32.wrapping_sub(x.leading_zeros()) as usize
}

/// libzstd's `BIT_DStream_t`: a 64-bit container read backwards from the end of `inp`. The
/// container holds `inp[ptr..ptr + 8]`.
#[derive(Clone, Copy)]
pub(crate) struct BitReader<'a> {
    pub inp: &'a [u8],
    pub ptr: usize,
    pub bits: u64,
    pub consumed: usize,
}

/// Results of `reload`, as `BIT_DStream_status`.
pub(crate) const UNFINISHED: u8 = 0;
pub(crate) const OVERFLOW: u8 = 3;

impl<'a> BitReader<'a> {
    pub fn empty() -> Self {
        Self {
            inp: &[],
            ptr: 0,
            bits: 0,
            consumed: 0,
        }
    }

    /// Ports `BIT_initDStream`.
    pub fn new(inp: &'a [u8]) -> Option<Self> {
        let n = inp.len();
        let last = *inp.last()?;
        if last == 0 {
            return None;
        }
        let mut br = Self {
            inp,
            ptr: 0,
            bits: 0,
            consumed: 0,
        };
        if n >= 8 {
            br.ptr = n - 8;
            br.bits = le64(&inp[n - 8..]);
            br.consumed = 8 - highbit(u32::from(last));
            return Some(br);
        }
        for &b in inp.iter().rev() {
            br.bits = (br.bits << 8) | u64::from(b);
        }
        br.consumed = 8 - highbit(u32::from(last)) + (8 - n) * 8;
        Some(br)
    }

    /// `BIT_lookBits`: garbage, not a panic, once past the end.
    pub fn look(&self, n: usize) -> u64 {
        let sh = 64usize.wrapping_sub(self.consumed).wrapping_sub(n) & 63;
        (self.bits >> sh) & ((1u64 << n) - 1)
    }

    pub fn read(&mut self, n: usize) -> u64 {
        let v = self.look(n);
        self.consumed += n;
        v
    }

    /// `BIT_lookBitsFast`; `n >= 1`.
    pub fn look_fast(&self, n: usize) -> u64 {
        (self.bits << (self.consumed & 63)) >> (64usize.wrapping_sub(n) & 63)
    }

    pub fn read_fast(&mut self, n: usize) -> u64 {
        let v = self.look_fast(n);
        self.consumed += n;
        v
    }

    /// Ports `BIT_reloadDStream`.
    pub fn reload(&mut self) -> u8 {
        if self.consumed > 64 {
            return OVERFLOW;
        }
        if self.ptr >= 8 {
            self.ptr -= self.consumed >> 3;
            self.consumed &= 7;
            self.bits = le64(&self.inp[self.ptr..]);
            return UNFINISHED;
        }
        if self.ptr == 0 {
            // BIT_DStream_endOfBuffer (1) or BIT_DStream_completed (2).
            return if self.consumed < 64 { 1 } else { 2 };
        }
        let mut nb = self.consumed >> 3;
        let mut res = UNFINISHED;
        if nb > self.ptr {
            nb = self.ptr;
            res = 1;
        }
        self.ptr -= nb;
        self.consumed -= nb * 8;
        self.bits = le64(&self.inp[self.ptr..]);
        res
    }

    /// Ports `BIT_reloadDStreamFast`.
    pub fn reload_fast(&mut self) -> u8 {
        if self.ptr < 8 {
            return OVERFLOW;
        }
        self.ptr -= self.consumed >> 3;
        self.consumed &= 7;
        self.bits = le64(&self.inp[self.ptr..]);
        UNFINISHED
    }

    pub fn finished(&self) -> bool {
        self.ptr == 0 && self.consumed == 64
    }
}

fn shr32(x: u32, n: i64) -> u32 {
    if (0..32).contains(&n) { x >> n } else { 0 }
}

/// Ports `FSE_readNCount`. `max_sv` is the largest symbol allowed. It returns the bytes read, the
/// largest symbol present and the table log.
pub(crate) fn read_ncount(
    norm: &mut [i16],
    max_sv: usize,
    hb: &[u8],
) -> Option<(usize, usize, usize)> {
    if hb.len() < 8 {
        let mut buf = [0u8; 8];
        buf[..hb.len()].copy_from_slice(hb);
        let (n, m, l) = read_ncount(norm, max_sv, &buf)?;
        return if n > hb.len() { None } else { Some((n, m, l)) };
    }
    let iend = hb.len() as i64;
    let mut ip: i64 = 0;
    let max_sv1 = (max_sv + 1) as i64;
    norm[..max_sv1 as usize].fill(0);
    let mut bit_stream = le32(hb);
    let mut nb_bits = i64::from(bit_stream & 0xF) + 5;
    if nb_bits > 15 {
        return None;
    }
    bit_stream >>= 4;
    let mut bit_count: i64 = 4;
    let table_log = nb_bits as usize;
    let mut remaining: i64 = (1 << nb_bits) + 1;
    let mut threshold: i64 = 1 << nb_bits;
    nb_bits += 1;
    let mut charnum: i64 = 0;
    let mut previous0 = false;
    let at = |ip: i64| le32(&hb[ip as usize..]);
    loop {
        if previous0 {
            let mut repeats = ((!bit_stream | 0x8000_0000).trailing_zeros() >> 1) as i64;
            while repeats >= 12 {
                charnum += 3 * 12;
                if ip <= iend - 7 {
                    ip += 3;
                } else {
                    bit_count -= 8 * (iend - 7 - ip);
                    bit_count &= 31;
                    ip = iend - 4;
                }
                bit_stream = shr32(at(ip), bit_count);
                repeats = ((!bit_stream | 0x8000_0000).trailing_zeros() >> 1) as i64;
            }
            charnum += 3 * repeats;
            bit_stream >>= 2 * repeats;
            bit_count += 2 * repeats;
            charnum += i64::from(bit_stream & 3);
            bit_count += 2;
            if charnum >= max_sv1 {
                break;
            }
            if ip <= iend - 7 || ip + (bit_count >> 3) <= iend - 4 {
                ip += bit_count >> 3;
                bit_count &= 7;
            } else {
                bit_count -= 8 * (iend - 4 - ip);
                bit_count &= 31;
                ip = iend - 4;
            }
            bit_stream = shr32(at(ip), bit_count);
        }
        let max = (2 * threshold - 1) - remaining;
        let mut count = if i64::from(bit_stream & (threshold - 1) as u32) < max {
            bit_count += nb_bits - 1;
            i64::from(bit_stream & (threshold - 1) as u32)
        } else {
            bit_count += nb_bits;
            let c = i64::from(bit_stream & (2 * threshold - 1) as u32);
            if c >= threshold { c - max } else { c }
        };
        count -= 1;
        if count >= 0 {
            remaining -= count;
        } else {
            remaining += count;
        }
        norm[charnum as usize] = count as i16;
        charnum += 1;
        previous0 = count == 0;
        if remaining < threshold {
            if remaining <= 1 {
                break;
            }
            nb_bits = highbit(remaining as u32) as i64 + 1;
            threshold = 1 << (nb_bits - 1);
        }
        if charnum >= max_sv1 {
            break;
        }
        if ip <= iend - 7 || ip + (bit_count >> 3) <= iend - 4 {
            ip += bit_count >> 3;
            bit_count &= 7;
        } else {
            bit_count -= 8 * (iend - 4 - ip);
            bit_count &= 31;
            ip = iend - 4;
        }
        bit_stream = shr32(at(ip), bit_count);
    }
    if remaining != 1 || charnum > max_sv1 || bit_count > 32 {
        return None;
    }
    ip += (bit_count + 7) >> 3;
    Some((ip as usize, (charnum - 1) as usize, table_log))
}

/// Places each symbol's cells as `FSE_buildDTable` does and returns the symbol of every cell in
/// `sym[..1 << table_log]`; false if the spread does not close.
pub(crate) fn spread_symbols(
    sym: &mut [u8],
    norm: &[i16],
    max_sv: usize,
    table_log: usize,
) -> bool {
    let size = 1usize << table_log;
    let mut high = size - 1;
    for s in 0..=max_sv {
        if norm[s] == -1 {
            sym[high] = s as u8;
            high = high.wrapping_sub(1);
        }
    }
    let mask = size - 1;
    let step = (size >> 1) + (size >> 3) + 3;
    let mut pos = 0usize;
    for s in 0..=max_sv {
        for _ in 0..norm[s].max(0) {
            sym[pos] = s as u8;
            pos = (pos + step) & mask;
            while pos > high {
                pos = (pos + step) & mask;
            }
        }
    }
    pos == 0
}

/// Ports `FSE_buildDTable_internal` for Huffman weights: cells are `symbol | nb_bits << 8 |
/// new_state << 16`.
fn build_fse(dt: &mut [u32], norm: &[i16], max_sv: usize, table_log: usize) -> bool {
    let mut sym = [0u8; 1 << 6];
    let mut next = [0u16; 256];
    for s in 0..=max_sv {
        next[s] = if norm[s] == -1 { 1 } else { norm[s] as u16 };
    }
    if !spread_symbols(&mut sym, norm, max_sv, table_log) {
        return false;
    }
    let size = 1usize << table_log;
    for u in 0..size {
        let s = sym[u];
        let ns = u32::from(next[usize::from(s)]);
        next[usize::from(s)] += 1;
        let nb = table_log.wrapping_sub(highbit(ns));
        let new_state = ns.wrapping_shl(nb as u32).wrapping_sub(size as u32);
        dt[u] = u32::from(s) | ((nb as u32) << 8) | (new_state << 16);
    }
    true
}

/// Ports `FSE_decompress_wksp_body` with max log 6 and libzstd's workspace size, whose limits it
/// also enforces. It fills `dst` and returns the number of weights.
pub(crate) fn fse_decompress_weights(dst: &mut [u8], src: &[u8]) -> Option<usize> {
    let mut norm = [0i16; 256];
    let (n, max_sv, table_log) = read_ncount(&mut norm, 255, src)?;
    if table_log > 6 {
        return None;
    }
    // FSE_DECOMPRESS_WKSP_SIZE_U32(table_log, max_sv) must fit
    // HUF_READ_STATS_WORKSPACE_SIZE_U32 = 219.
    if (1 + (1usize << table_log))
        + 1
        + (2 * (max_sv + 1) + (1usize << table_log) + 8).div_ceil(4)
        + 128
        + 1
        > 219
    {
        return None;
    }
    let mut dt = [0u32; 1 << 6];
    if !build_fse(&mut dt, &norm, max_sv, table_log) {
        return None;
    }
    let src = &src[n..];
    let mut br = BitReader::new(src)?;
    let mut s1 = br.read(table_log) as usize;
    br.reload();
    let mut s2 = br.read(table_log) as usize;
    br.reload();
    if br.reload() == OVERFLOW {
        return None;
    }
    fn decode(dt: &[u32; 64], br: &mut BitReader, s: &mut usize) -> u8 {
        let c = dt[*s];
        let nb = ((c >> 8) & 0xFF) as usize;
        *s = (c >> 16) as usize + br.read(nb) as usize;
        c as u8
    }
    let omax = dst.len();
    let mut op = 0usize;
    while br.reload() == UNFINISHED && op + 3 < omax {
        dst[op] = decode(&dt, &mut br, &mut s1);
        dst[op + 1] = decode(&dt, &mut br, &mut s2);
        dst[op + 2] = decode(&dt, &mut br, &mut s1);
        dst[op + 3] = decode(&dt, &mut br, &mut s2);
        op += 4;
    }
    loop {
        if op + 2 > omax {
            return None;
        }
        dst[op] = decode(&dt, &mut br, &mut s1);
        op += 1;
        if br.reload() == OVERFLOW {
            dst[op] = decode(&dt, &mut br, &mut s2);
            op += 1;
            break;
        }
        if op + 2 > omax {
            return None;
        }
        dst[op] = decode(&dt, &mut br, &mut s2);
        op += 1;
        if br.reload() == OVERFLOW {
            dst[op] = decode(&dt, &mut br, &mut s1);
            op += 1;
            break;
        }
    }
    Some(op)
}
