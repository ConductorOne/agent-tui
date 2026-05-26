//! Recorder writer + rotation integration tests.

use std::path::PathBuf;
use std::time::Duration;

use agent_tui_recorder::{Recorder, RecorderConfig};
use uuid::Uuid;

fn tempdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("agent-tui-rec-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

#[tokio::test]
async fn writes_o_events_as_ndjson() {
    let dir = tempdir();
    let cfg = RecorderConfig::new(dir.clone(), "p1");
    let (rec, _handle) = Recorder::start(cfg).expect("start");
    rec.push_output(b"hello");
    rec.push_output(b"world");
    // Wait for the writer task to drain.
    for _ in 0..50 {
        if rec.stats().await.events_written >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cast = std::fs::read_to_string(dir.join("p1.cast")).expect("read cast");
    let lines: Vec<&str> = cast.lines().collect();
    assert_eq!(lines.len(), 2, "two events expected, got: {cast}");
    let row0: serde_json::Value = serde_json::from_str(lines[0]).expect("parse");
    let row0_arr = row0.as_array().expect("array");
    assert_eq!(row0_arr[1].as_str().unwrap(), "o");
    assert_eq!(row0_arr[2].as_str().unwrap(), "hello");
}

#[tokio::test]
async fn rotates_at_size_threshold() {
    let dir = tempdir();
    let cfg = RecorderConfig {
        dir: dir.clone(),
        basename: "p1".into(),
        // Force a rotation after ~256 bytes so the test stays fast.
        rotate_bytes: 256,
        retention_bytes: agent_tui_recorder::DEFAULT_RETENTION_BYTES,
    };
    let (rec, _handle) = Recorder::start(cfg).expect("start");
    // 40 bytes per event ~ 10 events crosses the threshold.
    for i in 0..20 {
        rec.push_output(format!("line-{i:03}-some-padding-bytes").as_bytes());
    }
    for _ in 0..50 {
        if rec.stats().await.rotations >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stats = rec.stats().await;
    assert!(stats.rotations >= 1, "at least one rotation: {stats:?}");
    // First rotated file present.
    assert!(dir.join("p1.0001.cast.gz").exists(), "rotated gz missing");
    // Current cast is still writable and contains the post-rotation events.
    let current = std::fs::read_to_string(dir.join("p1.cast")).unwrap_or_default();
    assert!(
        !current.is_empty(),
        "post-rotation cast should resume writing"
    );
}

#[tokio::test]
async fn retention_evicts_oldest_gz_when_over_cap() {
    let dir = tempdir();
    let cfg = RecorderConfig {
        dir: dir.clone(),
        basename: "p1".into(),
        rotate_bytes: 256,     // force many rotations
        retention_bytes: 1024, // tiny cap → evicts after a few rotations
    };
    let (rec, _handle) = Recorder::start(cfg).expect("start");
    // Generate enough events to push past 1 KiB of .gz files.
    for i in 0..150 {
        rec.push_output(format!("padding-bytes-row-{i:03}").as_bytes());
    }
    for _ in 0..100 {
        if rec.stats().await.evictions >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let stats = rec.stats().await;
    assert!(stats.rotations >= 2, "need multiple rotations: {stats:?}");
    assert!(
        stats.evictions >= 1,
        "expected at least one eviction: {stats:?}"
    );
}

#[tokio::test]
async fn checkpoint_event_serializes() {
    let dir = tempdir();
    let cfg = RecorderConfig::new(dir.clone(), "p1");
    let (rec, _handle) = Recorder::start(cfg).expect("start");
    rec.push_checkpoint(42, "deadbeef");
    for _ in 0..50 {
        if rec.stats().await.events_written >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cast = std::fs::read_to_string(dir.join("p1.cast")).expect("read cast");
    let line = cast.lines().next().expect("at least one line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("parse");
    assert_eq!(parsed[1].as_str().unwrap(), "s");
    assert_eq!(parsed[2]["seq"], 42);
    assert_eq!(parsed[2]["hash"], "deadbeef");
}
