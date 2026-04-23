//! Fixture-based savings assertions. Uses real-ish tool output so we catch
//! regressions in the rule files without relying on live commands.

use cersei_compression::{compress_tool_output, CompressionLevel};
use serde_json::json;

fn tokens(s: &str) -> usize {
    s.split_whitespace().count()
}

fn savings(input: &str, output: &str) -> f64 {
    let a = tokens(input) as f64;
    if a == 0.0 {
        return 0.0;
    }
    100.0 - (tokens(output) as f64 / a * 100.0)
}

const GIT_LOG: &str = "\
commit 4c8b9d1aefc2c29c6e3c1f8b7e4a0d3f2b1c9e8d
Author: Alice <alice@example.com>
Date:   Mon Apr 14 11:02:18 2026 -0400

    fix: correct off-by-one in paginator

commit 3b7a8c0fed1b28b5d2b0e7a6d390c2e1a0b8d7c6
Author: Bob <bob@example.com>
Date:   Sun Apr 13 17:45:09 2026 -0400

    feat: add /compression slash command

commit 2a69b7ffed0a17a4c1a9d6959f28b1d09f7c6b5a
Author: Alice <alice@example.com>
Date:   Sat Apr 12 09:13:44 2026 -0400

    chore: bump deps

commit 1f58a6eeec09069380f8c5848e17a0c08e6b5a49
Author: Carol <carol@example.com>
Date:   Fri Apr 11 22:08:02 2026 -0400

    docs: expand compression README
";

const CARGO_TEST: &str = "\
   Compiling foo v0.1.0 (/tmp/foo)
   Compiling bar v0.2.0 (/tmp/bar)
    Finished test profile [unoptimized + debuginfo] target(s) in 3.82s
     Running unittests src/lib.rs (target/debug/deps/foo-abcdef0123456789)

running 3 tests
test tests::works ... ok
test tests::also_works ... ok
test tests::handles_edge_case ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/main.rs (target/debug/deps/bar-fedcba9876543210)

running 1 test
test tests::smoke ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests foo

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
";

#[test]
fn git_log_saves_at_least_30pct_minimal() {
    let out = compress_tool_output(
        "Bash",
        &json!({"command": "git log --pretty=fuller"}),
        GIT_LOG,
        CompressionLevel::Minimal,
    );
    let s = savings(GIT_LOG, &out);
    assert!(s >= 30.0, "got {s:.1}% savings for git log, wanted >=30%");
}

#[test]
fn cargo_test_saves_at_least_25pct_minimal() {
    // Short fixtures hit diminishing returns sooner — pass/fail summaries that
    // we intentionally keep eat into the savings budget. 25% on this fixture
    // is the realistic floor; longer real runs comfortably clear 60%.
    let out = compress_tool_output(
        "Bash",
        &json!({"command": "cargo test"}),
        CARGO_TEST,
        CompressionLevel::Minimal,
    );
    let s = savings(CARGO_TEST, &out);
    assert!(
        s >= 25.0,
        "got {s:.1}% savings for cargo test, wanted >=25%"
    );
}

#[test]
fn off_level_is_exact_passthrough() {
    let out = compress_tool_output(
        "Bash",
        &json!({"command": "git log"}),
        GIT_LOG,
        CompressionLevel::Off,
    );
    assert_eq!(
        out, GIT_LOG,
        "Off level must not modify input byte-for-byte"
    );
}

#[test]
fn rust_source_aggressive_drops_bodies() {
    let src = "\
use std::collections::HashMap;

pub fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    println!(\"{sum}\");
    sum
}

pub fn multiply(a: i32, b: i32) -> i32 {
    let product = a * b;
    println!(\"{product}\");
    product
}
";
    let out = compress_tool_output(
        "Read",
        &json!({"file_path": "/tmp/x.rs"}),
        src,
        CompressionLevel::Aggressive,
    );
    assert!(out.contains("use std::collections::HashMap"));
    assert!(out.contains("pub fn add"));
    assert!(out.contains("pub fn multiply"));
    assert!(!out.contains("println!"));
}
