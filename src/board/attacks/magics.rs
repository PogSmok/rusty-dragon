//! Magic numbers for slider lookups on CPUs without fast PEXT.
//!
//! Regenerate with `cargo test --release find_magics -- --ignored --nocapture`

#[expect(clippy::unreadable_literal, reason = "random magic numbers have no structure to group")]
pub(super) const ROOK_MAGICS: [u64; 64] = [
    0x2080002080400010,
    0x00c0002001401000,
    0x2100110008402002,
    0x0880080081041000,
    0x0200020020041008,
    0x2300040008010012,
    0x0c00283004008201,
    0x0180010000407a80,
    0x0168800080400020,
    0x0010400040201000,
    0x1001002001001048,
    0x1001002408100100,
    0x0801000408010012,
    0x4001000209000400,
    0x08a20004c8020001,
    0x2002801145002280,
    0x0080860021004200,
    0x001000c009402002,
    0x00b0002004002800,
    0x100a808010020800,
    0x8101010008000410,
    0x0244008002000480,
    0x0000040010810208,
    0x2000020000448534,
    0x4104400480008033,
    0x0000810100204000,
    0x0440430900200010,
    0x4600240900100100,
    0x0060080080040080,
    0x0001000300080400,
    0x0004084400011002,
    0x0023040200008041,
    0x0580050043002080,
    0x0400804002802008,
    0x0001002001004010,
    0x1000200901001000,
    0x4410800801800c00,
    0xa012003806001004,
    0x0020100104008802,
    0x0004808402000041,
    0x0010400170898000,
    0x0080500020004004,
    0x1040408012020020,
    0x8010040008004040,
    0x2001080100110004,
    0x0000020004008080,
    0x0021010810040002,
    0x0800008c43020024,
    0x0000800021005100,
    0x0070201040008080,
    0x0000d04282006a00,
    0x0010014400080240,
    0x0001080110050100,
    0x0012000810240600,
    0x0402000801040200,
    0x028100108a004100,
    0x0050800300102045,
    0x8208210040120882,
    0x8010600101183441,
    0x020b000910006045,
    0x0241001002480005,
    0x0081000400880241,
    0x0000009008024124,
    0x0048122980410402,
];
#[expect(clippy::unreadable_literal, reason = "random magic numbers have no structure to group")]
pub(super) const BISHOP_MAGICS: [u64; 64] = [
    0x0848020822040013,
    0x8010a40085821200,
    0x0008008430840822,
    0x0808048108040000,
    0x1304042100008104,
    0x5001012010204023,
    0x81048801b8200420,
    0x200a008084012000,
    0x0040102001042084,
    0x840a505042428020,
    0x0000700102202920,
    0x44101c0c10800002,
    0x0040040422000000,
    0x0180020802090202,
    0x4020020811041202,
    0x000104308c042000,
    0x4140661002424400,
    0x0028012008010460,
    0x0188062102002a00,
    0x0014004840102008,
    0x0105000290400002,
    0x8001022200410400,
    0x104a041918013446,
    0x008a000082008238,
    0x04a0060008100430,
    0x0008220008820801,
    0x2508041208005010,
    0x4008080200202020,
    0x2441001013004000,
    0x0030008060407000,
    0x4008108000420800,
    0x0012021050290100,
    0x0210080482200500,
    0xcc01112048100480,
    0x0020402806500440,
    0x00048e0080580080,
    0x0040102020020080,
    0x0028010440080807,
    0x4601041108008800,
    0x8040810e04104200,
    0x901210110400088a,
    0xa003080212081050,
    0x00c1004048401004,
    0x900000a014400800,
    0x0008021040405401,
    0x4020008206002090,
    0x0004190424030100,
    0x0424008a02026250,
    0x8004088250900040,
    0x1c00430088a04200,
    0x0001020094040001,
    0x8040210020880061,
    0x2010040450442032,
    0x0800840850044001,
    0x0004040802140004,
    0x0004080a04222020,
    0x8088802110022000,
    0x1081a10416114400,
    0x0205010a24060820,
    0x0000000720411080,
    0x1008000208430400,
    0x580c026028810840,
    0x802020441020a110,
    0x12c0022401020018,
];

#[cfg(test)]
mod tests {
    use super::super::rays::{self, BISHOP_DIRECTIONS, Direction, ROOK_DIRECTIONS};
    use super::super::xorshift::Xorshift64;
    use super::{BISHOP_MAGICS, ROOK_MAGICS};
    use crate::board::{Bitboard, Square};

    /// Whether magic can be used to hash all positions of blockers
    fn is_valid_magic(
        square: Square,
        directions: [Direction; 4],
        magic: u64,
        slots: &mut [Bitboard],
    ) -> bool {
        let mask = rays::relevant_mask(square, directions);
        let shift = 64 - mask.popcount();
        slots.fill(Bitboard::EMPTY);

        let mut subset = Bitboard::EMPTY;
        loop {
            #[expect(clippy::cast_possible_truncation, reason = "index < 2^12")]
            let index = (subset.0.wrapping_mul(magic) >> shift) as usize;
            let attacks = rays::sliding_attacks(square, subset, directions);
            if !slots[index].is_empty() && slots[index] != attacks {
                return false;
            }
            slots[index] = attacks;

            subset = rays::next_subset(subset, mask);
            // After iterating through all subsets, `next_subset` wraps around to empty
            if subset.is_empty() {
                return true;
            }
        }
    }

    /// An empty slot for each blocker subset of the relevant mask: 2^(mask popcount) slots.
    fn slots_for(square: Square, directions: [Direction; 4]) -> Vec<Bitboard> {
        vec![Bitboard::EMPTY; 1 << rays::relevant_mask(square, directions).popcount()]
    }

    /// Tries sparse random candidates until one is a valid magic for `square`.
    fn find_magic(square: Square, directions: [Direction; 4], rng: &mut Xorshift64) -> u64 {
        let mask = rays::relevant_mask(square, directions);
        let mut slots = slots_for(square, directions);
        loop {
            // Sparse candidates are more likely to be valid magic numbers
            let magic = rng.sparse();
            // Cheap filter: a good magic spreads the mask's bits into the top byte.
            if (mask.0.wrapping_mul(magic) >> 56).count_ones() < 6 {
                continue;
            }

            if is_valid_magic(square, directions, magic, &mut slots) {
                return magic;
            }
        }
    }

    #[test]
    fn magics_are_valid() {
        for (magics, directions) in
            [(&ROOK_MAGICS, ROOK_DIRECTIONS), (&BISHOP_MAGICS, BISHOP_DIRECTIONS)]
        {
            for (square, &magic) in Square::ALL.into_iter().zip(magics) {
                let mut slots = slots_for(square, directions);
                assert!(
                    is_valid_magic(square, directions, magic, &mut slots),
                    "bad magic for square {square}"
                );
            }
        }
    }

    /// Generates the magic tables and prints them as Rust source,
    /// ready to paste over the constants at the top of this file.
    #[test]
    #[ignore = "generator, run by hand to regenerate the magic tables"]
    fn find_magics() {
        // Changing the seed changes the generated tables.
        let mut rng = Xorshift64::new(Xorshift64::DEFAULT_SEED);
        for (name, directions) in
            [("ROOK_MAGICS", ROOK_DIRECTIONS), ("BISHOP_MAGICS", BISHOP_DIRECTIONS)]
        {
            println!(
                "#[expect(clippy::unreadable_literal, \
                reason = \"random magic numbers have no structure to group\")]"
            );
            println!("pub(super) const {name}: [u64; 64] = [");
            for square in Square::ALL {
                println!("    {:#018x},", find_magic(square, directions, &mut rng));
            }
            println!("];");
        }
    }
}
