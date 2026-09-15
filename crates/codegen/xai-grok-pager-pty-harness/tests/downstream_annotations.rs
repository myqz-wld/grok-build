//! Downstream annotation creation, persistence, and follow-up against a real pager.

#[allow(dead_code, unused_imports)]
#[path = "pty_e2e/common.rs"]
mod common;

use common::*;

const SOURCE: &str = "ANNOTATION_SOURCE_SENTINEL";
const FIRST_ANSWER: &str = "ANNOTATION_FIRST_ANSWER";
const FOLLOW_UP_ANSWER: &str = "ANNOTATION_FOLLOW_UP_ANSWER";

fn select_and_annotate(harness: &mut PtyHarness, text: &str, placeholder: &str) {
    harness.update(Duration::from_millis(500));
    let screen = harness.screen_contents();
    let (row, col) = locate_screen_text(&screen, text)
        .unwrap_or_else(|| panic!("missing selectable {text:?}\nscreen:\n{screen}"));
    harness
        .inject_keys(mouse_drag_line(row, col, col + text.len() as u16 - 1).as_bytes())
        .expect("select annotation text");
    harness.update(Duration::from_millis(100));
    harness.inject_keys(b"\x1ba").expect("Alt+A");
    harness
        .wait_for_text(placeholder, Duration::from_secs(10))
        .unwrap_or_else(|error| {
            panic!(
                "annotation composer: {error}\nscreen:\n{}",
                harness.screen_contents()
            )
        });
}

fn find_annotation_logs(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read sandbox directory") {
        let entry = entry.expect("sandbox entry");
        let kind = entry.file_type().expect("sandbox entry type");
        if kind.is_dir() {
            find_annotation_logs(&entry.path(), paths);
        } else if entry.file_name() == "annotation_threads.jsonl" {
            paths.push(entry.path());
        }
    }
}

fn annotation_events(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("read annotation log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("persisted annotation event"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires PAGER_BINARY; runs a real pager with isolated home and mock inference"]
async fn annotation_create_reload_and_follow_up_reuses_hidden_child() {
    let content = ContentController::start()
        .await
        .expect("start mock inference");
    content.set_response(format!("{SOURCE} explains this sentence."));
    let binary = pager_binary().expect("pager binary");
    let project = tempfile::tempdir().expect("isolated project");
    std::fs::create_dir_all(project.path().join(".git")).expect("project marker");
    let spawn = |args: &[&str]| {
        PtyHarness::spawn_with_content_env_in_dir(
            &binary,
            DEFAULT_ROWS,
            DEFAULT_COLS,
            &content,
            args,
            &[],
            Some(project.path()),
        )
        .expect("spawn isolated pager")
    };

    let mut first = spawn(&[]);
    first
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome");
    first
        .inject_keys(b"Explain a sentence\r")
        .expect("parent prompt");
    first
        .wait_for_text(SOURCE, Duration::from_secs(30))
        .expect("parent response");
    first.update(Duration::from_secs(1));
    first.inject_keys(b"\t").expect("focus transcript");
    first
        .wait_for_text("Space:prompt", Duration::from_secs(10))
        .expect("transcript focus");

    content.set_response(FIRST_ANSWER);
    select_and_annotate(&mut first, SOURCE, "Ask about the selected text");
    first
        .inject_keys(b"Explain the selected text\r")
        .expect("annotation question");
    first
        .wait_for_text(FIRST_ANSWER, Duration::from_secs(30))
        .unwrap_or_else(|error| panic!("first annotation: {error}\n{}", first.screen_contents()));
    first.update(Duration::from_secs(1));
    first.quit().expect("quit first pager");

    let mut logs = Vec::new();
    find_annotation_logs(&content.sandbox().grok_home(), &mut logs);
    let [log] = logs.as_slice() else {
        panic!("one parent annotation log expected: {logs:?}");
    };
    let before = annotation_events(log);
    let created = before
        .iter()
        .find(|event| event.get("kind").and_then(|v| v.as_str()) == Some("thread_created"))
        .expect("thread creation persisted")
        .clone();

    let mut resumed = spawn(&["--continue"]);
    resumed
        .wait_for_text(FIRST_ANSWER, Duration::from_secs(30))
        .unwrap_or_else(|error| {
            panic!(
                "restored annotation: {error}\n{}",
                resumed.screen_contents()
            )
        });
    assert!(
        resumed.contains_text(SOURCE),
        "the parent transcript must remain active"
    );
    content.set_response(FOLLOW_UP_ANSWER);
    select_and_annotate(
        &mut resumed,
        FIRST_ANSWER,
        "Ask about this annotation selection",
    );
    resumed
        .inject_keys(b"Explain that further\r")
        .expect("follow-up question");
    resumed
        .wait_for_text(FOLLOW_UP_ANSWER, Duration::from_secs(30))
        .unwrap_or_else(|error| {
            panic!(
                "annotation follow-up: {error}\n{}",
                resumed.screen_contents()
            )
        });
    resumed.update(Duration::from_secs(1));
    resumed.quit().expect("quit resumed pager");

    let after = annotation_events(log);
    let creations = after
        .iter()
        .filter(|event| event.get("kind").and_then(|v| v.as_str()) == Some("thread_created"))
        .collect::<Vec<_>>();
    assert_eq!(
        creations,
        vec![&created],
        "follow-up must reuse the persisted child"
    );
    assert_eq!(
        after
            .iter()
            .filter(|event| {
                event.get("kind").and_then(|v| v.as_str()) == Some("exchange_completed")
            })
            .count(),
        2,
        "both annotation exchanges must complete durably"
    );
    let transcript = std::fs::read_to_string(log.parent().unwrap().join("chat_history.jsonl"))
        .expect("parent transcript");
    assert!(!transcript.contains(FIRST_ANSWER));
    assert!(!transcript.contains(FOLLOW_UP_ANSWER));
}
