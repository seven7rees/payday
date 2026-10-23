//! T24 — Payday ABI pins: every address, discriminator, account list and encoding the pair
//! hard-codes or builds by hand, against the programs' own crates at the pinned revision.

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AccountSerialize, AnchorSerialize};

fn borsh<T: AnchorSerialize>(x: &T) -> Vec<u8> {
    let mut v = Vec::new();
    x.serialize(&mut v).unwrap();
    v
}

use bordrless_companion::constants::{
    COMPANION_INIT_DISCRIMINATOR, COMPANION_STEP_DISCRIMINATOR, MAX_PAYOUTS_PER_STEP,
    PAYOUT_AUTHORITY, PAYOUT_BOUNTY_FULL, RECORD_PAYOUT_DISCRIMINATOR, SCRIPT_BOUNTY,
};
use bordrless_companion::state::{PayoutAnswer, PayoutEntry, WalletPayoutAnswer, WalletPayoutEntry};

fn key(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

// ─── the hook ────────────────────────────────────────────────────────────────

#[test]
fn the_hooks_program_ids_and_seeds_are_the_protocols() {
    assert_eq!(payday_hook::SWAP_ID, bordrless_swap::ID);
    assert_eq!(payday_hook::BRIDGED_SOL_MINT, bordrless_swap::constants::BRIDGED_SOL_MINT);
    assert_eq!(payday_hook::PAYOUT_AUTHORITY, PAYOUT_AUTHORITY);
    assert_eq!(bordrless_game::LAUNCH_PROGRAM_ID, bordrless_launch::ID);
    assert_eq!(bordrless_game::TOKEN_PROGRAM_ID, bordrless_token::ID);
    assert_eq!(bordrless_game::COMPANION_PROGRAM_ID, bordrless_companion::ID);
    let mint = key(3);
    assert_eq!(
        payday_hook::pool_address(&mint),
        bordrless_launch::client::pool_address(&mint, &bordrless_swap::constants::BRIDGED_SOL_MINT, 30)
    );
    assert_eq!(payday_hook::own_hook_authority(), bordrless_hook::hook_authority(&payday_hook::ID).0);
}

#[test]
fn the_registry_lists_only_the_state() {
    use bordrless_hook::{AccountSource, Seed};
    let list = payday_hook::extra_accounts(&key(9));
    assert_eq!(list.accounts.len(), 1, "no fixed keys: the script moves no tokens");
    assert!(list.accounts[0].writable);
    assert_eq!(
        list.accounts[0].source,
        AccountSource::Pda { program: payday_hook::ID, seeds: vec![Seed::Literal(b"state".to_vec()), Seed::Account(1)] }
    );
}

#[test]
fn record_payout_is_called_as_the_companion_builds_it() {
    use anchor_lang::{Discriminator, InstructionData, ToAccountMetas};
    let (mint, owner, hook) = (key(1), key(2), payday_hook::ID);
    let holding = bordrless_token::client::holding_address(&mint, &owner);
    let reference = bordrless_companion::client::record_payout(hook, mint, holding, 77, 9);
    assert_eq!(&reference.data[..8], &RECORD_PAYOUT_DISCRIMINATOR[..]);
    assert_eq!(payday_hook::instruction::RecordPayout::DISCRIMINATOR, &RECORD_PAYOUT_DISCRIMINATOR[..]);
    assert_eq!(payday_hook::instruction::RecordPayout { lamports: 77, tag: 9 }.data(), reference.data);
    let ours = payday_hook::accounts::RecordPayout {
        payout_authority: PAYOUT_AUTHORITY,
        mint,
        holding,
        state: Pubkey::find_program_address(&[b"state", mint.as_ref()], &hook).0,
        hook_authority: payday_hook::own_hook_authority(),
        token_program: bordrless_token::ID,
        token_event_authority: bordrless_token::client::event_authority(),
    }
    .to_account_metas(None);
    let ours: Vec<(Pubkey, bool)> = ours.iter().map(|m| (m.pubkey, m.is_writable)).collect();
    let theirs: Vec<(Pubkey, bool)> = reference.accounts.iter().map(|m| (m.pubkey, m.is_writable)).collect();
    assert_eq!(ours, theirs);
    assert_eq!(bordrless_game::TOKEN_EVENT_AUTHORITY, bordrless_token::client::event_authority());
}

#[test]
fn the_callbacks_and_quote_cut_have_the_protocols_discriminators() {
    use anchor_lang::Discriminator;
    use bordrless_hook::discriminators as d;
    assert_eq!(payday_hook::instruction::BeforeTransfer::DISCRIMINATOR, &d::BEFORE_TRANSFER[..]);
    assert_eq!(payday_hook::instruction::BeforeBurn::DISCRIMINATOR, &d::BEFORE_BURN[..]);
    assert_eq!(payday_hook::instruction::QuoteCut::DISCRIMINATOR, my_hook::instruction::QuoteCut::DISCRIMINATOR, "same name as the MOON/WICK hook's");
}

#[test]
fn the_hook_reads_a_real_holdings_hook_data() {
    use bordrless_token::state::Holding;
    for delegate in [None, Some(key(8))] {
        let h = Holding {
            version: 1,
            bump: 250,
            mint: key(1),
            owner: key(2),
            amount: 123,
            delegate,
            delegated_amount: 0,
            frozen: false,
            hook_data: [0xAB; 64],
            reserved: [0; 16],
        };
        let mut data = Vec::new();
        h.try_serialize(&mut data).unwrap();
        let v = bordrless_game::parse_holding(&data).expect("parses");
        assert_eq!((v.mint, v.owner, v.amount, v.hook_data), (key(1), key(2), 123, [0xAB; 64]));
    }
}

// ─── the script ──────────────────────────────────────────────────────────────

#[test]
fn the_scripts_program_ids_discriminators_and_bounty() {
    use anchor_lang::Discriminator;
    assert_eq!(payday_script::COMPANION_ID, bordrless_companion::ID);
    assert_eq!(payday_script::instruction::CompanionInit::DISCRIMINATOR, &COMPANION_INIT_DISCRIMINATOR[..]);
    assert_eq!(payday_script::instruction::CompanionStep::DISCRIMINATOR, &COMPANION_STEP_DISCRIMINATOR[..]);
    assert_eq!(payday_script::params::STEP_BOUNTY, SCRIPT_BOUNTY, "0.00002 SOL a step");
    assert_eq!(payday_script::math::COMPANION_PAYOUT_BOUNTY_FULL, PAYOUT_BOUNTY_FULL);
    assert!(payday_script::params::PAYOUT_MAX_ENTRIES <= MAX_PAYOUTS_PER_STEP, "the companion's own limit");
}

#[test]
fn the_payout_answers_are_the_companions_borsh() {
    let ours = payday_script::math::encode_payout_answer(&[(3, 1_000, 7), (0, 0, 7)]);
    let theirs = borsh(&PayoutAnswer {
        version: 1,
        entries: vec![PayoutEntry { account: 3, lamports: 1_000, tag: 7 }, PayoutEntry { account: 0, lamports: 0, tag: 7 }],
    });
    assert_eq!(ours, theirs);
    assert_eq!(ours.len(), 1 + 4 + 13 * 2);
    let w = payday_script::math::encode_wallet_answer(&key(5), 9, 2);
    let theirs = borsh(&WalletPayoutAnswer { version: 2, entries: vec![WalletPayoutEntry { to: key(5), lamports: 9, tag: 2 }] });
    assert_eq!(&w[..], &theirs[..]);
}

#[test]
fn builder_wallet_is_the_wallet_file_and_the_keeper_matches_the_code() {
    use std::str::FromStr;
    let file = include_str!("../../WALLET.txt").trim();
    assert_eq!(payday_script::params::BUILDER_WALLET, Pubkey::from_str(file).unwrap());
    assert!(payday_script::params::BUILDER_WALLET.is_on_curve());
    let keeper = include_str!("../../script_variants/payday/keeper.json");
    assert!(keeper.contains(file), "keeper.json wallets must name the builder wallet");
    let flat = keeper.replace([' ', '\n'], "");
    assert!(flat.contains("\"payouts\":true"), "the companion's payout accounts come first");
    assert!(flat.contains("\"swapAccounts\":false"), "the script never trades");
    assert!(flat.contains("\"hookAccounts\":false"), "the script moves no tokens");
    assert!(flat.contains("\"mintWritable\":false"), "the script never burns");
    assert!(flat.contains("\"stepSecs\":300"));
    // The companion's step and holding caps stay open: the script's own piles bound every payout,
    // and a lower step cap would strand a holder whose weekly share exceeds it.
    assert!(flat.contains("\"stepBps\":10000,\"holdingBps\":10000,\"windowBps\":10000,\"windowSecs\":86400"));
    // Four holdings with their owners fit one step transaction beside the payout accounts, the
    // live protocol lookup table and a compute limit (T11 measures it); the script pays up to
    // PAYOUT_MAX_ENTRIES, so a larger table needs only this number raised.
    assert!(flat.contains("\"withSwapAccounts\":null,\"withoutSwapAccounts\":4"));
    assert!(4 <= payday_script::params::PAYOUT_MAX_ENTRIES);
    assert!(flat.contains("\"roundSecs\":604800"));
    assert!(flat.contains(&format!("\"stateLen\":{}", 8 + <payday_script::ScriptState as anchor_lang::Space>::INIT_SPACE)));
    assert!(flat.contains("\"minBalance\":\"1000000000\""), "1,000 coins at 6 decimals");
    assert_eq!(
        payday_script::params::MIN_HOLDING,
        1_000_000_000,
        "the script's least balance is keeper.json's minBalance: the code enforces what the keeper brings"
    );
    assert!(flat.contains(&format!("\"minBalance\":\"{}\"", payday_script::params::MIN_HOLDING)));
    assert_eq!(payday_script::params::ROUND_SECS, payday_hook::clock::ROUND_SECS, "hook and script count the same weeks");
    assert!(flat.contains("\"offset\":1,\"width\":4,\"compare\":\"ne\",\"against\":{\"epoch\":{\"secs\":604800"), "the round field, not this week's epoch");
    assert!(flat.contains("\"launcherShareBps\":5000"));
    assert_eq!(payday_script::params::ROUND_SECS, 604_800, "ROUND_SECS equals the keeper's epoch");
    assert_eq!(
        payday_script::params::STEPS_PER_ROUND,
        u64::from(payday_script::params::ROUND_SECS) / 300,
        "the bounty reserve counts the steps a round holds at stepSecs"
    );
}

#[test]
fn reserved_keys_are_the_companions() {
    // The script counts a reserved owner for 0 instead of answering a payment the companion would
    // refuse (`payable`): the two lists must be the same keys in the same order.
    assert_eq!(payday_script::params::RESERVED_KEYS, bordrless_companion::constants::RESERVED_KEYS);
    assert!(
        payday_script::params::RESERVED_KEYS.iter().any(|k| k.is_on_curve()),
        "some reserved keys pass the wallet test, which is why the script lists them"
    );
}
