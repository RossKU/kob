/*
 * kHeavyHash OpenCL kernel for Kaspa mining (tn10-miner `--backend gpu`).
 *
 * Provenance: the founder's own earlier code, `kheavyhash.cl` from his TN12 GPU miner (gpu_miner_remnants_2026-04-17.zip: this kernel,
 * a C host lib and the Python driver tn12_miner.py), which mined verified TN12 blocks on an Adreno 750 at ~260 KH/s. Kept here with
 * these changes (2026-10-02): the per-nonce hash is factored out of `mine_nonces` into `kheavyhash()` so that the test kernel
 * `hash_nonces` runs exactly the same code; `mine_nonces` collects up to MAX_RESULTS winners through an atomic counter instead of one
 * racy slot; the `-cl-std=CL2.0` build option of the old C host is not used (plain OpenCL C 1.2).
 *
 * Diffed against rusty-kaspa v2.1.0 (`kaspa_pow::State::calculate_pow`, `kaspa_hashes::{PowHash, KHeavyHash}`, `Matrix::heavy_hash`);
 * the host passes everything consensus-specific, computed by rusty-kaspa on the CPU:
 *   pow_init / heavy_init  the cSHAKE256("ProofOfWorkHash") / cSHAKE256("HeavyHash") initial states with their padding words
 *                          (PowHash::INITIAL_STATE / KHeavyHash::INITIAL_STATE)
 *   pre_pow                hash_override_nonce_time(header, 0, 0) as 4 little-endian u64 words (Hash::iter_le_u64)
 *   timestamp              header.timestamp (state word 4; words 5..8 are the 32 zero bytes; the nonce is word 9)
 *   matrix                 Matrix::generate(pre_pow_hash), 64 x 64 nibbles, row-major
 *   target                 Uint256::from_compact_target_bits(header.bits), 4 u64 words, [0] least significant
 * and every step below matches: pow hash = state[0..3] as little-endian bytes (Hash::from_le_u64); vec = (hi nibble, lo nibble) per
 * byte; product[i] = ((sum(row 2i) >> 10) << 4) | (sum(row 2i+1) >> 10) (sums <= 64*15*15 = 14400, so >> 10 <= 14 fits a nibble);
 * product ^= pow hash; KHeavyHash over it; pass iff hash <= target (Uint256 from the little-endian bytes). No divergence was found.
 * The host re-verifies every nonce this kernel reports with kaspa_pow before it submits a block.
 */

#define MAX_RESULTS 16

inline ulong rotl64(ulong x, uint n) {
    return (x << n) | (x >> (64 - n));
}

/* ---------- Keccak-f[1600] ---------- */

__constant ulong KECCAK_RC[24] = {
    0x0000000000000001UL, 0x0000000000008082UL, 0x800000000000808AUL, 0x8000000080008000UL,
    0x000000000000808BUL, 0x0000000080000001UL, 0x8000000080008081UL, 0x8000000000008009UL,
    0x000000000000008AUL, 0x0000000000000088UL, 0x0000000080008009UL, 0x000000008000000AUL,
    0x000000008000808BUL, 0x800000000000008BUL, 0x8000000000008089UL, 0x8000000000008003UL,
    0x8000000000008002UL, 0x8000000000000080UL, 0x000000000000800AUL, 0x800000008000000AUL,
    0x8000000080008081UL, 0x8000000000008080UL, 0x0000000080000001UL, 0x8000000080008008UL,
};

void keccak_f1600(ulong s[25]) {
    for (int round = 0; round < 24; round++) {
        /* theta */
        ulong c[5], d[5], t[25];
        for (int x = 0; x < 5; x++)
            c[x] = s[x] ^ s[x+5] ^ s[x+10] ^ s[x+15] ^ s[x+20];
        for (int x = 0; x < 5; x++)
            d[x] = c[(x+4)%5] ^ rotl64(c[(x+1)%5], 1);
        for (int x = 0; x < 5; x++)
            for (int y = 0; y < 25; y += 5)
                s[y+x] ^= d[x];

        /* rho + pi (fully unrolled — avoids __constant uchar arrays that crash Adreno) */
        t[0] = s[0];
        t[10]=rotl64(s[1],1);  t[7]=rotl64(s[10],3);  t[11]=rotl64(s[7],6);
        t[17]=rotl64(s[11],10); t[18]=rotl64(s[17],15); t[3]=rotl64(s[18],21);
        t[5]=rotl64(s[3],28);  t[16]=rotl64(s[5],36);  t[8]=rotl64(s[16],45);
        t[21]=rotl64(s[8],55); t[24]=rotl64(s[21],2);  t[4]=rotl64(s[24],14);
        t[15]=rotl64(s[4],27); t[23]=rotl64(s[15],41); t[19]=rotl64(s[23],56);
        t[13]=rotl64(s[19],8); t[12]=rotl64(s[13],25); t[2]=rotl64(s[12],43);
        t[20]=rotl64(s[2],62); t[14]=rotl64(s[20],18); t[22]=rotl64(s[14],39);
        t[9]=rotl64(s[22],61); t[6]=rotl64(s[9],20);  t[1]=rotl64(s[6],44);

        /* chi */
        for (int y = 0; y < 25; y += 5)
            for (int x = 0; x < 5; x++)
                s[y+x] = t[y+x] ^ ((~t[y+(x+1)%5]) & t[y+(x+2)%5]);

        /* iota */
        s[0] ^= KECCAK_RC[round];
    }
}

/* ---------- kHeavyHash of one nonce: out[0..3] = the 256-bit pow value, little-endian words ---------- */

inline void kheavyhash(
    __constant ulong* pow_init,
    __constant ulong* heavy_init,
    __constant ulong* pre_pow,
    ulong timestamp,
    __constant uchar* matrix,
    ulong nonce,
    ulong out[4]
) {
    /* --- 1. pow_hash: cSHAKE256("ProofOfWorkHash", pre_pow_hash || ts || 32 zero bytes || nonce) --- */
    ulong state[25];
    for (int i = 0; i < 25; i++) state[i] = pow_init[i];
    state[0] ^= pre_pow[0];
    state[1] ^= pre_pow[1];
    state[2] ^= pre_pow[2];
    state[3] ^= pre_pow[3];
    state[4] ^= timestamp;
    /* state[5..8] = zero padding (already zero in init) */
    state[9] ^= nonce;
    keccak_f1600(state);

    /* Extract pow_hash from state[0..3] as LE bytes → 64 nibbles */
    uchar pow_bytes[32];
    for (int i = 0; i < 4; i++) {
        ulong v = state[i];
        for (int j = 0; j < 8; j++)
            pow_bytes[i*8+j] = (uchar)(v >> (j*8));
    }

    /* Split into 64 nibbles */
    uchar vec[64];
    for (int i = 0; i < 32; i++) {
        vec[i*2]   = (pow_bytes[i] >> 4) & 0x0F;
        vec[i*2+1] =  pow_bytes[i]       & 0x0F;
    }

    /* --- 2. Matrix-vector multiply (64x64 * 64-vec) + XOR --- */
    uchar product[32];
    for (int i = 0; i < 32; i++) {
        uint sum_hi = 0, sum_lo = 0;
        int row_hi = (2*i) * 64;
        int row_lo = (2*i+1) * 64;
        for (int j = 0; j < 64; j++) {
            sum_hi += matrix[row_hi + j] * vec[j];
            sum_lo += matrix[row_lo + j] * vec[j];
        }
        product[i] = (uchar)(((sum_hi >> 10) << 4) | ((sum_lo >> 10) & 0x0F));
    }

    /* XOR with original pow_hash */
    for (int i = 0; i < 32; i++)
        product[i] ^= pow_bytes[i];

    /* --- 3. kheavy_hash: cSHAKE256("HeavyHash", product) --- */
    for (int i = 0; i < 25; i++) state[i] = heavy_init[i];
    for (int i = 0; i < 4; i++) {
        ulong v = 0;
        for (int j = 0; j < 8; j++)
            v |= ((ulong)product[i*8+j]) << (j*8);
        state[i] ^= v;
    }
    keccak_f1600(state);

    out[0] = state[0];
    out[1] = state[1];
    out[2] = state[2];
    out[3] = state[3];
}

/* ---------- Mining kernel: one nonce per work-item ---------- */

__kernel void mine_nonces(
    __constant ulong* pow_init,     /* [25] cSHAKE256("ProofOfWorkHash") state with padding */
    __constant ulong* heavy_init,   /* [25] cSHAKE256("HeavyHash") state with padding */
    __constant ulong* pre_pow,      /* [4]  pre_pow_hash as 4 x u64 LE */
    ulong timestamp,
    __constant uchar* matrix,       /* [64*64] nibble matrix (row-major) */
    __constant ulong* target,       /* [4]  target as 4 x u64 LE (index 0 = least significant) */
    ulong base_nonce,
    __global ulong* result_nonces,  /* [MAX_RESULTS] winning nonces */
    volatile __global uint* result_count /* number of winners found (may exceed MAX_RESULTS; only the first MAX_RESULTS are stored) */
) {
    if (*result_count) return;  /* Early exit once a winner is known: the host submits one block per template */

    ulong nonce = base_nonce + get_global_id(0);
    ulong h[4];
    kheavyhash(pow_init, heavy_init, pre_pow, timestamp, matrix, nonce, h);

    /* --- 4. Compare with target (256-bit LE, h[0]=LSW, h[3]=MSW): hash <= target --- */
    bool pass = true;
    for (int i = 3; i >= 0; i--) {
        if (h[i] < target[i]) break;                     /* definitely less → pass */
        if (h[i] > target[i]) { pass = false; break; }   /* definitely greater → fail */
        /* equal → check next word */
    }

    if (pass) {
        uint slot = atomic_inc(result_count);
        if (slot < MAX_RESULTS) result_nonces[slot] = nonce;
    }
}

/* ---------- Test kernel: the pow value of every nonce (kernel-vs-kaspa_pow equality tests) ---------- */

__kernel void hash_nonces(
    __constant ulong* pow_init,
    __constant ulong* heavy_init,
    __constant ulong* pre_pow,
    ulong timestamp,
    __constant uchar* matrix,
    ulong base_nonce,
    __global ulong* out             /* [4 * global size] */
) {
    size_t gid = get_global_id(0);
    ulong h[4];
    kheavyhash(pow_init, heavy_init, pre_pow, timestamp, matrix, base_nonce + gid, h);
    out[gid*4 + 0] = h[0];
    out[gid*4 + 1] = h[1];
    out[gid*4 + 2] = h[2];
    out[gid*4 + 3] = h[3];
}
