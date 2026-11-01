//! Fuzz tests for the vault-core chat/mail parsers.
//!
//! Contract under test: **no input may panic a parser** — malformed input
//! must surface as an `Err` (Telegram's JSON) or as a `Collected` with
//! failures counted (WhatsApp's lossy text parse, mbox), never as a panic,
//! abort, or hang. Every crash found here is a product bug to fix, not a
//! test to relax.
//!
//! These harnesses run in plain `cargo test` (stable toolchain, no
//! nightly/libFuzzer needed) using a deterministic seeded RNG plus a
//! corpus of hand-crafted edge cases, so:
//! - a failure reproduces exactly: the case description carries its seed,
//! - CI cost is bounded and the suite needs no special runner.
//!
//! The corpus exercises the classes of malformed input the sources can
//! really meet: truncated files, mixed-up formats, Unicode edge cases and
//! bidi marks, extremely long lines, empty/BOM-only files, duplicate JSON
//! keys, bogus timezone markers, and hostile MIME structure (deeply
//! nested multiparts — see the `mbox` depth guard regression).
//!
//! Iteration counts can be raised for local fuzzing campaigns:
//! `MC_FUZZ_ITERS=200000 cargo test -p vault-core --test fuzz`.

mod corpus;
mod gen_mbox;
mod gen_signal;
mod gen_telegram;
mod gen_whatsapp;
mod harness;
mod mutate;
mod rng;

use harness::check_no_panic;

/// Default structured-fuzz iterations per parser in CI. Deliberately
/// modest: the corpus carries the hand-crafted worst cases, the random
/// rounds sample around them.
const ITERS: usize = 4000;

fn iterations() -> usize {
    std::env::var("MC_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ITERS)
}

// ---------------------------------------------------------------------------
// WhatsApp .txt export
// ---------------------------------------------------------------------------

#[test]
fn whatsapp_fuzz_never_panics() {
    for (idx, (desc, input)) in corpus::whatsapp_cases().into_iter().enumerate() {
        check_no_panic("whatsapp", idx, &desc, &input, || {
            let _ = vault_core::whatsapp::parse_whatsapp(&input, "fuzz");
            let _ = vault_core::whatsapp::is_whatsapp_export(&input);
        });
    }
    let mut rng = rng::Rng::new(0x5a68_6174_7361_7070); // "zhatsapp"
    for i in 0..iterations() {
        let seed = rng.next_u64();
        let mut input = gen_whatsapp::export(&mut rng::Rng::new(seed));
        if rng.bool() {
            mutate::apply(&mut input, &mut rng);
        }
        check_no_panic(
            "whatsapp",
            i,
            &format!("random seed={seed:#x}"),
            &input,
            || {
                let _ = vault_core::whatsapp::parse_whatsapp(&input, "fuzz");
                let _ = vault_core::whatsapp::is_whatsapp_export(&input);
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Telegram result.json
// ---------------------------------------------------------------------------

#[test]
fn telegram_fuzz_never_panics() {
    for (idx, (desc, input)) in corpus::telegram_cases().into_iter().enumerate() {
        check_no_panic("telegram", idx, &desc, &input, || {
            let _ = vault_core::telegram::parse_telegram(&input, "fuzz");
            let _ = vault_core::telegram::is_telegram_export(&input);
        });
    }
    let mut rng = rng::Rng::new(0x7465_6c65_6772_616d); // "telegram"
    for i in 0..iterations() {
        let seed = rng.next_u64();
        let mut input = gen_telegram::export(&mut rng::Rng::new(seed));
        if rng.bool() {
            mutate::apply(&mut input, &mut rng);
        }
        check_no_panic(
            "telegram",
            i,
            &format!("random seed={seed:#x}"),
            &input,
            || {
                let _ = vault_core::telegram::parse_telegram(&input, "fuzz");
                let _ = vault_core::telegram::is_telegram_export(&input);
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Signal conversation JSON export
// ---------------------------------------------------------------------------

#[test]
fn signal_fuzz_never_panics() {
    for (idx, (desc, input)) in corpus::signal_cases().into_iter().enumerate() {
        check_no_panic("signal", idx, &desc, &input, || {
            let _ = vault_core::signal::parse_signal(&input, "fuzz");
            let _ = vault_core::signal::is_signal_export(&input);
        });
    }
    let mut rng = rng::Rng::new(0x7369_676e_616c_696d); // "signalim"
    for i in 0..iterations() {
        let seed = rng.next_u64();
        let mut input = gen_signal::export(&mut rng::Rng::new(seed));
        if rng.bool() {
            mutate::apply(&mut input, &mut rng);
        }
        check_no_panic(
            "signal",
            i,
            &format!("random seed={seed:#x}"),
            &input,
            || {
                let _ = vault_core::signal::parse_signal(&input, "fuzz");
                let _ = vault_core::signal::is_signal_export(&input);
            },
        );
    }
}

// ---------------------------------------------------------------------------
// .mbox files
// ---------------------------------------------------------------------------

#[test]
fn mbox_fuzz_never_panics() {
    for (idx, (desc, input)) in corpus::mbox_cases().into_iter().enumerate() {
        check_no_panic("mbox", idx, &desc, &input, || {
            let _ = vault_core::mbox::parse_mbox(&input, "fuzz");
        });
    }
    let mut rng = rng::Rng::new(0x6d62_6f78_6669_6c65); // "mboxfile"
    for i in 0..iterations() {
        let seed = rng.next_u64();
        let mut input = gen_mbox::file(&mut rng::Rng::new(seed));
        if rng.bool() {
            mutate::apply(&mut input, &mut rng);
        }
        check_no_panic("mbox", i, &format!("random seed={seed:#x}"), &input, || {
            let _ = vault_core::mbox::parse_mbox(&input, "fuzz");
        });
    }
}

// ---------------------------------------------------------------------------
// Format detection over every class of input
// ---------------------------------------------------------------------------

#[test]
fn detect_chat_format_fuzz_never_panics() {
    // Detection runs over every corpus input and fresh random exports of
    // all four shapes (including cross-format mixups), and must always
    // answer instead of crashing.
    let mut cases = corpus::whatsapp_cases();
    cases.extend(corpus::telegram_cases());
    cases.extend(corpus::signal_cases());
    cases.extend(corpus::mbox_cases());
    cases.extend(corpus::mixed_format_cases());
    for (idx, (desc, input)) in cases.into_iter().enumerate() {
        check_no_panic("detect", idx, &desc, &input, || {
            let _ = vault_core::chat_export::detect_chat_format(&input);
        });
    }
    let mut rng = rng::Rng::new(0x0064_6574_6563_7421); // "detect!"
    for i in 0..iterations() {
        let seed = rng.next_u64();
        let mut input = match rng.below(4) {
            0 => gen_whatsapp::export(&mut rng::Rng::new(seed)),
            1 => gen_telegram::export(&mut rng::Rng::new(seed)),
            2 => gen_signal::export(&mut rng::Rng::new(seed)),
            _ => gen_mbox::file(&mut rng::Rng::new(seed)),
        };
        mutate::apply(&mut input, &mut rng);
        check_no_panic(
            "detect",
            i,
            &format!("random seed={seed:#x}"),
            &input,
            || {
                let _ = vault_core::chat_export::detect_chat_format(&input);
            },
        );
    }
}
