//! Capture of the proxy's own `log` output, so a test can assert on which
//! lifecycle lines were — and were not — emitted.

use super::helpers::IO_TIMEOUT;
use std::sync::Mutex;
use std::sync::Once;
use std::time::Duration;
use tokio::time::sleep;
use tokio::time::timeout;

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
///
/// The delimiters come from `conn::logging` rather than being re-spelled here, so a
/// change to the tag's shape cannot leave this parser silently matching nothing —
/// which would surface as an unrelated-looking failure in the tests that consume it.
/// (Assertions on the tag deliberately keep their literals: those pin the output
/// contract and must not be derived from the code under test.)
fn strip_conn_tag(line: &str) -> &str {
    line.strip_prefix(crate::conn::logging::CONN_TAG_OPEN)
        .and_then(|rest| rest.split_once(crate::conn::logging::CONN_TAG_CLOSE))
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

/// Wait, bounded by [`IO_TIMEOUT`], for a captured line starting with `prefix`, and
/// return it.
///
/// For lines a client cannot wait for on its own socket: a connection's close
/// summary is logged only after the proxy has forwarded the close to both peers, so
/// a client sees its connection end a moment before the line exists. Per the module
/// comment, `prefix` must key off the test's own unique ephemeral addresses — and
/// should spell out as much of the expected line as it can, since a closed client's
/// port can be reused by a later test's client.
pub(super) async fn wait_for_line(prefix: &str) -> String {
    let find = || {
        CAPTURED_LOGS
            .lock()
            .expect("captured logs mutex poisoned")
            .iter()
            .find(|line| line.starts_with(prefix))
            .cloned()
    };
    let found = timeout(IO_TIMEOUT, async {
        loop {
            if let Some(line) = find() {
                return line;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // The panic message is built from a snapshot, never under the lock, so a failing
    // test cannot poison the capture for the tests running beside it.
    found.unwrap_or_else(|_| {
        panic!(
            "no captured line starts with {prefix:?}; captured: {:?}",
            captured_lines()
        )
    })
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
