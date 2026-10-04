//! State read from the screen of a coding-agent CLI that this dashboard holds
//! in the background, for providers without a screen reader of their own.
//! These CLIs share conventions: a running turn shows how to interrupt it
//! next to the composer, and a permission prompt offers numbered options
//! starting with "Yes" above a way to cancel.

use crate::domain::SessionState;

/// Interrupt hints shown only while a turn runs, matched without case:
/// Codex (`• Working (3s • esc to interrupt)`), Claude Code (`· esc to
/// interrupt` in its footer), Devin (`(esc twice to interrupt)`), and the
/// variants other CLIs print.
const INTERRUPT_HINTS: &[&str] = &[
    "esc to interrupt",
    "esc twice to interrupt",
    "esc again to interrupt",
    "escape to interrupt",
];
/// The composer, its status line, and a permission panel's options sit in
/// the last few non-empty rows, below the transcript. A longer window would
/// read the transcript, which can quote these hints.
const STATUS_ROWS: usize = 8;

/// The state the screen shows for sure, or `None` to leave the row to
/// discovery. An idle composer is not reported: a background task can keep
/// a session working without showing it, and discovery tells completed
/// sessions from ones awaiting input.
pub(super) fn screen_state(screen: &str) -> Option<(SessionState, &'static str)> {
    let rows = screen
        .lines()
        .rev()
        .map(|line| line.trim().to_lowercase())
        .filter(|line| !line.is_empty())
        .take(STATUS_ROWS)
        .collect::<Vec<_>>();
    if rows
        .iter()
        .any(|row| INTERRUPT_HINTS.iter().any(|hint| row.contains(hint)))
    {
        return Some((SessionState::Working, "running turn"));
    }
    // Claude Code (`❯ 1. Yes` above `Esc to cancel · Tab to amend`) and Codex
    // (`› 1. Yes, proceed (y)` above `Press enter to confirm or esc to
    // cancel`). Folder-trust prompts also offer to cancel, but their first
    // option is not "Yes".
    let offers_yes = rows.iter().any(|row| {
        row.trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
            .starts_with("1. yes")
    });
    if offers_yes && rows.iter().any(|row| row.contains("esc to cancel")) {
        return Some((SessionState::NeedsInput, "permission requested"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: &[&str]) -> String {
        let mut lines = vec!["earlier transcript"; 20];
        lines.extend_from_slice(rows);
        lines.extend_from_slice(&["", ""]);
        lines.join("\n")
    }

    #[test]
    fn running_turns_show_an_interrupt_hint_in_every_cli() {
        let codex = screen(&[
            "• Working (11s • esc to interrupt) · 2 background terminals running",
            "› Ask Codex to do anything",
            "  GPT-6-Astra low · ~/project",
            "  ← for agents · ? for shortcuts",
        ]);
        let claude = screen(&[
            "✢ Puttering… (11s · ↓ 80 tokens)",
            "────────────",
            "❯",
            "────────────",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← 1 agent",
        ]);
        let devin = screen(&[
            "⠀⢰ Running tools · 12s (esc twice to interrupt)",
            "──────────── (bypass permissions on) ─",
            "❭ Guide Devin while it works",
            "────────────",
            "SWE-2 High                Context: 19k / 262k tokens (7%)",
        ]);
        for screen in [codex, claude, devin] {
            assert_eq!(
                screen_state(&screen),
                Some((SessionState::Working, "running turn")),
                "{screen}"
            );
        }
    }

    #[test]
    fn permission_prompts_need_input() {
        let codex = screen(&[
            "  Reason: May I create hello.txt?",
            "  $ printf hi > hello.txt",
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again for commands that start with `printf` (p)",
            "  3. No, and tell Codex what to do differently (esc)",
            "  Press enter to confirm or esc to cancel",
        ]);
        let claude = screen(&[
            " Do you want to create hello.txt?",
            " ❯ 1. Yes",
            "   2. Yes, and switch to accept edits for this session (shift+tab)",
            "   3. No",
            " Esc to cancel · Tab to amend",
        ]);
        for screen in [codex, claude] {
            assert_eq!(
                screen_state(&screen),
                Some((SessionState::NeedsInput, "permission requested")),
                "{screen}"
            );
        }
    }

    #[test]
    fn idle_composers_and_trust_prompts_are_left_to_discovery() {
        let screens = [
            screen(&[
                "• OK",
                "  Worked for 33s • 10:16 AM",
                "› Ask Codex to do anything",
                "  ← for agents · ? for shortcuts",
            ]),
            screen(&[
                "✻ Cogitated for 23s · done 10:16 AM",
                "❯",
                "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent",
            ]),
            screen(&[
                " Quick safety check: Is this a project you created or one you trust?",
                " ❯ No, exit",
                "   Yes, I trust this folder",
                " Enter to confirm · Esc to cancel",
            ]),
            screen(&[
                "  Trust this folder?",
                "› 1. Trust and continue",
                "  2. Back to Agent Command Center",
                "  enter continue · esc back",
            ]),
        ];
        for screen in screens {
            assert_eq!(screen_state(&screen), None, "{screen}");
        }
    }

    #[test]
    fn a_hint_quoted_higher_in_the_transcript_is_not_a_running_turn() {
        let mut rows = vec!["Codex shows esc to interrupt while it works."];
        rows.extend(["more transcript"; STATUS_ROWS]);
        rows.extend(["› Ask Codex to do anything", "  ? for shortcuts"]);
        assert_eq!(screen_state(&screen(&rows)), None);
    }
}
