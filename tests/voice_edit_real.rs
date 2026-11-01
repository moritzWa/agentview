#![cfg(any(target_os = "linux", target_os = "macos"))]

use agentview::opencode_supervisor::OpenCodeSupervisor;
use std::time::{Duration, Instant};

/// Against the OpenCode server this dashboard already runs, in a scratch folder:
///   AGENTVIEW_REAL_OPENCODE_BIN=~/.local/bin/opencode-dev AGENTVIEW_VOICE_SCRATCH=~/Code/voicescratch \
///     cargo test --test voice_edit_real -- --ignored --nocapture
/// Starts a long task, corrects it while it runs, and checks that the first message is gone.
#[test]
#[ignore = "uses the running OpenCode server and a model; see the doc comment"]
fn correcting_a_running_session_rewinds_to_before_its_last_message() {
    let executable = std::env::var("AGENTVIEW_REAL_OPENCODE_BIN").unwrap();
    let scratch = std::path::PathBuf::from(std::env::var("AGENTVIEW_VOICE_SCRATCH").unwrap());
    let supervisor = OpenCodeSupervisor::host(executable).unwrap();
    let session = supervisor
        .launch(
            "Write a 400 word essay about dishwashers into essay.txt, then reread it and improve it five times.",
            &scratch,
        )
        .unwrap();
    std::thread::sleep(Duration::from_secs(4));
    supervisor
        .edit_last_message(
            &session.id,
            "Write the single word pineapple into pine.txt and nothing else.",
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut said = Vec::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        said = supervisor.recent_messages(&session.id, 10).unwrap();
        if scratch.join("pine.txt").exists() && said.iter().any(|m| m.role == "assistant") {
            break;
        }
    }
    println!("{said:#?}");
    let users: Vec<&str> = said
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(
        users.len(),
        1,
        "the first message should be rewound: {users:?}"
    );
    assert!(users[0].contains("pineapple"));
    assert!(scratch.join("pine.txt").exists());
}
