//! T11: the Payday pair, end to end, in LiteSVM against the Bordrless programs built from the
//! pinned source (or the mainnet program bytes, with SBF_OUT_DIR) and the pair's own SBF builds.
//! The story: prepare the hook, a quote-cut config, a native script launch named plainly, buys,
//! an early sell braked, a late one free, a delegate sale braked through the relay, fees claimed,
//! the builder paid a quarter, how many holders one step transaction carries (measured), a week
//! measured and the next paid by exact weights, a step with nobody to pay. After every step the
//! two piles add up to the companion's pot, the script never trades, and every lamport into the
//! pot is paid out or still in it.

use std::fs;

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{InstructionData, ToAccountMetas};
use bordrless_companion::client as companion;
use bordrless_companion::constants::{APPROVAL_KIND_SCRIPT, SCRIPT_BOUNTY};
use bordrless_companion::events::{ScriptPayout, ScriptStepped, ScriptWalletPayout};
use bordrless_companion::instructions::{CreateArgs, CreateScriptArgs, PayoutTermsArgs};
use bordrless_companion::state::{Companion, Split};
use bordrless_core::policy;
use bordrless_hook::token_flags;
use bordrless_launch::client::{self as launch, LaunchKeys};
use bordrless_launch::instructions::CreateConfigArgs;
use bordrless_launch::state::{LaunchRules, QuoteCut};
use bordrless_program_tests::env::{compute_unit_limit, compute_unit_price, Tx};
use bordrless_program_tests::fixture::World;
use bordrless_program_tests::launch::*;
use bordrless_program_tests::timelock::programdata_hash;
use bordrless_swap::events::Swapped;
use bordrless_token::client as token;
use payday_hook::clock::Record;
use payday_script::params::{BUILDER_WALLET, MIN_PAYOUT_LAMPORTS, PAYOUT_MAX_ENTRIES};
use payday_script::ScriptState;
use solana_keypair::Keypair;
use solana_signer::Signer;

const HOOK: Pubkey = payday_hook::ID;
const SCRIPT: Pubkey = payday_script::ID;
const FLAGS: u16 = token_flags::BEFORE_TRANSFER | token_flags::BEFORE_BURN | token_flags::WRITES_HOOK_DATA;
const CREATOR_FEE: u16 = 200;
const BORDRLESS_KEY: Pubkey = bordrless_launch::constants::HOOK_UPGRADE_AUTHORITIES[1];
const WEEK: i64 = 604_800;
const BUILDER_BPS: u64 = 2_500;
const STATE_LEN: u32 = 185;
/// The most a packet carries.
const PACKET: usize = 1_232;

fn so(name: &str) -> Vec<u8> {
    let dir = if name.ends_with("_hook") { "hook_variants" } else { "script_variants" };
    let path = format!("{}/../{dir}/payday/target-sbf/{name}.so", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}; build with cargo build-sbf first"))
}

/// A world with the pair loaded as Bordrless deploys it (its upgrade key), the script approved,
/// native launches on.
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

/// The standard `prepare` of the hook for `mint`.
fn prepare_hook(payer: Pubkey, mint: Pubkey) -> Instruction {
    Instruction {
        program_id: HOOK,
        accounts: payday_hook::accounts::Prepare {
            payer,
            mint,
            state: Pubkey::find_program_address(&[b"state", mint.as_ref()], &HOOK).0,
            registry: bordrless_hook::hook_accounts_address(&HOOK, &mint).0,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
        data: payday_hook::instruction::Prepare {}.data(),
    }
}

/// A quote-cut config on the hook: 1 bps cap on buys (always asked, answered 0), 80 on sells.
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

/// The pair launched by `launcher`, keeper.json's way: the companion, the hook prepared, the
/// script with its open terms and wallet payouts allowed, then the native launch from the config.
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
    assert_eq!(custom.extras.len(), 1, "the registry lists only the hook's state");
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
    let l = w.launch(&m);
    assert!(l.is_native(), "a native launch");
    assert_eq!(l.custom_hook, Some(HOOK));
    assert!(l.quote_cut().is_some(), "the SOL cut is on");
    m
}

/// A native swap by `trader` (its own wallet pays a buy in lamports), with the SOL cut's accounts.
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

fn record_of(w: &World, mint: &Pubkey, owner: &Pubkey) -> Record {
    Record::decode(&w.env.hook_data(mint, owner))
}

fn state_of(w: &World, mint: &Pubkey) -> ScriptState {
    w.env.read(&companion::script_state_address(mint))
}

fn companion_of(w: &World, mint: &Pubkey) -> Companion {
    w.env.read(&companion::companion_address(mint))
}

fn hook_state(w: &World, mint: &Pubkey) -> payday_hook::HookState {
    w.env.read(&Pubkey::find_program_address(&[b"state", mint.as_ref()], &HOOK).0)
}

/// The accounts a keeper's step brings for this keeper.json (payouts true, swapAccounts false,
/// hookAccounts false): the companion's payout accounts, the builder wallet, then each holding
/// with its owner, all writable.
fn step_extras(w: &World, mint: &Pubkey, holders: &[Pubkey]) -> Vec<AccountMeta> {
    let mut extras = companion::payout_accounts(mint, &HOOK);
    extras.push(AccountMeta::new(BUILDER_WALLET, false));
    for h in holders {
        extras.push(AccountMeta::new(token::holding_address(mint, h), false));
        extras.push(AccountMeta::new(*h, false));
    }
    let _ = w;
    extras
}

/// The protocol's lookup table as it is on mainnet (4vxVcYLdkqT1rMfGHjAu4kMa9XSEhUdQU5ThVfU5grGQ,
/// read 2026-10-21): the fixture's 18 plus the launchpad, the DEX, the token program and one more.
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

/// Every lamport into the pot (the fee claims) and out of it (holder payouts, builder payouts,
/// the senders' bounties), counted from the companion's own events.
#[derive(Default, Debug)]
struct Ledger {
    start: u64,
    claimed: u64,
    to_holders: u64,
    to_builder: u64,
    bounties: u64,
    steps: u64,
    max_cu: u64,
}

fn claim(w: &mut World, l: &mut Ledger, cranker: &Keypair, mint: &Pubkey) {
    let before = companion_of(w, mint).pending_pot;
    let tx = w.env.send_paid_by(&[companion::claim_fees_script(cranker.pubkey(), *mint, None)], cranker, &[]);
    assert!(tx.result.is_ok(), "claim: {}", tx.logs().join("\n"));
    l.claimed += companion_of(w, mint).pending_pot - before;
}

/// One keeper step with `holders`, sent as the keeper sends it: a v0 transaction with the live
/// protocol table and a compute limit. Checks, every time: the step succeeded, the script made no
/// trade, the pot fell by exactly what the companion paid out, the piles equal the pot.
fn step(w: &mut World, l: &mut Ledger, cranker: &Keypair, mint: &Pubkey, holders: &[Pubkey]) -> Tx {
    let pot_before = companion_of(w, mint).pending_pot;
    let extras = step_extras(w, mint, holders);
    let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(mint)), SCRIPT, extras);
    let addresses = live_table(w);
    let table = w.env.put_lookup_table(Pubkey::new_from_array([0x7a; 32]), &addresses);
    let ixs = [compute_unit_limit(1_400_000), ix];
    let size = w.env.v0_size(&ixs, cranker, &[], &[table.clone()]);
    assert!(size <= PACKET, "a {}-holder step is {size} bytes, over one packet", holders.len());
    let tx = w.env.send_v0(&ixs, cranker, &[], &[table]);
    assert!(tx.result.is_ok(), "step: {}", tx.logs().join("\n"));
    assert!(tx.events::<Swapped>().is_empty(), "the script never trades");
    let stepped: ScriptStepped = tx.event();
    let to_holders: u64 = tx.events::<ScriptPayout>().iter().map(|p| p.lamports).sum();
    let to_builder: u64 = tx.events::<ScriptWalletPayout>().iter().map(|p| p.lamports).sum();
    for p in tx.events::<ScriptWalletPayout>() {
        assert_eq!(p.to, BUILDER_WALLET, "the only wallet ever paid");
    }
    let pot_after = companion_of(w, mint).pending_pot;
    assert_eq!(pot_after, stepped.pot_after);
    assert_eq!(pot_before - pot_after, to_holders + to_builder + stepped.bounty, "the pot fell by exactly what was paid");
    assert_eq!(stepped.tokens_before, stepped.tokens_after, "the creator's tokens never move");
    l.to_holders += to_holders;
    l.to_builder += to_builder;
    l.bounties += stepped.bounty;
    l.steps += 1;
    l.max_cu = l.max_cu.max(tx.cu());
    println!(
        "T11 step: {} holders brought, {} paid, {} measured only, {} CU",
        holders.len(),
        tx.events::<ScriptPayout>().iter().filter(|p| p.lamports > 0).count(),
        tx.events::<ScriptPayout>().iter().filter(|p| p.lamports == 0).count(),
        tx.cu()
    );
    piles_equal_pot(w, mint);
    tx
}

/// After every step: the two piles add up to the companion's pot.
/// A step the script must refuse (`NothingToDo`): the transaction fails, the companion's pot and
/// the script's state are byte for byte as before, and the sender is paid no bounty (it pays only
/// the network fee).
#[track_caller]
fn refused_step(w: &mut World, cranker: &Keypair, mint: &Pubkey, holders: &[Pubkey]) {
    let companion_key = companion::companion_address(mint);
    let state_key = companion::script_state_address(mint);
    let companion_before = w.env.account(&companion_key).expect("companion").data;
    let state_before = w.env.account(&state_key).expect("state").data;
    let sender_before = w.env.lamports(&cranker.pubkey());
    let extras = step_extras(w, mint, holders);
    let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(mint)), SCRIPT, extras);
    let addresses = live_table(w);
    let table = w.env.put_lookup_table(Pubkey::new_from_array([0x7a; 32]), &addresses);
    let tx = w.env.send_v0(&[compute_unit_limit(1_400_000), ix], cranker, &[], &[table]);
    assert!(tx.result.is_err(), "a step with nothing to do must be refused");
    assert!(tx.logs().iter().any(|l| l.contains("Error Code: NothingToDo")), "{}", tx.logs().join("\n"));
    assert_eq!(w.env.account(&companion_key).expect("companion").data, companion_before, "the pot is untouched");
    assert_eq!(w.env.account(&state_key).expect("state").data, state_before, "the state is untouched");
    assert!(w.env.lamports(&cranker.pubkey()) <= sender_before, "no bounty for a refused step");
}

#[track_caller]
fn piles_equal_pot(w: &World, mint: &Pubkey) {
    let s = state_of(w, mint);
    let c = companion_of(w, mint);
    assert_eq!(s.last_pot, c.pending_pot, "last_pot is the companion's pot");
    assert_eq!(s.owed + s.holder_pile, s.last_pot, "the piles add up to the pot");
}

/// The next week's first second plus `extra`, from now.
fn to_next_week(w: &World, extra: i64) -> i64 {
    WEEK - w.env.now.rem_euclid(WEEK) + extra
}

#[test]
fn t11_payday_story() {
    let mut w = world();
    let mut l = Ledger::default();
    let launcher = w.env.funded(50 * SOL);
    let mint = launch_pair(&mut w, &launcher, "Payday Coin");
    l.start = companion_of(&w, &mint).pending_pot;

    // ── Trades: the hold clock, the brake, the relay ──
    w.env.warp(31);
    let a = w.env.funded(20 * SOL);
    let b = w.env.funded(20 * SOL);
    let c = w.env.funded(20 * SOL);
    buy(&mut w, &a, &mint, SOL).ok();
    let ra = record_of(&w, &mint, &a.pubkey());
    assert!(ra.is_set() && ra.stamp == w.env.now && ra.amount == w.env.holding(&mint, &a.pubkey()), "{ra:?}");
    w.env.warp(60);
    buy(&mut w, &b, &mint, SOL).ok();
    let b_bought_at = w.env.now;
    buy(&mut w, &c, &mint, SOL / 2).ok();
    w.env.warp(120);
    let accrued0 = w.launch(&mint).quote_cut_accrued();
    let held_b = w.env.holding(&mint, &b.pubkey());
    let tx = sell(&mut w, &b, &mint, held_b / 10);
    tx.ok();
    let swapped: Swapped = tx.event();
    let accrued1 = w.launch(&mint).quote_cut_accrued();
    let cut = accrued1 - accrued0;
    let bps = cut * 10_000 / swapped.amount_out;
    assert!((79..=80).contains(&bps), "the brake is 80 bps of the SOL out: {cut} of {} is {bps} bps", swapped.amount_out);
    let rb = record_of(&w, &mint, &b.pubkey());
    assert!(rb.sold() && rb.stamp == b_bought_at, "a sell keeps the clock, sets the flag");
    w.env.warp(601);
    let held_a = w.env.holding(&mint, &a.pubkey());
    sell(&mut w, &a, &mint, held_a / 10).ok();
    assert_eq!(w.launch(&mint).quote_cut_accrued(), accrued1, "after 10 minutes a sell is free");
    assert_eq!(hook_state(&w, &mint).relay_owner, a.pubkey(), "the relay holds the last seller");
    // The dust-delegate trick is braked through the relay.
    let d = w.env.funded(5 * SOL);
    buy(&mut w, &d, &mint, SOL / 100).ok();
    w.env.warp(1_000);
    let y = w.env.funded(5 * SOL);
    buy(&mut w, &y, &mint, SOL).ok();
    let y_holding = token::holding_address(&mint, &y.pubkey());
    let y_held = w.env.holding(&mint, &y.pubkey());
    w.env.send_paid_by(&[token::approve(y.pubkey(), y_holding, d.pubkey(), y_held)], &y, &[]).ok();
    let mut ix = swap_ix(&w, &d.pubkey(), &mint, false, y_held / 2);
    let d_holding = token::holding_address(&mint, &d.pubkey());
    let slot_index = ix.accounts.iter().position(|m| m.pubkey == d_holding).expect("the trader's base holding");
    ix.accounts[slot_index].pubkey = y_holding;
    let accrued_before = w.launch(&mint).quote_cut_accrued();
    let tx = w.env.send_paid_by(&[ix], &d, &[]);
    assert!(tx.result.is_ok(), "a delegate sell: {}", tx.logs().join("\n"));
    assert!(w.launch(&mint).quote_cut_accrued() > accrued_before, "the brake keys on y's clock through the relay");
    assert_eq!(hook_state(&w, &mint).relay_owner, y.pubkey());

    // ── The first step: the builder's quarter, all the rest to the holder pile ──
    let cranker = w.env.funded(5 * SOL);
    claim(&mut w, &mut l, &cranker, &mint);
    let pot0 = companion_of(&w, &mint).pending_pot;
    assert!(pot0 > 10 * SCRIPT_BOUNTY, "pot {pot0}");
    w.env.fund(BUILDER_WALLET, SOL);
    let builder_before = w.env.lamports(&BUILDER_WALLET);
    let tx = step(&mut w, &mut l, &cranker, &mint, &[]);
    let paid: ScriptWalletPayout = tx.event();
    assert_eq!(w.env.lamports(&BUILDER_WALLET) - builder_before, paid.lamports);
    let s1 = state_of(&w, &mint);
    let builder = (u128::from(pot0) * u128::from(BUILDER_BPS) / 10_000) as u64;
    assert_eq!(s1.owed + paid.lamports, builder, "the builder's quarter exactly: paid plus still owed");
    assert_eq!(s1.holder_pile, pot0 - builder - SCRIPT_BOUNTY / 8, "ALL the rest to holders, less the step's bounty");
    assert_eq!(s1.steps, 1);

    // ── How many holders one step transaction carries ──
    let mut holders: Vec<Keypair> = Vec::new();
    for _ in 0..16 {
        let h = w.env.funded(5 * SOL);
        buy(&mut w, &h, &mint, SOL / 4).ok();
        holders.push(h);
    }
    let keys: Vec<Pubkey> = holders.iter().map(|h| h.pubkey()).collect();
    let addresses = live_table(&w);
    let table = w.env.put_lookup_table(Pubkey::new_from_array([0x7b; 32]), &addresses);
    let mut fit = 0usize;
    for n in 1..=16 {
        let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(&mint)), SCRIPT, step_extras(&w, &mint, &keys[..n]));
        let size = w.env.v0_size(&[compute_unit_limit(1_400_000), ix], &cranker, &[], &[table.clone()]);
        println!("T11 {n}-holder step: {size} bytes as v0 with the live protocol table and a compute limit (packet {PACKET})");
        if size <= PACKET {
            fit = n;
        }
    }
    println!("T11 holders per step that fit one packet: {fit}");
    assert!(fit >= 1);
    for (label, with_limit, with_price) in [("no compute instructions", false, false), ("a compute limit and a price", true, true)] {
        let mut fits = 0;
        for n in 1..=16 {
            let ix = companion::script_step(cranker.pubkey(), &LaunchKeys::of(&w.launch(&mint)), SCRIPT, step_extras(&w, &mint, &keys[..n]));
            let mut ixs = Vec::new();
            if with_limit {
                ixs.push(compute_unit_limit(1_400_000));
            }
            if with_price {
                ixs.push(compute_unit_price(1_000));
            }
            ixs.push(ix);
            if w.env.v0_size(&ixs, &cranker, &[], &[table.clone()]) <= PACKET {
                fits = n;
            }
        }
        println!("T11 holders per step with {label}: {fits}");
    }
    assert!(fit <= PAYOUT_MAX_ENTRIES, "the script pays at least as many a step as fit one transaction");
    let keeper = include_str!("../../script_variants/payday/keeper.json").replace([' ', '\n'], "");
    assert!(keeper.contains(&format!("\"withoutSwapAccounts\":{fit}")), "keeper.json perStep is what fits: {fit}");

    // Everyone in `all` holds from before next week's start by more than the least hold.
    w.env.warp(21_601);
    let mut all: Vec<Pubkey> = vec![a.pubkey(), b.pubkey(), c.pubkey()];
    all.extend(keys.iter().copied());
    let week = |t: i64| (t / WEEK) as u32;

    // ── Week one: every holding measured once, nobody paid (nothing measured the week before) ──
    let skip = to_next_week(&w, 1);
    w.env.warp(skip);
    let buyer = w.env.funded(5 * SOL);
    buy(&mut w, &buyer, &mint, SOL / 2).ok();
    claim(&mut w, &mut l, &cranker, &mint);
    let w1 = week(w.env.now);
    let mut steps_used = 0;
    loop {
        let batch: Vec<Pubkey> = all.iter().copied().filter(|h| record_of(&w, &mint, h).round != w1).take(fit).collect();
        if batch.is_empty() {
            break;
        }
        let tx = step(&mut w, &mut l, &cranker, &mint, &batch);
        let events: Vec<ScriptPayout> = tx.events();
        assert_eq!(events.len(), batch.len(), "every holding in the batch is measured\n{}", tx.logs().join("\n"));
        assert!(events.iter().all(|p| p.lamports == 0), "week one measures only");
        steps_used += 1;
        w.env.warp(300);
    }
    println!("T11 week one: {} holdings measured in {steps_used} steps of up to {fit}", all.len());
    let s2 = state_of(&w, &mint);
    assert_eq!((s2.round, s2.measured as usize), (w1, all.len()));
    for h in &all {
        let r = record_of(&w, &mint, h);
        assert_eq!(r.round, w1, "the hook recorded the round");
        assert_eq!(r.counted, w.env.holding(&mint, h), "and the balance counted");
    }
    // The weights, worked out apart from the script: every holder bought last week, so at this
    // week's first second its clock is past 6 hours and under 7 days (1x), +10% if it never sold.
    let weight_of = |w: &World, h: &Pubkey| -> u128 {
        let r = record_of(w, &mint, h);
        let base = u128::from(r.counted);
        if r.sold() { base } else { base * 11_000 / 10_000 }
    };
    for h in &all {
        assert_eq!(u128::from(record_of(&w, &mint, h).measured), weight_of(&w, h), "the hook locked the weight");
    }
    let total_w: u128 = all.iter().map(|h| weight_of(&w, h)).sum();
    assert_eq!(s2.w_cur, total_w, "the week's total weight is every holding's by the rule");
    assert!(record_of(&w, &mint, &b.pubkey()).sold() && !record_of(&w, &mint, &c.pubkey()).sold());

    // ── A step with nobody left to measure: pays nobody, loses nothing ──
    let pile_before = state_of(&w, &mint).holder_pile;
    let tx = step(&mut w, &mut l, &cranker, &mint, &all[..fit.min(all.len())]);
    assert!(tx.events::<ScriptPayout>().is_empty(), "nothing twice in a week");
    let stepped: ScriptStepped = tx.event();
    let s3 = state_of(&w, &mint);
    let to_builder: u64 = tx.events::<ScriptWalletPayout>().iter().map(|p| p.lamports).sum();
    assert!(to_builder > 0, "a step with no holder due answers the builder's wallet payout");
    assert_eq!(pile_before - s3.holder_pile, stepped.bounty, "only the sender's bounty left the holder pile");
    assert_eq!(s3.holders_paid, s2.holders_paid);
    assert_eq!(s3.owed, 0, "the builder is paid all it is owed");
    w.env.warp(300);
    // Nothing owed and nobody to measure: the step is refused, takes no slot, pays no bounty.
    let stranger = Keypair::new();
    w.env.fund(stranger.pubkey(), SOL);
    refused_step(&mut w, &cranker, &mint, &[stranger.pubkey()]);
    refused_step(&mut w, &cranker, &mint, &[]);
    refused_step(&mut w, &cranker, &mint, &all[..fit.min(all.len())]);

    // ── Week two: every holding paid its exact share of the week's budget ──
    let skip = to_next_week(&w, 1);
    w.env.warp(skip);
    buy(&mut w, &buyer, &mint, SOL / 2).ok();
    claim(&mut w, &mut l, &cranker, &mint);
    let tx = step(&mut w, &mut l, &cranker, &mint, &[]);
    assert!(tx.events::<ScriptPayout>().is_empty());
    let s4 = state_of(&w, &mint);
    let w2 = week(w.env.now);
    assert_eq!((s4.round, s4.w_prev, s4.round_paid), (w2, total_w, 0));
    assert_eq!(s4.round_start, i64::from(w2) * WEEK);
    let budget = s4.round_budget;
    assert!(budget > 0);
    let expected: Vec<u64> = all.iter().map(|h| (u128::from(budget) * weight_of(&w, h) / total_w) as u64).collect();
    let before: Vec<u64> = all.iter().map(|h| w.env.lamports(h)).collect();
    let mut steps_used = 0;
    loop {
        let batch: Vec<Pubkey> = all.iter().copied().filter(|h| record_of(&w, &mint, h).round != w2).take(fit).collect();
        if batch.is_empty() {
            break;
        }
        w.env.warp(300);
        let tx = step(&mut w, &mut l, &cranker, &mint, &batch);
        let events: Vec<ScriptPayout> = tx.events();
        assert_eq!(events.len(), batch.len(), "every holding in the batch is paid\n{}", tx.logs().join("\n"));
        for p in &events {
            assert!(p.lamports >= MIN_PAYOUT_LAMPORTS, "paid in full, never marked with 0: {}", p.lamports);
        }
        steps_used += 1;
        assert!(steps_used <= all.len(), "the holders are paid within a step each at worst");
    }
    println!("T11 week two: {} holdings paid in {steps_used} steps of up to {fit}; budget {budget}", all.len());
    let mut total_paid = 0u64;
    for ((h, b0), want) in all.iter().zip(before).zip(expected) {
        let got = w.env.lamports(h) - b0;
        assert_eq!(got, want, "{h} is paid budget * its weight / the week's total");
        assert_eq!(record_of(&w, &mint, h).paid, got, "the hook's record matches the SOL paid");
        total_paid += got;
    }
    let s5 = state_of(&w, &mint);
    assert_eq!(s5.round_paid, total_paid);
    assert!(s5.round_paid <= s5.round_budget);
    assert_eq!(s5.measured as usize, all.len(), "measured again for next week while paid");

    // ── The books ──
    let end = companion_of(&w, &mint).pending_pot;
    println!("T11 ledger: {l:?}, pot at the end {end}");
    assert_eq!(l.start + l.claimed, l.to_holders + l.to_builder + l.bounties + end, "every lamport in is paid out or still in the pot");
    assert_eq!(s5.holders_paid, l.to_holders);
    assert_eq!(s5.paid_builder, l.to_builder);
    // The builder's share is a quarter of every claim, rounded down once per step that split new
    // money, never more.
    let quarter = (u128::from(l.claimed + l.start) * u128::from(BUILDER_BPS) / 10_000) as u64;
    let builder_total = s5.paid_builder + s5.owed;
    assert!(builder_total <= quarter && quarter - builder_total <= l.steps, "builder {builder_total} vs a quarter {quarter}");
    println!("T11 most compute one step used: {} CU", l.max_cu);
}
