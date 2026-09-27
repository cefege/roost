//! The two streams one `roost api` verb writes to: the answer the operator
//! asked for, and this command's own progress and remedies. Called by every
//! verb in the module; nothing else in the crate depends on it.
//!
//! The split is a trait rather than a pair of `println!` calls because the
//! stdout half is a contract: `agent-status --json` promises exactly one
//! parseable JSON document and nothing else, and the only way a test can see
//! that promise kept is by reading what a verb wrote. A verb that reached for
//! `println!` directly would be a verb no test could hold to it.

/// Where one verb's two kinds of line go.
///
/// A trait method rather than a function pointer pair so a test can collect the
/// two streams apart and assert that a machine-readable answer is alone on
/// stdout while a range or a remedy is not.
pub trait ApiOutput {
    /// A line the operator asked for: a readout, a value, a document, a
    /// command to copy. This is the only thing that belongs on stdout.
    fn answer(&mut self, line: &str);

    /// This command's own progress, the range it is reading, or the remedy for
    /// a refusal. Never the answer, and never a credential.
    fn progress(&mut self, line: &str);
}

/// The production sink: stdout for answers, stderr for everything else.
///
/// The binary's stdout is this program's product, which is why this crate is
/// exempt from the no-stdout lint at all — and why the exemption stops here.
#[derive(Debug, Default)]
pub struct TerminalOutput;

impl ApiOutput for TerminalOutput {
    fn answer(&mut self, line: &str) {
        println!("{line}");
    }

    fn progress(&mut self, line: &str) {
        eprintln!("{line}");
    }
}
