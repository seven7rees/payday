//! The weekly holder payouts: rounds, what one holding is due, and the touch rule.
//!
//! A round is a week (`ROUND_SECS`), numbered `floor(now / ROUND_SECS)` like the keeper's epoch.
//! Every holding the keeper brings is measured once a round: its weight joins the round's total
//! `w_cur`, and the hook records the round in its hook data so it is not brought again until the
//! next one. The weight counts only coins held through the whole round: the lowest balance the
//! holding held during it (`record::low_of`), at its hold clock as of the round's first
//! second (`record::weight`; a clock started after the round began counts 0). Coins that arrive,
//! or move to a fresh wallet, during a round count from the next one, so moving coins cannot earn
//! twice in a week and a top-up earns nothing until it has been held a full round. A holding
//! measured is paid at once from the round's budget (the holder pile at the round's first step,
//! less a reserve for the step bounties) by its share of the total measured the round before:
//! `budget * w / w_prev`, with `w` the weight locked when it was measured the round before
//! (`record::pay_weight`), scaled down if its balance went lower later in that round, never over
//! the weight added to that round's total. A later top-up or clock change does not move it. The
//! first round measures only.
//! What a holding earns this round but the step cannot pay now (the step's budget or the pile is
//! used up) is not touched: the holding is skipped and the keeper brings it again next step
//! ([`Decision::Skip`]). Weight measured beyond the round before's total is paid up to what the
//! round's budget has left, first come first served, and a budget not fully paid stays in the
//! pile.

use crate::math::share;
use crate::params::{MIN_PAYOUT_LAMPORTS, ROUND_BOUNTY_RESERVE};

/// The round `now` falls in: `floor(now / round_secs)`, 0 before 1970; never panics.
pub fn round_of(now: i64, round_secs: u32) -> u32 {
    if round_secs == 0 || now <= 0 {
        return 0;
    }
    u32::try_from((now as u64) / u64::from(round_secs)).unwrap_or(u32::MAX)
}

/// When `round` begins: `round * round_secs` (saturating).
pub fn round_start(round: u32, round_secs: u32) -> i64 {
    i64::from(round).saturating_mul(i64::from(round_secs))
}

/// The round's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Round {
    pub round: u32,
    /// The total weight measured the round before (0: this round measures only).
    pub w_prev: u128,
    /// The weight measured so far this round.
    pub w_cur: u128,
    /// The round's budget: the holder pile at its first step.
    pub budget: u64,
    /// Paid so far this round.
    pub paid: u64,
    /// Holdings measured this round.
    pub measured: u32,
}

/// The counters rolled to round `now_round` when it is later than the current one: the weight
/// measured becomes the total to pay by (0 after a skipped round: nobody was measured), the
/// budget is `holder_pile` less the round's bounty reserve (`ROUND_BOUNTY_RESERVE`), the rest
/// clears. Unchanged within the round.
pub fn roll(r: Round, now_round: u32, holder_pile: u64) -> Round {
    if now_round <= r.round {
        return r;
    }
    Round {
        round: now_round,
        w_prev: if now_round == r.round.saturating_add(1) {
            r.w_cur
        } else {
            0
        },
        w_cur: 0,
        budget: holder_pile.saturating_sub(ROUND_BOUNTY_RESERVE),
        paid: 0,
        measured: 0,
    }
}

/// What a step does with one holding this round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Paid this many lamports now, and measured.
    Pay(u64),
    /// Measured (and marked for the round) with nothing to pay: under the least hold, the round
    /// measures only, or its share is under the least payout. The step also touches a holding
    /// whose owner can never take the payment (a program, an account another program owns, or a
    /// wallet the share would leave under the rent-exempt minimum); its share stays in the pile.
    Touch,
    /// Left alone: it earns at least the least payout this round, but this step cannot pay it
    /// (the step's budget, the per-holding cap or the pile is used up). The keeper brings it
    /// again next step.
    Skip,
}

/// What a holding of weight `w` earns this round: its share of the budget by the previous round's
/// total, never over what the round's budget has left. 0 when the round measures only (`w_prev`
/// 0) or the weight is 0.
pub fn earned(r: &Round, w: u128) -> u64 {
    if w == 0 || r.w_prev == 0 {
        return 0;
    }
    share(r.budget, w, r.w_prev).min(r.budget.saturating_sub(r.paid))
}

/// The step's decision for a holding of weight `w`: what it earns, if under the least payout a
/// touch; else paid in full when the step's remaining budget, the per-holding cap and the holder
/// pile cover it, else skipped for a later step.
pub fn decide(r: &Round, w: u128, holder_pile: u64, step_left: u64, holding_cap: u64) -> Decision {
    let due = earned(r, w);
    if due < MIN_PAYOUT_LAMPORTS {
        return Decision::Touch;
    }
    if due <= step_left && due <= holding_cap && due <= holder_pile {
        Decision::Pay(due)
    } else {
        Decision::Skip
    }
}
