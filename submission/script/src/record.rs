//! A holding's record as the Payday hook writes it: the hook data layout, byte for byte
//! (`hook_variants/payday/src/clock.rs`), and the weight a holding earns from it.
//!
//! # Hook data layout (64 bytes per holding, little-endian)
//! offset  0 : version     u8   1; 0 means no record
//! offset  1 : round       u32  the payout round this holding was last measured or paid in
//! offset  5 : stamp       i64  the hold clock: the weighted-average entry time (unix seconds)
//! offset 13 : amount      u64  the balance as of the last write
//! offset 21 : paid        u64  lamports paid to this holding, all time
//! offset 29 : counted     u64  the balance the last measurement counted: the lowest balance held
//!                              in that round so far (`low_of`)
//! offset 37 : flags       u8   bit 0: sold into the pool since the holding was last funded
//! offset 38 : measured    u64  the weight the last measurement gave (`measured_weight`), locked:
//!                              the round after pays on it (`pay_weight`), never recomputed from
//!                              the clock
//! offset 46 : low         u64  the lowest balance the holding held during round `low_round`
//! offset 54 : prev_low    u64  the lowest balance it held during the round before `low_round`
//! offset 62 : low_round   u16  the round `low` belongs to, its low 16 bits (0: none tracked)

use crate::params::{MIN_HOLDING, MIN_HOLD_SECS, NEVER_SOLD_BONUS_BPS, ROUND_SECS, WEIGHT_TIERS};

/// Hook data per holding, in bytes.
pub const HOOK_DATA_LEN: usize = 64;
/// Layout version of a record; 0 is no record.
pub const RECORD_VERSION: u8 = 1;
/// `flags` bit 0: sold into the pool since the holding was funded.
pub const FLAG_SOLD: u8 = 1;
/// Basis points in a whole.
const BPS: u128 = 10_000;

/// Where each field sits.
pub mod record_offsets {
    pub const VERSION: usize = 0;
    pub const ROUND: usize = 1;
    pub const STAMP: usize = 5;
    pub const AMOUNT: usize = 13;
    pub const PAID: usize = 21;
    pub const COUNTED: usize = 29;
    pub const FLAGS: usize = 37;
    pub const MEASURED: usize = 38;
    pub const LOW: usize = 46;
    pub const PREV_LOW: usize = 54;
    pub const LOW_ROUND: usize = 62;
}

/// A holding's record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub version: u8,
    pub round: u32,
    pub stamp: i64,
    pub amount: u64,
    pub paid: u64,
    pub counted: u64,
    pub flags: u8,
    pub measured: u64,
    pub low: u64,
    pub prev_low: u64,
    pub low_round: u16,
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&data[at..at + 4]);
    u32::from_le_bytes(b)
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[at..at + 8]);
    u64::from_le_bytes(b)
}

impl Record {
    /// Reads hook data. All zeros is no record.
    pub fn decode(data: &[u8; HOOK_DATA_LEN]) -> Self {
        use record_offsets as o;
        Self {
            version: data[o::VERSION],
            round: u32_at(data, o::ROUND),
            stamp: u64_at(data, o::STAMP) as i64,
            amount: u64_at(data, o::AMOUNT),
            paid: u64_at(data, o::PAID),
            counted: u64_at(data, o::COUNTED),
            flags: data[o::FLAGS],
            measured: u64_at(data, o::MEASURED),
            low: u64_at(data, o::LOW),
            prev_low: u64_at(data, o::PREV_LOW),
            low_round: u16::from_le_bytes([data[o::LOW_ROUND], data[o::LOW_ROUND + 1]]),
        }
    }

    /// Whether the holding has a record.
    pub fn is_set(&self) -> bool {
        self.version == RECORD_VERSION
    }

    /// Whether the holding sold into the pool since it was funded.
    pub fn sold(&self) -> bool {
        self.flags & FLAG_SOLD != 0
    }
}

/// A round as the record keeps it: its low 16 bits.
pub fn round_tag(round: u32) -> u16 {
    (round & 0xFFFF) as u16
}

/// The lowest balance a holding of `amount` held during `round`, which is the current round or
/// the one before it: the tracked low when its balance changed in that round (for the round
/// before, the low the record kept when the current round began, whatever the holding did since),
/// else `amount` (unchanged since before that round began). The hook records `counted` by this
/// rule and `measured` by `measured_weight`.
pub fn low_of(r: &Record, amount: u64, round: u32) -> u64 {
    let tag = round_tag(round);
    if r.low_round == tag {
        amount.min(r.low)
    } else if r.low_round == tag.wrapping_add(1) {
        r.prev_low
    } else {
        amount
    }
}

/// When `round` begins: `round * ROUND_SECS` (saturating).
pub fn round_start(round: u32) -> i64 {
    i64::from(round).saturating_mul(i64::from(ROUND_SECS))
}

/// The weight a measurement in `round` gives a holding of `amount`: its lowest balance this round
/// (`low_of`) at its clock as of the round's first second; 0 under `MIN_HOLDING`. The hook records
/// this same number as `measured` (`record_payout`), and the script adds it to the round's total.
pub fn measured_weight(r: &Record, amount: u64, round: u32) -> u64 {
    let counted = low_of(r, amount, round);
    if counted < MIN_HOLDING {
        return 0;
    }
    u64::try_from(weight(r, counted, round_start(round))).unwrap_or(u64::MAX)
}

/// The weight a holding measured in round `measured_round` is paid on the round after: the weight
/// locked at measurement (`measured`), scaled down when its lowest balance over the whole of that
/// round ended under the balance counted (a sell later in that round); 0 when that lowest balance
/// is under `MIN_HOLDING`. Never above `measured`, so the weights paid in a round never add up past
/// the total measured the round before. A later change of the clock (a top-up) does not move it.
pub fn pay_weight(r: &Record, amount: u64, measured_round: u32) -> u64 {
    if !r.is_set() || r.measured == 0 || r.counted == 0 {
        return 0;
    }
    let low = low_of(r, amount, measured_round);
    if low < MIN_HOLDING {
        return 0;
    }
    if low >= r.counted {
        return r.measured;
    }
    let scaled = u128::from(r.measured) * u128::from(low) / u128::from(r.counted);
    u64::try_from(scaled).unwrap_or(r.measured).min(r.measured)
}

/// The hold-time multiplier of a holding whose clock reads `held` seconds, in basis points of its
/// balance: 0 before `MIN_HOLD_SECS`, then the highest tier reached. Never panics.
pub fn tier_bps(held: i64) -> u16 {
    if held < MIN_HOLD_SECS {
        return 0;
    }
    let mut bps = 0;
    for t in WEIGHT_TIERS.iter() {
        if held >= t.from_secs {
            bps = t.bps;
        }
    }
    bps
}

/// A holding's weight as of `at` (a round's first second): `balance` times its hold-time multiplier
/// at that time, plus the never-sold bonus. 0 for a holding under the least hold then, or with no
/// record.
pub fn weight(r: &Record, balance: u64, at: i64) -> u128 {
    if !r.is_set() {
        return 0;
    }
    let held = at.saturating_sub(r.stamp);
    let tier = u128::from(tier_bps(held));
    if tier == 0 {
        return 0;
    }
    let bonus = if r.sold() { 0 } else { u128::from(NEVER_SOLD_BONUS_BPS) };
    u128::from(balance).saturating_mul(tier) / BPS * (BPS + bonus) / BPS
}
