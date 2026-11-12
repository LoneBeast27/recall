//! Report probe: `report_probe <empty folder>` builds a sample report there from made-up
//! files, so a check can open the zip with a standard reader. Touches nothing else.

use std::fs;
use std::path::PathBuf;

use recall_trace::report::{build, Input, Names};

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: report_probe <empty folder>"));
    let trace = dir.join("trace");
    fs::create_dir_all(&trace).unwrap();
    fs::write(trace.join("island.trace"), "{\"t\":0,\"e\":\"island.state\",\"f\":{\"shows\":\"nothing\"}}\n").unwrap();
    let log = dir.join("sample.log");
    fs::write(&log, "start C:\\Users\\sample\\x.log\nforeground: game.exe pid 1\n").unwrap();
    let input = Input {
        text: "Sample report\nThe island did not show.".into(),
        version: "0.0.0".into(),
        windows: "0".into(),
        trace_dir: Some(trace),
        logs: vec![("sample.log".into(), log)],
        media: Vec::new(),
    };
    let names = Names { user: Some("sample".into()), pc: None };
    let built = build(&input, &dir.join("reports"), &names).unwrap();
    println!("{}", built.path.display());
}
