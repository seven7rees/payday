//! T26 — Payday: the round's lowest balance and the locked measurement. The hook keeps, in each
//! holding's record, the lowest balance the holding held during the current weekly round; a
//! measurement counts that balance at the hold clock as of the round's first second and locks the
//! weight it gives (`measured`) beside the balance it counted (`counted`). The round after pays on
//! that locked weight, scaled down only if the holding's balance went lower later in the measured
//! round. The hook (`clock.rs`) and the script (`record.rs`) read the same bytes by the same rule.

use payday_hook::clock::{self, on_receive, on_send, round_tag, Record};
use payday_script::holders;
use payday_script::params::{self, MIN_HOLDING, ROUND_SECS};
use payday_script::record;

const WEEK: i64 = 604_800;
const DAY: i64 = 86_400;
const COIN: u64 = 1_000_000;
/// A time in round 2900, an hour after it began.
const T0: i64 = 2_900 * WEEK + 3_600;

fn funded(balance: u64, stamp: i64) -> Record {
    let mut r = Record::default();
    on_receive(&mut r, balance, 0, stamp);
    r
}

fn script_view(r: &Record) -> record::Record {
    record::Record::decode(&r.encode())
}

/// What the hook's `record_payout` does to a record measured in `round` (no lamports).
fn measure(r: &mut Record, round: u32) {
    r.round = round;
    r.counted = clock::low_of(r, r.amount, round);
    r.measured = clock::measured_weight(r, r.amount, round);
}

/// The weight, computed here from the tier table as written in the spec, not from either crate:
/// 1x from 6 hours, 1.5x from 7 days, 2x from 30 days, +10% if never sold, 0 under 1,000 coins.
fn independent_weight(balance: u64, held_secs: i64, sold: bool) -> u64 {
    if balance < 1_000 * COIN {
        return 0;
    }
    let tier: u128 = if held_secs >= 30 * DAY {
        20_000
    } else if held_secs >= 7 * DAY {
        15_000
    } else if held_secs >= 6 * 3_600 {
        10_000
    } else {
        0
    };
    let bonus: u128 = if sold { 0 } else { 1_000 };
    (u128::from(balance) * tier / 10_000 * (10_000 + bonus) / 10_000) as u64
}

struct Xorshift64(u64);
impl Xorshift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn hook_and_script_share_every_number() {
    assert_eq!(clock::ROUND_SECS, ROUND_SECS);
    assert_eq!(clock::MIN_HOLDING, params::MIN_HOLDING);
    assert_eq!(clock::MIN_HOLD_SECS, params::MIN_HOLD_SECS);
    assert_eq!(clock::NEVER_SOLD_BONUS_BPS, params::NEVER_SOLD_BONUS_BPS);
    assert_eq!(clock::WEIGHT_TIERS.len(), params::WEIGHT_TIERS.len());
    for (h, s) in clock::WEIGHT_TIERS.iter().zip(params::WEIGHT_TIERS.iter()) {
        assert_eq!((h.from_secs, h.bps), (s.from_secs, s.bps));
    }
    for t in [i64::MIN, -1, 0, 1, WEEK - 1, WEEK, T0, i64::MAX] {
        assert_eq!(clock::round_of(t), holders::round_of(t, ROUND_SECS), "{t}");
    }
    for round in [0u32, 1, 2_900, u32::MAX] {
        assert_eq!(clock::round_start(round), record::round_start(round));
        assert_eq!(clock::round_start(round), holders::round_start(round, ROUND_SECS));
    }
}

#[test]
fn the_first_change_in_a_round_starts_low_at_the_balance_held_before() {
    let mut r = funded(1_000 * COIN, T0 - 30 * DAY);
    let round = clock::round_of(T0);
    assert_ne!(r.low_round, round_tag(round));
    assert_eq!(clock::low_of(&r, r.amount, round), 1_000 * COIN);
    on_receive(&mut r, 100_000 * COIN, 1_000 * COIN, T0);
    assert_eq!((r.low, r.low_round), (1_000 * COIN, round_tag(round)));
    assert_eq!(clock::low_of(&r, r.amount, round), 1_000 * COIN, "the top-up counts nothing this round");
    on_send(&mut r, 101_000 * COIN, 600 * COIN, false, T0 + 60);
    assert_eq!(r.low, 600 * COIN);
    on_receive(&mut r, 50_000 * COIN, 600 * COIN, T0 + 120);
    assert_eq!((r.low, r.amount), (600 * COIN, 50_600 * COIN));
    assert_eq!(clock::low_of(&r, r.amount, round), 600 * COIN);
}

#[test]
fn a_new_round_resets_low_and_unchanged_coins_count_whole() {
    let mut r = funded(5_000 * COIN, T0 - DAY);
    on_send(&mut r, 5_000 * COIN, 2_000 * COIN, false, T0);
    let round = clock::round_of(T0);
    assert_eq!(clock::low_of(&r, r.amount, round), 2_000 * COIN);
    assert_eq!(clock::low_of(&r, r.amount, round + 1), 2_000 * COIN);
    on_receive(&mut r, 3_000 * COIN, 2_000 * COIN, T0 + WEEK);
    assert_eq!((r.low, r.prev_low, r.low_round), (2_000 * COIN, 2_000 * COIN, round_tag(round + 1)));
    assert_eq!(clock::low_of(&r, r.amount, round), 2_000 * COIN, "the round before's low is kept");
}

#[test]
fn a_fresh_holding_counts_zero_in_the_round_it_was_funded() {
    let round = clock::round_of(T0);
    let r = funded(9_000 * COIN, T0);
    assert_eq!((r.low, r.low_round, r.counted, r.measured), (0, round_tag(round), 0, 0));
    assert_eq!(clock::low_of(&r, r.amount, round), 0);
    assert_eq!(clock::measured_weight(&r, r.amount, round), 0);
    assert_eq!(clock::low_of(&r, r.amount, round + 1), 9_000 * COIN, "a full round later it counts");
    let mut r = funded(9_000 * COIN, T0 - 30 * DAY);
    on_send(&mut r, 9_000 * COIN, 0, false, T0);
    assert_eq!(r, Record::default(), "emptied: cleared");
    on_receive(&mut r, 9_000 * COIN, 0, T0 + 10);
    assert_eq!(clock::low_of(&r, r.amount, round), 0);
}

#[test]
fn a_measurement_locks_the_weight_and_the_balance_it_counted() {
    let mut r = funded(4_000 * COIN, T0 - 40 * DAY);
    on_receive(&mut r, 1, 4_000 * COIN, T0);
    let round = clock::round_of(T0);
    measure(&mut r, round);
    r.paid += 123;
    let bytes = r.encode();
    let back = Record::decode(&bytes);
    assert_eq!(back, r);
    assert_eq!(back.counted, 4_000 * COIN, "counted on the coins held all round, not the top-up");
    let held = clock::round_start(round) - r.stamp;
    assert_eq!(back.measured, independent_weight(4_000 * COIN, held, false), "the weight from the tier table");
    assert_eq!(&bytes[29..37], &(4_000 * COIN).to_le_bytes(), "counted at offset 29");
    assert_eq!(&bytes[38..46], &back.measured.to_le_bytes(), "measured at offset 38");
    assert_eq!(&bytes[46..54], &r.low.to_le_bytes());
    assert_eq!(&bytes[54..62], &r.prev_low.to_le_bytes());
    assert_eq!(&bytes[62..64], &r.low_round.to_le_bytes());
    let s = script_view(&r);
    assert_eq!((s.counted, s.measured), (back.counted, back.measured), "the script reads the same fields");
}

#[test]
fn the_hook_and_the_script_agree_and_match_the_tier_table() {
    let mut rng = Xorshift64(0x5EED_10E5);
    for _ in 0..30_000 {
        let start = T0 - (rng.next() % (60 * DAY as u64)) as i64;
        let mut r = funded(1 + rng.next() % (1_000_000 * COIN), start);
        let mut t = T0 + (rng.next() % 1_000) as i64;
        let mut measured_in: Option<u32> = None;
        for _ in 0..(rng.next() % 8) {
            t += (rng.next() % (3 * DAY as u64)) as i64;
            match rng.next() % 3 {
                0 => {
                    let before = r.amount;
                    on_receive(&mut r, 1 + rng.next() % (100_000 * COIN), before, t);
                }
                1 if r.amount > 0 => {
                    let before = r.amount;
                    let left = rng.next() % (before + 1);
                    on_send(&mut r, before, left, rng.next() % 3 == 0, t);
                }
                _ if r.is_set() && measured_in.is_none() => {
                    let round = clock::round_of(t);
                    measure(&mut r, round);
                    measured_in = Some(round);
                }
                _ => {}
            }
        }
        let s = script_view(&r);
        assert_eq!(
            (s.low, s.prev_low, s.low_round, s.measured, s.counted, s.amount, s.stamp),
            (r.low, r.prev_low, r.low_round, r.measured, r.counted, r.amount, r.stamp)
        );
        let now_round = clock::round_of(t);
        for round in [now_round.saturating_sub(1), now_round, now_round + 1] {
            assert_eq!(clock::low_of(&r, r.amount, round), record::low_of(&s, r.amount, round));
            let mw = clock::measured_weight(&r, r.amount, round);
            assert_eq!(mw, record::measured_weight(&s, r.amount, round), "same weight, same bytes");
            if r.is_set() {
                let held = clock::round_start(round).saturating_sub(r.stamp);
                assert_eq!(mw, independent_weight(clock::low_of(&r, r.amount, round), held, r.sold()));
            } else {
                assert_eq!(mw, 0);
            }
            let pw = clock::pay_weight(&r, r.amount, round);
            assert_eq!(pw, record::pay_weight(&s, r.amount, round));
            assert!(pw <= r.measured, "a pay weight never exceeds the weight measured");
        }
    }
}

#[test]
fn under_the_least_balance_a_holding_weighs_nothing() {
    let round = clock::round_of(T0);
    let r = funded(MIN_HOLDING, T0 - 30 * DAY);
    assert!(clock::measured_weight(&r, MIN_HOLDING, round) > 0);
    let r = funded(MIN_HOLDING - 1, T0 - 30 * DAY);
    assert_eq!(clock::measured_weight(&r, MIN_HOLDING - 1, round), 0);
    assert_eq!(record::measured_weight(&script_view(&r), MIN_HOLDING - 1, round), 0);
}

#[test]
fn the_top_up_farm_counts_the_coins_held_all_week() {
    let round = clock::round_of(T0);
    let aged = T0 - 30 * DAY;
    let honest = funded(1_000 * COIN, aged);
    let mut farmer = funded(1_000 * COIN, aged);
    on_receive(&mut farmer, 100_000 * COIN, 1_000 * COIN, T0);
    let w_honest = clock::measured_weight(&honest, honest.amount, round);
    let w_farmer = clock::measured_weight(&farmer, farmer.amount, round);
    let held = clock::round_start(round) - farmer.stamp;
    assert_eq!(w_farmer, independent_weight(1_000 * COIN, held, false), "the 1,000-coin weight at its own clock");
    assert!(w_farmer <= w_honest, "never more than the honest holder: {w_farmer} vs {w_honest}");
}

#[test]
fn selling_later_in_the_measured_round_scales_the_locked_weight_down() {
    // Measured an hour into round N on 10,000 coins, then sells 9,000 and buys them back the same
    // week: the round's low is 1,000, so the round after pays a tenth of the locked weight.
    let round = clock::round_of(T0);
    let mut r = funded(10_000 * COIN, T0 - 30 * DAY);
    measure(&mut r, round);
    let locked = r.measured;
    assert!(locked > 0);
    on_send(&mut r, 10_000 * COIN, 1_000 * COIN, true, T0 + 60);
    on_receive(&mut r, 9_000 * COIN, 1_000 * COIN, T0 + 120);
    assert_eq!(clock::pay_weight(&r, r.amount, round), locked / 10);
    // A change in round N+1 before the payout does not move it.
    on_receive(&mut r, 5 * COIN, 10_000 * COIN, T0 + WEEK);
    assert_eq!(clock::pay_weight(&r, r.amount, round), locked / 10, "round N's low is kept");
    let s = script_view(&r);
    assert_eq!(record::pay_weight(&s, s.amount, round), locked / 10);
    // Going under 1,000 coins during round N weighs nothing.
    let mut r = funded(10_000 * COIN, T0 - 30 * DAY);
    measure(&mut r, round);
    on_send(&mut r, 10_000 * COIN, 999 * COIN, false, T0 + 60);
    assert_eq!(clock::pay_weight(&r, r.amount, round), 0);
}

#[test]
fn a_change_in_the_pay_round_never_moves_the_locked_weight() {
    // Held 5,000 coins for 40 days through round N and measured then.
    let round = clock::round_of(T0);
    let mut r = funded(5_000 * COIN, T0 - 40 * DAY);
    measure(&mut r, round);
    let locked = r.measured;
    let held = clock::round_start(round) - r.stamp;
    assert_eq!(locked, independent_weight(5_000 * COIN, held, false), "2x plus the bonus");
    // A 50,000-coin top-up an hour into round N+1 drags the blended clock past round N's start:
    // the share for round N stays the locked weight.
    on_receive(&mut r, 50_000 * COIN, 5_000 * COIN, T0 + WEEK);
    assert!(r.stamp > clock::round_start(round), "the clock moved past round N's start");
    assert_eq!(clock::pay_weight(&r, r.amount, round), locked, "locked at measurement");
    // Selling part in round N+1 keeps it too (the sold flag does not reach back).
    on_send(&mut r, 55_000 * COIN, 2_000 * COIN, true, T0 + WEEK + 60);
    assert_eq!(clock::pay_weight(&r, r.amount, round), locked);
    assert_eq!(record::pay_weight(&script_view(&r), r.amount, round), locked);
    // Selling everything clears the record: nothing is left to pay.
    let held = r.amount;
    on_send(&mut r, held, 0, true, T0 + WEEK + 120);
    assert_eq!(clock::pay_weight(&r, r.amount, round), 0, "emptied: the share forfeited");
}
