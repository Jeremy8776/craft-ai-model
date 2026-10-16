use std::sync::Arc;

use craft_ai_core::connection::Locality;
use craft_ai_core::history::{EndRecord, HISTORY_SCHEMA, Outcome, StartRecord};

use super::*;

fn start(run: &str) -> HistoryRecord {
    HistoryRecord::Start(StartRecord {
        v: HISTORY_SCHEMA,
        run: run.into(),
        at_ms: 1,
        app: "photocraft".into(),
        role_id: "fill".into(),
        role_label: "Generative Fill".into(),
        capability: "image.inpaint".into(),
        connection: "c".into(),
        connection_label: "C".into(),
        model: "m".into(),
        locality: Locality::Local,
        destination: "local".into(),
        sent: Vec::new(),
        params: Vec::new(),
        provider_job: None,
        estimate: None,
        prompt: None,
    })
}

fn end(run: &str) -> HistoryRecord {
    HistoryRecord::End(EndRecord {
        v: HISTORY_SCHEMA,
        run: run.into(),
        at_ms: 2,
        outcome: Outcome::Done,
        usage: None,
    })
}

fn runs(read: &HistoryRead) -> Vec<String> {
    read.records
        .iter()
        .map(|r| match r {
            HistoryRecord::Start(s) => format!("s:{}", s.run),
            HistoryRecord::End(e) => format!("e:{}", e.run),
        })
        .collect()
}

#[test]
fn records_come_back_in_order_and_a_missing_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("nested").join("jobs.jsonl"));
    assert!(file.read().unwrap().records.is_empty());
    file.append(&start("a")).unwrap();
    file.append(&end("a")).unwrap();
    let read = file.read().unwrap();
    assert_eq!(runs(&read), ["s:a", "e:a"]);
    assert_eq!(read.skipped, 0);
}

#[test]
fn garbage_and_unknown_lines_are_skipped_without_losing_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl"));
    file.append(&start("a")).unwrap();
    let mut raw = fs::read(file.path()).unwrap();
    raw.extend_from_slice(b"not json at all\n{\"phase\":\"pause\",\"run\":\"x\"}\n\n");
    fs::write(file.path(), raw).unwrap();
    file.append(&end("a")).unwrap();
    let read = file.read().unwrap();
    assert_eq!(runs(&read), ["s:a", "e:a"]);
    assert_eq!(read.skipped, 2);
}

#[test]
fn a_torn_last_line_does_not_swallow_the_next_record() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl"));
    file.append(&start("a")).unwrap();
    let mut raw = fs::read(file.path()).unwrap();
    raw.extend_from_slice(br#"{"phase":"end","run":"a","at_"#);
    fs::write(file.path(), raw).unwrap();
    file.append(&end("b")).unwrap();
    let read = file.read().unwrap();
    assert_eq!(runs(&read), ["s:a", "e:b"]);
    assert_eq!(read.skipped, 1);
}

#[test]
fn the_file_rotates_at_the_cap_and_the_newest_records_survive() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl")).with_max_bytes(1024);
    for i in 0..40 {
        file.append(&start(&format!("r{i:02}"))).unwrap();
    }
    let live = fs::metadata(file.path()).unwrap().len();
    assert!(live <= 1024, "live file is {live} bytes");
    assert!(dir.path().join("jobs.1.jsonl").exists());
    let read = file.read().unwrap();
    let names = runs(&read);
    assert_eq!(names.last().unwrap(), "s:r39", "newest kept");
    assert!(names.len() < 40, "oldest dropped");
    assert!(names.windows(2).all(|w| w[0] < w[1]), "still in order");
    let total: u64 = ["jobs.jsonl", "jobs.1.jsonl"]
        .iter()
        .map(|n| fs::metadata(dir.path().join(n)).unwrap().len())
        .sum();
    assert!(total <= 2048, "bounded to two caps, was {total}");
}

#[test]
fn several_writers_at_once_never_interleave_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let file = Arc::new(HistoryFile::new(dir.path().join("jobs.jsonl")));
    let threads: Vec<_> = (0..6)
        .map(|t| {
            let file = Arc::clone(&file);
            std::thread::spawn(move || {
                for i in 0..15 {
                    file.append(&start(&format!("t{t}-{i}"))).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let read = file.read().unwrap();
    assert_eq!(read.records.len(), 90);
    assert_eq!(read.skipped, 0);
}

#[test]
fn an_unwritable_location_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, b"x").unwrap();
    let file = HistoryFile::new(blocker.join("jobs.jsonl"));
    assert!(file.append(&start("a")).is_err());
    assert!(file.read().is_err() || file.read().unwrap().records.is_empty());
}

#[test]
fn an_oversize_record_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl"));
    let HistoryRecord::Start(mut s) = start("a") else {
        return;
    };
    s.prompt = Some("x".repeat(MAX_LINE_BYTES));
    assert!(file.append(&HistoryRecord::Start(s)).is_err());
    assert!(!file.path().exists() || fs::metadata(file.path()).unwrap().len() == 0);
}

#[test]
fn clear_removes_both_files_and_is_fine_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl")).with_max_bytes(1024);
    file.clear().unwrap();
    for i in 0..40 {
        file.append(&start(&format!("r{i}"))).unwrap();
    }
    file.clear().unwrap();
    assert!(file.read().unwrap().records.is_empty());
    assert!(!file.path().exists());
}

#[test]
fn a_file_far_over_the_cap_is_refused_not_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let file = HistoryFile::new(dir.path().join("jobs.jsonl")).with_max_bytes(1024);
    fs::write(file.path(), vec![b'a'; 1024 + MAX_LINE_BYTES + 1]).unwrap();
    assert!(file.read().is_err());
}
