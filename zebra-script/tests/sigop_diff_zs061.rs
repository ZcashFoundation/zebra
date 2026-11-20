//! Compares zcash_script 0.6.1's `sig_op_count(false)` with the `GetOp` port that
//! zebra#12512 shipped, which matched zcashd's C++ count on 20M scripts.

use zebra_script::legacy_sigop_count;

/// The ported counter from zebra#12512 (cb08f3ec4), kept here as the reference.
fn port_count(mut script: &[u8]) -> u32 {
    fn skip_push_data(opcode: u8, script: &[u8]) -> Option<&[u8]> {
        fn length<const N: usize>(script: &[u8]) -> Option<(usize, &[u8])> {
            let (length, rest) = script.split_first_chunk::<N>()?;
            let mut bytes = [0; 4];
            bytes[..N].copy_from_slice(length);
            Some((usize::try_from(u32::from_le_bytes(bytes)).ok()?, rest))
        }
        let (length, script) = match opcode {
            0x00..=0x4b => (usize::from(opcode), script),
            0x4c => length::<1>(script)?,
            0x4d => length::<2>(script)?,
            0x4e => length::<4>(script)?,
            _ => (0, script),
        };
        script.get(length..)
    }

    let mut count: u32 = 0;
    while let Some((&opcode, rest)) = script.split_first() {
        let Some(rest) = skip_push_data(opcode, rest) else {
            break;
        };
        script = rest;
        count = count.saturating_add(match opcode {
            0xac | 0xad => 1,
            0xae | 0xaf => 20,
            _ => 0,
        });
    }
    count
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Appends one random element, weighted towards sigop opcodes, push lengths near the
/// 520-byte limit, and truncated pushes.
fn element(r: &mut Rng, out: &mut Vec<u8>) {
    match r.below(10) {
        0..=2 => out.push([0xac, 0xad, 0xae, 0xaf][r.below(4) as usize]),
        3 => out.push(r.below(256) as u8),
        4 => {
            let n = 1 + r.below(75) as usize;
            out.push(n as u8);
            let take = if r.below(8) == 0 {
                r.below(n as u64) as usize
            } else {
                n
            };
            out.extend((0..take).map(|_| r.below(256) as u8));
        }
        _ => {
            let len = match r.below(4) {
                0 => r.below(80) as usize,
                1 => 510 + r.below(20) as usize,
                2 => r.below(1100) as usize,
                _ => 255 + r.below(3) as usize,
            };
            let op = [0x4c, 0x4d, 0x4e][r.below(3) as usize];
            let (len_bytes, len) = match op {
                0x4c => (vec![len.min(255) as u8], len.min(255)),
                0x4d => ((len as u16).to_le_bytes().to_vec(), len),
                _ => ((len as u32).to_le_bytes().to_vec(), len),
            };
            out.push(op);
            if r.below(16) == 0 {
                out.extend(&len_bytes[..r.below(len_bytes.len() as u64) as usize]);
                return;
            }
            out.extend(&len_bytes);
            let take = if r.below(10) == 0 {
                r.below(len as u64 + 1) as usize
            } else {
                len
            };
            // Sigop bytes in the payload change the count if a push is mis-parsed.
            out.extend((0..take).map(|_| {
                if r.below(2) == 0 {
                    0xac
                } else {
                    r.below(256) as u8
                }
            }));
        }
    }
}

#[test]
fn zcash_script_sig_op_count_matches_ported_get_op() {
    let push = |prefix: &[u8], len: usize| [prefix, &vec![0xac; len], &[0xac]].concat();
    let vectors: Vec<Vec<u8>> = vec![
        vec![],
        vec![0xac],
        vec![0xad],
        vec![0xae],
        vec![0xaf],
        vec![0x52, 0xae],
        push(&[0x4b], 75),
        push(&[0x4c, 0xff], 255),
        push(&[0x4d, 0x08, 0x02], 520),
        push(&[0x4d, 0x09, 0x02], 521),
        push(&[0x4e, 0x00, 0x01, 0x00, 0x00], 256),
        vec![0xac, 0x02, 0xac],
        vec![0xac, 0x4c],
        vec![0xac, 0x4d, 0x01],
        vec![0xac, 0x4e, 0x00, 0x00, 0x00],
        vec![0xac, 0x4e, 0xff, 0xff, 0xff, 0xff, 0xac],
        vec![0x7e, 0xac],
        vec![0xab, 0xac],
        vec![0x50, 0xba, 0xff, 0xac],
    ];
    for v in &vectors {
        assert_eq!(legacy_sigop_count(v), port_count(v), "vector {v:02x?}");
    }

    let mut exhaustive = 0u64;
    for len in 0..=3usize {
        for i in 0..(1u64 << (8 * len)) {
            let s: Vec<u8> = (0..len).map(|b| (i >> (8 * b)) as u8).collect();
            assert_eq!(
                legacy_sigop_count(&s),
                port_count(&s),
                "exhaustive {s:02x?}"
            );
            exhaustive += 1;
        }
    }

    let n: u64 = std::env::var("SIGOP_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000_000);
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let mut long = 0u64;
    for _ in 0..n {
        let mut s = Vec::new();
        for _ in 0..=r.below(8) {
            element(&mut r, &mut s);
        }
        let count = port_count(&s);
        if s.len() > 520 && count > 0 {
            long += 1;
        }
        assert_eq!(legacy_sigop_count(&s), count, "random {s:02x?}");
    }

    #[allow(clippy::print_stderr)]
    {
        eprintln!(
            "match on {} vectors, {exhaustive} exhaustive, {n} random ({long} over 520 bytes with sigops)",
            vectors.len()
        );
    }
}
