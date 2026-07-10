use std::{
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use husk::{Host, Value, Vm};

const WARMUP_ITERATIONS: usize = 200;
const MEASURED_ITERATIONS: usize = 2_000;
const PLUGIN_INSTRUCTION_BUDGET: usize = 100_000;
const CALLBACK_P95_BUDGET: Duration = Duration::from_millis(4);

struct BenchHost {
    viewport_layout: Value,
    effects: usize,
}

impl BenchHost {
    fn new() -> Self {
        Self {
            viewport_layout: Value::from_json(representative_viewport(20)),
            effects: 0,
        }
    }

    fn set_cursor_line(&mut self, line: usize) {
        self.viewport_layout = Value::from_json(representative_viewport(line));
    }
}

impl Host for BenchHost {
    fn log(&mut self, _message: &str) {}

    fn execute(&mut self, _plugin: &str, _action: &str, _args: &[Value]) -> anyhow::Result<Value> {
        self.effects += 1;
        Ok(Value::Unit)
    }

    fn query(&mut self, _plugin: &str, query: &str) -> anyhow::Result<Value> {
        match query {
            "viewport_layout" => Ok(self.viewport_layout.clone()),
            "editor_info" => Ok(Value::from_json(representative_editor_info())),
            other => anyhow::bail!("unexpected benchmark query `{other}`"),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let assert_budget = env::args().any(|arg| arg == "--assert");
    let mut vm = Vm::new();
    vm.set_instruction_budget(PLUGIN_INSTRUCTION_BUDGET);
    let mut host = BenchHost::new();
    vm.load_plugin_at(
        "indent_guides",
        "plugins/indent_guides.hk",
        include_str!("../plugins/indent_guides.hk"),
        &mut host,
    )?;

    for iteration in 0..WARMUP_ITERATIONS {
        host.set_cursor_line(iteration % 40);
        notify_cursor(&mut vm, &mut host, iteration)?;
    }

    let mut samples = Vec::with_capacity(MEASURED_ITERATIONS);
    for iteration in 0..MEASURED_ITERATIONS {
        host.set_cursor_line(iteration % 40);
        let started = Instant::now();
        notify_cursor(&mut vm, &mut host, iteration)?;
        samples.push(started.elapsed());
    }
    samples.sort_unstable();

    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    let p99 = percentile(&samples, 99);
    let max = samples.last().copied().unwrap_or_default();
    println!(
        "husk indent_guides cursor callback: count={} p50={}us p95={}us p99={}us max={}us effects={}",
        samples.len(),
        p50.as_micros(),
        p95.as_micros(),
        p99.as_micros(),
        max.as_micros(),
        host.effects,
    );

    if assert_budget && p95 > CALLBACK_P95_BUDGET {
        anyhow::bail!(
            "Husk callback p95 {}us exceeds {}us budget",
            p95.as_micros(),
            CALLBACK_P95_BUDGET.as_micros()
        );
    }

    Ok(())
}

fn notify_cursor(vm: &mut Vm, host: &mut BenchHost, iteration: usize) -> anyhow::Result<()> {
    let y = iteration % 40;
    vm.notify(
        "cursor:moved",
        black_box(serde_json::json!({
            "window_id": 1,
            "x": 8,
            "y": y,
            "lsp_character": 8,
        })),
        host,
    )
}

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    samples[(samples.len() - 1) * percentile / 100]
}

fn representative_viewport(cursor_y: usize) -> serde_json::Value {
    let rows = (0..48)
        .map(|line| {
            let depth = line % 8;
            let text = format!("{}call_{line}();", "    ".repeat(depth));
            serde_json::json!({
                "line": line,
                "text": text,
                "first_segment": true,
                "indent_width": depth * 4,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "buffer_index": 3,
        "revision": 1,
        "vtop": 0,
        "width": 120,
        "height": 48,
        "cursor": { "x": 8, "y": cursor_y },
        "indentation": {
            "shift_width": 4,
            "tab_width": 4,
        },
        "rows": rows,
    })
}

fn representative_editor_info() -> serde_json::Value {
    serde_json::json!({
        "theme": {
            "colors": {
                "editorIndentGuide.background": { "r": 80, "g": 80, "b": 80 },
                "editorIndentGuide.activeBackground": { "r": 160, "g": 160, "b": 160 },
                "editor.foreground": { "r": 220, "g": 220, "b": 220 },
                "editor.background": { "r": 16, "g": 16, "b": 16 },
            },
            "style": {
                "fg": { "r": 220, "g": 220, "b": 220 },
                "bg": { "r": 16, "g": 16, "b": 16 },
            },
            "gutter_style": { "fg": null },
        }
    })
}
