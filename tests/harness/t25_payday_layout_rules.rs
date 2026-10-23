//! T25 — Payday layouts against the real accounts, and the Workshop's source rules.

use anchor_lang::prelude::Pubkey;
use anchor_lang::{AccountSerialize, Space};
use bordrless_companion::state::{Companion, GameKind, NativeSol, QuoteCutClaimed, Split};
use payday_script::layout::{read_companion_beneficiary, read_companion_mint, read_pending_pot};
use payday_script::ScriptState;

fn pubkey(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

fn account_bytes<T: AccountSerialize>(acct: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    acct.try_serialize(&mut buf).expect("try_serialize");
    buf
}

fn make_companion() -> Companion {
    Companion {
        version: 1,
        bump: 200,
        creator_bump: 201,
        mint: pubkey(7),
        beneficiary: pubkey(8),
        split: Split {
            buyback_bps: 5_000,
            holders_bps: 3_000,
            beneficiary_bps: 2_000,
        },
        bounty_bps: 50,
        max_buyback: 1_000_000_000,
        buyback_interval: 300,
        vest_secs: 86400,
        launched: true,
        launched_at: 1_760_000_000,
        dev_tokens: 1_000_000,
        dev_released: 500_000,
        pending_buyback: 111_111,
        pending_holders: 222_222,
        pending_beneficiary: 333_333,
        last_buyback_at: 1_760_000_300,
        claimed_total: 999_999,
        spent_total: 888_888,
        burned_total: 777_777,
        shared_total: 666_666,
        paid_beneficiary_total: 555_555,
        bounties_total: 444_444,
        reference_price: 12_345_678_901_234_567_890_u128,
        reference_at: 1_760_000_600,
        game_hook: Pubkey::default(),
        pot_bps: 0,
        // ← the field under test
        pending_pot: 0x_DEAD_BEEF_CAFE_1234_u64,
        round_secs: 0,
        stranded_burned_at: 0,
        game_kind: GameKind::Lottery,
        pot_locked: 0,
        mode: 2, // MODE_SCRIPT
        quote_cut_claimed: QuoteCutClaimed {
            amount: 42_000,
            stored: true,
        },
        native_sol: NativeSol {
            on: true,
            stored: true,
        },
    }
}

#[test]
fn companion_beneficiary_mint_and_pot_read_at_their_offsets() {
    let bytes = account_bytes(&make_companion());
    assert_eq!(read_companion_mint(&bytes), Some(pubkey(7)));
    assert_eq!(read_companion_beneficiary(&bytes), Some(pubkey(8)));
    assert_eq!(read_pending_pot(&bytes), Some(0x_DEAD_BEEF_CAFE_1234_u64));
}

#[test]
fn script_state_is_185_bytes_at_its_offsets() {
    assert_eq!(8 + ScriptState::INIT_SPACE, 185, "keeper.json stateLen");
    let s = ScriptState {
        version: 1,
        mint: pubkey(0xBB),
        last_pot: 1,
        owed: 2,
        holder_pile: 3,
        paid_builder: 4,
        holders_paid: 5,
        steps: 6,
        round: 7,
        w_prev: 8,
        w_cur: 9,
        round_budget: 10,
        round_paid: 11,
        measured: 12,
        round_start: 13,
        reserved: [0; 32],
    };
    let d = account_bytes(&s);
    assert_eq!(d.len(), 185);
    assert_eq!(d[8], 1);
    assert_eq!(&d[9..41], pubkey(0xBB).as_ref());
    let u64_at = |at: usize| u64::from_le_bytes(d[at..at + 8].try_into().unwrap());
    let u128_at = |at: usize| u128::from_le_bytes(d[at..at + 16].try_into().unwrap());
    let u32_at = |at: usize| u32::from_le_bytes(d[at..at + 4].try_into().unwrap());
    assert_eq!([u64_at(41), u64_at(49), u64_at(57), u64_at(65), u64_at(73), u64_at(81)], [1, 2, 3, 4, 5, 6]);
    assert_eq!(u32_at(89), 7);
    assert_eq!((u128_at(93), u128_at(109)), (8, 9));
    assert_eq!((u64_at(125), u64_at(133)), (10, 11));
    assert_eq!(u32_at(141), 12);
    assert_eq!(u64_at(145), 13);
    assert_eq!(&d[153..], &[0u8; 32][..]);
    // keeper.json `view` reads these offsets live on the coin's page.
    let keeper = include_str!("../../script_variants/payday/keeper.json").replace([' ', '\n'], "");
    for (label, offset, width, as_) in [
        ("Paidtoholders", 73, 8, "lamports"),
        ("Holderpot", 57, 8, "lamports"),
        ("Thisweek'spayout", 125, 8, "lamports"),
        ("Paidthisweek", 133, 8, "lamports"),
        ("Holderscounted", 141, 4, "number"),
        ("Weekbegan", 145, 8, "time"),
    ] {
        assert!(
            keeper.contains(&format!("\"label\":\"{label}\",\"account\":\"script\",\"offset\":{offset},\"width\":{width},\"as\":\"{as_}\"")),
            "view {label} at {offset}"
        );
    }
    assert_eq!(keeper.matches("\"label\"").count(), 6, "at most 6 view figures");
    for label in ["Paid to holders", "Holder pot", "This week's payout", "Paid this week", "Holders counted", "Week began"] {
        assert!(label.len() <= 24, "a view label is at most 24 characters");
    }
}

#[test]
fn the_script_calls_no_program_and_the_notes_fit() {
    let code: String = SCRIPT_SOURCES.iter().map(|s| blanked(s)).collect();
    for forbidden in ["invoke(", "invoke_signed", "CpiContext", "swap_v2", "burn_with", "add_liquidity", "wrap_sol"] {
        assert!(!code.contains(forbidden), "script source contains `{forbidden}`: Payday never trades, burns or adds liquidity");
    }
    let notes = include_str!("../../variants/payday/notes.txt").trim();
    assert!(notes.chars().count() <= 600, "{} characters", notes.chars().count());
}

// ─── the Workshop's source rules, as the hook and companion briefs state them ─────────────────

/// A source with its line comments and string literals blanked, roughly as the Workshop reads it.
fn blanked(src: &str) -> String {
    src.lines()
        .map(|l| {
            let code = l.split("//").next().unwrap_or("");
            let mut out = String::new();
            let mut in_str = false;
            for c in code.chars() {
                if c == '"' {
                    in_str = !in_str;
                } else if !in_str {
                    out.push(c);
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const HOOK_SOURCES: [&str; 2] = [
    include_str!("../../hook_variants/payday/src/lib.rs"),
    include_str!("../../hook_variants/payday/src/clock.rs"),
];
const SCRIPT_SOURCES: [&str; 6] = [
    include_str!("../../script_variants/payday/src/lib.rs"),
    include_str!("../../script_variants/payday/src/params.rs"),
    include_str!("../../script_variants/payday/src/math.rs"),
    include_str!("../../script_variants/payday/src/layout.rs"),
    include_str!("../../script_variants/payday/src/record.rs"),
    include_str!("../../script_variants/payday/src/holders.rs"),
];

#[test]
fn the_hook_calls_no_program_and_moves_no_sol_in_its_own_source() {
    let code: String = HOOK_SOURCES.iter().map(|s| blanked(s)).collect();
    for forbidden in ["invoke(", "invoke_signed", "CpiContext", "cpi::", "unsafe", "asm!", "macro_rules!", "cfg_attr", "extern", "no_mangle", "include_str!", "include_bytes!", "env!", "std::fs", "std::net", "std::process", "std::env", "std::thread", "realloc", "close =", "sub_lamports", "add_lamports", "lamports.borrow_mut"] {
        assert!(!code.contains(forbidden), "hook source contains `{forbidden}`");
    }
    assert_eq!(code.matches("declare_id!").count(), 1);
    assert_eq!(code.matches("#[program]").count(), 1);
    assert!(code.contains("pub fn prepare("));
}

#[test]
fn the_script_keeps_the_common_rules() {
    let code: String = SCRIPT_SOURCES.iter().map(|s| blanked(s)).collect();
    for forbidden in ["unsafe", "asm!", "macro_rules!", "cfg_attr", "extern", "no_mangle", "include_str!", "include_bytes!", "env!", "std::fs", "std::net", "std::process", "std::env", "std::thread"] {
        assert!(!code.contains(forbidden), "script source contains `{forbidden}`");
    }
    assert_eq!(code.matches("declare_id!").count(), 1);
    assert_eq!(code.matches("#[program]").count(), 1);
    let bytes: usize = SCRIPT_SOURCES.iter().map(|s| s.len()).sum();
    assert!(bytes <= 120_000, "{bytes} bytes");
    let hook_bytes: usize = HOOK_SOURCES.iter().map(|s| s.len()).sum();
    assert!(hook_bytes <= 120_000);
}

/// The Workshop's three forbidden words, as whole words, with the one allowed phrase removed
/// first; over every text that ships or is sent out.
#[test]
fn never_the_three_forbidden_words() {
    let texts: Vec<&str> = HOOK_SOURCES
        .iter()
        .chain(SCRIPT_SOURCES.iter())
        .copied()
        .chain([
            include_str!("../../variants/payday/notes.txt"),
            include_str!("../../script_variants/payday/keeper.json"),
        ])
        .collect();
    for src in texts {
        let lower = src.to_lowercase().replace("not audited", "");
        let words: Vec<String> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect();
        for forbidden in ["audited", "safe", "guaranteed"] {
            assert!(!words.iter().any(|w| w == forbidden), "found `{forbidden}`");
        }
    }
}
