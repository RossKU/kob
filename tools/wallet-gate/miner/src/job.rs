//! One mining job per block template: everything a nonce search needs, derived from the header by rusty-kaspa (the source of truth
//! for the pre-PoW hash, the matrix, the target and the cSHAKE256 states) and laid out the way the OpenCL kernel reads it.

use kaspa_consensus_core::{Hash, hashing, header::Header};
use kaspa_math::Uint256;
use kaspa_pow::{State, matrix::Matrix};

/// `kaspa_hashes::PowHash::INITIAL_STATE` (rusty-kaspa v2.1.0, crypto/hashes/src/pow_hashers.rs): the cSHAKE256("ProofOfWorkHash")
/// state with the padding of the 80-byte message folded in (word 10 ^= 0x04, word 16 ^= 1 << 63). The constant is private upstream;
/// `tests::host_data_reproduces_kaspa_pow` proves this copy against `PowHash` and `State::calculate_pow`.
pub const POW_INIT: [u64; 25] = [
    1242148031264380989,
    3008272977830772284,
    2188519011337848018,
    1992179434288343456,
    8876506674959887717,
    5399642050693751366,
    1745875063082670864,
    8605242046444978844,
    17936695144567157056,
    3343109343542796272,
    1123092876221303306,
    4963925045340115282,
    17037383077651887893,
    16629644495023626889,
    12833675776649114147,
    3784524041015224902,
    1082795874807940378,
    13952716920571277634,
    13411128033953605860,
    15060696040649351053,
    9928834659948351306,
    5237849264682708699,
    12825353012139217522,
    6706187291358897596,
    196324915476054915,
];

/// `kaspa_hashes::KHeavyHash::INITIAL_STATE`: cSHAKE256("HeavyHash") with the padding of the 32-byte message folded in.
pub const HEAVY_INIT: [u64; 25] = [
    4239941492252378377,
    8746723911537738262,
    8796936657246353646,
    1272090201925444760,
    16654558671554924250,
    8270816933120786537,
    13907396207649043898,
    6782861118970774626,
    9239690602118867528,
    11582319943599406348,
    17596056728278508070,
    15212962468105129023,
    7812475424661425213,
    3370482334374859748,
    5690099369266491460,
    8596393687355028144,
    570094237299545110,
    9119540418498120711,
    16901969272480492857,
    13372017233735502424,
    14372891883993151831,
    5171152063242093102,
    10573107899694386186,
    6096431547456407061,
    1592359455985097269,
];

pub struct Job {
    /// kaspa_pow's own state: the CPU backend searches with it and every nonce of any backend is verified with it before submit
    pub state: State,
    /// hash_override_nonce_time(header, 0, 0) as little-endian u64 words
    pub pre_pow: [u64; 4],
    pub timestamp: u64,
    /// Matrix::generate(pre_pow_hash), 64 x 64 nibbles, row-major
    pub matrix: Vec<u8>,
    /// Uint256::from_compact_target_bits(bits), little-endian u64 words
    pub target: [u64; 4],
}

impl Job {
    pub fn new(header: &Header) -> Self {
        let pre_pow_hash = hashing::header::hash_override_nonce_time(header, 0, 0);
        Self {
            state: State::new(header),
            pre_pow: le_words(&pre_pow_hash),
            timestamp: header.timestamp,
            matrix: matrix_nibbles(&Matrix::generate(pre_pow_hash)),
            target: Uint256::from_compact_target_bits(header.bits).0,
        }
    }

    /// kaspa_pow's verdict on `nonce` (pow <= target).
    pub fn verify(&self, nonce: u64) -> bool {
        self.state.check_pow(nonce).0
    }

    /// kaspa_pow's pow value of `nonce`, little-endian words.
    pub fn pow(&self, nonce: u64) -> [u64; 4] {
        self.state.calculate_pow(nonce).0
    }
}

fn le_words(h: &Hash) -> [u64; 4] {
    let mut w = [0u64; 4];
    for (o, v) in w.iter_mut().zip(h.iter_le_u64()) {
        *o = v;
    }
    w
}

/// The 4096 matrix nibbles, row-major. `kaspa_pow::matrix::Matrix` keeps its `[[u16; 64]; 64]` private; its derived `Debug` prints
/// `Matrix([[a, b, ...], ...])` in row-major order, which is a stable, safe way to read the elements without re-implementing
/// `Matrix::generate` (xoshiro256++ draws plus the rank-64 rejection loop).
pub fn matrix_nibbles(m: &Matrix) -> Vec<u8> {
    let s = format!("{m:?}");
    let v: Vec<u8> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<u16>().expect("matrix element"))
        .map(|x| {
            assert!(x < 16, "matrix element {x} is not a nibble");
            x as u8
        })
        .collect();
    assert_eq!(v.len(), 64 * 64, "matrix Debug output changed shape");
    v
}

/// A synthetic header with fixed fields (benchmarks, tests); `seed` varies the pre-PoW hash, `bits` the target.
pub fn synthetic_header(seed: u8, bits: u32) -> Header {
    use kaspa_consensus_core::header::CompressedParents;
    use kaspa_math::Uint192;
    let h = |b: u8| Hash::from_bytes([b; 32]);
    Header::new_finalized(
        1,
        CompressedParents::try_from(vec![vec![h(seed), h(seed.wrapping_add(1))]]).unwrap(),
        h(seed.wrapping_add(2)),
        h(seed.wrapping_add(3)),
        h(seed.wrapping_add(4)),
        1_790_000_000_000 + seed as u64 * 1013,
        bits,
        0,
        586_000_000 + seed as u64,
        Uint192::from_u64(123_456_789),
        586_000_000,
        h(seed.wrapping_add(5)),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn header(seed: u8, bits: u32) -> Header {
        synthetic_header(seed, bits)
    }

    /// The kernel's arithmetic in plain Rust, fed with exactly the data the host uploads (Job + POW_INIT / HEAVY_INIT). Not used for
    /// mining: it proves on any CI machine (no GPU) that the host-side data reproduce kaspa_pow, so a GPU mismatch can only be the
    /// kernel itself (covered by `gpu::tests` where an OpenCL device exists).
    #[allow(clippy::needless_range_loop)] // indexed like the kernel on purpose
    pub fn kernel_mirror(job: &Job, nonce: u64) -> [u64; 4] {
        let mut s = POW_INIT;
        for i in 0..4 {
            s[i] ^= job.pre_pow[i];
        }
        s[4] ^= job.timestamp;
        s[9] ^= nonce;
        keccak::f1600(&mut s);
        let mut pow_bytes = [0u8; 32];
        for i in 0..4 {
            pow_bytes[i * 8..i * 8 + 8].copy_from_slice(&s[i].to_le_bytes());
        }
        let mut vec = [0u8; 64];
        for i in 0..32 {
            vec[2 * i] = pow_bytes[i] >> 4;
            vec[2 * i + 1] = pow_bytes[i] & 0x0F;
        }
        let mut product = [0u8; 32];
        for i in 0..32 {
            let (mut hi, mut lo) = (0u32, 0u32);
            for j in 0..64 {
                hi += job.matrix[2 * i * 64 + j] as u32 * vec[j] as u32;
                lo += job.matrix[(2 * i + 1) * 64 + j] as u32 * vec[j] as u32;
            }
            product[i] = (((hi >> 10) << 4) | ((lo >> 10) & 0x0F)) as u8 ^ pow_bytes[i];
        }
        let mut s = HEAVY_INIT;
        for i in 0..4 {
            s[i] ^= u64::from_le_bytes(product[i * 8..i * 8 + 8].try_into().unwrap());
        }
        keccak::f1600(&mut s);
        [s[0], s[1], s[2], s[3]]
    }

    pub fn le_leq(a: &[u64; 4], b: &[u64; 4]) -> bool {
        for i in (0..4).rev() {
            if a[i] != b[i] {
                return a[i] < b[i];
            }
        }
        true
    }

    #[test]
    fn host_data_reproduces_kaspa_pow() {
        for seed in [0u8, 1, 7, 42, 200, 255] {
            let job = Job::new(&header(seed, 0x1e7fffff));
            for k in 0..64u64 {
                let nonce = k.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (seed as u64) << 56;
                assert_eq!(kernel_mirror(&job, nonce), job.pow(nonce), "seed {seed} nonce {nonce:#x}");
            }
        }
    }

    #[test]
    fn target_words_and_compare_match_check_pow() {
        // 0x1f0fffff: target ~2^244, about one nonce in 4096 passes; find some and compare both verdicts on passes and misses
        let job = Job::new(&header(9, 0x1f0fffff));
        let (mut pass, mut fail) = (0, 0);
        for nonce in 0..100_000u64 {
            let p = job.pow(nonce);
            let ours = le_leq(&p, &job.target);
            assert_eq!(ours, job.verify(nonce), "nonce {nonce}");
            if ours { pass += 1 } else { fail += 1 }
        }
        assert!(pass > 0 && fail > 0, "pass {pass} fail {fail}");
    }

    #[test]
    fn matrix_is_row_major_nibbles() {
        let pre = hashing::header::hash_override_nonce_time(&header(3, 0x1e7fffff), 0, 0);
        let m = matrix_nibbles(&Matrix::generate(pre));
        assert_eq!(m.len(), 4096);
        assert!(m.iter().all(|&x| x < 16));
        // different templates, different matrices
        let pre2 = hashing::header::hash_override_nonce_time(&header(4, 0x1e7fffff), 0, 0);
        assert_ne!(m, matrix_nibbles(&Matrix::generate(pre2)));
    }
}
