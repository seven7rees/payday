//! T23 — Payday script: the pot split (the builder's quarter, ALL the rest to the holder pile), the
//! round budget's bounty reserve, and the accounting invariant: the two piles always add up to the
//! pot, rounding dust lands in the holder pile, every lamport in and out accounted for.

use payday_script::holders::{self, Decision, Round};
use payday_script::math::{pot_after_step, step_bounty, Piles};
use payday_script::params::{BUILDER_BPS, MIN_PAYOUT_LAMPORTS, ROUND_BOUNTY_RESERVE, STEP_BOUNTY};

struct Xorshift64(u64);
impl Xorshift64 {
    fn new(s: u64) -> Self { Self(s | 1) }
    fn next(&mut self) -> u64 {
        let mut x = self.0; x ^= x << 13; x ^= x >> 7; x ^= x << 17; self.0 = x; x
    }
}

const SOL: u64 = 1_000_000_000;

#[test]
fn the_builder_share_is_a_quarter() {
    assert_eq!(BUILDER_BPS, 2_500, "owner's pick 2026-10-22");
}

#[test]
fn every_lamport_lands_in_the_builders_pile_or_the_holder_pile() {
    for new in [0u64, 1, 2, 3, 7, 999, 1_000_000, 1_000_003, u64::MAX / 4, u64::MAX] {
        let mut p = Piles::default();
        p.split(new, BUILDER_BPS);
        assert_eq!(p.total(), new, "nothing lost, nothing made ({new})");
        assert_eq!(p.owed, (u128::from(new) * 2_500 / 10_000) as u64, "the builder's quarter, rounded down");
        assert_eq!(p.holders, new - p.owed, "ALL the rest, rounding included, to the holders");
    }
    let mut p = Piles::default();
    p.split(1_000_000, BUILDER_BPS);
    assert_eq!(p, Piles { owed: 250_000, holders: 750_000 });
    let mut p = Piles::default();
    p.split(7, BUILDER_BPS);
    assert_eq!(p, Piles { owed: 1, holders: 6 }, "tiny amounts: the rounding goes to holders");
}

#[test]
fn take_and_reconcile_keep_the_sum_exact() {
    let mut p = Piles { owed: 10, holders: 5 };
    assert_eq!(p.take(6), 0);
    assert_eq!(p, Piles { owed: 9, holders: 0 }, "holders first, then the builder's");
    assert_eq!(p.take(100), 91, "what the piles cannot cover is returned");
    assert_eq!(p.total(), 0);
    let mut p = Piles { owed: 1, holders: 2 };
    p.reconcile(20);
    assert_eq!(p, Piles { owed: 1, holders: 19 }, "a surplus goes to the holder pile");
    p.reconcile(5);
    assert_eq!(p.total(), 5, "a shortfall comes out in order");
    assert_eq!(p, Piles { owed: 1, holders: 4 });
    p.reconcile(0);
    assert_eq!(p.total(), 0);
}

#[test]
fn the_bounty_and_the_pot_after_a_step() {
    assert_eq!(step_bounty(SOL, None), STEP_BOUNTY);
    assert_eq!(step_bounty(SOL, Some(1)), STEP_BOUNTY / 8);
    assert_eq!(step_bounty(SOL, Some(8)), STEP_BOUNTY);
    assert_eq!(step_bounty(SOL, Some(16)), STEP_BOUNTY, "capped at the full bounty");
    assert_eq!(step_bounty(STEP_BOUNTY - 1, None), 0, "nothing when the pot cannot cover it");
    assert_eq!(pot_after_step(SOL, 1_000, Some(1)), SOL - 1_000 - STEP_BOUNTY / 8);
    assert_eq!(pot_after_step(SOL, 0, None), SOL - STEP_BOUNTY);
}

#[test]
fn a_rounds_budget_keeps_back_a_week_of_bounties() {
    assert_eq!(ROUND_BOUNTY_RESERVE, 2_016 * 20_000);
    let r = holders::roll(Round::default(), 5, SOL);
    assert_eq!(r.budget, SOL - ROUND_BOUNTY_RESERVE);
    assert_eq!((r.round, r.paid, r.measured, r.w_cur), (5, 0, 0, 0));
    let tiny = holders::roll(Round::default(), 5, ROUND_BOUNTY_RESERVE - 1);
    assert_eq!(tiny.budget, 0, "a pile under the reserve pays nobody that round");
    // A week of bounties out of the pile still leaves the whole budget payable.
    let mut pile = SOL;
    pile -= 2_016 * STEP_BOUNTY;
    assert!(pile >= r.budget);
    let unchanged = holders::roll(r, 5, 7);
    assert_eq!(unchanged, r, "unchanged within the round");
    let skipped = holders::roll(Round { round: 5, w_cur: 99, ..r }, 7, SOL);
    assert_eq!(skipped.w_prev, 0, "after a skipped round nobody was measured");
    let next = holders::roll(Round { round: 5, w_cur: 99, ..r }, 6, SOL);
    assert_eq!(next.w_prev, 99);
}

#[test]
fn a_share_is_paid_whole_or_left_for_a_later_step() {
    let r = Round { round: 6, w_prev: 1_000, w_cur: 0, budget: 10 * SOL, paid: 0, measured: 0 };
    assert_eq!(holders::decide(&r, 100, 10 * SOL, 10 * SOL, 10 * SOL), Decision::Pay(SOL));
    assert_eq!(holders::decide(&r, 100, SOL - 1, 10 * SOL, 10 * SOL), Decision::Skip, "the pile short: wait");
    assert_eq!(holders::decide(&r, 100, 10 * SOL, SOL - 1, 10 * SOL), Decision::Skip, "the step short: wait");
    assert_eq!(holders::decide(&r, 0, 10 * SOL, 10 * SOL, 10 * SOL), Decision::Touch);
    let dust = Round { budget: MIN_PAYOUT_LAMPORTS * 1_000 - 1, ..r };
    assert_eq!(holders::decide(&dust, 1, SOL, SOL, SOL), Decision::Touch, "under the least payout: measured only");
    let first = Round { w_prev: 0, ..r };
    assert_eq!(holders::decide(&first, 100, SOL, SOL, SOL), Decision::Touch, "the first round measures only");
}

/// The step as the script accounts it: inflow, a split, payouts out of the holder pile, the
/// builder out of owed, the sender's bounty, then reconcile against the pot the companion keeps.
/// The invariants: piles.total() == pot after every step, no pile ever pays what it does not hold.
#[test]
fn n_step_simulation_keeps_the_piles_equal_to_the_pot() {
    let mut rng = Xorshift64::new(0xB0B_5EED);
    let mut pot = 0u64;
    let mut piles = Piles::default();
    let (mut in_total, mut out_total) = (0u64, 0u64);
    for step in 0..20_000u64 {
        let inflow = rng.next() % 50_000_000;
        pot += inflow;
        in_total += inflow;
        let new_money = pot - piles.total();
        piles.split(new_money, BUILDER_BPS);
        piles.reconcile(pot);
        assert_eq!(piles.total(), pot, "after the split, step {step}");
        let mut paid_holders = 0u64;
        let mut pay_builder = 0u64;
        let mut entries = None;
        match rng.next() % 3 {
            0 => {
                let n = 1 + rng.next() % 16;
                for _ in 0..n {
                    let pay = piles.holders.min(rng.next() % 1_000_000);
                    piles.holders -= pay;
                    paid_holders += pay;
                }
                entries = Some(n);
            }
            1 => {
                pay_builder = piles.owed.min(rng.next() % 2_000_000);
                piles.owed -= pay_builder;
                if pay_builder > 0 { entries = Some(1); }
            }
            _ => {}
        }
        let bounty = step_bounty(pot, entries);
        let short = piles.take(bounty);
        assert_eq!(short, 0, "the bounty is always covered when the companion pays it");
        let pot_left = pot - paid_holders - pay_builder - bounty;
        assert_eq!(piles.total(), pot_left, "before reconcile, step {step}");
        piles.reconcile(pot_left);
        assert_eq!(piles.total(), pot_left, "after the step, step {step}");
        out_total += paid_holders + pay_builder + bounty;
        pot = pot_left;
    }
    assert_eq!(in_total, out_total + pot, "every lamport in is out or still in the pot");
}
