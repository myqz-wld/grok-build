//! Fail-closed command policy for restricted, read-only shell access.
//!
//! This is intentionally narrower than the normal permission manager's
//! "always safe" list. A caller using this policy has no interactive escape
//! hatch: every shell segment must be a known local inspection command, and
//! syntax that can hide execution or write files is rejected before dispatch.

use std::path::Path;

use tree_sitter::Node;

use super::bash_command_splitting::{try_parse_shell, try_parse_word_only_commands_sequence};
use super::exec_risk::{
    ambient_exec_risk_from_plan, ambient_scan_plan_from_segments, segment_exec_facts,
};
use super::shell_access::command_write_paths_in_tree;

const MAX_COMMAND_BYTES: usize = 16 * 1024;

/// Whether `command` is safe for a restricted local read-only shell.
///
/// Accepted scripts may use pipes and ordinary sequencing, but every segment
/// must be allowlisted. Variable assignments, redirections, substitutions,
/// control flow, backgrounding, file writes, network clients, project code
/// execution, and mutating Git subcommands all fail closed.
///
/// Git commands also inspect local/worktree configuration and are rejected if
/// it can launch an fsmonitor, external diff, textconv, or command alias.
pub fn is_read_only_shell_command(command: &str, cwd: &Path) -> bool {
    if command.trim().is_empty() || command.len() > MAX_COMMAND_BYTES {
        return false;
    }

    let Some(tree) = try_parse_shell(command) else {
        return false;
    };
    if tree.root_node().has_error() || tree_has_forbidden_syntax(tree.root_node()) {
        return false;
    }
    let Some(commands) = try_parse_word_only_commands_sequence(&tree, command) else {
        return false;
    };
    if commands.is_empty() || !command_write_paths_in_tree(tree.root_node(), command).is_empty() {
        return false;
    }

    let mut raw_segments = Vec::with_capacity(commands.len());
    let mut has_git = false;
    for parsed in commands {
        let words = parsed.words();
        let facts = segment_exec_facts(words);
        if facts.exec_risk || !read_only_command_words(words) {
            return false;
        }
        has_git |= facts.has_git;
        raw_segments.push(words.to_vec());
    }

    if has_git {
        let plan = ambient_scan_plan_from_segments(&raw_segments, cwd);
        if ambient_exec_risk_from_plan(&plan) {
            return false;
        }
    }
    true
}

fn tree_has_forbidden_syntax(root: Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "variable_assignment"
                | "redirected_statement"
                | "file_redirect"
                | "heredoc_redirect"
                | "heredoc_body"
        ) {
            return true;
        }
        for index in 0..node.child_count() {
            if let Some(child) = node.child(index) {
                stack.push(child);
            }
        }
    }
    false
}

fn read_only_command_words(words: &[String]) -> bool {
    let Some(program) = words.first().map(String::as_str) else {
        return false;
    };
    // Do not bless an arbitrary binary merely because its basename resembles
    // an allowlisted command. The normal inherited PATH remains the sole
    // command-resolution mechanism.
    if program.contains(['/', '\\']) {
        return false;
    }

    match program {
        "git" => read_only_git_words(words),
        "find" => find_is_read_only(words),
        "rg" => !rg_has_preprocessor(words),
        "sort" => !sort_has_external_program(words),
        "tree" => !tree_has_output_flag(words),
        "tail" => !tail_follows(words),
        "date" => date_is_read_only(words),
        "hostname" => words.len() == 1,
        // Local filesystem and text inspection.
        "ls" | "pwd" | "cat" | "head" | "wc" | "grep" | "cut" | "tr" | "uniq"
        | "diff" | "jq" | "stat" | "basename" | "dirname" | "realpath" | "readlink"
        | "strings" | "du" | "df" |
        // Local process/environment facts with no network or mutation.
        "whoami" | "uname" | "which" | "type" | "true" | "false" => true,
        _ => false,
    }
}

fn read_only_git_words(words: &[String]) -> bool {
    // Global options can retarget the repository or inject configuration.
    // Keeping the subcommand in position 1 makes the accepted grammar obvious.
    let Some(subcommand) = words.get(1).map(String::as_str) else {
        return false;
    };
    if subcommand.starts_with('-') {
        return false;
    }
    let args = &words[2..];
    if args.iter().any(|arg| {
        long_option_or_prefix(arg, "--output")
            || long_option_or_prefix(arg, "--ext-diff")
            || long_option_or_prefix(arg, "--textconv")
    }) {
        return false;
    }

    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "ls-files" | "rev-parse" | "describe"
        | "merge-base" => true,
        "grep" => !git_grep_opens_pager(args),
        "branch" => git_branch_is_listing(args),
        "worktree" => args.first().map(String::as_str) == Some("list"),
        _ => false,
    }
}

fn git_grep_opens_pager(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg.starts_with("-O") || long_option_or_prefix(arg, "--open-files-in-pager"))
}

/// Git accepts some unambiguous long-option abbreviations. Reject any spelling
/// that could denote a dangerous option, even if a particular Git version
/// considers the abbreviation ambiguous or invalid.
fn long_option_or_prefix(argument: &str, option: &str) -> bool {
    let flag = argument.split('=').next().unwrap_or(argument);
    flag.len() > 2 && flag.starts_with("--") && option.starts_with(flag)
}

fn git_branch_is_listing(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    args.iter().all(|arg| {
        matches!(
            arg.as_str(),
            "--show-current"
                | "--list"
                | "--all"
                | "-a"
                | "--remotes"
                | "-r"
                | "--verbose"
                | "-v"
                | "-vv"
                | "--no-abbrev"
                | "--ignore-case"
                | "--omit-empty"
        )
    })
}

fn find_is_read_only(words: &[String]) -> bool {
    const MUTATING_ACTIONS: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
    ];
    !words
        .iter()
        .any(|word| MUTATING_ACTIONS.contains(&word.as_str()))
}

fn rg_has_preprocessor(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| long_option_or_prefix(word, "--pre"))
}

fn sort_has_external_program(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| long_option_or_prefix(word, "--compress-program"))
}

fn tree_has_output_flag(words: &[String]) -> bool {
    words.iter().skip(1).any(|word| {
        word == "-o"
            || (word.starts_with('-') && !word.starts_with("--") && word.contains('o'))
            || long_option_or_prefix(word, "--output")
    })
}

fn tail_follows(words: &[String]) -> bool {
    words.iter().skip(1).any(|word| {
        long_option_or_prefix(word, "--follow")
            || long_option_or_prefix(word, "--retry")
            || (word.starts_with('-')
                && !word.starts_with("--")
                && word.chars().skip(1).any(|flag| matches!(flag, 'f' | 'F')))
    })
}

fn date_is_read_only(words: &[String]) -> bool {
    words
        .iter()
        .skip(1)
        .all(|word| word == "-u" || word == "--utc" || word.starts_with('+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("temp repo");
        git2::Repository::init(temp.path()).expect("init repo");
        temp
    }

    #[test]
    fn allows_common_local_inspection_commands() {
        let repo = clean_repo();
        for command in [
            "git diff main...HEAD",
            "git status --short --branch",
            "git log --oneline -5 | head -3",
            "git show HEAD; git rev-parse --show-toplevel",
            "git branch --show-current",
            "git diff --output-indicator-new=+",
            "rg -n TODO crates | head -20",
            "find . -name '*.rs'",
            "ls -la && pwd",
            "cat Cargo.toml | grep workspace",
            "date -u +%F",
            "hostname",
        ] {
            assert!(
                is_read_only_shell_command(command, repo.path()),
                "expected read-only command: {command}"
            );
        }
    }

    #[test]
    fn rejects_mutation_execution_network_and_hidden_shell_syntax() {
        let repo = clean_repo();
        for command in [
            "git add .",
            "git commit -m nope",
            "git checkout main",
            "git branch new-branch",
            "git diff --output=patch.txt",
            "git diff --out=patch.txt",
            "touch nope",
            "rm -rf .",
            "curl https://example.com",
            "cargo test",
            "python script.py",
            "rg --pre ./convert TODO .",
            "rg --pr=./convert TODO .",
            "find . -exec rm {} ';'",
            "cat Cargo.toml > copy",
            "cat $(printf Cargo.toml)",
            "FOO=bar git diff",
            "git diff &",
            "git diff && touch nope",
            "/tmp/git diff",
            "date 010100002026",
            "date --set=2026-01-01",
            "hostname changed.example",
        ] {
            assert!(
                !is_read_only_shell_command(command, repo.path()),
                "expected blocked command: {command}"
            );
        }
    }

    #[test]
    fn rejects_command_options_that_execute_or_do_not_terminate() {
        let repo = clean_repo();
        for command in [
            "git -c core.fsmonitor=/tmp/pwn status",
            "git grep -Oless needle",
            "git diff --ext-diff",
            "git show --textconv",
            "sort --compress-program=/tmp/pwn data",
            "tree --out=listing.txt",
            "tree -o listing.txt",
            "tail -f logfile",
            "tail --fol=name logfile",
        ] {
            assert!(
                !is_read_only_shell_command(command, repo.path()),
                "expected blocked option: {command}"
            );
        }
    }

    #[test]
    fn rejects_git_commands_when_local_config_can_execute() {
        let repo = clean_repo();
        std::fs::write(
            repo.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n[diff \"unsafe\"]\n\tcommand = /tmp/pwn\n",
        )
        .expect("write risky config");
        assert!(!is_read_only_shell_command("git diff", repo.path()));
    }
}
