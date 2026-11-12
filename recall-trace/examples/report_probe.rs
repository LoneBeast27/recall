//! Report probe: `report_probe <empty folder>` builds a sample report there from made-up
//! files, so a check can open the zip with a standard reader. Touches nothing else.
//! `report_probe --settings <config.toml>` prints the settings summary of a file (read only).

use std::fs;
use std::path::PathBuf;

use recall_trace::report::{prepare, save, Input, Names};

fn main() {
    // `report_probe --settings <config.toml>`: print the settings summary a report would hold.
    if std::env::args().nth(1).as_deref() == Some("--settings") {
        let path = std::env::args().nth(2).expect("usage: report_probe --settings <config.toml>");
        print!("{}", recall_trace::report::settings::summary(&fs::read_to_string(path).unwrap()));
        return;
    }
    // `report_probe --clean <in> <out>`: write the cleaned copy of a picture or clip.
    if std::env::args().nth(1).as_deref() == Some("--clean") {
        let (src, dst) = (std::env::args().nth(2).unwrap(), std::env::args().nth(3).unwrap());
        let kind = recall_trace::report::check_media(std::path::Path::new(&src)).expect("type");
        let clean = recall_trace::report::media::clean(kind, fs::read(&src).unwrap()).expect("refused");
        fs::write(&dst, clean).unwrap();
        return;
    }
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
        settings: Some(log),
        media: Vec::new(),
    };
    let names = Names { user: Some("sample".into()) };
    let prepared = prepare(&input, &names).unwrap();
    println!("{}", save(&prepared, &dir.join("reports")).unwrap().display());
}
