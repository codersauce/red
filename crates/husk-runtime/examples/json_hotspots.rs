//! Focused release-build measurements for Husk JSON boundary conversion.

use std::{hint::black_box, time::Instant};

use husk_runtime::Value;
use serde_json::json;

const ROWS: usize = 64;
const CONVERSIONS: usize = 512;

fn main() {
    let scenario = std::env::args().nth(1).unwrap_or_else(|| "json".into());
    assert_eq!(scenario, "json", "unknown JSON performance scenario");

    let payload = json!({
        "cursor": { "x": 8, "y": 20 },
        "rows": (0..ROWS).map(|line| json!({
            "line": line,
            "kind": "source",
            "text": format!("fn function_{line}() {{ value(); }}"),
            "indentation": { "width": line % 8 * 4, "tabs": false },
        })).collect::<Vec<_>>()
    });
    let payloads = (0..CONVERSIONS)
        .map(|_| payload.clone())
        .collect::<Vec<_>>();

    let started = Instant::now();
    for payload in payloads {
        black_box(Value::from_json(black_box(payload)));
    }
    println!(
        "[{{\"scenario\":\"husk_json_conversion\",\"iterations\":{CONVERSIONS},\"elapsed_us\":{}}}]",
        started.elapsed().as_micros()
    );
}
