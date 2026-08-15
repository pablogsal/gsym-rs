//! Discovery and execution of the external toolchain binaries tests shell out to.

use std::collections::HashMap;
use std::process::{Command, Output};
use std::sync::{LazyLock, Mutex};

/// LLVM releases whose suffixed binaries are acceptable substitutes.
const LLVM_RELEASES: std::ops::RangeInclusive<u32> = 16..=22;

/// Locate `name`, falling back to the versioned binaries LLVM installs.
///
/// Distributions rarely ship an unsuffixed `llvm-dwp` or `yaml2obj`, so the
/// plain name is tried first and then every supported release, newest first.
/// These tools are required rather than optional: silently skipping a test on a
/// machine that lacks them would hide real conversion regressions.
///
/// Answers are cached: probing spawns a process per candidate, and several
/// tests in a suite ask for the same tool.
pub(crate) fn required_tool(name: &str) -> String {
    static RESOLVED: LazyLock<Mutex<HashMap<String, String>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    if let Some(found) = RESOLVED.lock().unwrap().get(name) {
        return found.clone();
    }

    let candidates = std::iter::once(name.to_owned()).chain(
        LLVM_RELEASES
            .rev()
            .map(|release| format!("{name}-{release}")),
    );
    let mut attempted = Vec::new();
    for candidate in candidates {
        if Command::new(&candidate).arg("--version").output().is_ok() {
            RESOLVED
                .lock()
                .unwrap()
                .insert(name.to_owned(), candidate.clone());
            return candidate;
        }
        attempted.push(candidate);
    }
    panic!(
        "required test tool is unavailable: {}",
        attempted.join(", ")
    )
}

/// Run `command` to completion and fail the test if it reports an error.
pub(crate) fn run(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
