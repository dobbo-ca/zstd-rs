//! Huffman literal tables and decoding (ports huf.go and hufloop.go, which port
//! huf_decompress.c).
#![forbid(unsafe_code)]

use crate::bits::{BitReader, UNFINISHED, fse_decompress_weights, highbit};
use crate::hufloop;
use crate::xxhash::le64;

/// The Huffman decoding table of a frame, kept for treeless literals. X1 cells are
/// `byte << 8 | nb_bits` (`HUF_DEltX1`); X2 cells are `seq | nb_bits << 16 | len << 24`
/// (`HUF_DEltX2`).
pub(crate) struct HufTable {
    pub(crate) x1: [u16; 1 << 12],
    pub(crate) x2: [u32; 1 << 12],
    /// `HUF_DTableDesc.tableLog`.
    log: usize,
    /// An X2 table.
    double: bool,
    weight: [u8; 256],
    rank_val: [u32; 13],
}

/// `HUF_DECODER_FAST_TABLELOG`.
const FAST_LOG: usize = 11;

impl HufTable {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            x1: [0; 1 << 12],
            x2: [0; 1 << 12],
            log: 0,
            double: false,
            weight: [0; 256],
            rank_val: [0; 13],
        })
    }

    /// Ports `HUF_readStats`. It fills `weight` and `rank_val` and returns the bytes read, the
    /// number of symbols and the table log.
    fn read_stats(&mut self, src: &[u8]) -> Option<(usize, usize, usize)> {
        let mut i_size = usize::from(*src.first()?);
        let o_size;
        if i_size >= 128 {
            o_size = i_size - 127;
            i_size = o_size.div_ceil(2);
            if i_size + 1 > src.len() || o_size >= 256 {
                return None;
            }
            for n in (0..o_size).step_by(2) {
                let b = src[1 + n / 2];
                self.weight[n] = b >> 4;
                self.weight[n + 1] = b & 15;
            }
        } else {
            if i_size + 1 > src.len() {
                return None;
            }
            o_size = fse_decompress_weights(&mut self.weight[..255], &src[1..1 + i_size])?;
        }
        self.rank_val = [0; 13];
        let mut total = 0u32;
        for &w in &self.weight[..o_size] {
            if w > 12 {
                return None;
            }
            self.rank_val[usize::from(w)] += 1;
            total += (1u32 << w) >> 1;
        }
        if total == 0 {
            return None;
        }
        let table_log = highbit(total) + 1;
        if table_log > 12 {
            return None;
        }
        let rest = (1u32 << table_log) - total;
        if 1u32 << highbit(rest) != rest {
            return None;
        }
        let last = highbit(rest) + 1;
        self.weight[o_size] = last as u8;
        self.rank_val[last] += 1;
        if self.rank_val[1] < 2 || self.rank_val[1] & 1 != 0 {
            return None;
        }
        Some((i_size + 1, o_size + 1, table_log))
    }

    /// Ports `HUF_readDTableX1_wksp`, including the rescale to log 11.
    pub fn read_x1(&mut self, src: &[u8]) -> Option<usize> {
        let (i_size, nb_symbols, mut table_log) = self.read_stats(src)?;
        if table_log < FAST_LOG {
            let scale = FAST_LOG - table_log;
            for w in &mut self.weight[..nb_symbols] {
                if *w != 0 {
                    *w += scale as u8;
                }
            }
            let mut s = FAST_LOG;
            while s > scale {
                self.rank_val[s] = self.rank_val[s - scale];
                s -= 1;
            }
            for s in 1..=scale {
                self.rank_val[s] = 0;
            }
            table_log = FAST_LOG;
        }
        self.log = table_log;
        self.double = false;

        let mut rank_start = [0u32; 13];
        let mut next = 0u32;
        for n in 0..=table_log {
            rank_start[n] = next;
            next += self.rank_val[n];
        }
        let mut symbols = [0u8; 256];
        for n in 0..nb_symbols {
            let w = usize::from(self.weight[n]);
            symbols[rank_start[w] as usize] = n as u8;
            rank_start[w] += 1;
        }
        let mut symbol = self.rank_val[0] as usize;
        let mut u = 0usize;
        for w in 1..=table_log {
            let length = (1usize << w) >> 1;
            let nb = (table_log + 1 - w) as u16;
            let cnt = self.rank_val[w] as usize;
            for &s in &symbols[symbol..symbol + cnt] {
                let d = (u16::from(s) << 8) | nb;
                self.x1[u..u + length].fill(d);
                u += length;
            }
            symbol += cnt;
        }
        Some(i_size)
    }

    /// Ports `HUF_readDTableX2_wksp` with max table log 12.
    pub fn read_x2(&mut self, src: &[u8]) -> Option<usize> {
        let (i_size, nb_symbols, table_log) = self.read_stats(src)?;
        let max_table_log = if table_log <= FAST_LOG { FAST_LOG } else { 12 };
        let mut max_w = table_log;
        while self.rank_val[max_w] == 0 {
            max_w -= 1;
        }
        // rs0[w + 1] is Go's rankStart[w]; fill_x2 reads rs0[w] .. rs0[w + 1] for weight w.
        let mut rs0 = [0u32; 15];
        let mut next = 0u32;
        for w in 1..=max_w {
            rs0[w + 1] = next;
            next += self.rank_val[w];
        }
        rs0[1] = next;
        rs0[max_w + 2] = next;
        let mut sorted = [0u8; 256];
        for s in 0..nb_symbols {
            let w = usize::from(self.weight[s]);
            sorted[rs0[w + 1] as usize] = s as u8;
            rs0[w + 1] += 1;
        }
        rs0[1] = 0;

        let mut rank_val = [[0u32; 13]; 12];
        let rescale = max_table_log as i64 - table_log as i64 - 1;
        let mut next_rank = 0u32;
        for w in 1..=max_w {
            rank_val[0][w] = next_rank;
            next_rank += self.rank_val[w] << (w as i64 + rescale);
        }
        let min_bits = table_log + 1 - max_w;
        for consumed in min_bits..(max_table_log - min_bits + 1) {
            for w in 1..=max_w {
                rank_val[consumed][w] = rank_val[0][w] >> consumed;
            }
        }
        fill_x2(
            &mut self.x2,
            max_table_log,
            &sorted,
            &rs0,
            &rank_val,
            max_w,
            table_log + 1,
        );
        self.log = max_table_log;
        self.double = true;
        Some(i_size)
    }
}

fn x2_cell(symbol: u32, nb_bits: u32, base_seq: u32, level: u32) -> u32 {
    let seq = if level == 2 {
        base_seq + (symbol << 8)
    } else {
        symbol
    };
    seq | (nb_bits << 16) | (level << 24)
}

/// Ports `HUF_fillDTableX2ForWeight`.
fn fill_x2_for_weight(
    dt: &mut [u32],
    syms: &[u8],
    nb_bits: u32,
    table_log: u32,
    base_seq: u32,
    level: u32,
) {
    let length = 1usize << (table_log - nb_bits);
    let mut u = 0usize;
    for &s in syms {
        let c = x2_cell(u32::from(s), nb_bits, base_seq, level);
        dt[u..u + length].fill(c);
        u += length;
    }
}

/// Ports `HUF_fillDTableX2` and `HUF_fillDTableX2Level2`.
fn fill_x2(
    x2: &mut [u32; 1 << 12],
    target_log: usize,
    sorted: &[u8; 256],
    rank_start: &[u32; 15],
    rank_val_origin: &[[u32; 13]; 12],
    max_weight: usize,
    nb_bits_baseline: usize,
) {
    let rank_val = &rank_val_origin[0];
    let scale_log = nb_bits_baseline as i64 - target_log as i64;
    let min_bits = nb_bits_baseline - max_weight;
    let w_end = max_weight + 1;
    let syms_of = |w: usize| &sorted[rank_start[w] as usize..rank_start[w + 1] as usize];
    for w in 1..w_end {
        let syms = syms_of(w);
        let nb_bits = nb_bits_baseline - w;
        if target_log - nb_bits < min_bits {
            fill_x2_for_weight(
                &mut x2[rank_val[w] as usize..],
                syms,
                nb_bits as u32,
                target_log as u32,
                0,
                1,
            );
            continue;
        }
        let mut start = rank_val[w] as usize;
        let length = 1usize << (target_log - nb_bits);
        let min_weight = (nb_bits as i64 + scale_log).max(1) as usize;
        let level2 = &rank_val_origin[nb_bits];
        for &base in syms {
            let dt = &mut x2[start..];
            if min_weight > 1 {
                let c = x2_cell(u32::from(base), nb_bits as u32, 0, 1);
                dt[..level2[min_weight] as usize].fill(c);
            }
            for w2 in min_weight..w_end {
                let total = nb_bits_baseline - w2 + nb_bits;
                fill_x2_for_weight(
                    &mut dt[level2[w2] as usize..],
                    syms_of(w2),
                    total as u32,
                    target_log as u32,
                    u32::from(base),
                    2,
                );
            }
            start += length;
        }
    }
}

/// `algoTime`: table and decode-256 times of one decoder at one compression quantile.
const ALGO_TIMES: [[(u32, u32); 2]; 16] = [
    [(0, 0), (1, 1)],
    [(0, 0), (1, 1)],
    [(150, 216), (381, 119)],
    [(170, 205), (514, 112)],
    [(177, 199), (539, 110)],
    [(197, 194), (644, 107)],
    [(221, 192), (735, 107)],
    [(256, 189), (881, 106)],
    [(359, 188), (1167, 109)],
    [(582, 187), (1570, 114)],
    [(688, 187), (1712, 122)],
    [(825, 186), (1965, 136)],
    [(976, 185), (2131, 150)],
    [(1180, 186), (2070, 175)],
    [(1377, 185), (1731, 202)],
    [(1412, 185), (1695, 202)],
];

/// Ports `HUF_selectDecoder`: true for the X2 decoder.
pub(crate) fn select_x2(dst_size: usize, c_src_size: usize) -> bool {
    let q = if c_src_size < dst_size {
        c_src_size * 16 / dst_size
    } else {
        15
    };
    let d256 = (dst_size >> 8) as u32;
    let t0 = ALGO_TIMES[q][0].0 + ALGO_TIMES[q][0].1 * d256;
    let mut t1 = ALGO_TIMES[q][1].0 + ALGO_TIMES[q][1].1 * d256;
    t1 += t1 >> 5;
    t1 < t0
}

/// `HUF_DecompressFastArgs`; indexes are into `src` and `dst`.
pub(crate) struct FastArgs {
    pub(crate) ip: [usize; 4],
    pub(crate) op: [usize; 4],
    pub(crate) bits: [u64; 4],
    iend: [usize; 4],
    pub(crate) oend: usize,
}

impl HufTable {
    /// Ports `HUF_decodeStreamX1` into `dst[p..end]`.
    fn decode_stream_x1(&self, dst: &mut [u8], mut p: usize, end: usize, br: &mut BitReader) {
        let dt = &self.x1;
        let log = self.log;
        macro_rules! sym {
            () => {{
                let c = dt[br.look_fast(log) as usize];
                dst[p] = (c >> 8) as u8;
                br.consumed += usize::from(c & 0xFF);
                p += 1;
            }};
        }
        if end - p > 3 {
            while br.reload() == UNFINISHED && p + 3 < end {
                sym!();
                sym!();
                sym!();
                sym!();
            }
        } else {
            br.reload();
        }
        while p < end {
            sym!();
        }
    }

    /// Ports `HUF_decodeStreamX2` into `dst[p..end]` and returns the new `p`.
    fn decode_stream_x2(
        &self,
        dst: &mut [u8],
        mut p: usize,
        end: usize,
        br: &mut BitReader,
    ) -> usize {
        let dt = &self.x2;
        let log = self.log;
        macro_rules! sym {
            () => {{
                let c = dt[br.look_fast(log) as usize];
                dst[p] = c as u8;
                dst[p + 1] = (c >> 8) as u8;
                br.consumed += ((c >> 16) & 0xFF) as usize;
                p += (c >> 24) as usize;
            }};
        }
        if end - p >= 8 {
            if log <= 11 {
                while br.reload() == UNFINISHED && p + 9 < end {
                    sym!();
                    sym!();
                    sym!();
                    sym!();
                    sym!();
                }
            } else {
                while br.reload() == UNFINISHED && p + 7 < end {
                    sym!();
                    sym!();
                    sym!();
                    sym!();
                }
            }
        } else {
            br.reload();
        }
        if end - p >= 2 {
            while br.reload() == UNFINISHED && p + 2 <= end {
                sym!();
            }
            while p + 2 <= end {
                sym!();
            }
        }
        if p < end {
            let c = dt[br.look_fast(log) as usize];
            dst[p] = c as u8;
            if c >> 24 == 1 {
                br.consumed += ((c >> 16) & 0xFF) as usize;
            } else if br.consumed < 64 {
                br.consumed += ((c >> 16) & 0xFF) as usize;
                if br.consumed > 64 {
                    br.consumed = 64;
                }
            }
            p += 1;
        }
        p
    }

    /// Ports `HUF_decompress1X{1,2}_usingDTable_internal`.
    pub fn decode_1x(&self, dst: &mut [u8], src: &[u8]) -> bool {
        let Some(mut br) = BitReader::new(src) else {
            return false;
        };
        let n = dst.len();
        if self.double {
            self.decode_stream_x2(dst, 0, n, &mut br);
        } else {
            self.decode_stream_x1(dst, 0, n, &mut br);
        }
        br.finished()
    }

    /// Ports `HUF_decompress4X{1,2}_usingDTable_internal`: the fast loop when it applies, else the
    /// reference loop.
    pub fn decode_4x(&self, dst: &mut [u8], src: &[u8]) -> bool {
        match self.decode_4x_fast(dst, src) {
            Fast::Done => true,
            Fast::Corrupt => false,
            Fast::Fallback => self.decode_4x_slow(dst, src),
        }
    }

    /// Ports `HUF_DecompressFastArgs_init`: the fast loop's start for `dst_len` bytes of output, or
    /// the path to take instead.
    pub(crate) fn fast_args(&self, dst_len: usize, src: &[u8]) -> Result<FastArgs, Fast> {
        if dst_len == 0 {
            return Err(Fast::Fallback);
        }
        if src.len() < 10 {
            return Err(Fast::Corrupt);
        }
        if self.log != FAST_LOG {
            return Err(Fast::Fallback);
        }
        let l1 = usize::from(src[0]) | (usize::from(src[1]) << 8);
        let l2 = usize::from(src[2]) | (usize::from(src[3]) << 8);
        let l3 = usize::from(src[4]) | (usize::from(src[5]) << 8);
        let l4 = src.len() as i64 - (l1 + l2 + l3 + 6) as i64;
        let iend = [6, 6 + l1, 6 + l1 + l2, 6 + l1 + l2 + l3];
        if l1 < 8 || l2 < 8 || l3 < 8 || (0..8).contains(&l4) {
            return Err(Fast::Fallback);
        }
        if l4 < 0 {
            return Err(Fast::Corrupt);
        }
        let ip = [iend[1] - 8, iend[2] - 8, iend[3] - 8, src.len() - 8];
        let seg = dst_len.div_ceil(4);
        let op = [0, seg, 2 * seg, 3 * seg];
        if op[3] >= dst_len {
            return Err(Fast::Fallback);
        }
        let mut bits = [0u64; 4];
        for i in 0..4 {
            let last = src[ip[i] + 7];
            let c = if last != 0 {
                8 - highbit(u32::from(last))
            } else {
                0
            };
            bits[i] = (le64(&src[ip[i]..]) | 1) << c;
        }
        Ok(FastArgs {
            ip,
            op,
            bits,
            iend,
            oend: dst_len,
        })
    }

    fn decode_4x_fast(&self, dst: &mut [u8], src: &[u8]) -> Fast {
        let mut a = match self.fast_args(dst.len(), src) {
            Ok(a) => a,
            Err(f) => return f,
        };
        if self.double {
            hufloop::loop_x2(&mut a, src, dst, &self.x2);
        } else {
            hufloop::loop_x1(&mut a, src, dst, &self.x1);
        }

        let seg = dst.len().div_ceil(4);
        let mut seg_end = 0usize;
        for i in 0..4 {
            if seg <= dst.len() - seg_end {
                seg_end += seg;
            } else {
                seg_end = dst.len();
            }
            // HUF_initRemainingDStream
            if a.op[i] > seg_end || a.ip[i] + 8 < a.iend[i] {
                return Fast::Corrupt;
            }
            let mut br = BitReader {
                inp: src,
                ptr: a.ip[i],
                bits: le64(&src[a.ip[i]..]),
                consumed: a.bits[i].trailing_zeros() as usize,
            };
            if self.double {
                a.op[i] = self.decode_stream_x2(dst, a.op[i], seg_end, &mut br);
            } else {
                self.decode_stream_x1(dst, a.op[i], seg_end, &mut br);
                a.op[i] = seg_end;
            }
            if a.op[i] != seg_end {
                return Fast::Corrupt;
            }
        }
        Fast::Done
    }

    /// Ports `HUF_decompress4X{1,2}_usingDTable_internal_body`.
    fn decode_4x_slow(&self, dst: &mut [u8], src: &[u8]) -> bool {
        if src.len() < 10 || dst.len() < 6 {
            return false;
        }
        let l1 = usize::from(src[0]) | (usize::from(src[1]) << 8);
        let l2 = usize::from(src[2]) | (usize::from(src[3]) << 8);
        let l3 = usize::from(src[4]) | (usize::from(src[5]) << 8);
        let Some(l4) = src.len().checked_sub(l1 + l2 + l3 + 6) else {
            return false;
        };
        let seg = dst.len().div_ceil(4);
        let start = [0, seg, 2 * seg, 3 * seg];
        let oend = dst.len();
        if start[3] > oend {
            return false;
        }
        let mut br = [BitReader::empty(); 4];
        let mut inp = &src[6..];
        for (i, l) in [l1, l2, l3, l4].into_iter().enumerate() {
            let Some(b) = BitReader::new(&inp[..l]) else {
                return false;
            };
            br[i] = b;
            inp = &inp[l..];
        }
        let mut op = start;
        if self.double {
            if oend - op[3] >= 8 {
                let olimit = oend - 7;
                let dt = &self.x2;
                let log = self.log;
                let mut end = true;
                while end && op[3] < olimit {
                    for _ in 0..4 {
                        for i in 0..4 {
                            let c = dt[br[i].look_fast(log) as usize];
                            dst[op[i]] = c as u8;
                            dst[op[i] + 1] = (c >> 8) as u8;
                            br[i].consumed += ((c >> 16) & 0xFF) as usize;
                            op[i] += (c >> 24) as usize;
                        }
                    }
                    for b in &mut br {
                        let r = b.reload_fast() == UNFINISHED;
                        end = r && end;
                    }
                }
            }
        } else if oend - op[3] >= 8 {
            let olimit = oend - 3;
            let dt = &self.x1;
            let log = self.log;
            let mut end = true;
            while end && op[3] < olimit {
                for _ in 0..4 {
                    for i in 0..4 {
                        let c = dt[br[i].look_fast(log) as usize];
                        dst[op[i]] = (c >> 8) as u8;
                        br[i].consumed += usize::from(c & 0xFF);
                        op[i] += 1;
                    }
                }
                for b in &mut br {
                    let r = b.reload_fast() == UNFINISHED;
                    end = r && end;
                }
            }
        }
        if op[0] > start[1] || op[1] > start[2] || op[2] > start[3] {
            return false;
        }
        let ends = [start[1], start[2], start[3], oend];
        for i in 0..4 {
            if self.double {
                self.decode_stream_x2(dst, op[i], ends[i], &mut br[i]);
            } else {
                self.decode_stream_x1(dst, op[i], ends[i], &mut br[i]);
            }
        }
        br.iter().all(BitReader::finished)
    }
}

/// The result of the fast 4-stream path.
pub(crate) enum Fast {
    Done,
    Fallback,
    Corrupt,
}

/// Ports `HUF_decompress4X1_usingDTable_internal_fast_c_loop`. `dt` is a log-11 table, and the
/// loop leaves the last symbols of each stream to the careful decoder. It is the reference of the
/// `hufloop` kernels and what `ZSTD_SIMD=scalar` runs.
pub(crate) fn huf4x1_loop(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u16; 1 << 12]) {
    let (mut ip, mut op, mut b) = (a.ip, a.op, a.bits);
    loop {
        // Five symbols of at most 11 bits use under 7 input bytes.
        let iters = ((a.oend - op[3]) / 5).min(ip[0] / 7);
        let olimit = op[3] + iters * 5;
        if op[3] == olimit {
            break;
        }
        if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
            break;
        }
        while op[3] < olimit {
            for s in 0..4 {
                let o = &mut dst[op[s]..op[s] + 5];
                let mut bs = b[s];
                for ob in o.iter_mut() {
                    let c = dt[(bs >> 53) as usize];
                    bs <<= c & 0x3F;
                    *ob = (c >> 8) as u8;
                }
                let ctz = bs.trailing_zeros() as usize;
                op[s] += 5;
                ip[s] -= ctz >> 3;
                b[s] = (le64(&src[ip[s]..]) | 1) << (ctz & 7);
            }
        }
    }
    (a.ip, a.op, a.bits) = (ip, op, b);
}

/// Ports `HUF_decompress4X2_usingDTable_internal_fast_c_loop`.
pub(crate) fn huf4x2_loop(a: &mut FastArgs, src: &[u8], dst: &mut [u8], dt: &[u32; 1 << 12]) {
    let (mut ip, mut op, mut b) = (a.ip, a.op, a.bits);
    let oend = [op[1], op[2], op[3], a.oend];
    loop {
        // Five lookups write at most 10 bytes per stream.
        let mut iters = ip[0] / 7;
        for s in 0..4 {
            iters = iters.min((oend[s] - op[s]) / 10);
        }
        let olimit = op[3] + iters * 5;
        if op[3] == olimit {
            break;
        }
        if ip[1] < ip[0] || ip[2] < ip[1] || ip[3] < ip[2] {
            break;
        }
        while op[3] < olimit {
            for s in 0..4 {
                let mut o = op[s];
                let mut bs = b[s];
                for _ in 0..5 {
                    let c = dt[(bs >> 53) as usize];
                    dst[o] = c as u8;
                    dst[o + 1] = (c >> 8) as u8;
                    bs <<= (c >> 16) & 0x3F;
                    o += (c >> 24) as usize;
                }
                op[s] = o;
                let ctz = bs.trailing_zeros() as usize;
                ip[s] -= ctz >> 3;
                b[s] = (le64(&src[ip[s]..]) | 1) << (ctz & 7);
            }
        }
    }
    (a.ip, a.op, a.bits) = (ip, op, b);
}
