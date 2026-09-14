//!
//! Robustness of the generated parsers against untrusted input.
//!
//! A parser built by `include_shipstern_parser!` runs on bytes taken straight off
//! the wire, so every length guard it emits has to hold for buffers the IDL never
//! described: short ones, long ones, and ones that are simply wrong. The contract
//! exercised here is narrow and total — return `Ok` or `Err`, never panic — which
//! in Rust also rules out an over-read, since a slice past the end aborts.
//!
//! Two IDLs are driven, because one is not enough to reach what matters.
//! `discriminator_guards` carries a size-only arm and a zero-width one, and the
//! zero-width arm matches `data.get(0..0)` for every buffer, so it shadows every
//! account arm behind it and no real byte window is ever sliced. `truncation_guards`
//! has no zero-width arm: its windows, 8 bytes at offset 0 and 4 at offset 8, are
//! what actually exercise truncation on the account side.
//!

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/discriminator_guards.json");
include_shipstern_parser!("../idls/truncation_guards.json");

///
/// Deterministic xorshift64* so a failure reproduces from the seed alone.
///
/// A real fuzzer belongs in a separate long-running job; this is the cheap part
/// that runs on every PR, and it is only useful if CI failures are replayable.
///
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;

        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;

        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn fill(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| (self.next_u64() >> 24) as u8).collect()
    }
}

fn path() -> shipstern_core::instruction::Path { shipstern_core::instruction::Path::new_single(0) }

///
/// Tally of parse outcomes.
///
/// Account arms are recorded by name rather than counted. A count cannot tell a
/// corpus that reached several arms from one that tripped the same unconditional
/// arm every time, and an unconditional arm is exactly what a zero-width
/// discriminator generates.
///
#[derive(Default, Debug)]
struct Outcomes {
    ix_ok: usize,
    ix_err: usize,
    acct_arms: std::collections::BTreeSet<&'static str>,
    acct_err: usize,
}

///
/// Every parse entry point the IDL generates, driven by one buffer.
///
/// Returning the tally is what keeps these tests honest: "never panicked" is
/// also true of a corpus that bounced off the first length check every time.
///
fn drive(data: &[u8], accounts: &[shipstern_core::Pubkey], seen: &mut Outcomes) {
    if discriminator_guards::resolve_instruction_default(accounts, data, &path()).is_ok() {
        seen.ix_ok += 1;
    } else {
        seen.ix_err += 1;
    }

    match discriminator_guards::DiscriminatorGuardsAccount::try_unpack(data) {
        Ok(parsed) => {
            seen.acct_arms.insert(match parsed.account {
                discriminator_guards::account::Account::SizedOnly(_) => "SizedOnly",
                discriminator_guards::account::Account::EmptyDiscriminator(_) => {
                    "EmptyDiscriminator"
                },
                discriminator_guards::account::Account::WidthMismatch(_) => "WidthMismatch",
            });
        },
        Err(_) => seen.acct_err += 1,
    }
}

///
/// The same, for the IDL whose account arms all have a real byte window.
///
fn drive_truncation(data: &[u8], seen: &mut Outcomes) {
    if truncation_guards::resolve_instruction_default(&[], data, &path()).is_ok() {
        seen.ix_ok += 1;
    } else {
        seen.ix_err += 1;
    }

    match truncation_guards::TruncationGuardsAccount::try_unpack(data) {
        Ok(parsed) => {
            seen.acct_arms.insert(match parsed.account {
                truncation_guards::account::Account::WideWindow(_) => "WideWindow",
                truncation_guards::account::Account::LateWindow(_) => "LateWindow",
            });
        },
        Err(_) => seen.acct_err += 1,
    }
}

/// A buffer matching `wideWindow`: 8-byte discriminator at offset 0, then a u64.
fn valid_wide_window() -> Vec<u8> {
    let mut data = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

    data.extend_from_slice(&42_u64.to_le_bytes());

    data
}

/// A buffer matching `lateWindow`: 8 bytes of padding, a 4-byte window, then a u8.
fn valid_late_window() -> Vec<u8> {
    let mut data = vec![0_u8; 8];

    data.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    data.push(1);

    data
}

/// A buffer that matches `sizedOnly`: u64 + u8, matched purely on length.
fn valid_sized_only() -> Vec<u8> {
    let mut data = 7_u64.to_le_bytes().to_vec();

    data.push(1);

    data
}

///
/// Every prefix of a valid buffer must be handled, including the empty one.
///
/// Truncation is the common shape of hostile input: a length guard that checks
/// `data.len() == N` but then slices `data[..M]` only fails on the prefixes
/// between M and N, which is exactly the range a single happy-path test misses.
///
#[test]
fn truncated_buffers_never_panic() {
    let valid = valid_sized_only();

    let mut seen = Outcomes::default();

    for len in 0..=valid.len() {
        drive(&valid[..len], &[], &mut seen);
    }

    // Discriminator-bearing instructions, truncated through the discriminator.
    let disc = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR;
    let offset = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR_OFFSET;

    let mut full = vec![0_u8; offset];

    full.extend_from_slice(disc);
    full.extend_from_slice(&[0xab; 16]);

    for len in 0..=full.len() {
        drive(&full[..len], &[], &mut seen);
    }

    // `full` is the discriminator at its declared offset followed by a body, so every
    // prefix at least as long as that window must satisfy it. Asserting the count rather
    // than "> 0" means an off-by-one in the guard shows up here instead of passing.
    let window = offset + disc.len();

    assert_eq!(
        seen.ix_ok,
        full.len() + 1 - window,
        "every prefix at least the window long must match: {seen:?}",
    );
}

///
/// Account arms with a real byte window, truncated through that window.
///
/// This is the case `discriminator_guards` cannot reach. Its zero-width arm matches
/// `data.get(0..0)` for every buffer and returns before any later arm is tried, so the
/// one arm behind it that slices a real window is dead code. Both arms here slice a
/// declared window, so a guard that checks a length and then indexes past it fails on
/// exactly the prefixes in between.
///
#[test]
fn account_windows_are_truncated_without_panicking() {
    let mut seen = Outcomes::default();

    for buf in [valid_wide_window(), valid_late_window()] {
        for len in 0..=buf.len() {
            drive_truncation(&buf[..len], &mut seen);
        }
    }

    let mut rng = Rng(0x7ca9_0c10_2b3d_4e5f);

    for len in 0..=40_usize {
        for _ in 0..16 {
            drive_truncation(&rng.fill(len), &mut seen);
        }
    }

    assert!(
        seen.acct_arms.contains("WideWindow") && seen.acct_arms.contains("LateWindow"),
        "both real-window arms must be reached, or account truncation is untested: {seen:?}",
    );
}

///
/// Trailing bytes past the described layout must not panic.
///
/// This asserts only that the parser stays total. Whether it *rejects* the extra
/// bytes is a separate question, pinned by `trailing_bytes_are_accepted_today`.
///
#[test]
fn oversized_buffers_never_panic() {
    let valid = valid_sized_only();

    let mut seen = Outcomes::default();

    for extra in [1_usize, 2, 7, 64, 1024, 8192] {
        let mut data = valid.clone();

        data.extend(std::iter::repeat_n(0xff_u8, extra));

        drive(&data, &[], &mut seen);
    }

    assert_eq!(
        seen.ix_ok + seen.ix_err,
        6,
        "every oversized buffer must reach the instruction parser: {seen:?}",
    );
}

///
/// Random buffers across the length range where the guards actually branch.
///
#[test]
fn garbage_buffers_never_panic() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);

    let mut seen = Outcomes::default();

    let disc = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR;
    let offset = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR_OFFSET;

    for len in 0..=72_usize {
        for _ in 0..32 {
            drive(&rng.fill(len), &[], &mut seen);

            // Uniformly random bytes satisfy a 4-byte discriminator with probability
            // 2^-32, so without priming this loop never reaches the body decoder and
            // the tally proves only that the first compare rejected everything.
            let mut primed = vec![0_u8; offset];

            primed.extend_from_slice(disc);
            primed.extend_from_slice(&rng.fill(len));

            drive(&primed, &[], &mut seen);
        }
    }

    assert!(
        seen.ix_ok >= 73 * 32,
        "priming with a valid discriminator did not reach the decoder: {seen:?}",
    );
}

///
/// A short account vector must not change the outcome.
///
/// Both instructions in `discriminator_guards` declare no accounts, so nothing here
/// indexes the vector. What this pins is that varying it is inert. The case where
/// generated accessors really do index a short vector is covered in `pump_fun.rs`,
/// whose instructions declare real account lists.
///
#[test]
fn short_account_vectors_never_panic() {
    let mut rng = Rng(0xa5a5_0f0f_1111_2222);

    let accounts: Vec<shipstern_core::Pubkey> = (0..8)
        .map(|_| {
            let bytes: [u8; 32] = rng.fill(32).try_into().expect("32 bytes");

            shipstern_core::Pubkey::new(bytes)
        })
        .collect();

    let valid = valid_sized_only();

    let mut seen = Outcomes::default();

    for n in 0..=accounts.len() {
        drive(&valid, &accounts[..n], &mut seen);

        let mut rng = Rng(0xdead_beef_0000_0001 ^ n as u64);

        for len in 0..=24_usize {
            drive(&rng.fill(len), &accounts[..n], &mut seen);
        }
    }

    assert!(
        seen.acct_arms.contains("SizedOnly"),
        "the valid buffer must still parse whatever the account vector holds: {seen:?}",
    );
}

///
/// Trailing bytes are **accepted**, not rejected. This pins that, deliberately.
///
/// The generated arms decode with `BorshDeserialize::deserialize(&mut &data[..])`,
/// which reads the prefix it needs and ignores whatever follows; rejecting the
/// remainder would need `try_from_slice`. So a parse is not proof that the buffer
/// held exactly one canonically encoded value, and a consumer that needs that
/// guarantee has to check the length itself.
///
/// The account assertion below rides the zero-width arm in this IDL rather than the
/// borsh prefix rule: the 9-byte buffer matches `sizedOnly` on length, and appending
/// 8 bytes makes it miss that arm and fall into the zero-width one. The instruction
/// assertion is the one that pins the prefix behaviour.
///
/// This is characterisation, not endorsement: the behaviour predates this test
/// and is load-bearing for accounts whose declared layout is a prefix of the real
/// account. Pinning it means a future move to canonical decoding shows up here as
/// a deliberate change rather than a silent one.
///
#[test]
fn trailing_bytes_are_accepted_today() {
    let mut data = valid_sized_only();

    assert!(
        discriminator_guards::DiscriminatorGuardsAccount::try_unpack(&data).is_ok(),
        "the exact-length buffer must parse, or this test proves nothing",
    );

    data.extend_from_slice(&[0xff; 8]);

    assert!(
        discriminator_guards::DiscriminatorGuardsAccount::try_unpack(&data).is_ok(),
        "trailing bytes are currently accepted; update this test if that changes",
    );

    let disc = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR;
    let offset = discriminator_guards::Instructions::WELL_FORMED_IX_DISCRIMINATOR_OFFSET;

    let mut ix = vec![0_u8; offset];

    ix.extend_from_slice(disc);
    ix.extend_from_slice(&[0xaa; 16]);

    assert!(
        discriminator_guards::resolve_instruction_default(&[], &ix, &path()).is_ok(),
        "instruction decoding ignores trailing bytes the same way",
    );
}
