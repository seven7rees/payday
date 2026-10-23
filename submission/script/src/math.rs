//! Pure functions for the pot split, the payout budget and the answers.
//!
//! Nothing here can panic, whatever the inputs: arithmetic goes through u128, and anything that would
//! not fit a u64 is capped instead of wrapping.

use anchor_lang::prelude::Pubkey;

use crate::params::STEP_BOUNTY;

/// Basis points in a whole.
const BPS_DENOMINATOR: u128 = 10_000;

/// `floor(x * b / 10_000)` for a u16 rate; capped at `u64::MAX`.
pub fn bps128(x: u64, b: u16) -> u64 {
    let scaled = u128::from(x) * u128::from(b) / BPS_DENOMINATOR;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// `floor(x * b / 10_000)` for a u64 rate; capped at `u64::MAX`.
pub fn bps64(x: u64, b: u64) -> u64 {
    let scaled = u128::from(x) * u128::from(b) / BPS_DENOMINATOR;
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// The first second of the payout window that `now` falls in:
/// `floor(now / max(window_secs, 1)) * max(window_secs, 1)`. `div_euclid` keeps a negative clock
/// on the right side of the boundary.
pub fn window_start_now(now: i64, window_secs: u32) -> i64 {
    let secs = i64::from(window_secs.max(1));
    now.div_euclid(secs).saturating_mul(secs)
}

/// The pot a window's share is taken of: the pot after this step's spending plus what the window has
/// already paid, and never less than the pot at the window's first step.
pub fn window_base(pot_after: u64, w_paid: u64, w_pot: u64) -> u64 {
    pot_after.saturating_add(w_paid).max(w_pot)
}

/// The most this step's payout answer may add up to. The companion computes the same number and
/// refuses a whole step whose answer is above it, so this must match its rule exactly:
///
/// ```text
/// budget = min(
///     bps(pot_before,  step_bps),
///     bps(window_base, window_bps) - w_paid   (floor at 0),
///     pot_after - bounty                      (floor at 0),
/// )
/// ```
///
/// `bounty` is an upper bound on what the companion pays the step's sender, so the result is never
/// above the companion's own budget. `w_pot` and `w_paid` are the window counters after the window
/// roll (a new window starts at the pot after this step, with nothing paid).
pub fn step_budget(
    pot_before: u64,
    pot_after: u64,
    bounty: u64,
    w_pot: u64,
    w_paid: u64,
    step_bps: u16,
    window_bps: u16,
) -> u64 {
    let base = window_base(pot_after, w_paid, w_pot);
    let by_step = bps128(pot_before, step_bps);
    let by_window = bps128(base, window_bps).saturating_sub(w_paid);
    let by_pot = pot_after.saturating_sub(bounty);
    by_step.min(by_window).min(by_pot)
}

/// An answer of this many entries earns the whole bounty (`PAYOUT_BOUNTY_FULL`).
pub const COMPANION_PAYOUT_BOUNTY_FULL: u64 = 8;

/// What the companion pays a step's sender out of the pot once the script has run: the flat
/// `STEP_BOUNTY` (20_000) when the step answers nothing, `STEP_BOUNTY * min(n, 8) / 8` for an
/// answer of `n` entries, and nothing at all when the pot after the script does not cover it. The
/// script needs the exact figure to remember the pot as the companion leaves it.
pub fn step_bounty(pot_after_script: u64, answer_entries: Option<u64>) -> u64 {
    let wanted = match answer_entries {
        None => STEP_BOUNTY,
        Some(n) => STEP_BOUNTY * n.min(COMPANION_PAYOUT_BOUNTY_FULL) / COMPANION_PAYOUT_BOUNTY_FULL,
    };
    if pot_after_script >= wanted {
        wanted
    } else {
        0
    }
}

/// The pot as the companion leaves it after a step that pays `paid` in `entries` answer entries
/// (`None`: no answer): the pot, less what the step paid, less the sender's bounty.
pub fn pot_after_step(pot: u64, paid: u64, entries: Option<u64>) -> u64 {
    let bounty = step_bounty(pot, entries);
    pot.saturating_sub(paid).saturating_sub(bounty)
}

/// The two piles the pot is kept in. Their sum is always the pot as the companion left it after
/// the last step (`ScriptState.last_pot`): `owed` to the builder, `holders` for the weekly
/// payouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Piles {
    pub owed: u64,
    pub holders: u64,
}

impl Piles {
    /// The two added up (saturating).
    pub fn total(&self) -> u64 {
        self.owed.saturating_add(self.holders)
    }

    /// `new_money` split: `builder_bps` of it to the builder, ALL the rest to the holder pile.
    pub fn split(&mut self, new_money: u64, builder_bps: u16) {
        let builder = bps128(new_money, builder_bps);
        self.owed = self.owed.saturating_add(builder);
        self.holders = self.holders.saturating_add(new_money.saturating_sub(builder));
    }

    /// `amount` taken from the piles, the holder pile first, then the builder's (each as far as
    /// it goes): what the sender's bounty costs, or a pot shortfall. Returns what could not be
    /// taken.
    pub fn take(&mut self, amount: u64) -> u64 {
        let mut left = amount;
        for pile in [&mut self.holders, &mut self.owed] {
            let t = (*pile).min(left);
            *pile -= t;
            left -= t;
        }
        left
    }

    /// The piles made to add up to `pot` exactly: a surplus (rounding dust) goes to the holder
    /// pile, a shortfall comes out in `take`'s order. After this `total() == pot`.
    pub fn reconcile(&mut self, pot: u64) {
        let total = self.total();
        if pot > total {
            self.holders = self.holders.saturating_add(pot - total);
        } else if total > pot {
            self.take(total - pot);
        }
    }
}

/// A payout answer naming holdings, as return data: `version u8 = 1 | entries u32 LE | each entry
/// account u8, lamports u64 LE, tag u32 LE` (1 + 4 + 13 per entry).
pub fn encode_payout_answer(entries: &[(u8, u64, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + 13 * entries.len());
    out.push(1u8);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (account, lamports, tag) in entries {
        out.push(*account);
        out.extend_from_slice(&lamports.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
    }
    out
}

/// A wallet payout answer with one entry, as return data:
/// `version u8 = 2 | entries u32 LE = 1 | to Pubkey | lamports u64 LE | tag u32 LE` (49 bytes).
pub fn encode_wallet_answer(to: &Pubkey, lamports: u64, tag: u32) -> [u8; 49] {
    let mut buf = [0u8; 49];
    buf[0] = 2;
    buf[1..5].copy_from_slice(&1u32.to_le_bytes());
    buf[5..37].copy_from_slice(to.as_ref());
    buf[37..45].copy_from_slice(&lamports.to_le_bytes());
    buf[45..49].copy_from_slice(&tag.to_le_bytes());
    buf
}

/// `whole * part / of`, rounded down, in u128; 0 when `of` is 0; capped at `u64::MAX`.
pub fn share(whole: u64, part: u128, of: u128) -> u64 {
    if of == 0 {
        return 0;
    }
    let s = u128::from(whole).saturating_mul(part) / of;
    u64::try_from(s).unwrap_or(u64::MAX)
}
