//! The pure arithmetic of the Payday hook: each holding's record (its 64 bytes of hook data),
//! the hold clock, the sell brake and the sell relay. Nothing here reads an account, and nothing
//! here can panic on any input: every sum is u128 or saturating.
//!
//! # Hook data layout (64 bytes per holding, little-endian; the companion script reads the same)
//! offset  0 : version     u8   1; 0 means no record (never written, or cleared when emptied)
//! offset  1 : round       u32  the weekly payout round this holding was last measured or paid in
//!                              (`record_payout`'s tag); 0 before the first. The keeper brings a
//!                              holding whose round is not the current epoch.
//! offset  5 : stamp       i64  the hold clock: the weighted-average entry time (unix seconds)
//! offset 13 : amount      u64  the balance as of the last write
//! offset 21 : paid        u64  lamports paid to this holding by the companion, all time
//! offset 29 : counted     u64  the balance the last measurement counted: the lowest balance held
//!                              in that round so far (`low_of`), written by `record_payout`
//! offset 37 : flags       u8   bit 0: sold into the pool since the holding was last funded
//! offset 38 : measured    u64  the weight the last measurement gave (`measured_weight`): the
//!                              counted balance times the hold-time tier at that round's first
//!                              second, plus the never-sold bonus; 0 under `MIN_HOLDING`. The
//!                              script pays the round after on it, scaled down by the round's
//!                              final lowest balance (`pay_weight`), never recomputed from the
//!                              clock
//! offset 46 : low         u64  the lowest balance this holding held during round `low_round`
//! offset 54 : prev_low    u64  the lowest balance it held during the round before `low_round`
//! offset 62 : low_round   u16  the round `low` belongs to, its low 16 bits (0: none tracked)

use bordrless_hook::HOOK_DATA_LEN;

/// Layout version of a holding's [`Record`]; 0 is no record.
pub const RECORD_VERSION: u8 = 1;
/// Basis points in a whole.
pub const BPS: u64 = 10_000;
/// The sell brake lasts this long after the hold clock: 10 minutes.
pub const BRAKE_SECS: i64 = 600;
/// The brake's SOL cut on a sell inside the window, in basis points of the SOL out: 80. The
/// launchpad adds a quarter on top, so the seller pays about 1% (`hook.json` caps sells at 80).
pub const BRAKE_BPS: u16 = 80;
/// The SOL cut after the window, and on every buy: none.
pub const FREE_BPS: u16 = 0;
/// `Record.flags` bit 0: the holding sold into the pool since it was last funded.
pub const FLAG_SOLD: u8 = 1;
/// A payout round: a week, numbered `floor(now / ROUND_SECS)`, the same as the companion script's
/// and the keeper's epoch.
pub const ROUND_SECS: u32 = 604_800;
/// The least balance a holding is counted at: 1,000 coins (6 decimals). The same number as the
/// companion script's.
pub const MIN_HOLDING: u64 = 1_000_000_000;
/// A holding earns nothing before its hold clock reaches this: 6 hours.
pub const MIN_HOLD_SECS: i64 = 21_600;
/// One hold-time tier: from this many seconds held, this multiplier in basis points.
pub struct WeightTier {
    pub from_secs: i64,
    pub bps: u16,
}
/// Hold-time weights: 1x from 6 hours, 1.5x from 7 days, 2x from 30 days. Rising order. The same
/// table as the companion script's.
pub const WEIGHT_TIERS: [WeightTier; 3] = [
    WeightTier { from_secs: 21_600, bps: 10_000 },
    WeightTier { from_secs: 604_800, bps: 15_000 },
    WeightTier { from_secs: 2_592_000, bps: 20_000 },
];
/// A holding that has not sold into the pool since it was funded weighs this much more, in basis
/// points: 10%.
pub const NEVER_SOLD_BONUS_BPS: u16 = 1_000;

/// Where each field sits in a holding's 64 bytes of hook data.
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

/// A holding's record, as the hook data lays it out.
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

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[at..at + 8]);
    u64::from_le_bytes(b)
}

impl Record {
    /// Reads hook data. All zeros (never written, or cleared) is no record.
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
            low_round: u16_at(data, o::LOW_ROUND),
        }
    }

    /// The hook data to write (the reserved bytes zero).
    pub fn encode(&self) -> [u8; HOOK_DATA_LEN] {
        use record_offsets as o;
        let mut out = [0u8; HOOK_DATA_LEN];
        out[o::VERSION] = self.version;
        out[o::ROUND..o::ROUND + 4].copy_from_slice(&self.round.to_le_bytes());
        out[o::STAMP..o::STAMP + 8].copy_from_slice(&self.stamp.to_le_bytes());
        out[o::AMOUNT..o::AMOUNT + 8].copy_from_slice(&self.amount.to_le_bytes());
        out[o::PAID..o::PAID + 8].copy_from_slice(&self.paid.to_le_bytes());
        out[o::COUNTED..o::COUNTED + 8].copy_from_slice(&self.counted.to_le_bytes());
        out[o::FLAGS] = self.flags;
        out[o::MEASURED..o::MEASURED + 8].copy_from_slice(&self.measured.to_le_bytes());
        out[o::LOW..o::LOW + 8].copy_from_slice(&self.low.to_le_bytes());
        out[o::PREV_LOW..o::PREV_LOW + 8].copy_from_slice(&self.prev_low.to_le_bytes());
        out[o::LOW_ROUND..o::LOW_ROUND + 2].copy_from_slice(&self.low_round.to_le_bytes());
        out
    }

    /// Whether the holding has a record (version 1).
    pub fn is_set(&self) -> bool {
        self.version == RECORD_VERSION
    }

    /// The hold clock, when the record is set.
    pub fn stamp_opt(&self) -> Option<i64> {
        self.is_set().then_some(self.stamp)
    }

    /// Whether the holding sold into the pool since it was funded.
    pub fn sold(&self) -> bool {
        self.flags & FLAG_SOLD != 0
    }
}

/// The round `now` falls in: `floor(now / ROUND_SECS)`, 0 before 1970; never panics.
pub fn round_of(now: i64) -> u32 {
    if now <= 0 {
        return 0;
    }
    u32::try_from((now as u64) / u64::from(ROUND_SECS)).unwrap_or(u32::MAX)
}

/// A round as the record keeps it: its low 16 bits.
pub fn round_tag(round: u32) -> u16 {
    (round & 0xFFFF) as u16
}

/// The lowest balance a holding of `amount` held during `round`, which is the current round or
/// the one before it: the tracked low when its balance changed in that round (for the round
/// before, the low the record kept when the current round began, whatever the holding did since),
/// else `amount` (unchanged since before that round began). Coins that arrive during a round never
/// count in it.
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

/// The hold-time multiplier of a clock that reads `held` seconds, in basis points: 0 before
/// `MIN_HOLD_SECS`, then the highest tier reached.
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

/// `balance` times the hold-time multiplier of the record's clock at `at`, plus the never-sold
/// bonus. 0 with no record or under the least hold.
pub fn weight(r: &Record, balance: u64, at: i64) -> u128 {
    if !r.is_set() {
        return 0;
    }
    let tier = u128::from(tier_bps(at.saturating_sub(r.stamp)));
    if tier == 0 {
        return 0;
    }
    let bonus = if r.sold() { 0 } else { u128::from(NEVER_SOLD_BONUS_BPS) };
    let bps = u128::from(BPS);
    u128::from(balance).saturating_mul(tier) / bps * (bps + bonus) / bps
}

/// The weight a measurement in `round` gives a holding of `amount`: its lowest balance this round
/// (`low_of`) at its clock as of the round's first second; 0 under `MIN_HOLDING`. This is what
/// `record_payout` stores as `measured`; the script adds the same number to the round's total.
pub fn measured_weight(r: &Record, amount: u64, round: u32) -> u64 {
    let counted = low_of(r, amount, round);
    if counted < MIN_HOLDING {
        return 0;
    }
    u64::try_from(weight(r, counted, round_start(round))).unwrap_or(u64::MAX)
}

/// The weight a holding measured in round `measured_round` is paid on the round after: the weight
/// locked at measurement (`measured`), scaled down when the holding's lowest balance over the
/// whole of that round ended under the balance counted (a sell later in that round); 0 when that
/// lowest balance is under `MIN_HOLDING`. Never above `measured`. A later change of the clock (a
/// top-up) does not move it.
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

/// The tracker after a change from `before` to `after` in `round`. The first change in a round
/// keeps the round before's lowest balance (its tracked low when it changed then, else the
/// balance it held all through it) and starts this round's at the balance held before; every
/// change keeps the minimum.
fn note_balance(r: &Record, before: u64, after: u64, round: u32) -> (u64, u64, u16) {
    let tag = round_tag(round);
    let (low, prev_low) = if r.low_round == tag {
        (r.low, r.prev_low)
    } else if r.low_round == tag.wrapping_sub(1) {
        (before, r.low)
    } else {
        (before, before)
    };
    (low.min(after), prev_low, tag)
}

/// A time as the blend counts it: never before 1970.
fn secs(t: i64) -> u128 {
    u128::from(u64::try_from(t).unwrap_or(0))
}

/// The hold clock after `amount` arrives at `now` into a holding of `old_balance` whose clock is
/// `old_stamp`: the entry times averaged by amount, so a top-up moves the clock toward now in
/// proportion to what arrives. An empty holding starts at `now`. Never after `now`.
pub fn blended_stamp(old_stamp: i64, old_balance: u64, amount: u64, now: i64) -> i64 {
    if old_balance == 0 {
        return now;
    }
    let total = u128::from(old_balance) + u128::from(amount);
    let weighted = secs(old_stamp)
        .saturating_mul(u128::from(old_balance))
        .saturating_add(secs(now).saturating_mul(u128::from(amount)));
    let stamp = i64::try_from(weighted / total).unwrap_or(i64::MAX);
    stamp.min(now)
}

/// A receive of `amount` at `now` into a holding of `old_balance`: a fresh holding (empty, or
/// with no record) starts its clock, its round, its paid total, its flags and its measurement from
/// zero; a funded
/// one blends its clock and keeps the rest. The lowest balance of the round is kept either way.
pub fn on_receive(r: &mut Record, amount: u64, old_balance: u64, now: i64) {
    let fresh = !r.is_set() || old_balance == 0;
    let new_balance = old_balance.saturating_add(amount);
    let (low, prev_low, low_round) = note_balance(r, old_balance, new_balance, round_of(now));
    let stamp = if fresh {
        now
    } else {
        blended_stamp(r.stamp, old_balance, amount, now)
    };
    *r = Record {
        version: RECORD_VERSION,
        round: if fresh { 0 } else { r.round },
        stamp,
        amount: new_balance,
        paid: if fresh { 0 } else { r.paid },
        counted: if fresh { 0 } else { r.counted },
        flags: if fresh { 0 } else { r.flags },
        measured: if fresh { 0 } else { r.measured },
        low,
        prev_low,
        low_round,
    };
}

/// A send or a burn from a holding of `before` tokens that leaves it `left`: the clock stays, the
/// amount is cut, the round's lowest balance kept, and a sell into the pool sets the sold flag.
/// Nothing left: the record is cleared, so the next arrival starts a fresh clock (and counts from
/// the next round) and the holding can close.
pub fn on_send(r: &mut Record, before: u64, left: u64, sold: bool, now: i64) {
    if left == 0 {
        *r = Record::default();
        return;
    }
    let (low, prev_low, low_round) = note_balance(r, before, left, round_of(now));
    if !r.is_set() {
        // Tokens with no record: they count as just arrived.
        *r = Record {
            version: RECORD_VERSION,
            stamp: now,
            ..Record::default()
        };
    }
    r.amount = left;
    r.low = low;
    r.prev_low = prev_low;
    r.low_round = low_round;
    if sold {
        r.flags |= FLAG_SOLD;
    }
}

/// The SOL cut on a sell: `BRAKE_BPS` while the seller's hold clock is under `BRAKE_SECS` (or
/// unknown: a holding with no record counts as just arrived), else none.
pub fn brake_bps(now: i64, stamp: Option<i64>) -> u16 {
    match stamp {
        Some(s) if now.saturating_sub(s) >= BRAKE_SECS => FREE_BPS,
        _ => BRAKE_BPS,
    }
}

/// The hold clock a sell is judged by. The token transfer of a sell runs `before_transfer` before
/// the launchpad asks `quote_cut`, in the same transaction, and that callback sees the holding the
/// tokens really leave; it records that holding's clock and the slot in the hook's state (the
/// relay). `quote_cut` only sees the signer's holding, which a delegate could pick old, so it takes
/// the relay when it was written in this slot and the signer's clock otherwise.
pub fn seller_stamp(
    relay_slot: u64,
    relay_stamp: i64,
    slot: u64,
    actor_stamp: Option<i64>,
) -> Option<i64> {
    if relay_slot != 0 && relay_slot == slot {
        Some(relay_stamp)
    } else {
        actor_stamp
    }
}
