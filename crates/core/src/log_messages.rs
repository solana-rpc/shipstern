//! Utilities for splitting and assigning Solana transaction log messages
//! to individual instructions.

use super::instruction::InstructionUpdate;

/// Structural shape of a Solana log line. Matching is on the line's structure,
/// not substrings, so a `Program log:` payload containing `invoke [2]` is `Other`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LogLineKind {
    /// `Program <pubkey> invoke [<n>]`
    Invoke,
    /// `Program <pubkey> success` or `Program <pubkey> failed: ...`
    Close,
    /// Anything else.
    Other,
}

/// Classify a Solana log line.
///
/// A line is only an `Invoke` or `Close` if its second whitespace-separated
/// token is a real pubkey-shaped string — i.e. *not* one of the reserved
/// runtime tags `log:`, `data:`, `return:`. This rejects adversarial
/// `Program log: ... invoke [2]` payloads from being misclassified.
pub(crate) fn classify_log_line(line: &str) -> LogLineKind {
    let Some(rest) = line.strip_prefix("Program ") else {
        return LogLineKind::Other;
    };

    // Second token must be a pubkey, not a runtime-reserved tag.
    let Some((second, tail)) = rest.split_once(' ') else {
        return LogLineKind::Other;
    };
    if matches!(second, "log:" | "data:" | "return:") {
        return LogLineKind::Other;
    }

    // Invoke shape: `invoke [<digits>]`.
    if let Some(after_invoke) = tail.strip_prefix("invoke [")
        && let Some(digits) = after_invoke.split(']').next()
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        return LogLineKind::Invoke;
    }

    // Close shape: literal `success` or starts with `failed:`.
    if tail == "success" || tail.starts_with("failed:") {
        return LogLineKind::Close;
    }

    LogLineKind::Other
}

///
/// Walk the transaction log messages and assign each instruction's log slice.
///
/// Solana logs follow a strict nesting structure:
///
/// ```text
/// Program ABC invoke [1]      ← outer instruction 0
///   Program log: hello
///   Program DEF invoke [2]    ← inner instruction 0.0
///     Program log: inner
///   Program DEF success
/// Program ABC success
/// ```
///
/// Each instruction receives all log lines from its `invoke` through its
/// `success`/`failed` (inclusive), including lines from inner instructions.
///
pub(crate) fn assign_log_messages(logs: &[String], outer: &mut [InstructionUpdate]) {
    if logs.is_empty() || outer.is_empty() {
        return;
    }

    let mut cursor = 0;

    for ix in outer.iter_mut() {
        cursor = assign_logs_recursive(logs, cursor, ix);
    }
}

/// Recursively assign log lines to an instruction and its inner instructions.
///
/// Returns the new cursor position (index of the first unconsumed log line).
fn assign_logs_recursive(logs: &[String], start: usize, ix: &mut InstructionUpdate) -> usize {
    // Find the invoke line for this instruction.
    let Some(invoke_pos) = find_invoke(logs, start) else {
        return start;
    };

    // Walk forward from the invoke line, tracking depth to find the matching
    // success/failed line that closes this instruction.
    let mut depth: u32 = 1;
    let mut pos = invoke_pos + 1;
    let mut inner_idx = 0;

    while pos < logs.len() && depth > 0 {
        match classify_log_line(&logs[pos]) {
            LogLineKind::Invoke => {
                // Entering a nested instruction — assign logs to the
                // corresponding inner instruction if one exists.
                if inner_idx < ix.inner.len() {
                    pos = assign_logs_recursive(logs, pos, &mut ix.inner[inner_idx]);
                    inner_idx += 1;
                    continue;
                }
                depth += 1;
            },
            LogLineKind::Close => {
                depth -= 1;
                if depth == 0 {
                    ix.log_range = invoke_pos..(pos + 1);
                    return pos + 1;
                }
            },
            LogLineKind::Other => {},
        }

        pos += 1;
    }

    // Fallback: if we never found a matching close, take everything from invoke to end.
    ix.log_range = invoke_pos..pos;

    pos
}

/// Find the next real `Program <pubkey> invoke [N]` line at or after `start`,
/// ignoring adversarial `Program log:` payloads that contain that substring.
fn find_invoke(logs: &[String], start: usize) -> Option<usize> {
    logs.iter()
        .enumerate()
        .skip(start)
        .find(|(_, line)| classify_log_line(line) == LogLineKind::Invoke)
        .map(|(i, _)| i)
}

///
/// Split transaction logs by outer instruction index.
///
/// Returns a vec where entry `i` contains the log lines for outer instruction `i`
/// (from its `Program ... invoke [1]` through its `Program ... success`/`failed`).
///
#[must_use]
pub fn split_logs_by_outer_ix(logs: &[String]) -> Vec<Vec<String>> {
    let mut result: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut depth: u32 = 0;

    for line in logs {
        let kind = classify_log_line(line);
        let is_outer_invoke = matches!(kind, LogLineKind::Invoke) && line.contains(" invoke [1]");

        if is_outer_invoke && depth == 0 {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }

            depth = 1;

            current.push(line.clone());
        } else if depth > 0 {
            match kind {
                LogLineKind::Invoke => depth += 1,
                LogLineKind::Close => depth -= 1,
                LogLineKind::Other => {},
            }

            current.push(line.clone());
        }
    }

    if !current.is_empty() {
        result.push(current);
    }

    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        instruction::{InstructionShared, Path},
        KeyBytes,
    };

    #[test]
    fn test_assign_log_messages() {
        let logs: Vec<String> = vec![
            "Program ABC invoke [1]",
            "Program log: outer hello",
            "Program DEF invoke [2]",
            "Program log: inner hello",
            "Program DEF success",
            "Program ABC consumed 5000 units",
            "Program ABC success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        // Build a simple instruction tree: outer ix with one inner ix
        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![InstructionUpdate {
                program: KeyBytes::new([2; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::from(vec![0, 0]),
                log_range: 0..0,
            }],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        // Outer instruction gets all logs (invoke through success, inclusive)
        assert_eq!(outer[0].log_messages().len(), 7);
        assert_eq!(outer[0].log_messages()[0], "Program ABC invoke [1]");
        assert_eq!(outer[0].log_messages()[6], "Program ABC success");

        // Inner instruction gets its own slice
        assert_eq!(outer[0].inner[0].log_messages().len(), 3);
        assert_eq!(
            outer[0].inner[0].log_messages()[0],
            "Program DEF invoke [2]"
        );
        assert_eq!(outer[0].inner[0].log_messages()[2], "Program DEF success");
    }

    #[test]
    fn test_assign_log_messages_multiple_outer() {
        let logs: Vec<String> = vec![
            "Program ABC invoke [1]",
            "Program log: first",
            "Program ABC success",
            "Program DEF invoke [1]",
            "Program log: second",
            "Program DEF success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![
            InstructionUpdate {
                program: KeyBytes::new([1; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::new_single(0),
                log_range: 0..0,
            },
            InstructionUpdate {
                program: KeyBytes::new([2; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::new_single(1),
                log_range: 0..0,
            },
        ];

        assign_log_messages(&shared.log_messages, &mut outer);

        assert_eq!(outer[0].log_messages().len(), 3);
        assert_eq!(outer[0].log_messages()[1], "Program log: first");

        assert_eq!(outer[1].log_messages().len(), 3);
        assert_eq!(outer[1].log_messages()[1], "Program log: second");
    }

    /// Regression for #186: a handler asking for its own program's logs must not
    /// see lines from inner CPIs.
    #[test]
    fn direct_log_messages_excludes_inner_cpi_lines() {
        let logs: Vec<String> = vec![
            "Program RAY invoke [1]",
            "Program log: ray_log: swap-base-in",
            "Program TOKEN invoke [2]",
            "Program log: Instruction: Transfer",
            "Program TOKEN success",
            "Program log: ray_log: swap done",
            "Program RAY success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![InstructionUpdate {
                program: KeyBytes::new([2; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::from(vec![0, 0]),
                log_range: 0..0,
            }],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        let direct: Vec<&str> = outer[0].direct_log_messages().collect();

        assert_eq!(
            direct,
            vec![
                "Program RAY invoke [1]",
                "Program log: ray_log: swap-base-in",
                "Program log: ray_log: swap done",
                "Program RAY success",
            ],
            "direct_log_messages must skip inner CPI lines"
        );

        let inner_direct: Vec<&str> = outer[0].inner[0].direct_log_messages().collect();
        assert_eq!(
            inner_direct,
            vec![
                "Program TOKEN invoke [2]",
                "Program log: Instruction: Transfer",
                "Program TOKEN success",
            ],
            "inner ix direct_log_messages contains only its own lines"
        );
    }

    /// Edge case: inner CPI ends with `failed:` rather than `success`. The
    /// closing line still pops depth, so the outer program's trailing line
    /// must be visible.
    #[test]
    fn direct_log_messages_handles_failed_inner_cpi() {
        let logs: Vec<String> = vec![
            "Program RAY invoke [1]",
            "Program TOKEN invoke [2]",
            "Program TOKEN failed: custom error",
            "Program log: ray_log: recovering",
            "Program RAY success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![InstructionUpdate {
                program: KeyBytes::new([2; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::from(vec![0, 0]),
                log_range: 0..0,
            }],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        let direct: Vec<&str> = outer[0].direct_log_messages().collect();
        assert_eq!(direct, vec![
            "Program RAY invoke [1]",
            "Program log: ray_log: recovering",
            "Program RAY success",
        ]);
    }

    /// With no inner CPIs, `direct_log_messages()` equals `log_messages()`.
    #[test]
    fn direct_log_messages_flat_ix_equals_log_messages() {
        let logs: Vec<String> = vec![
            "Program SOLO invoke [1]",
            "Program log: nothing nested here",
            "Program log: another line",
            "Program SOLO success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        let full: Vec<&str> = outer[0].log_messages().iter().map(String::as_str).collect();
        let direct: Vec<&str> = outer[0].direct_log_messages().collect();
        assert_eq!(direct, full, "flat ix: direct must equal full");
    }

    /// A -> B -> C: A's direct view skips every line while B or C is on the stack.
    #[test]
    fn direct_log_messages_filters_three_deep_cpi() {
        let logs: Vec<String> = vec![
            "Program A invoke [1]",
            "Program log: A start",
            "Program B invoke [2]",
            "Program log: B start",
            "Program C invoke [3]",
            "Program log: C only",
            "Program C success",
            "Program log: B middle",
            "Program B success",
            "Program log: A end",
            "Program A success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![InstructionUpdate {
                program: KeyBytes::new([2; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![InstructionUpdate {
                    program: KeyBytes::new([3; 32]),
                    accounts: vec![],
                    data: vec![],
                    shared: Arc::clone(&shared),
                    inner: vec![],
                    path: Path::from(vec![0, 0, 0]),
                    log_range: 0..0,
                }],
                path: Path::from(vec![0, 0]),
                log_range: 0..0,
            }],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        let direct: Vec<&str> = outer[0].direct_log_messages().collect();
        assert_eq!(direct, vec![
            "Program A invoke [1]",
            "Program log: A start",
            "Program log: A end",
            "Program A success",
        ]);
    }

    /// An outer ix keeps its own `Program data:` and drops an inner CPI's, which
    /// Anchor-event handlers rely on.
    #[test]
    fn direct_log_messages_keeps_own_program_data_drops_inner() {
        let logs: Vec<String> = vec![
            "Program OUTER invoke [1]",
            "Program data: OUTER_PAYLOAD_BASE64==",
            "Program INNER invoke [2]",
            "Program data: INNER_PAYLOAD_BASE64==",
            "Program INNER success",
            "Program OUTER success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![InstructionUpdate {
                program: KeyBytes::new([2; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::from(vec![0, 0]),
                log_range: 0..0,
            }],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        let direct: Vec<&str> = outer[0].direct_log_messages().collect();
        assert!(
            direct.contains(&"Program data: OUTER_PAYLOAD_BASE64=="),
            "outer Program data: must be preserved"
        );
        assert!(
            !direct.contains(&"Program data: INNER_PAYLOAD_BASE64=="),
            "inner Program data: must be filtered out"
        );
    }

    /// `Program return:` lines sit just before `success` at depth 1 and are kept.
    #[test]
    fn direct_log_messages_keeps_program_return() {
        let logs: Vec<String> = vec![
            "Program OUTER invoke [1]",
            "Program return: OUTER 8fo2SxUAAAA=",
            "Program OUTER success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = vec![InstructionUpdate {
            program: KeyBytes::new([1; 32]),
            accounts: vec![],
            data: vec![],
            shared: Arc::clone(&shared),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        }];

        assign_log_messages(&shared.log_messages, &mut outer);

        let direct: Vec<&str> = outer[0].direct_log_messages().collect();
        assert_eq!(direct, vec![
            "Program OUTER invoke [1]",
            "Program return: OUTER 8fo2SxUAAAA=",
            "Program OUTER success",
        ]);
    }

    /// Regression for tx
    /// `bLyrwGENd4hog5k5aVbtxz5jrpDmUEQR62GyWzbJHdjwdgPTz8LhY5WV9dUjX6zsHycUaigFLxaj4LYZ6QBQQz9`:
    /// `Program log: Order failed: ...` must not close the frame and drop the
    /// trailing `Program data:` payload.
    #[test]
    fn direct_log_messages_ignores_failed_in_program_log_payload() {
        let logs: Vec<String> = vec![
            "Program ComputeBudget111111111111111111111111111111 invoke [1]",
            "Program ComputeBudget111111111111111111111111111111 success",
            "Program ComputeBudget111111111111111111111111111111 invoke [1]",
            "Program ComputeBudget111111111111111111111111111111 success",
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp invoke [1]",
            "Program log: Instruction: FillBuyOrder",
            "Program log: Order failed: 4eef0e8bd59547c7a78c42466a6f99de - 8 contracts",
            "Program data: PLOdAKSDJuxLZszcvjrux1i3dIf1tgBQ0nQMjTMRL5pL1u21Iyqb6SA",
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp consumed 25636 of 31932 compute \
             units",
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp success",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        // Classifier-level guarantee.
        assert_eq!(
            classify_log_line(
                "Program log: Order failed: 4eef0e8bd59547c7a78c42466a6f99de - 8 contracts",
            ),
            LogLineKind::Other,
            "`Program log:` payload starting with `failed:` must not be a Close",
        );

        // split_logs_by_outer_ix groups all 10 lines into 3 outer ixs (2 + 2 + 6).
        let groups = split_logs_by_outer_ix(&logs);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[2].len(), 6);
        assert_eq!(
            groups[2][2],
            "Program log: Order failed: 4eef0e8bd59547c7a78c42466a6f99de - 8 contracts",
        );
        assert_eq!(
            groups[2][5],
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp success",
        );

        // assign_log_messages + direct_log_messages: the depth-1 `failed:`
        // payload, the trailing `Program data:`, and the consumed-units line
        // must all survive on the FillBuyOrder ix.
        let shared = Arc::new(InstructionShared {
            log_messages: logs.clone(),
            ..InstructionShared::default()
        });

        let mut outer = (0..3u8)
            .map(|i| InstructionUpdate {
                program: KeyBytes::new([i + 1; 32]),
                accounts: vec![],
                data: vec![],
                shared: Arc::clone(&shared),
                inner: vec![],
                path: Path::new_single(i.into()),
                log_range: 0..0,
            })
            .collect::<Vec<_>>();

        assign_log_messages(&shared.log_messages, &mut outer);

        let direct: Vec<&str> = outer[2].direct_log_messages().collect();
        assert_eq!(direct, vec![
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp invoke [1]",
            "Program log: Instruction: FillBuyOrder",
            "Program log: Order failed: 4eef0e8bd59547c7a78c42466a6f99de - 8 contracts",
            "Program data: PLOdAKSDJuxLZszcvjrux1i3dIf1tgBQ0nQMjTMRL5pL1u21Iyqb6SA",
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp consumed 25636 of 31932 compute \
             units",
            "Program 3ZZuTbwC6aJbvteyVxXUS7gtFYdf7AuXeitx6VyvjvUp success",
        ]);
    }

    /// Covers payloads that mimic invoke/close lines for every caller of the
    /// classifier.
    #[test]
    fn classify_log_line_covers_all_shapes() {
        // Real invokes
        assert_eq!(
            classify_log_line("Program ABC invoke [1]"),
            LogLineKind::Invoke
        );
        assert_eq!(
            classify_log_line("Program ABC invoke [2]"),
            LogLineKind::Invoke
        );
        assert_eq!(
            classify_log_line("Program ABC invoke [99]"),
            LogLineKind::Invoke
        );

        // Real closes
        assert_eq!(classify_log_line("Program ABC success"), LogLineKind::Close);
        assert_eq!(
            classify_log_line("Program ABC failed: custom error: 0x1"),
            LogLineKind::Close
        );

        // Runtime-tagged lines are Other regardless of payload
        assert_eq!(classify_log_line("Program log: hello"), LogLineKind::Other);
        assert_eq!(
            classify_log_line("Program log: invoke [2]"),
            LogLineKind::Other
        );
        assert_eq!(
            classify_log_line("Program log: success"),
            LogLineKind::Other
        );
        assert_eq!(
            classify_log_line("Program log: failed: nope"),
            LogLineKind::Other
        );
        assert_eq!(
            classify_log_line("Program data: BASE64=="),
            LogLineKind::Other
        );
        assert_eq!(
            classify_log_line("Program return: ABC abc="),
            LogLineKind::Other
        );

        // Compute-units consumption line is Other (not a close)
        assert_eq!(
            classify_log_line("Program ABC consumed 1234 of 200000 compute units"),
            LogLineKind::Other,
        );

        // Non-Program lines are Other
        assert_eq!(classify_log_line("just a random log"), LogLineKind::Other);
        assert_eq!(classify_log_line(""), LogLineKind::Other);
        assert_eq!(classify_log_line("Program "), LogLineKind::Other);

        // Malformed invoke variants are Other
        assert_eq!(
            classify_log_line("Program ABC invoke []"),
            LogLineKind::Other
        );
        assert_eq!(
            classify_log_line("Program ABC invoke [abc]"),
            LogLineKind::Other
        );
        assert_eq!(
            classify_log_line("Program ABC invoke 1"),
            LogLineKind::Other
        );
    }
}
