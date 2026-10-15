//! ZIM desktop shell — binary entry.
//!
//! All composition lives in the library half (`zim_shell::run`) so
//! the smoke/integration tests can drive the interesting parts
//! without launching a GUI.

fn main() {
    zim_shell::run();
}
