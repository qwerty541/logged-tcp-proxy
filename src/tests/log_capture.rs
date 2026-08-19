//! Capture of the proxy's own `log` output, so a test can assert on which
//! lifecycle lines were — and were not — emitted.

use std::sync::Mutex;
use std::sync::Once;

/// Every message the proxy logs, captured so a test can assert on what was — and
/// was not — logged. Tests share one process and run in parallel, so assertions
/// must key off their own unique ephemeral addresses rather than the contents or
/// length of this buffer as a whole.
static CAPTURED_LOGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct CapturingLogger;

impl log::Log for CapturingLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        CAPTURED_LOGS
            .lock()
            .expect("captured logs mutex poisoned")
            .push(record.args().to_string());
    }

    fn flush(&self) {}
}

/// Install [`CapturingLogger`] the first time a test needs it (a process may only
/// ever set one logger). `Info` keeps the payload `debug` records out of the
/// capture; the lifecycle lines under test are logged at `info`.
pub(super) fn install_capturing_logger() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        log::set_boxed_logger(Box::new(CapturingLogger))
            .expect("failed to install the test logger");
        log::set_max_level(log::LevelFilter::Info);
    });
}

/// Strip a leading `[#N] ` connection-id tag from a captured line, returning the
/// bare message. A line without the tag (a listener-level line, or any line with
/// `--no-connection-ids`) is returned unchanged.
fn strip_conn_tag(line: &str) -> &str {
    line.strip_prefix("[#")
        .and_then(|rest| rest.split_once("] "))
        .filter(|(id, _)| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(line, |(_, message)| message)
}

/// A snapshot of every captured log message. Per the module comment, assertions
/// on it must key off the test's own unique ephemeral addresses.
pub(super) fn captured_lines() -> Vec<String> {
    CAPTURED_LOGS
        .lock()
        .expect("captured logs mutex poisoned")
        .clone()
}

/// The `<target>` field of every captured `[#N] Connected to destination <target> ...`
/// line (the per-connection `[#N] ` tag is stripped first, and is optional so the
/// helper also works with `--no-connection-ids`). Returned for exact-equality
/// comparison, so one test's ephemeral port can never match another's merely by
/// being a prefix of it (`:4523` vs `:45231`).
pub(super) fn logged_destinations() -> Vec<String> {
    CAPTURED_LOGS
        .lock()
        .expect("captured logs mutex poisoned")
        .iter()
        .filter_map(|line| strip_conn_tag(line).strip_prefix("Connected to destination "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .collect()
}
