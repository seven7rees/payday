//! T12: the Payday weight tiers on chain, the part T11's story does not reach. Five equal bags,
//! each sent to a fresh wallet at a chosen time, so that at the measured week's first second they
//! have held 35+ days (2x), 35+ days but sold a little (2x, no never-sold bonus), 8 days (1.5x),
//! 1 day (1x) and 2 hours (under the 6-hour least hold: weight 0). Week N measures them; week N+1
//! pays each `budget * w / W`, with every weight worked out here from the tier constants apart
//! from the script. After every step the two piles add up to the companion's pot.

use std::fs;

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use bordrless_companion::client as companion;
use bordrless_companion::constants::APPROVAL_KIND_SCRIPT;
use bordrless_companion::events::{ScriptPayout, ScriptStepped, ScriptWalletPayout};
use bordrless_companion::instructions::{CreateArgs, CreateScriptArgs, PayoutTermsArgs};
use bordrless_companion::state::{Companion, Split};
use bordrless_core::policy;
use bordrless_hook::token_flags;
use bordrless_launch::client::{self as launch, LaunchKeys};
use bordrless_launch::instructions::CreateConfigArgs;
use bordrless_launch::state::{LaunchRules, QuoteCut};
use bordrless_program_tests::env::{compute_unit_limit, Tx};
use bordrless_program_tests::fixture::World;
use bordrless_program_tests::launch::*;
use bordrless_program_tests::timelock::programdata_hash;
use bordrless_swap::events::Swapped;
use bordrless_token::client as token;
use payday_hook::clock::Record;
use payday_script::params::{
    BUILDER_WALLET, MIN_HOLD_SECS, MIN_PAYOUT_LAMPORTS, NEVER_SOLD_BONUS_BPS, ROUND_BOUNTY_RESERVE,
    WEIGHT_TIERS,
};
use payday_script::ScriptState;
use solana_keypair::Keypair;
use solana_signer::Signer;

const HOOK: Pubkey = payday_hook::ID;
const SCRIPT: Pubkey = payday_script::ID;
const FLAGS: u16 = token_flags::BEFORE_TRANSFER | token_flags::BEFORE_BURN | token_flags::WRITES_HOOK_DATA;
const CREATOR_FEE: u16 = 200;
const BORDRLESS_KEY: Pubkey = bordrless_launch::constants::HOOK_UPGRADE_AUTHORITIES[1];
const WEEK: i64 = 604_800;
const DAY: i64 = 86_400;
const STATE_LEN: u32 = 185;
const PACKET: usize = 1_232;

fn so(name: &str) -> Vec<u8> {
    let dir = if name.ends_with("_hook") { "hook_variants" } else { "script_variants" };
    let path = format!("{}/../{dir}/payday/target-sbf/{name}.so", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}; build with cargo build-sbf first"))
}

fn world() -> World {
    let mut w = World::new();
    w.env.svm.add_program(HOOK, &so("payday_hook")).expect("load the hook");
    w.env.svm.add_program(SCRIPT, &so("payday_script")).expect("load the script");
    w.env.set_upgrade_authority(HOOK, Some(BORDRLESS_KEY));
    w.env.set_upgrade_authority(SCRIPT, Some(BORDRLESS_KEY));
    let deployer = w.env.deployer.insecure_clone();
    let hash = programdata_hash(&w.env, &SCRIPT);
    w.env
        .send(
            &[companion::approve(deployer.pubkey(), SCRIPT, APPROVAL_KIND_SCRIPT, hash, Pubkey::default())],
            &[&deployer],
        )
        .ok();
    let admin = w.admin();
    w.as_admin(launch::set_native_launches(admin.pubkey(), true)).ok();
    w
}

fn hook_state_address(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"state", mint.as_ref()], &HOOK).0
}

fn prepare_hook(payer: Pubkey, mint: Pubkey) -> Instruction {
    Instruction {
        program_id: HOOK,
        accounts: payday_hook::accounts::Prepare {
            payer,
            mint,
            state: hook_state_address(&mint),
            registry: bordrless_hook::hook_accounts_address(&HOOK, &mint).0,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: payday_hook::instruction::Prepare {}.data(),
    }
}

fn config(w: &mut World, creator: &Keypair) -> Pubkey {
    let cfg = Keypair::new();
    let ix = launch::create_quote_cut_config(
        creator.pubkey(),
        cfg.pubkey(),
        CreateConfigArgs {
            rules: LaunchRules::NONE,
            creator_fee_bps: CREATOR_FEE,
            custom_hook: Some(HOOK),
            custom_hook_flags: FLAGS,
            label: "Payday".to_string(),
        },
        0,
        QuoteCut { max_buy_bps: 1, max_sell_bps: 80 },
    );
    w.env.send_paid_by(&[ix], creator, &[&cfg]).ok();
    cfg.pubkey()
}

fn launch_pair(w: &mut World, launcher: &Keypair, name: &str) -> Pubkey {
    let cfg = config(w, launcher);
    let mint = Keypair::new();
    let m = mint.pubkey();
    let ixs = [
        companion::create(
            launcher.pubkey(),
            launcher.pubkey(),
            m,
            CreateArgs {
                split: Split { buyback_bps: 0, holders_bps: 0, beneficiary_bps: 10_000 },
                bounty_bps: 50,
                max_buyback: SOL,
                buyback_interval: 60,
                vest_secs: 0,
                fund: SOL / 2,
            },
        ),
        prepare_hook(launcher.pubkey(), m),
        companion::create_script_v2(
            launcher.pubkey(),
            m,
            CreateScriptArgs {
                split: Split { buyback_bps: 0, holders_bps: 0, beneficiary_bps: 5_000 },
                pot_bps: 5_000,
                program: SCRIPT,
                interval: 300,
                state_len: STATE_LEN,
                init_data: vec![],
            },
            PayoutTermsArgs { step_bps: 10_000, holding_bps: 10_000, window_bps: 10_000, window_secs: 86_400 },
            vec![],
        ),
        companion::allow_wallet_payouts(m),
    ];
    w.env.send_paid_by(&ixs, launcher, &[&mint]).ok();
    let c = w.launch_config(&cfg);
    let custom = w.custom_hook_accounts(&HOOK, &m);
    let mut args = World::launch_args("PAYDAY", c.creator_fee_bps, VQ, c.rules);
    args.name = name.to_string();
    let inner = launch::create_launch_with(
        companion::creator_address(&m),
        m,
        w.env.treasury.pubkey(),
        w.sol,
        policy::LP_FEE_BPS,
        args.clone(),
        Some(cfg),
        Some(&custom),
    );
    let ix = companion::launch_script(launcher.pubkey(), m, &inner, args);
    let tx = w.env.send_paid_by(&[ix], launcher, &[&mint]);
    assert!(tx.result.is_ok(), "launch: {}", tx.logs().join("\n"));
    assert!(w.launch(&m).is_native());
    m
}

fn swap_ix(w: &World, trader: &Pubkey, mint: &Pubkey, buy: bool, amount: u64) -> Instruction {
    let slice = w.launch_base_slice(mint, trader, trader, buy);
    let ix = launch::swap_v2_with_base_slice(&w.launch_keys(mint), *trader, *trader, u8::from(buy), amount, 0, slice);
    w.with_quote_cut(ix, mint, trader)
}

fn buy(w: &mut World, trader: &Keypair, mint: &Pubkey, lamports: u64) -> Tx {
    let ixs = [
        token::create_holding(trader.pubkey(), *mint, trader.pubkey()),
        swap_ix(w, &trader.pubkey(), mint, true, lamports),
    ];
    w.env.send_paid_by(&ixs, trader, &[])
}

fn sell(w: &mut World, trader: &Keypair, mint: &Pubkey, amount: u64) -> Tx {
    let ix = swap_ix(w, &trader.pubkey(), mint, false, amount);
    w.env.send_paid_by(&[ix], trader, &[])
}

/// `amount` from `from` to `to`'s fresh holding, with the hook and its one registry extra.
fn send(w: &mut World, from: &Keypair, mint: &Pubkey, to: &Pubkey, amount: u64) {
    let ixs = [
        token::create_holding(from.pubkey(), *mint, *to),
        token::transfer_with(
            from.pubkey(),
            token::holding_address(mint, &from.pubkey()),
            token::holding_address(mint, to),
            *mint,
            Some(token::Hook::of(HOOK)),
            vec![AccountMeta::new(hook_state_address(mint), false)],
            amount,
        ),
    ];
    let tx = w.env.send_paid_by(&ixs, from, &[]);
    assert!(tx.result.is_ok(), "send: {}", tx.logs().join("\n"));
}

fn record_of(w: &World, mint: &Pubkey, owner: &Pubkey) -> Record {
    Record::decode(&w.env.hook_data(mint, owner))
}

fn state_of(w: &World, mint: &Pubkey) -> ScriptState {
    w.env.read(&companion::script_state_address(mint))
}

fn companion_of(w: &World, mint: &Pubkey) -> Companion {
    w.env.read(&companion::companion_address(mint))
}

fn step_extras(mint: &Pubkey, holders: &[Pubkey]) -> Vec<AccountMeta> {
    let mut extras = companion::payout_accounts(mint, &HOOK);
    extras.push(AccountMeta::new(BUILDER_WALLET, false));
    for h in holders {
        extras.push(AccountMeta::new(token::holding_address(mint, h), false));
        extras.push(AccountMeta::new(*h, false));
    }
    extras
}

/// The protocol's lookup table as it is on mainnet (read 2026-10-21), as T11 sends steps.
fn live_table(w: &World) -> Vec<Pubkey> {
    let live: [&str; 22] = [
        "69vhpnkYjtiJgdfU7QsA5Ww7V8FWtvPcZ2r4znuyByvq",
        "C2Y3B3hZTesJQqLYrZ7qoaZUoRmwYWh5Qh3MuFxruouE",
        "9cBz2tsg7FwopSsd6HzvtbiL6b7B46eLG436etkB7cyx",
        "6Ztfr97cUewdViXDXdZUsQq4pz7MYdygvK1WijALjZ5q",
        "2XLvgczuVACmvvjfRLLAcLwMzFHutbAAro61zZKP8fsx",
        "EiUK62AP8DwUJWBosf7Ceyih3sGAicZxmsmcBftHzM8n",
        "Dwf4C8tTMYwicp8cU5MYcTtMQVVJTN7W3LcXCQ1UEhMs",
        "3ta59VvLDKLnCKMhYihiC3fQ7W9jPKty6xfx3h5puHGS",
        "EAcZR2i8A6BbnKRdWuyMVDTuiNkD6qc4RkFpxUYJ9Qba",
        "6ogQR9x8o86egutFri8ev6Kx6reTDg8YdwPmvkW29efh",
        "3dfEZLdjRcpTJ4RxPzgkqnqaG2FipRdaHgRW72AL6kqL",
        "5n25iAaXFsjs4UgGQM6fCiRhaQcCVQ1L5BgyRZ7UrmaE",
        "9abhxVTuwck5Ux79act3e2Vkfem7Q4zBwATMctUvHvuE",
        "2repKA1JgDkBo4AffscVee342dcBcH6c2yfpUTRrAiEN",
        "14RJQXPdJfkehit6ezktjd3xujamf8nVSKw2shKamaEH",
        "A49oVhX22ExMwTEtFC6Y8nhBdZ4LJDGhdXLDn4c2f59i",
        "11111111111111111111111111111111",
        "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
        "7QuoYRuD9MHzX5EN7qqj62iwNPVqfy74RzmwJ524cgDt",
        "1jcBymHxBjniZDhNPy51Vgm5Nz7pLUdxa9UBHc4TavC",
        "GyzKSnnEu2uN5bBRecE4XYY2enbfR2D2MtxnbJPGy7hk",
        "2XoEWp8cF3kRXg74eVwPAyTFhVCAztn3V88komxAvr22",
    ];
    let mut addresses = protocol_lookup_table(w);
    for a in live {
        let k: Pubkey = a.parse().unwrap();
        if !addresses.contains(&k) {
            addresses.push(k);
        }
    }
    addresses
}

#[track_caller]
fn piles_equal_pot(w: &World, mint: &Pubkey) {
    let s = state_of(w, mint);
    let c = companion_of(w, mint);
    assert_eq!(s.last_pot, c.pending_pot, "last_pot is the companion's pot");
    assert_eq!(s.owed + s.holder_pile, s.last_pot, "the piles add up to the pot");
}

fn claim(w: &mut World, cranker: &Keypair, mint: &Pubkey) {
    let tx = w.env.send_paid_by(&[companion::claim_fees_script(cranker.pubkey(), *mint, None)], cranker, &[]);
    assert!(tx.result.is_ok(), "claim: {}", tx.logs().join("\n"));
}

/// One keeper step, sent as the keeper sends it (v0, the live table, a compute limit). Checks: it
/// succeeded, no trade, the pot fell by exactly what was paid, the piles equal the pot.
fn step(w: &mut World, cranker: &Keypair, mint: &Pubkey, holders: &[Pubkey]) -> Tx {
    let pot_before = companion_of(w, mint).pending_pot;
    let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(mint)), SCRIPT, step_extras(mint, holders));
    let addresses = live_table(w);
    let table = w.env.put_lookup_table(Pubkey::new_from_array([0x7c; 32]), &addresses);
    let ixs = [compute_unit_limit(1_400_000), ix];
    assert!(w.env.v0_size(&ixs, cranker, &[], &[table.clone()]) <= PACKET);
    let tx = w.env.send_v0(&ixs, cranker, &[], &[table]);
    assert!(tx.result.is_ok(), "step: {}", tx.logs().join("\n"));
    assert!(tx.events::<Swapped>().is_empty(), "the script never trades");
    let stepped: ScriptStepped = tx.event();
    let to_holders: u64 = tx.events::<ScriptPayout>().iter().map(|p| p.lamports).sum();
    let to_builder: u64 = tx.events::<ScriptWalletPayout>().iter().map(|p| p.lamports).sum();
    let pot_after = companion_of(w, mint).pending_pot;
    assert_eq!(pot_before - pot_after, to_holders + to_builder + stepped.bounty, "the pot fell by exactly what was paid");
    piles_equal_pot(w, mint);
    tx
}

/// Round trips by a whale so the launch earns creator fees (and the early-sell brake) for the pot.
fn volume(w: &mut World, whale: &Keypair, mint: &Pubkey, trips: usize) {
    for _ in 0..trips {
        buy(w, whale, mint, 2 * SOL).ok();
        let held = w.env.holding(mint, &whale.pubkey());
        sell(w, whale, mint, held).ok();
        w.env.warp(5);
    }
}

/// A holding's weight worked out here from the tier constants, apart from the script: `balance`
/// times the highest tier its hold reaches by `held` seconds (0 under the least hold), plus the
/// never-sold bonus, each step rounded down as basis points are.
fn expected_weight(balance: u64, held: i64, sold: bool) -> u128 {
    if held < MIN_HOLD_SECS {
        return 0;
    }
    let tier = WEIGHT_TIERS.iter().filter(|t| held >= t.from_secs).map(|t| t.bps).last().unwrap_or(0);
    let bonus = if sold { 0 } else { u128::from(NEVER_SOLD_BONUS_BPS) };
    u128::from(balance) * u128::from(tier) / 10_000 * (10_000 + bonus) / 10_000
}

#[test]
fn t12_payday_weight_tiers() {
    // The tiers this test is built around.
    assert_eq!(
        WEIGHT_TIERS.iter().map(|t| (t.from_secs, t.bps)).collect::<Vec<_>>(),
        vec![(21_600, 10_000), (604_800, 15_000), (2_592_000, 20_000)]
    );
    assert_eq!(NEVER_SOLD_BONUS_BPS, 1_000);

    let mut w = world();
    let launcher = w.env.funded(50 * SOL);
    let mint = launch_pair(&mut w, &launcher, "Payday Coin");
    let t0 = w.env.now;
    let n = (t0 / WEEK) as u32 + 6; // the measured week: its first second is 35 to 42 days away
    let start_n = i64::from(n) * WEEK;
    let start_next = start_n + WEEK;
    println!("T12 launch at {t0}; week {n} measured from {start_n}, paid in week {} from {start_next}", n + 1);

    // A funder buys one bag and sends five equal parts out at chosen times.
    w.env.warp(31);
    let funder = w.env.funded(20 * SOL);
    buy(&mut w, &funder, &mint, 3 * SOL).ok();
    let bag = w.env.holding(&mint, &funder.pubkey()) / 8;
    let dust = bag / 10;
    assert!(bag > 1_000_000_000, "each bag is over the keeper's 1,000-coin minimum: {bag}");
    let a = w.env.funded(SOL); // 2x, never sold
    let e = w.env.funded(SOL); // 2x, sold a little
    let b = w.env.funded(SOL); // 1.5x
    let c = w.env.funded(SOL); // 1x
    let d = w.env.funded(SOL); // under 6 hours: 0
    send(&mut w, &funder, &mint, &a.pubkey(), bag);
    send(&mut w, &funder, &mint, &e.pubkey(), bag + dust);
    let stamp_ae = w.env.now;
    w.env.warp(DAY);
    sell(&mut w, &e, &mint, dust).ok();
    let re = record_of(&w, &mint, &e.pubkey());
    assert!(re.sold() && re.stamp == stamp_ae, "a sell keeps the clock and sets the sold flag: {re:?}");
    assert_eq!(w.env.holding(&mint, &e.pubkey()), bag, "e now holds the same bag as the others");

    let whale = w.env.funded(200 * SOL);
    volume(&mut w, &whale, &mint, 12);

    let to = |w: &World, t: i64| t - w.env.now;
    let jump = to(&w, start_n - 8 * DAY);
    w.env.warp(jump);
    send(&mut w, &funder, &mint, &b.pubkey(), bag);
    let jump = to(&w, start_n - DAY);
    w.env.warp(jump);
    send(&mut w, &funder, &mint, &c.pubkey(), bag);
    let jump = to(&w, start_n - 2 * 3_600);
    w.env.warp(jump);
    send(&mut w, &funder, &mint, &d.pubkey(), bag);

    let holders = [a.pubkey(), e.pubkey(), b.pubkey(), c.pubkey(), d.pubkey()];
    let labels = ["2x never sold", "2x sold", "1.5x", "1x", "under 6h"];
    for h in &holders {
        assert_eq!(w.env.holding(&mint, h), bag, "equal bags");
    }
    let held: Vec<i64> = holders.iter().map(|h| start_n - record_of(&w, &mint, h).stamp).collect();
    println!("T12 held at the measured week's start (s): {held:?}");
    assert!(held[0] >= 30 * DAY && held[1] >= 30 * DAY);
    assert!((7 * DAY..30 * DAY).contains(&held[2]));
    assert!((MIN_HOLD_SECS..7 * DAY).contains(&held[3]));
    assert!(held[4] < MIN_HOLD_SECS);
    let weights: Vec<u128> = holders
        .iter()
        .zip(&held)
        .map(|(h, t)| expected_weight(bag, *t, record_of(&w, &mint, h).sold()))
        .collect();
    let total: u128 = weights.iter().sum();
    println!("T12 weights worked out here: {weights:?}, total {total}");
    assert_eq!(weights[0], u128::from(bag) * 2 * 11 / 10);
    assert_eq!(weights[1], u128::from(bag) * 2);
    assert_eq!(weights[4], 0);

    // ── Week N: every bag measured once, nobody paid (nobody was measured the week before) ──
    let cranker = w.env.funded(5 * SOL);
    w.env.fund(BUILDER_WALLET, SOL);
    let jump = to(&w, start_n + 600);
    w.env.warp(jump);
    claim(&mut w, &cranker, &mint);
    step(&mut w, &cranker, &mint, &[]);
    w.env.warp(300);
    for batch in [&holders[..4], &holders[4..]] {
        let tx = step(&mut w, &cranker, &mint, batch);
        let events: Vec<ScriptPayout> = tx.events();
        assert_eq!(events.len(), batch.len(), "every bag in the batch is measured\n{}", tx.logs().join("\n"));
        assert!(events.iter().all(|p| p.lamports == 0), "week N measures only");
        w.env.warp(300);
    }
    let s = state_of(&w, &mint);
    assert_eq!((s.round, s.measured), (n, 5));
    assert_eq!(s.w_cur, total, "the script's week total equals the one worked out here");
    for (h, wt) in holders.iter().zip(weights.iter()) {
        let r = record_of(&w, &mint, h);
        assert_eq!((r.round, r.counted), (n, bag), "the hook recorded the week and the bag");
        assert_eq!(u128::from(r.measured), *wt, "and locked the weight worked out here");
    }

    // More fees for week N+1's budget.
    volume(&mut w, &whale, &mint, 12);

    // ── Week N+1: each bag paid budget * w / W ──
    let jump = to(&w, start_next + 600);
    w.env.warp(jump);
    claim(&mut w, &cranker, &mint);
    let tx = step(&mut w, &cranker, &mint, &[]);
    assert!(tx.events::<ScriptPayout>().is_empty());
    let s = state_of(&w, &mint);
    assert_eq!((s.round, s.w_prev, s.round_paid), (n + 1, total, 0));
    let budget = s.round_budget;
    println!("T12 week {} budget {budget} (reserve {ROUND_BOUNTY_RESERVE} held back), holder pile {}", n + 1, s.holder_pile);
    let expected: Vec<u64> = weights.iter().map(|wt| (u128::from(budget) * wt / total) as u64).collect();
    for (x, label) in expected.iter().zip(labels).take(4) {
        assert!(*x >= MIN_PAYOUT_LAMPORTS, "{label} earns at least the least payout: {x}");
    }
    let before: Vec<u64> = holders.iter().map(|h| w.env.lamports(h)).collect();
    w.env.warp(300);
    for batch in [&holders[..4], &holders[4..]] {
        let tx = step(&mut w, &cranker, &mint, batch);
        let events: Vec<ScriptPayout> = tx.events();
        assert_eq!(events.len(), batch.len(), "every bag in the batch is answered\n{}", tx.logs().join("\n"));
        w.env.warp(300);
    }
    let paid: Vec<u64> = holders.iter().zip(&before).map(|(h, b0)| w.env.lamports(h) - b0).collect();
    for (((h, label), got), want) in holders.iter().zip(labels).zip(&paid).zip(&expected) {
        println!("T12 {label}: weight {}, paid {got} lamports (expected {want})", weights[labels.iter().position(|l| *l == label).unwrap()]);
        assert_eq!(got, want, "{label} is paid budget * its weight / the week's total");
        let r = record_of(&w, &mint, h);
        assert_eq!(r.round, n + 1, "{label} is marked for week N+1");
        assert_eq!(r.paid, *got, "the hook's record matches the SOL paid to {label}");
    }
    assert_eq!(paid[4], 0, "the 2-hour bag earns nothing and is still touched");
    let s = state_of(&w, &mint);
    assert_eq!(s.round_paid, paid.iter().sum::<u64>());
    assert!(s.round_paid <= budget);

    // The ratios, each within the rounding of one floor per payout.
    let (p2, p2s, p15, p1) = (paid[0] as i128, paid[1] as i128, paid[2] as i128, paid[3] as i128);
    assert!((p2 - 2 * p1).abs() <= 2, "2x pays twice 1x: {p2} vs {p1}");
    assert!((2 * p15 - 3 * p1).abs() <= 3, "1.5x pays one and a half times 1x: {p15} vs {p1}");
    assert!((11 * p2s - 10 * p2).abs() <= 11, "a bag that sold earns 1/1.1 of its never-sold peer: {p2s} vs {p2}");
    println!("T12 ratios: 2x/1x {:.6}, 1.5x/1x {:.6}, sold/never-sold {:.6}", p2 as f64 / p1 as f64, p15 as f64 / p1 as f64, p2s as f64 / p2 as f64);
}
