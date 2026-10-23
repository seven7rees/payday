//! T13: Payday against a hostile or careless step sender, on chain. A step with nothing to do is
//! refused (no slot, no bounty, nothing changes); a holding under 1,000 coins is never touched or
//! paid; a top-up during a week counts nothing that week; a holding passed twice counts once; a
//! holding whose owner wallet is left out (or passed read-only) is not paid that step and is paid
//! in full on a later step that carries it right; a holding that sells part after being measured
//! is paid on what it kept, and one emptied is paid nothing. After every successful step the two
//! piles add up to the companion's pot.
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
    BUILDER_WALLET, MIN_HOLDING, MIN_HOLD_SECS, MIN_PAYOUT_LAMPORTS, NEVER_SOLD_BONUS_BPS, WEIGHT_TIERS,
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
    step_with(w, cranker, mint, step_extras(mint, holders))
}

/// A step with the remaining accounts exactly as given.
fn step_with(w: &mut World, cranker: &Keypair, mint: &Pubkey, extras: Vec<AccountMeta>) -> Tx {
    let pot_before = companion_of(w, mint).pending_pot;
    let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(mint)), SCRIPT, extras);
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


/// A step the script must refuse with `NothingToDo`: the transaction fails, the companion's pot
/// and the script's state are byte for byte as before, and the sender gains no bounty.
#[track_caller]
fn refused(w: &mut World, cranker: &Keypair, mint: &Pubkey, extras: Vec<AccountMeta>) {
    let companion_key = companion::companion_address(mint);
    let state_key = companion::script_state_address(mint);
    let companion_before = w.env.account(&companion_key).expect("companion").data;
    let state_before = w.env.account(&state_key).expect("state").data;
    let sender_before = w.env.lamports(&cranker.pubkey());
    let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(mint)), SCRIPT, extras);
    let addresses = live_table(w);
    let table = w.env.put_lookup_table(Pubkey::new_from_array([0x7d; 32]), &addresses);
    let tx = w.env.send_v0(&[compute_unit_limit(1_400_000), ix], cranker, &[], &[table]);
    assert!(tx.result.is_err(), "a step with nothing to do is refused");
    assert!(tx.logs().iter().any(|l| l.contains("Error Code: NothingToDo")), "{}", tx.logs().join("\n"));
    assert_eq!(w.env.account(&companion_key).expect("companion").data, companion_before, "the pot is untouched");
    assert_eq!(w.env.account(&state_key).expect("state").data, state_before, "the state is untouched");
    let sender_after = w.env.lamports(&cranker.pubkey());
    assert!(sender_after <= sender_before, "no bounty for a refused step");
    println!("T13 refused step: the sender paid {} lamports of fees, gained nothing", sender_before - sender_after);
}

const COIN: u64 = 1_000_000;

fn warp_to(w: &mut World, t: i64) {
    let d = t - w.env.now;
    assert!(d >= 0, "time only moves forward");
    w.env.warp(d);
}

/// A launch with a funder holding plenty of coins, a whale to trade fees into the pot, a funded
/// sender and builder wallet, and the week `n` the test measures (a few weeks out).
struct Setup {
    w: World,
    mint: Pubkey,
    launcher: Keypair,
    funder: Keypair,
    whale: Keypair,
    cranker: Keypair,
    n: u32,
    start_n: i64,
}

fn setup() -> Setup {
    let mut w = world();
    let launcher = w.env.funded(50 * SOL);
    let mint = launch_pair(&mut w, &launcher, "Payday Coin");
    w.env.warp(31);
    let funder = w.env.funded(40 * SOL);
    buy(&mut w, &funder, &mint, 10 * SOL).ok();
    assert!(w.env.holding(&mint, &funder.pubkey()) > 1_000_000 * COIN, "the funder holds enough coins");
    let whale = w.env.funded(400 * SOL);
    let cranker = w.env.funded(5 * SOL);
    w.env.fund(BUILDER_WALLET, SOL);
    let n = (w.env.now / WEEK) as u32 + 7;
    Setup { w, mint, launcher, funder, whale, cranker, n, start_n: i64::from(n) * WEEK }
}

/// Payout events of a step, by holding.
fn paid_to(tx: &Tx, mint: &Pubkey, owner: &Pubkey) -> Vec<u64> {
    let holding = token::holding_address(mint, owner);
    tx.events::<ScriptPayout>().iter().filter(|p| p.holding == holding).map(|p| p.lamports).collect()
}

/// Week `n` measured, then week `n + 1` opened (fees claimed, the round rolled by a step that pays
/// the builder). Answers the week's budget and the total weight measured in week `n`.
fn open_pay_week(s: &mut Setup) -> (u64, u128) {
    volume(&mut s.w, &s.whale, &s.mint, 8);
    warp_to(&mut s.w, s.start_n + WEEK + 600);
    claim(&mut s.w, &s.cranker, &s.mint);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[]);
    assert!(tx.events::<ScriptPayout>().is_empty(), "the week's first step answers the builder");
    let st = state_of(&s.w, &s.mint);
    assert_eq!(st.round, s.n + 1);
    s.w.env.warp(300);
    (st.round_budget, st.w_prev)
}

/// Opens week `n` for measuring: fees claimed and the builder paid, so a later step with nothing
/// due is refused.
fn open_measure_week(s: &mut Setup) {
    volume(&mut s.w, &s.whale, &s.mint, 8);
    warp_to(&mut s.w, s.start_n + 600);
    claim(&mut s.w, &s.cranker, &s.mint);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[]);
    assert!(!tx.events::<ScriptWalletPayout>().is_empty(), "the builder is paid on a step with no holder");
    assert_eq!(state_of(&s.w, &s.mint).owed, 0);
    s.w.env.warp(300);
}

#[test]
fn t13_1_a_step_with_nothing_to_do_is_refused() {
    let mut s = setup();
    let holder = s.w.env.funded(SOL);
    let dust = s.w.env.funded(SOL);
    let nobody = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &holder.pubkey(), 5_000 * COIN);
    send(&mut s.w, &f, &m, &dust.pubkey(), MIN_HOLDING - 1);
    open_measure_week(&mut s);
    // No remaining accounts at all.
    refused(&mut s.w, &s.cranker, &s.mint, vec![]);
    // The payout accounts and the builder wallet, nobody to measure, nothing owed.
    refused(&mut s.w, &s.cranker, &s.mint, step_extras(&s.mint, &[]));
    // Only ineligible holdings: under 1,000 coins, a wallet with no holding, the launch pool.
    let pool = s.w.launch(&s.mint).pool;
    refused(&mut s.w, &s.cranker, &s.mint, step_extras(&s.mint, &[dust.pubkey(), nobody.pubkey(), pool]));
    // The next keeper step works.
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[holder.pubkey()]);
    assert_eq!(paid_to(&tx, &s.mint, &holder.pubkey()), vec![0], "the holder is measured");
    assert_eq!(state_of(&s.w, &s.mint).measured, 1);
}

#[test]
fn t13_2_a_holding_under_1000_coins_is_never_touched_or_paid() {
    let mut s = setup();
    let small = s.w.env.funded(SOL);
    let good = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &small.pubkey(), MIN_HOLDING - 1);
    send(&mut s.w, &f, &m, &good.pubkey(), 5_000 * COIN);
    open_measure_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[small.pubkey(), good.pubkey()]);
    assert!(paid_to(&tx, &s.mint, &small.pubkey()).is_empty(), "under the least balance: not measured");
    assert_eq!(paid_to(&tx, &s.mint, &good.pubkey()), vec![0]);
    assert_eq!(state_of(&s.w, &s.mint).measured, 1);
    let before = s.w.env.lamports(&small.pubkey());
    open_pay_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[small.pubkey(), good.pubkey()]);
    assert!(paid_to(&tx, &s.mint, &small.pubkey()).is_empty(), "never paid");
    assert!(paid_to(&tx, &s.mint, &good.pubkey())[0] > 0, "the other holder is paid");
    let r = record_of(&s.w, &s.mint, &small.pubkey());
    assert_eq!((r.round, r.paid, r.measured), (0, 0, 0), "never touched by the companion");
    assert_eq!(s.w.env.lamports(&small.pubkey()), before);
}

#[test]
fn t13_3_a_top_up_during_the_week_counts_nothing_that_week() {
    let mut s = setup();
    let farmer = s.w.env.funded(SOL);
    let peer = s.w.env.funded(SOL);
    let honest_buyer = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    // Both hold 1,000 coins from 40 days before week n.
    warp_to(&mut s.w, s.start_n - 40 * DAY);
    send(&mut s.w, &f, &m, &farmer.pubkey(), 1_000 * COIN);
    send(&mut s.w, &f, &m, &peer.pubkey(), 1_000 * COIN);
    open_measure_week(&mut s);
    // An hour into week n the farmer adds 100,000 coins; a newcomer gets 100,000 coins too.
    warp_to(&mut s.w, s.start_n + 3_600);
    send(&mut s.w, &f, &m, &farmer.pubkey(), 100_000 * COIN);
    send(&mut s.w, &f, &m, &honest_buyer.pubkey(), 100_000 * COIN);
    let rf = record_of(&s.w, &s.mint, &farmer.pubkey());
    assert_eq!((rf.amount, rf.low), (101_000 * COIN, 1_000 * COIN), "the week's low is what was held before the top-up");
    s.w.env.warp(300);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[farmer.pubkey(), peer.pubkey(), honest_buyer.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 3, "all three measured");
    let (rf, rp, rb) = (
        record_of(&s.w, &s.mint, &farmer.pubkey()),
        record_of(&s.w, &s.mint, &peer.pubkey()),
        record_of(&s.w, &s.mint, &honest_buyer.pubkey()),
    );
    assert_eq!((rf.counted, rp.counted, rb.counted), (1_000 * COIN, 1_000 * COIN, 0), "counted on coins held all week");
    let wf = expected_weight(1_000 * COIN, s.start_n - rf.stamp, rf.sold());
    let wp = expected_weight(1_000 * COIN, s.start_n - rp.stamp, rp.sold());
    assert_eq!((u128::from(rf.measured), u128::from(rp.measured), rb.measured), (wf, wp, 0), "the hook locked those weights");
    let w_bag = expected_weight(101_000 * COIN, s.start_n - rf.stamp, rf.sold());
    println!("T13 farmer held {} s at the week's start (blended clock), weight {wf}; peer {wp}; the whole bag would weigh {w_bag}", s.start_n - rf.stamp);
    assert!(wf > 0 && wf <= wp, "the farmer weighs no more than its peer");
    assert!(wf * 50 < w_bag, "far less than its bag");
    assert_eq!(state_of(&s.w, &s.mint).w_cur, wf + wp, "the week's total: 1,000 coins each, the newcomer 0");
    // Week n + 1: each paid budget * w / W, on what it held all week.
    let (budget, total) = open_pay_week(&mut s);
    assert_eq!(total, wf + wp);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[farmer.pubkey(), peer.pubkey(), honest_buyer.pubkey()]);
    let pf = paid_to(&tx, &s.mint, &farmer.pubkey())[0];
    let pp = paid_to(&tx, &s.mint, &peer.pubkey())[0];
    let pb = paid_to(&tx, &s.mint, &honest_buyer.pubkey())[0];
    assert_eq!(pf, (u128::from(budget) * wf / total) as u64);
    assert_eq!(pp, (u128::from(budget) * wp / total) as u64);
    assert_eq!(pb, 0, "coins that arrived during week n earn nothing for it");
    println!("T13 top-up: budget {budget}, farmer paid {pf}, peer paid {pp}, newcomer {pb}");
}

#[test]
fn t13_4_a_holding_passed_twice_counts_once() {
    let mut s = setup();
    let h = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &h.pubkey(), 5_000 * COIN);
    open_measure_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), h.pubkey()]);
    assert_eq!(paid_to(&tx, &s.mint, &h.pubkey()), vec![0], "measured once");
    assert_eq!(state_of(&s.w, &s.mint).measured, 1);
    let (budget, total) = open_pay_week(&mut s);
    let before = s.w.env.lamports(&h.pubkey());
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), h.pubkey()]);
    let p = paid_to(&tx, &s.mint, &h.pubkey());
    assert_eq!(p.len(), 1, "paid once");
    let r = record_of(&s.w, &s.mint, &h.pubkey());
    let w = expected_weight(5_000 * COIN, s.start_n - r.stamp, r.sold());
    assert_eq!(total, w);
    assert_eq!(p[0], (u128::from(budget) * w / total) as u64);
    assert_eq!(s.w.env.lamports(&h.pubkey()) - before, p[0]);
}

#[test]
fn t13_5_a_holding_without_its_wallet_writable_waits_and_is_paid_later() {
    let mut s = setup();
    let h = s.w.env.funded(SOL);
    let g = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &h.pubkey(), 5_000 * COIN);
    send(&mut s.w, &f, &m, &g.pubkey(), 5_000 * COIN);
    open_measure_week(&mut s);
    step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), g.pubkey()]);
    let (budget, total) = open_pay_week(&mut s);
    let r = record_of(&s.w, &s.mint, &h.pubkey());
    let due = (u128::from(budget) * expected_weight(5_000 * COIN, s.start_n - r.stamp, r.sold()) / total) as u64;
    let before = s.w.env.lamports(&h.pubkey());
    // h's holding without its wallet, beside g passed right: g paid, h left alone.
    let mut extras = step_extras(&s.mint, &[g.pubkey()]);
    extras.push(AccountMeta::new(token::holding_address(&s.mint, &h.pubkey()), false));
    let tx = step_with(&mut s.w, &s.cranker, &s.mint, extras);
    assert!(paid_to(&tx, &s.mint, &h.pubkey()).is_empty(), "no wallet: not paid, not touched");
    assert_eq!(record_of(&s.w, &s.mint, &h.pubkey()).round, s.n, "still due this week");
    s.w.env.warp(300);
    // h's wallet passed read-only: refused (nobody else to answer, nothing owed).
    let mut extras = companion::payout_accounts(&s.mint, &HOOK);
    extras.push(AccountMeta::new(BUILDER_WALLET, false));
    extras.push(AccountMeta::new(token::holding_address(&s.mint, &h.pubkey()), false));
    extras.push(AccountMeta::new_readonly(h.pubkey(), false));
    refused(&mut s.w, &s.cranker, &s.mint, extras);
    assert_eq!(s.w.env.lamports(&h.pubkey()), before);
    // Carried right: paid in full.
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey()]);
    assert_eq!(paid_to(&tx, &s.mint, &h.pubkey()), vec![due]);
    assert_eq!(s.w.env.lamports(&h.pubkey()) - before, due);
}

#[test]
fn t13_6_selling_after_being_measured_pays_on_what_was_held_that_week() {
    let mut s = setup();
    let half = s.w.env.funded(SOL);
    let gone = s.w.env.funded(SOL);
    let flip = s.w.env.funded(SOL);
    let keep = s.w.env.funded(SOL);
    let sink = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    for h in [&half, &gone, &flip, &keep] {
        send(&mut s.w, &f, &m, &h.pubkey(), 10_000 * COIN);
    }
    open_measure_week(&mut s);
    step(&mut s.w, &s.cranker, &s.mint, &[half.pubkey(), gone.pubkey(), flip.pubkey(), keep.pubkey()]);
    let w_each = {
        let r = record_of(&s.w, &s.mint, &keep.pubkey());
        expected_weight(10_000 * COIN, s.start_n - r.stamp, r.sold())
    };
    assert_eq!(state_of(&s.w, &s.mint).w_cur, 4 * w_each);
    // Later in week n: `flip` sends 9,000 coins out and takes them back the same week.
    s.w.env.warp(DAY);
    send(&mut s.w, &flip, &m, &sink.pubkey(), 9_000 * COIN);
    send(&mut s.w, &sink, &m, &flip.pubkey(), 9_000 * COIN);
    // Week n + 1, before the payout: `half` sends half away, `gone` sends everything, `flip`
    // receives a coin (its tracker rolls to week n + 1, week n's low is kept).
    let (budget, total) = open_pay_week(&mut s);
    send(&mut s.w, &half, &m, &sink.pubkey(), 5_000 * COIN);
    send(&mut s.w, &gone, &m, &sink.pubkey(), 10_000 * COIN);
    send(&mut s.w, &f, &m, &flip.pubkey(), COIN);
    s.w.env.warp(300);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[half.pubkey(), gone.pubkey(), flip.pubkey(), keep.pubkey()]);
    let r = record_of(&s.w, &s.mint, &half.pubkey());
    let w_half = expected_weight(10_000 * COIN, s.start_n - r.stamp, r.sold());
    assert_eq!(w_half, w_each, "a send keeps the clock and sets no sold flag");
    // Measured on 10,000 coins, then held only 1,000 through the rest of week n: paid the locked
    // weight scaled to the week's lowest balance.
    let w_flip = w_each * 1_000 / 10_000;
    assert_eq!(total, 4 * w_each);
    assert_eq!(paid_to(&tx, &s.mint, &half.pubkey()), vec![(u128::from(budget) * w_half / total) as u64], "paid on the 10,000 it held all week, though it kept half");
    assert!(paid_to(&tx, &s.mint, &gone.pubkey()).is_empty(), "emptied: paid nothing");
    assert_eq!(paid_to(&tx, &s.mint, &flip.pubkey()), vec![(u128::from(budget) * w_flip / total) as u64], "paid on the 1,000 it held all week, at the tier locked when measured");
    assert_eq!(paid_to(&tx, &s.mint, &keep.pubkey()), vec![(u128::from(budget) * w_each / total) as u64]);
    let st = state_of(&s.w, &s.mint);
    assert!(st.round_paid < budget, "what the sellers gave up stays in the pile");
}

/// The rent-exempt minimum of an empty system account (the companion's and the script's
/// `Rent::minimum_balance(0)` at the default rent).
const RENT_MIN: u64 = 890_880;

#[test]
fn t13_7_a_wallet_too_empty_to_take_its_share_is_touched_and_the_share_stays() {
    let mut s = setup();
    let big = s.w.env.funded(SOL);
    let poor = Keypair::new(); // never funded: 0 lamports, no account
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &big.pubkey(), 1_000_000 * COIN);
    send(&mut s.w, &f, &m, &poor.pubkey(), 1_000 * COIN);
    assert_eq!(s.w.env.lamports(&poor.pubkey()), 0);
    open_measure_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[big.pubkey(), poor.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 2, "both measured");
    let (budget, total) = open_pay_week(&mut s);
    let rb = record_of(&s.w, &s.mint, &big.pubkey());
    let rp = record_of(&s.w, &s.mint, &poor.pubkey());
    let wb = expected_weight(1_000_000 * COIN, s.start_n - rb.stamp, rb.sold());
    let wp = expected_weight(1_000 * COIN, s.start_n - rp.stamp, rp.sold());
    assert_eq!(total, wb + wp);
    let due_big = (u128::from(budget) * wb / total) as u64;
    let due_poor = (u128::from(budget) * wp / total) as u64;
    println!("T13 unfundable wallet: budget {budget}, big due {due_big}, poor due {due_poor} (rent {RENT_MIN})");
    assert!(due_poor >= MIN_PAYOUT_LAMPORTS && due_poor < RENT_MIN, "the case under test: worth paying, too small to open the wallet");
    let pile_before = state_of(&s.w, &s.mint).holder_pile;
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[big.pubkey(), poor.pubkey()]);
    assert_eq!(paid_to(&tx, &s.mint, &big.pubkey()), vec![due_big]);
    assert_eq!(paid_to(&tx, &s.mint, &poor.pubkey()), vec![0], "touched for 0, the step succeeds");
    assert_eq!(s.w.env.lamports(&poor.pubkey()), 0, "nothing sent to the empty wallet");
    let rp = record_of(&s.w, &s.mint, &poor.pubkey());
    assert_eq!((rp.round, rp.paid), (s.n + 1, 0), "marked for this week: the keeper's filter (round != epoch) stops bringing it");
    let st = state_of(&s.w, &s.mint);
    assert_eq!(st.round_paid, due_big, "only the big holder's share left the round");
    let stepped: ScriptStepped = tx.event();
    assert_eq!(pile_before - st.holder_pile, due_big + stepped.bounty, "the poor holder's share stays in the pile");
    // Brought again the same week, alone: nothing to do (already counted this round).
    s.w.env.warp(300);
    refused(&mut s.w, &s.cranker, &s.mint, step_extras(&s.mint, &[poor.pubkey()]));
}

#[test]
fn t13_8_the_pool_the_creator_address_and_the_launcher_are_never_measured() {
    let mut s = setup();
    let h = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    let launcher = s.launcher.insecure_clone();
    let pool = s.w.launch(&s.mint).pool;
    let creator = companion::creator_address(&s.mint);
    assert_eq!(companion_of(&s.w, &s.mint).beneficiary, launcher.pubkey(), "the launcher is the beneficiary");
    buy(&mut s.w, &launcher, &m, SOL).ok();
    assert!(s.w.env.holding(&m, &launcher.pubkey()) >= MIN_HOLDING, "the launcher holds 1,000+ coins");
    send(&mut s.w, &f, &m, &creator, 5_000 * COIN);
    send(&mut s.w, &f, &m, &h.pubkey(), 5_000 * COIN);
    assert!(s.w.env.holding(&m, &pool) >= MIN_HOLDING, "the pool vault holds coins");
    let three = [pool, creator, launcher.pubkey()];
    open_measure_week(&mut s);
    refused(&mut s.w, &s.cranker, &s.mint, step_extras(&s.mint, &three));
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[pool, creator, launcher.pubkey(), h.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 1, "only the eligible holding is measured");
    assert_eq!(paid_to(&tx, &s.mint, &h.pubkey()), vec![0]);
    let (budget, total) = open_pay_week(&mut s);
    refused(&mut s.w, &s.cranker, &s.mint, step_extras(&s.mint, &three));
    let launcher_before = s.w.env.lamports(&launcher.pubkey());
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[pool, creator, launcher.pubkey(), h.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 1, "only the eligible holding is paid");
    let r = record_of(&s.w, &s.mint, &h.pubkey());
    let w = expected_weight(5_000 * COIN, s.start_n - r.stamp, r.sold());
    assert_eq!(total, w);
    assert_eq!(paid_to(&tx, &s.mint, &h.pubkey()), vec![(u128::from(budget) * w / total) as u64]);
    assert_eq!(s.w.env.lamports(&launcher.pubkey()), launcher_before, "the launcher is not paid as a holder");
    assert_eq!(record_of(&s.w, &s.mint, &launcher.pubkey()).round, 0, "never touched");
}


/// A wallet with exactly `lamports` (an empty system account; under the rent-exempt minimum it is
/// a rent-paying account, which the runtime lets stay as it is).
fn set_wallet(w: &mut World, key: &Pubkey, lamports: u64) {
    w.env.fund(*key, lamports);
    assert_eq!(w.env.lamports(key), lamports);
}

#[test]
fn t13_9_the_rent_boundary_exactly() {
    let mut s = setup();
    let big = s.w.env.funded(SOL);
    let at = Keypair::new();
    let under = Keypair::new();
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &big.pubkey(), 1_000_000 * COIN);
    send(&mut s.w, &f, &m, &at.pubkey(), 1_000 * COIN);
    send(&mut s.w, &f, &m, &under.pubkey(), 1_000 * COIN);
    open_measure_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[big.pubkey(), at.pubkey(), under.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 3);
    let (budget, total) = open_pay_week(&mut s);
    let ra = record_of(&s.w, &s.mint, &at.pubkey());
    let ru = record_of(&s.w, &s.mint, &under.pubkey());
    let due_at = (u128::from(budget) * u128::from(ra.measured) / total) as u64;
    let due_under = (u128::from(budget) * u128::from(ru.measured) / total) as u64;
    assert!(due_at >= MIN_PAYOUT_LAMPORTS && due_at < RENT_MIN && due_under == due_at, "{due_at} {due_under}");
    // One wallet ends at exactly the rent-exempt minimum, the other one lamport under it.
    set_wallet(&mut s.w, &at.pubkey(), RENT_MIN - due_at);
    set_wallet(&mut s.w, &under.pubkey(), RENT_MIN - due_under - 1);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[big.pubkey(), at.pubkey(), under.pubkey()]);
    assert_eq!(paid_to(&tx, &s.mint, &at.pubkey()), vec![due_at], "exactly at the minimum: paid");
    assert_eq!(s.w.env.lamports(&at.pubkey()), RENT_MIN);
    assert_eq!(paid_to(&tx, &s.mint, &under.pubkey()), vec![0], "one lamport short: touched for 0");
    assert_eq!(s.w.env.lamports(&under.pubkey()), RENT_MIN - due_under - 1, "nothing sent");
    assert_eq!(record_of(&s.w, &s.mint, &under.pubkey()).round, s.n + 1, "marked for the week");
    println!("T13 rent boundary: due {due_at}, paid into a wallet left at exactly {RENT_MIN}; the one a lamport short touched");
}

/// Makes `key` an account no payment can reach: an executable one, or one another program owns.
fn make_unpayable(w: &mut World, key: &Pubkey, executable: bool) {
    let a = if executable {
        // A deployed program's account, copied onto this key: executable, owned by the loader.
        w.env.account(&HOOK).expect("the hook's program account")
    } else {
        let mut a = w.env.account(&BUILDER_WALLET).expect("a template account");
        a.data = vec![0u8; 8];
        a.owner = SCRIPT;
        a
    };
    w.env.svm.set_account(*key, a).expect("set the account");
    let set = w.env.account(key).expect("set");
    assert_eq!(set.executable, executable);
    assert_ne!(set.owner, anchor_lang::system_program::ID);
}

#[test]
fn t13_10_an_owner_no_payment_can_reach_is_touched_for_0() {
    let mut s = setup();
    let good = s.w.env.funded(SOL);
    let exe = Keypair::new();
    let owned = Keypair::new();
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &good.pubkey(), 5_000 * COIN);
    send(&mut s.w, &f, &m, &exe.pubkey(), 5_000 * COIN);
    send(&mut s.w, &f, &m, &owned.pubkey(), 5_000 * COIN);
    open_measure_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[good.pubkey(), exe.pubkey(), owned.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 3, "all three measured");
    let (budget, total) = open_pay_week(&mut s);
    make_unpayable(&mut s.w, &exe.pubkey(), true);
    make_unpayable(&mut s.w, &owned.pubkey(), false);
    let rg = record_of(&s.w, &s.mint, &good.pubkey());
    let due_good = (u128::from(budget) * u128::from(rg.measured) / total) as u64;
    let pile_before = state_of(&s.w, &s.mint).holder_pile;
    let lamports_before = [s.w.env.lamports(&exe.pubkey()), s.w.env.lamports(&owned.pubkey())];
    let mut extras = step_extras(&s.mint, &[good.pubkey()]);
    for h in [exe.pubkey(), owned.pubkey()] {
        extras.push(AccountMeta::new(token::holding_address(&s.mint, &h), false));
        extras.push(AccountMeta::new_readonly(h, false));
    }
    let tx = step_with(&mut s.w, &s.cranker, &s.mint, extras);
    assert_eq!(paid_to(&tx, &s.mint, &good.pubkey()), vec![due_good]);
    assert_eq!(paid_to(&tx, &s.mint, &exe.pubkey()), vec![0], "an executable owner: touched for 0");
    assert_eq!(paid_to(&tx, &s.mint, &owned.pubkey()), vec![0], "an owner another program owns: touched for 0");
    for (h, before) in [(exe.pubkey(), lamports_before[0]), (owned.pubkey(), lamports_before[1])] {
        assert_eq!(record_of(&s.w, &s.mint, &h).round, s.n + 1, "marked: not brought again this week");
        assert_eq!(s.w.env.lamports(&h), before, "nothing sent");
    }
    let stepped: ScriptStepped = tx.event();
    assert_eq!(pile_before - state_of(&s.w, &s.mint).holder_pile, due_good + stepped.bounty, "their shares stay in the pile");
    // Brought again the same week, alone: already counted, so nothing to do.
    s.w.env.warp(300);
    let mut extras = companion::payout_accounts(&s.mint, &HOOK);
    extras.push(AccountMeta::new(BUILDER_WALLET, false));
    for h in [exe.pubkey(), owned.pubkey()] {
        extras.push(AccountMeta::new(token::holding_address(&s.mint, &h), false));
        extras.push(AccountMeta::new_readonly(h, false));
    }
    refused(&mut s.w, &s.cranker, &s.mint, extras);
}

#[test]
fn t13_11_a_top_up_in_the_pay_week_keeps_the_locked_share() {
    let mut s = setup();
    let h = s.w.env.funded(SOL);
    let peer = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    warp_to(&mut s.w, s.start_n - 40 * DAY);
    send(&mut s.w, &f, &m, &h.pubkey(), 5_000 * COIN);
    send(&mut s.w, &f, &m, &peer.pubkey(), 5_000 * COIN);
    open_measure_week(&mut s);
    step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), peer.pubkey()]);
    let r = record_of(&s.w, &s.mint, &h.pubkey());
    let locked = expected_weight(5_000 * COIN, s.start_n - r.stamp, r.sold());
    assert_eq!(u128::from(r.measured), locked, "2x plus the never-sold bonus, locked");
    let (budget, total) = open_pay_week(&mut s);
    // 50,000 more coins an hour into the pay week: the blended clock moves past week n's start.
    warp_to(&mut s.w, s.start_n + WEEK + 3_600);
    send(&mut s.w, &f, &m, &h.pubkey(), 50_000 * COIN);
    let r = record_of(&s.w, &s.mint, &h.pubkey());
    assert!(r.stamp > s.start_n, "the clock now reads younger than week n");
    assert_eq!(expected_weight(5_000 * COIN, s.start_n - r.stamp, r.sold()), 0, "recomputed from the clock it would be 0");
    s.w.env.warp(300);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), peer.pubkey()]);
    assert_eq!(paid_to(&tx, &s.mint, &h.pubkey()), vec![(u128::from(budget) * locked / total) as u64], "paid the locked share");
    assert_eq!(paid_to(&tx, &s.mint, &peer.pubkey()), vec![(u128::from(budget) * locked / total) as u64], "the same as its peer");
}

#[test]
fn t13_12_going_under_1000_coins_before_the_payout_forfeits_it() {
    let mut s = setup();
    let h = s.w.env.funded(SOL);
    let peer = s.w.env.funded(SOL);
    let sink = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &h.pubkey(), 5_000 * COIN);
    send(&mut s.w, &f, &m, &peer.pubkey(), 5_000 * COIN);
    open_measure_week(&mut s);
    step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), peer.pubkey()]);
    let (budget, total) = open_pay_week(&mut s);
    // In the pay week, before its step: down to 999 coins.
    send(&mut s.w, &h, &m, &sink.pubkey(), 5_000 * COIN - 999 * COIN);
    assert_eq!(s.w.env.holding(&m, &h.pubkey()), 999 * COIN);
    s.w.env.warp(300);
    let before = s.w.env.lamports(&h.pubkey());
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[h.pubkey(), peer.pubkey()]);
    assert!(paid_to(&tx, &s.mint, &h.pubkey()).is_empty(), "under 1,000 coins: skipped, not paid");
    assert_eq!(record_of(&s.w, &s.mint, &h.pubkey()).round, s.n, "untouched: its week-n share is not paid");
    assert_eq!(s.w.env.lamports(&h.pubkey()), before);
    let rp = record_of(&s.w, &s.mint, &peer.pubkey());
    assert_eq!(paid_to(&tx, &s.mint, &peer.pubkey()), vec![(u128::from(budget) * u128::from(rp.measured) / total) as u64]);
    // Alone, it is nothing to do.
    s.w.env.warp(300);
    refused(&mut s.w, &s.cranker, &s.mint, step_extras(&s.mint, &[h.pubkey()]));
}

#[test]
fn t13_13_the_same_coins_are_never_paid_twice_across_wallets() {
    let mut s = setup();
    let a = s.w.env.funded(SOL);
    let b = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &a.pubkey(), 10_000 * COIN);
    open_measure_week(&mut s);
    // A is measured early in week n on its 10,000 coins.
    step(&mut s.w, &s.cranker, &s.mint, &[a.pubkey()]);
    let w_a = u128::from(record_of(&s.w, &s.mint, &a.pubkey()).measured);
    assert!(w_a > 0);
    // Later in week n A moves 9,000 to B; both are stepped that week.
    s.w.env.warp(DAY);
    send(&mut s.w, &a, &m, &b.pubkey(), 9_000 * COIN);
    s.w.env.warp(300);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[a.pubkey(), b.pubkey()]);
    assert!(paid_to(&tx, &s.mint, &a.pubkey()).is_empty(), "A was counted already this week");
    assert_eq!(paid_to(&tx, &s.mint, &b.pubkey()), vec![0], "B is measured");
    let rb = record_of(&s.w, &s.mint, &b.pubkey());
    assert_eq!((rb.counted, rb.measured), (0, 0), "coins that arrived in week n count 0 in it");
    let st = state_of(&s.w, &s.mint);
    assert_eq!(st.w_cur, w_a, "the week's total holds the 10,000 coins once");
    // Week n + 1: A is paid on its week-n low (1,000 of the 10,000 measured), B on nothing.
    let (budget, total) = open_pay_week(&mut s);
    assert_eq!(total, w_a);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[a.pubkey(), b.pubkey()]);
    let pay_a = (u128::from(budget) * (w_a * 1_000 / 10_000) / total) as u64;
    assert_eq!(paid_to(&tx, &s.mint, &a.pubkey()), vec![pay_a], "A: its week-n low");
    assert_eq!(paid_to(&tx, &s.mint, &b.pubkey()), vec![0], "B: nothing for week n");
    assert!(state_of(&s.w, &s.mint).round_paid <= budget / 10 + 1, "nine tenths of the week stays in the pile");
}

#[test]
fn t13_14_a_reserved_owner_is_touched_for_0_with_or_without_its_account() {
    // A reserved key on the curve passes the wallet test, so the hook keeps its record and the
    // script measures it; the companion would refuse to pay it, so its share is counted for 0.
    let reserved: Vec<Pubkey> = payday_script::params::RESERVED_KEYS.iter().copied().filter(|k| k.is_on_curve()).take(2).collect();
    assert_eq!(reserved.len(), 2, "two on-curve reserved keys");
    let mut s = setup();
    let good = s.w.env.funded(SOL);
    let (f, m) = (s.funder.insecure_clone(), s.mint);
    send(&mut s.w, &f, &m, &good.pubkey(), 5_000 * COIN);
    for k in &reserved {
        send(&mut s.w, &f, &m, k, 5_000 * COIN);
    }
    open_measure_week(&mut s);
    let tx = step(&mut s.w, &s.cranker, &s.mint, &[good.pubkey()]);
    assert_eq!(tx.events::<ScriptPayout>().len(), 1);
    // The reserved holdings are measured without their owners' accounts (a touch reads none).
    s.w.env.warp(300);
    let mut extras = companion::payout_accounts(&s.mint, &HOOK);
    for k in &reserved {
        extras.push(AccountMeta::new(token::holding_address(&s.mint, k), false));
    }
    let tx = step_with(&mut s.w, &s.cranker, &s.mint, extras);
    assert_eq!(tx.events::<ScriptPayout>().len(), 2, "both reserved holdings measured");
    for k in &reserved {
        assert!(record_of(&s.w, &s.mint, k).measured > 0, "counted with a weight");
    }
    let (budget, total) = open_pay_week(&mut s);
    let rg = record_of(&s.w, &s.mint, &good.pubkey());
    let due_good = (u128::from(budget) * u128::from(rg.measured) / total) as u64;
    let pile_before = state_of(&s.w, &s.mint).holder_pile;
    let lamports_before: Vec<u64> = reserved.iter().map(|k| s.w.env.lamports(k)).collect();
    // One reserved owner passed (writable: the runtime demotes it to read-only), one not passed.
    let mut extras = step_extras(&s.mint, &[good.pubkey()]);
    extras.push(AccountMeta::new(token::holding_address(&s.mint, &reserved[0]), false));
    extras.push(AccountMeta::new(reserved[0], false));
    extras.push(AccountMeta::new(token::holding_address(&s.mint, &reserved[1]), false));
    let tx = step_with(&mut s.w, &s.cranker, &s.mint, extras);
    assert_eq!(paid_to(&tx, &s.mint, &good.pubkey()), vec![due_good]);
    for (k, before) in reserved.iter().zip(&lamports_before) {
        assert_eq!(paid_to(&tx, &s.mint, k), vec![0], "a reserved owner: touched for 0");
        assert_eq!(record_of(&s.w, &s.mint, k).round, s.n + 1, "marked: not brought again this week");
        assert_eq!(s.w.env.lamports(k), *before, "nothing sent");
    }
    let stepped: ScriptStepped = tx.event();
    assert_eq!(pile_before - state_of(&s.w, &s.mint).holder_pile, due_good + stepped.bounty, "their shares stay in the pile");
    // Brought again the same week, alone: already counted, so the step is refused.
    s.w.env.warp(300);
    let mut extras = companion::payout_accounts(&s.mint, &HOOK);
    extras.push(AccountMeta::new(BUILDER_WALLET, false));
    for k in &reserved {
        extras.push(AccountMeta::new(token::holding_address(&s.mint, k), false));
    }
    refused(&mut s.w, &s.cranker, &s.mint, extras);
    println!("T13 reserved owners {:?}: counted, then touched for 0; due to the good holder {due_good}", reserved);
}
