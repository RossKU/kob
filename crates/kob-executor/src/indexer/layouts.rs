//! Retired template layouts, so a record log outlives the templates it recorded.
//!
//! A record-log frame of format 1 (written before 2026-10-01, see `recordlog`) stores a revealed order state at the
//! length its template had WHEN THE FRAME WAS WRITTEN, without that length and without the template hash. A template
//! that changed since (protocol v2.6 pair-market auction: `KobCross` 333 -> 351 bytes) cannot be read with today's
//! length. This table lists every order-template artifact this repository ever committed that is not the pinned one
//! (`git log -- contracts/artifacts/Kob*.json`; the v2.4 trade receipts included): its record-log code, state length,
//! template hash and entry dispatch tags.
//!
//! Every format-1 frame predates protocol v3 (2026-10-05), whose templates are the first that changed what a state's numbers
//! mean (base units and prices per whole token, where the retired templates counted lots of `lotUnits x unit` base units) at
//! the SAME state lengths and dispatch tags for most kinds: a format-1 reveal is therefore always of a retired template. The
//! format-1 reader parses it with the retired layouts of its code whose dispatch tag matches the bytes after the state
//! (backtracking over the frame when several fit, [`super::record::DecodeCtx`]) and drops it, counted: this build cannot
//! interpret an older template's state. Format-2 frames carry the state length and a template table (code -> hash): a
//! reveal of a hash listed here is dropped and counted the same way (one of a hash nobody knows, as unknown).
//!
//! Append a row here whenever a pinned order template changes (the test `pinned_templates_are_not_retired` and the
//! `layouts_cover_history` comment below keep it honest).

/// A retired order-template layout (one committed artifact that is no longer pinned).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredLayout {
    /// Record-log wire code (`model::wire_code`: KOB1 kind code, high bit for KRON).
    pub code: u8,
    /// Length of the state span.
    pub state_len: usize,
    /// blake3 template hash (hex).
    pub hash: &'static str,
    /// Dispatch tags of its entries (big-endian: the 4 bytes as they appear in the signature script).
    pub tags: &'static [u32],
}

impl RetiredLayout {
    pub fn has_tag(&self, tag: &[u8]) -> bool {
        tag.len() == 4 && self.tags.contains(&u32::from_be_bytes([tag[0], tag[1], tag[2], tag[3]]))
    }

    pub fn hash_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&self.hash[2 * i..2 * i + 2], 16).expect("hex");
        }
        out
    }
}

/// The retired layouts of a wire code (distinct state lengths first seen first).
pub fn retired(code: u8) -> impl Iterator<Item = &'static RetiredLayout> {
    RETIRED_LAYOUTS.iter().filter(move |l| l.code == code)
}

/// The retired layout with this template hash, if any.
pub fn retired_by_hash(hash: &[u8; 32]) -> Option<&'static RetiredLayout> {
    RETIRED_LAYOUTS.iter().find(|l| &l.hash_bytes() == hash)
}

// layouts_cover_history: generated from every commit of any branch touching contracts/artifacts/Kob*.json or the deployment
// builds contracts/deploy/*/artifacts/Kob*.json (`git log --full-history --all`, through 69c9014: the four if-done entries
// retired on 2026-10-02); one row per distinct non-pinned template hash, the testnet-10 deployment builds of the receipt
// era (R_ID-dependent conditional and if-done templates) included; then the fourteen protocol v2.6 templates of both
// families retired on 2026-10-05 by the v3 revision (8dd4ebf; the KRON cross limit `KobCrossKron` under its own code 0x88,
// which today reads as the one `KobCross` kind). Every template `kob_protocol::retired` can spend has its row (test
// `every_spendable_retired_template_has_its_row`).
#[rustfmt::skip]
pub const RETIRED_LAYOUTS: &[RetiredLayout] = &[
    RetiredLayout { code: 0x01, state_len: 198, hash: "e161c6500fd6c28de20bc96891fde3bda1251617ea77c058f4168e53abfa9d5c", tags: &[0x1e6cf528, 0xa0893109] }, // KobAsk
    RetiredLayout { code: 0x01, state_len: 225, hash: "5a9fa66fe6b228d1e542efd72690193f5e1e583e6d262f76f0b2ec4b3bffebc7", tags: &[0x26a2c631, 0xa0893109] }, // KobAsk
    RetiredLayout { code: 0x01, state_len: 243, hash: "21cf26fc19afe61423645f5a80bbef0255a5438df15f6cd753d89c440edc7b2d", tags: &[0x26a2c631, 0xa0893109] }, // KobAsk
    RetiredLayout { code: 0x01, state_len: 243, hash: "ccc62fa3730ada83b7b0e1bedcef4fc9635742a5a1972cf918be6639cf73ed02", tags: &[0x26a2c631, 0xa0893109] }, // KobAsk
    RetiredLayout { code: 0x02, state_len: 249, hash: "c3f2de715d49a013b246f8388d9b7adc96d0321b4f2ff60cf05cb4f3bb6fe5bc", tags: &[0x25e088f9, 0x777f5b11, 0xa0893109] }, // KobBid
    RetiredLayout { code: 0x02, state_len: 276, hash: "552565298c36ce15e0a4eaf4806b991936745eff6a146f7557d9c544402224da", tags: &[0x5f698675, 0x777f5b11, 0xa0893109] }, // KobBid
    RetiredLayout { code: 0x02, state_len: 285, hash: "39af759ef52f23a265efb6ea9c7b857c8a777940dae5aa65b8c7ae4e5e360d09", tags: &[0x5f698675, 0x777f5b11, 0xa0893109] }, // KobBid
    RetiredLayout { code: 0x03, state_len: 252, hash: "ce6c58df9ae5097e83165d9dda6c2c8fa8f3cfc363fb5ffb4dc0a46ead83fd62", tags: &[0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 261, hash: "4313e2768f2b27eee2c404c858386e47c83d302b2f96aba00cd7718dc3bd7350", tags: &[0x26a2c631, 0xa0893109, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 261, hash: "af84f6b24ad170600b071231a2104c410568a11e3b89350d37eba1f2035ffda3", tags: &[0x26a2c631, 0xa0893109, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 279, hash: "8f3ca118d9f6b68c624b8644fd2aa1a43a1ddf1e2bd20c2a34552fc769d5e827", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 330, hash: "4f9692d424bf2387a94d83b3278a564b50b46344b83645313f80b3bbbacdc77c", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 330, hash: "92d36ff2ecad7a04679b41de616c56cf8c633d8179a4acedec9fff8eae82ae28", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 330, hash: "5cafde88a6c2be696178983e506cb9be3c99e0bb824a97cc346630f50098c4b6", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 330, hash: "9fa11943c280056239e61f3690d734adbfd5667e8ec8b1721fe6bcb794f8fe9f", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAsk
    RetiredLayout { code: 0x03, state_len: 330, hash: "d17183a89c58a7eef3f0bbd0f501f0161721a1a15565bf860defe1fdecd7c991", tags: &[0x46ade55d, 0x68a357bc, 0xa0893109] }, // KobCondAsk
    RetiredLayout { code: 0x04, state_len: 303, hash: "375b1d2ceb3861ec18e2c94b2a970647769ad30d0e8f6cda07961882b955f208", tags: &[0x26a2c631, 0x777f5b11, 0xa0893109, 0xcd93c503] }, // KobCondBid
    RetiredLayout { code: 0x04, state_len: 312, hash: "f93423c69e6b27ffeec6fe887caab76c20a8c1a231fa78d7e3e14cf5ccc4edeb", tags: &[0x1e6cf528, 0x777f5b11, 0xa0893109, 0xcd93c503] }, // KobCondBid
    RetiredLayout { code: 0x04, state_len: 321, hash: "98e0f7b29c0ca056439ad20377005a8e95adcb655b16ad3d6941797807e3b239", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBid
    RetiredLayout { code: 0x04, state_len: 381, hash: "5ed4931a49d7370dc2c77512f3fb7866dc5051014493f306b0b3d80b6ac25196", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBid
    RetiredLayout { code: 0x04, state_len: 381, hash: "6cf8eba27a174725340b3cc2b2fd68af9831be464af167c5eea557d0288a0078", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBid
RetiredLayout { code: 0x04, state_len: 381, hash: "040f5ac8e23b4360a7956028fa05d3f370110cbe1b13fe87c6213317d17e32ad", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBid
    RetiredLayout { code: 0x04, state_len: 381, hash: "6f4eb6398254a5192cf2f5de026414164f1ee783c4ab9ab474d2a0a5c652009c", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBid
    RetiredLayout { code: 0x04, state_len: 381, hash: "f959c978dde45ad2ff23498a742ca489617cea411cb6d933264e47b2d2c26ed6", tags: &[0x46ade55d, 0x777f5b11, 0xa0893109, 0xbd44243d] }, // KobCondBid
    RetiredLayout { code: 0x05, state_len: 485, hash: "7780a32a0da9641ae52f1d1518aa23c2c735a573e43bf58687153e0698feae87", tags: &[0x777f5b11, 0xa0893109, 0xc7ddaf0c] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 494, hash: "b9c56395182d5c88a8f7309dbb5fb2081b617150176ced6bbb85b7d4242e16f0", tags: &[0x104383c5, 0x777f5b11, 0xa0893109] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 504, hash: "a2c162ec5a23e8b502b3157bf76edb82926610a348993a86ac60bf194468cdb9", tags: &[0x777f5b11, 0xa0893109, 0xc7ddaf0c] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 504, hash: "da2351adb20c06478adb6884ecb3b877df60076249c212d9e10791d7e4a20c69", tags: &[0x777f5b11, 0xa0893109, 0xc7ddaf0c] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 585, hash: "7a2d395067407df91bddb70295e6a4187c6881de1731bfcf14368404ab0dd7d6", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "c3c9e2067fe67ee932d2fa848c827e2907fa6c4ddb8237e4f42b775c0a62579a", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "f77e5897fdbccfdf474590b73090c4c77b7100fa4c5aff72f0f70bf981b302f6", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "35ed6bed435f75faaac2f2c7f843312af9d5a4e5a6d404f34ad1ac0690feebc7", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "6a4f2fe4d81c002da86c0ce076918f8ea7eaea6b998baeec94828c9b6a2cae69", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "824b42ee47df65e236fef29896225c9a39cdd4153fc8698957f6c826fbcb79ca", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "ef5a575fbf1003bf3779be7efa169c795dc39a03348123091a0ed319fc42eeda", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x06, state_len: 495, hash: "94490639b22440109f53cedbc324ab3c2ae48ca67b1da6203fb976c5475dd879", tags: &[0x5a28b4fe, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 504, hash: "398b1d8ddbecafcb1a6b178d22ff26a4736736d47f655aec20460be054204f72", tags: &[0x6892042b, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 513, hash: "9947f833d620035343c852cd2d05242dcbe864759dba7d8c77c05398b86d52ba", tags: &[0x5a28b4fe, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 594, hash: "3a307844a9120659e78f190c1af117f95210e2671ff60061eb8515cc17473f46", tags: &[0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 603, hash: "34c4e33163bf969a16fe10c878926fac3980d0c32a0fc82df0c060f8c189f8ea", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 603, hash: "526b6b77322c1e620d51dacc7c31678d8e3f2ec28d11077a4bdf02d272185df5", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 603, hash: "789ddaa134f29a1f5fe23387963e05c51d29c737d2d44a196823a5623df3dad3", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 603, hash: "0acad08dd434a42fb7d202e45b294945881e39a29c9a467fbb017510b62a2163", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 603, hash: "8afde0646e060be0efd60c263aa0d719106046aa95dcc8a4aa79c625f69b2e93", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x06, state_len: 603, hash: "dd3054d09dfedab7244735bfd2f5e8ba46c0be8cbce01f9463033c94c8f13dab", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAsk
    RetiredLayout { code: 0x07, state_len: 120, hash: "39ba4a4b9533ebb92a2dba9c5726cf3a60956676ee2f4d2cfc09915a96bf470c", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceipt
    RetiredLayout { code: 0x07, state_len: 120, hash: "8680e445b49c9218b7521da4dbdb8e41626691a5498c3fc97d611e7ce1efe131", tags: &[0x3670a508, 0x3b597c7e, 0x5f9d04ca, 0xe84db6b0] }, // KobReceipt
    RetiredLayout { code: 0x07, state_len: 120, hash: "8edb93043b7d772a4c99d0a3391b205d8fa88a553dbbf47dcf9f0e06a759b9cc", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceipt
    RetiredLayout { code: 0x07, state_len: 120, hash: "96717ef5fe703b00c0356a6d051247c5ed3c168f79bbb55f0f3f6b842f526d7a", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceipt
    RetiredLayout { code: 0x07, state_len: 120, hash: "b62c8f9f5e7da523c2c3c670af99fca89d7df0f0127f232a7273b20c16a04a94", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceipt
    RetiredLayout { code: 0x08, state_len: 333, hash: "0b1467488070f986ade9237c8af63b09404054c7f9ae91b3d60ba18e2a152929", tags: &[0x1e6cf528, 0xa0893109] }, // KobCross
    RetiredLayout { code: 0x08, state_len: 333, hash: "70b1dcd3e5f2c3821f031d5346680d0772fb37851760aea8fa841e80424a92c7", tags: &[0x1e6cf528, 0xa0893109] }, // KobCross
    RetiredLayout { code: 0x08, state_len: 351, hash: "b23ae732e42b52bf04dc9fde31a1c2d6601cede9c3a1151d102b9933ee32ec4b", tags: &[0xa0893109, 0xbd44243d] }, // KobCross
    RetiredLayout { code: 0x81, state_len: 225, hash: "ae5408dffc8560faf57e4ac754bd6e2aaad598602b4b662d8b5129233548484c", tags: &[0x26a2c631, 0xa0893109] }, // KobAskKron
    RetiredLayout { code: 0x81, state_len: 243, hash: "301774bd22a192e52ac565b08e2c9488896348aa1eaa8a8e94e460d439f5f82f", tags: &[0x26a2c631, 0xa0893109] }, // KobAskKron
    RetiredLayout { code: 0x81, state_len: 243, hash: "452ec1f8ea9f4debbe8182809ed945b83c6e55572675ad64caffb9f1972054ab", tags: &[0x26a2c631, 0xa0893109] }, // KobAskKron
    RetiredLayout { code: 0x82, state_len: 243, hash: "0cfd9a5cde69fe68cfc8e0284d9d8f48f6decf51b318c52510f71bd1bd7e551a", tags: &[0x5f698675, 0x777f5b11, 0xa0893109] }, // KobBidKron
    RetiredLayout { code: 0x82, state_len: 252, hash: "87c84a09979ee4b097ea0b7c2ef1bec48d0d765c2b820136278c17f75f9ccf96", tags: &[0x5f698675, 0x777f5b11, 0xa0893109] }, // KobBidKron
    RetiredLayout { code: 0x83, state_len: 252, hash: "4bca73e859e4708d26157864c84bb7cec319ceadd31dfa2b7821097db9d5e961", tags: &[0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondAskKron
    RetiredLayout { code: 0x83, state_len: 330, hash: "695917526f63e76cfbcc45f65e82f0a606699427115cb1adf5519f99a625b3d3", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAskKron
    RetiredLayout { code: 0x83, state_len: 330, hash: "adda1e3106c0439ad564c8ec23117304d0c6f42a6850e6c1ce17a69ef6fc2d10", tags: &[0xa0893109, 0xb62164ca, 0xcd93c503] }, // KobCondAskKron
    RetiredLayout { code: 0x83, state_len: 330, hash: "a2f64f94a9e1e4e86b2fd97c2b97ef92d0398597d32b5fe181318df44e573128", tags: &[0x46ade55d, 0x68a357bc, 0xa0893109] }, // KobCondAskKron
    RetiredLayout { code: 0x84, state_len: 270, hash: "1138528cfff61dfd393d2f6c1d9991b7058f6ece63b38dcf5366701551f6b55b", tags: &[0x26a2c631, 0x777f5b11, 0xa0893109, 0xcd93c503] }, // KobCondBidKron
    RetiredLayout { code: 0x84, state_len: 348, hash: "5c1106498d6572d683f7d75afca22654aa650dc366733fb7e80d070d60ad7ee9", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBidKron
    RetiredLayout { code: 0x84, state_len: 348, hash: "c6552b4e472f3c512b320708023352bcc995a04dbc453d8bd9ed086a2377801f", tags: &[0x777f5b11, 0xa0893109, 0xbd44243d, 0xcd93c503] }, // KobCondBidKron
    RetiredLayout { code: 0x84, state_len: 348, hash: "339a1bb18e291f1a94bd0faafa3ece05d0bdbd9ee84d370830df6fe8f97cff45", tags: &[0x46ade55d, 0x777f5b11, 0xa0893109, 0xbd44243d] }, // KobCondBidKron
    RetiredLayout { code: 0x85, state_len: 461, hash: "7ffc92a2774a18a1ad888769bfc161de0578e6bed1b95be671399702e2d816fb", tags: &[0x104383c5, 0x777f5b11, 0xa0893109] }, // KobIfdBidKron
    RetiredLayout { code: 0x85, state_len: 561, hash: "0784282f33c59af7eee628c90c63430639817f246faaa9aba80a53ed47e08703", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBidKron
    RetiredLayout { code: 0x85, state_len: 561, hash: "ac5df87b24050ac960d2301b3de457116904f65addab3631993fb9d709b5d436", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBidKron
    RetiredLayout { code: 0x85, state_len: 561, hash: "d16e03fff6199106e85790c7fcc962269f0f2019521507f720b063841b2429a6", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBidKron
    RetiredLayout { code: 0x85, state_len: 561, hash: "6af8728dab4e5daf75d57f05c2cc3bf5e1c9d94b6ee639046854ef02d6f9bc62", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBidKron
    RetiredLayout { code: 0x86, state_len: 471, hash: "1557a5e645f4de55e4414d61ee76bd9ec911d770de856395459237032ececf61", tags: &[0x6892042b, 0xa0893109] }, // KobIfdAskKron
    RetiredLayout { code: 0x86, state_len: 570, hash: "987a2d4081ef7bb44e7dbe0f7211d4f7dbdda63cd84493aab486803e7b069c12", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAskKron
    RetiredLayout { code: 0x86, state_len: 570, hash: "ca774fe1fc6f50751e18c74a2a15ddf35fe7532b29e837f6a3930899f8825a62", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAskKron
    RetiredLayout { code: 0x86, state_len: 570, hash: "de56abde8b6bfa99c2eb32589443907fb4bfd15fd9564756f991cb4badd7fe98", tags: &[0x2291ad1d, 0xa0893109, 0xa2a8ade5, 0xcd93c503] }, // KobIfdAskKron
    RetiredLayout { code: 0x86, state_len: 570, hash: "6d19a13d97d88187f48b3fac6a6cd6bf0f8d50f05f64c4b2069237fed90459ac", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAskKron
    RetiredLayout { code: 0x86, state_len: 570, hash: "3c73e5c4e0a4dd1278e7cbec7586ff7fe6eaa8fcad19af3a111e43b0f19bcbc8", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAskKron
    RetiredLayout { code: 0x87, state_len: 120, hash: "3cf6f404f985f00c543b6ef35f2aff585e088bbce836f6a0a90a5de1dd1e75bc", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceiptKron
    RetiredLayout { code: 0x87, state_len: 120, hash: "a6890844ecf33c1a3f0627d878a425e9ccb266100a8d35b4bcc6238c60196778", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceiptKron
    RetiredLayout { code: 0x87, state_len: 120, hash: "ba94ac803afa2c51ed43c14be38d160b968e9a4fd6ca68d0f69516061b1aeb97", tags: &[0x3670a508, 0x3b597c7e, 0x5dc68e50, 0xe84db6b0] }, // KobReceiptKron
    RetiredLayout { code: 0x88, state_len: 333, hash: "7371a59d8d5502089e7dbdff1d2f78b52028d58a70d2c037759895f384578f87", tags: &[0x1e6cf528, 0xa0893109] }, // KobCrossKron
    RetiredLayout { code: 0x88, state_len: 333, hash: "d1d8d49890ff9325523f616b810912e386639a355415b10bc62b70625d6196bc", tags: &[0x1e6cf528, 0xa0893109] }, // KobCrossKron
    RetiredLayout { code: 0x88, state_len: 351, hash: "87a01b1dcbb079d4bd9811789cdee6195b9e1ca596853dd8ece09a3359d5eca0", tags: &[0xa0893109, 0xbd44243d] }, // KobCrossKron
    // protocol v2.6, pinned from 2026-10-01 (cost pass 1; the if-done entries from 69c9014) to 36d569c, retired 2026-10-05 (v3)
    RetiredLayout { code: 0x01, state_len: 243, hash: "66853f1603149ff6371a06655ac58fa9f775ad80fce0141876c31dc41364c3c2", tags: &[0x26a2c631, 0xa0893109] }, // KobAsk
    RetiredLayout { code: 0x02, state_len: 285, hash: "208589746fefe2ea9082cef18b75b0e5a31f9e1b059421dc0caeef96b3d55728", tags: &[0x5f698675, 0x777f5b11, 0xa0893109] }, // KobBid
    RetiredLayout { code: 0x03, state_len: 330, hash: "fe74058698d86e6c22aae188441e853398f770a71c53f8a2b3d7229821e7d3ff", tags: &[0x46ade55d, 0x68a357bc, 0xa0893109] }, // KobCondAsk
    RetiredLayout { code: 0x04, state_len: 381, hash: "3a5f1e2227c115360a2ef4143f597d31c6e1e5e00f44244ab22851f5458680d3", tags: &[0x46ade55d, 0x777f5b11, 0xa0893109, 0xbd44243d] }, // KobCondBid
    RetiredLayout { code: 0x05, state_len: 594, hash: "ca757658e4872b64efb9a25c39aeefb68a2622dcc93b04426261377e2bb4d199", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBid
    RetiredLayout { code: 0x06, state_len: 603, hash: "138d4fd83941a072ceaaa3e1ce95f565f2a8ec21e529d43287245839b17a79e8", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x08, state_len: 351, hash: "692cfdd07dae71749db2d0ad8249eb70c7476e9ef67a4f8f4c09c409e991b752", tags: &[0xa0893109, 0xbd44243d] }, // KobCross
    RetiredLayout { code: 0x81, state_len: 243, hash: "3389eeb2e971cacbbbbda193582d38db5f1bf56acff55478d9afd4a4d956b7d7", tags: &[0x26a2c631, 0xa0893109] }, // KobAskKron
    RetiredLayout { code: 0x82, state_len: 252, hash: "a116125531ae2aaa1218a836c6891c11174faa15427da34672c7b11d19f0151e", tags: &[0x5f698675, 0x777f5b11, 0xa0893109] }, // KobBidKron
    RetiredLayout { code: 0x83, state_len: 330, hash: "219c53b28b33579f699fdb93cff1c7c3f2bba2ee4033a1a22cfde6980807343e", tags: &[0x46ade55d, 0x68a357bc, 0xa0893109] }, // KobCondAskKron
    RetiredLayout { code: 0x84, state_len: 348, hash: "0cc7eeb9b02f90f0fa031acb48826dba4b35ce614ea6e659ef4772ebe38f9cb4", tags: &[0x46ade55d, 0x777f5b11, 0xa0893109, 0xbd44243d] }, // KobCondBidKron
    RetiredLayout { code: 0x85, state_len: 561, hash: "354fb5de47e5f9141da32cb3d8f282323200d915b9bacbec126a370479a8c232", tags: &[0x777f5b11, 0xa0893109, 0xcd93c503, 0xd434627a] }, // KobIfdBidKron
    RetiredLayout { code: 0x86, state_len: 570, hash: "fd13762a9157ff89b996f38bd9641937ab983a7c7375537b356747a972043540", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAskKron
    RetiredLayout { code: 0x88, state_len: 351, hash: "ef254324cc270929a9b5b70116af1739983faa3dd4bb7278dc5b01044d631354", tags: &[0xa0893109, 0xbd44243d] }, // KobCrossKron
    // protocol v3 (no lots), committed by 8dd4ebf (2026-10-05) and replaced by the pair orders before any deployment
    RetiredLayout { code: 0x08, state_len: 360, hash: "ea23f1fec9719d56a31f4b54b15cc3e6a286af0384b7e7769f571dfd39a74b89", tags: &[0xa0893109, 0xb62164ca] }, // KobCross
    // protocol v3 sell-first entries whose refund did not require tokens held, retired 2026-10-06 (security review); today's
    // state layout under the same codes, told apart by template hash (format 2)
    RetiredLayout { code: 0x06, state_len: 594, hash: "189b9c3297cac33c0de6b5defefcbee5deee42306e8aa72b4615d8f2d4515a17", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAsk
    RetiredLayout { code: 0x86, state_len: 561, hash: "85d8783813f861d16d2679d13fd062d81ce572f7fe4efa111943bd5a6a6b4ac8", tags: &[0x2291ad1d, 0x46ade55d, 0x46d7fcb4, 0xa0893109] }, // KobIfdAskKron
];

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::artifacts::template;

    #[test]
    fn pinned_templates_are_not_retired() {
        for id in crate::model::ORDER_KINDS {
            assert!(retired_by_hash(&template(id).hash).is_none(), "{} is pinned and listed as retired", id.name());
        }
        // every row is well formed: a known kind code (or the retired receipt), a parsable hash, at least one tag
        for l in RETIRED_LAYOUTS {
            assert!(crate::model::from_wire_code(l.code).is_some() || crate::model::is_retired_wire_code(l.code), "{l:?}");
            assert_eq!(l.hash.len(), 64);
            let _ = l.hash_bytes();
            assert!(!l.tags.is_empty());
        }
    }

    #[test]
    fn the_cross_limit_before_the_pair_market_auction_is_listed() {
        // the layout that broke the replay of the soak's pre-958d013 log (TN10, 2026-10-01)
        let l = retired(0x08).find(|l| l.hash.starts_with("70b1dcd3")).expect("KobCross v2.6 before the auction");
        assert_eq!(l.state_len, 333);
        assert!(l.has_tag(&[0x1e, 0x6c, 0xf5, 0x28]) && l.has_tag(&[0xa0, 0x89, 0x31, 0x09]));
        // the v2.6 cross limit with the auction (351 bytes) is retired too; today's code 0x08 is KobPair (payload v4), longer
        let v26 = retired(0x08).find(|l| l.hash.starts_with("692cfdd0")).expect("KobCross v2.6");
        assert_eq!(v26.state_len, 351);
        assert_eq!(crate::model::from_wire_code(0x08), Some(kob_protocol::artifacts::TemplateId::KobPair));
        assert_eq!(template(kob_protocol::artifacts::TemplateId::KobPair).state_len, 414);
    }

    #[test]
    fn the_if_done_entries_retired_on_2026_10_02_are_listed() {
        for (code, hash) in [(0x05, "35ed6bed"), (0x06, "789ddaa1"), (0x85, "d16e03ff"), (0x86, "6d19a13d")] {
            let l = retired(code).find(|l| l.hash.starts_with(hash)).unwrap_or_else(|| panic!("{code:#x} {hash}"));
            let fam = if code & 0x80 != 0 { kob_protocol::family::Family::Kron } else { kob_protocol::family::Family::Kcc20 };
            let v26 = kob_protocol::retired::payload_layout(fam, code & 0x7f).expect("the v2.6 layout of the kind");
            assert_eq!(l.state_len, v26.template.state_len, "same state layout as the v2.6 template of the kind");
        }
    }

    /// The fourteen protocol v2.6 templates the v3 revision retired (both families, every kind) are listed under the code
    /// their record-log frames carry. The cross limits' codes 0x08 / 0x88 are today's `KobPair` (one template for both
    /// families): their old frames are told apart by template hash (format 2) or state length and dispatch tag (format 1).
    #[test]
    fn the_v2_6_templates_retired_by_v3_are_listed() {
        let all = kob_protocol::retired::retired();
        let v26 = &all[all.len() - 14..];
        for r in v26 {
            assert!(r.note.contains("v3 no-lot revision"), "{}", r.note);
            let l = retired_by_hash(&r.template.hash).unwrap_or_else(|| panic!("{}: no row", r.kind_name()));
            let kron = r.family == kob_protocol::family::Family::Kron;
            assert_eq!(l.code & 0x80 != 0, kron, "{}", r.kind_name());
            let today = if r.kind == kob_protocol::artifacts::TemplateId::KobCross {
                kob_protocol::artifacts::TemplateId::KobPair
            } else {
                r.kind.base()
            };
            assert_eq!(crate::model::from_wire_code(l.code).map(|t| t.base()), Some(today), "{}", r.kind_name());
        }
        let cross_kron = retired(0x88).find(|l| l.hash.starts_with("ef254324")).expect("KobCrossKron v2.6");
        assert_eq!(cross_kron.state_len, 351);
        assert_eq!(crate::model::from_wire_code(0x88), Some(kob_protocol::artifacts::TemplateId::KobPair));
    }

    #[test]
    fn every_spendable_retired_template_has_its_row() {
        for r in kob_protocol::retired::retired() {
            let l = retired_by_hash(&r.template.hash).unwrap_or_else(|| panic!("{} {}: no layout row", r.kind.name(), r.hash_hex()));
            // the code of the template's family (a KRON cross limit: 0x88, the code `KobCrossKron` had)
            let kron = if r.family == kob_protocol::family::Family::Kron { 0x80 } else { 0 };
            assert_eq!(l.code, crate::model::wire_code(r.kind) | kron, "{}", r.note);
            assert_eq!(l.state_len, r.template.state_len, "{}", r.note);
            let mut tags: Vec<u32> =
                r.template.entries().values().map(|t| u32::from_str_radix(t, 16).expect("4-byte tag hex")).collect();
            tags.sort_unstable();
            assert_eq!(l.tags, tags.as_slice(), "{}", r.note);
        }
    }
}
