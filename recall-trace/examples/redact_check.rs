//! Redaction check: `redact_check <file>...` prints each file as a report would carry it
//! (names and paths replaced), reading the names to hide from this PC's environment. It
//! only reads the files; the output goes to the console.

fn main() {
    let names = recall_trace::report::Names::from_env();
    for path in std::env::args().skip(1) {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        print!("{}", recall_trace::report::redact::redact(&String::from_utf8_lossy(&bytes), &names));
    }
}
