//! Replay native terminal input and observe the resulting Red statusline.
//! Run on the same host as Red so the input and observation clocks agree.
use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, ValueEnum};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Scenario {
    Inside,
    Scroll,
    Reverse,
    Window,
    Buffer,
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    binary: PathBuf,
    #[arg(long, value_enum, default_value = "scroll")]
    scenario: Scenario,
    #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(1..=100000))]
    presses: u32,
    #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u64).range(0..=1000))]
    interval_ms: u64,
    #[arg(long, default_value_t = 40, value_parser = clap::value_parser!(u16).range(12..=512))]
    rows: u16,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u16).range(50..=1024))]
    cols: u16,
    /// Send this many same-direction inputs on schedule before waiting for the display.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100000))]
    batch: u32,
    #[arg(long = "config-override")]
    overrides: Vec<String>,
    /// Use a representative file with sufficient lines.
    #[arg(long)]
    file: Option<PathBuf>,
    /// Print stage timings in the log file at this path.
    #[arg(long)]
    trace: Option<PathBuf>,
    /// Write Red's fixed-size timing histograms on exit.
    #[arg(long, conflicts_with = "trace")]
    bounded: Option<PathBuf>,
}

// ConPTY asks its terminal for the inherited cursor position before it will
// paint. Parse the request through VT100 so split reads are handled correctly.
#[derive(Default)]
struct TerminalReplies(Vec<u8>);

impl vt100::Callbacks for TerminalReplies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        first: Option<u8>,
        second: Option<u8>,
        params: &[&[u16]],
        command: char,
    ) {
        if (first, second, command) == (None, None, 'n') && params == [&[6_u16][..]] {
            let (row, col) = screen.cursor_position();
            write!(self.0, "\x1b[{};{}R", row + 1, col + 1).expect("writing to Vec");
        }
    }
}

struct Observer {
    parser: vt100::Parser<TerminalReplies>,
    chunks: Receiver<Vec<u8>>,
    bytes: usize,
    rows: u16,
    cols: u16,
}

impl Observer {
    fn position_visible(&self, line: usize) -> bool {
        let expected = format!("{line}:1");
        self.parser
            .screen()
            .contents_between(self.rows - 2, 0, self.rows - 2, self.cols)
            .split_whitespace()
            .any(|field| field == expected)
    }

    fn process(&mut self, chunk: &[u8], input: &mut impl Write) -> Result<()> {
        self.bytes += chunk.len();
        self.parser.process(chunk);
        let replies = &mut self.parser.callbacks_mut().0;
        if !replies.is_empty() {
            input.write_all(replies)?;
            input.flush()?;
            replies.clear();
        }
        Ok(())
    }

    fn observe(&mut self, line: usize, input: &mut impl Write) -> Result<Instant> {
        let deadline = Instant::now() + Duration::from_secs(12);
        while !self.position_visible(line) {
            let chunk = self
                .chunks
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .with_context(|| {
                    format!(
                        "waiting for {line}:1 on screen:\n{}",
                        self.parser.screen().contents()
                    )
                })?;
            self.process(&chunk, input)?;
            ensure!(Instant::now() < deadline, "timeout waiting for {line}:1");
        }
        Ok(Instant::now())
    }
}

fn distribution(mut samples: Vec<u128>) -> serde_json::Value {
    samples.sort_unstable();
    let quantile = |p: usize| samples[(samples.len() * p).div_ceil(100) - 1];
    let over = |limit| samples.len() - samples.partition_point(|&sample| sample <= limit);
    json!({"count": samples.len(), "p50": quantile(50), "p95": quantile(95),
        "p99": quantile(99), "max": samples.last(), "over_8_ms": over(8_000),
        "over_16_ms": over(16_000), "over_32_ms": over(32_000)})
}

fn main() -> Result<()> {
    let args = Args::parse();
    let binary = args.binary.canonicalize()?;
    let dir = tempfile::tempdir()?;
    let (name, content) = match &args.file {
        Some(path) => (
            path.file_name()
                .context("missing fixture filename")?
                .to_owned(),
            std::fs::read_to_string(path)?,
        ),
        None => {
            let lines = (0..args.presses + u32::from(args.rows) * 2 + 10)
                .map(|i| format!("fn row_{i}() {{ /* 界 😀 */ }}\n"))
                .collect();
            ("navigation.rs".into(), lines)
        }
    };
    let start = match args.scenario {
        Scenario::Inside => usize::from(args.rows / 2),
        _ => usize::from(args.rows),
    };
    let max_line = start
        + match args.scenario {
            Scenario::Inside | Scenario::Window | Scenario::Buffer => 1,
            Scenario::Scroll => args.presses as usize,
            Scenario::Reverse => args.presses.div_ceil(2) as usize,
        };
    ensure!(content.lines().count() >= max_line, "fixture too short");
    let file = dir.path().join(name);
    std::fs::write(&file, &content)?;
    let switching = matches!(args.scenario, Scenario::Window | Scenario::Buffer);
    let peer = dir.path().join("nav-peer/peer.rs");
    if switching {
        std::fs::create_dir(peer.parent().context("missing peer directory")?)?;
        std::fs::write(&peer, &content)?;
    }
    let config = dir.path().join("config/red");
    std::fs::create_dir_all(&config)?;
    // Use an unshifted command-entry alias for fixture setup/teardown. ConPTY
    // synthesizes modifier state for punctuation according to the guest layout.
    std::fs::write(
        config.join("config.toml"),
        "[keys.normal]\nz = { EnterMode = \"Command\" }\n",
    )?;
    let mut cmd = CommandBuilder::new(&binary);
    cmd.cwd(dir.path());
    cmd.env("XDG_CONFIG_HOME", dir.path().join("config"));
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env(
        "RED_PERF",
        if args.trace.is_some() {
            "trace"
        } else if args.bounded.is_some() {
            "bounded"
        } else {
            "off"
        },
    );
    for setting in [
        "lsp.enabled=false",
        "show_whats_new=false",
        "fetch_release_notes=false",
        "statusline.left=[]",
        "statusline.right=[\"position\"]",
    ] {
        cmd.args(["--config-override", setting]);
    }
    for setting in &args.overrides {
        cmd.args(["--config-override", setting]);
    }
    if let Some(log) = args.trace.as_ref().or(args.bounded.as_ref()) {
        let log = std::path::absolute(log)?;
        cmd.args([
            "--config-override",
            &format!("log_file={}", serde_json::to_string(&log)?),
        ]);
    }
    cmd.arg(&file);
    let pair = native_pty_system().openpty(PtySize {
        rows: args.rows,
        cols: args.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let (tx, rx) = sync_channel(32);
    thread::spawn(move || {
        let mut buf = [0; 16384];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let result = (|| -> Result<()> {
        let mut screen = Observer {
            parser: vt100::Parser::new_with_callbacks(
                args.rows,
                args.cols,
                0,
                TerminalReplies::default(),
            ),
            chunks: rx,
            bytes: 0,
            rows: args.rows,
            cols: args.cols,
        };
        screen.observe(1, &mut writer)?;
        write!(writer, "{}j", start - 1)?;
        writer.flush()?;
        screen.observe(start, &mut writer)?;
        if switching {
            let command = match args.scenario {
                Scenario::Window => "zvsplit nav-peer/peer.rs\r",
                _ => "zedit nav-peer/peer.rs\r",
            };
            writer.write_all(command.as_bytes())?;
            writer.flush()?;
            screen.observe(1, &mut writer)?;
        }
        let mut line = start;
        let mut latency = Vec::new();
        let mut lateness = Vec::new();
        let bytes_before = screen.bytes;
        let begin = Instant::now();
        let mut batch_len = 0;
        for i in 0..args.presses {
            let due = begin + Duration::from_millis(u64::from(i) * args.interval_ms);
            thread::sleep(due.saturating_duration_since(Instant::now()));
            if batch_len > 0 {
                while let Ok(chunk) = screen.chunks.try_recv() {
                    screen.process(&chunk, &mut writer)?;
                }
            }
            let sent = Instant::now();
            let (keys, next_line): (&[u8], usize) = match args.scenario {
                Scenario::Window => (b"\x17w", if i % 2 == 0 { start } else { 1 }),
                Scenario::Buffer => (b" n", if i % 2 == 0 { start } else { 1 }),
                Scenario::Inside if i % 2 == 1 => (b"k", line - 1),
                Scenario::Reverse if i >= args.presses.div_ceil(2) => (b"k", line - 1),
                _ => (b"j", line + 1),
            };
            writer.write_all(keys)?;
            writer.flush()?;
            line = next_line;
            batch_len += 1;
            let direction_boundary = match args.scenario {
                Scenario::Inside | Scenario::Window | Scenario::Buffer => true,
                Scenario::Reverse => i + 1 == args.presses.div_ceil(2),
                Scenario::Scroll => false,
            };
            if batch_len == args.batch || direction_boundary || i + 1 == args.presses {
                let seen = screen.observe(line, &mut writer)?;
                latency.push((seen - sent).as_micros());
                batch_len = 0;
            }
            lateness.push((sent - due).as_micros());
        }
        let bytes = screen.bytes - bytes_before;
        let elapsed = begin.elapsed();
        // An independent document oracle catches incorrect cursor/buffer state even
        // if a statusline happened to show the expected coordinates.
        writer.write_all(b"0iRED_NAV_OK\x1b")?;
        writer.flush()?;
        // Give terminals time to disambiguate a standalone Escape from Alt+:.
        thread::sleep(Duration::from_millis(150));
        writer.write_all(if switching { b"zwqall\r" } else { b"zwq\r" })?;
        writer.flush()?;
        let end = Instant::now() + Duration::from_secs(12);
        loop {
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "editor exited with {status:?}");
                break;
            }
            if Instant::now() >= end {
                bail!("editor failed to quit");
            }
            // Keep draining large final repaints while the child is exiting.
            let _ = screen.chunks.recv_timeout(Duration::from_millis(5));
        }
        let offset = content
            .split_inclusive('\n')
            .take(line - 1)
            .map(str::len)
            .sum();
        let edited = if switching && line == 1 { &peer } else { &file };
        if switching {
            let other = if edited == &peer { &file } else { &peer };
            ensure!(
                std::fs::read(other)? == content.as_bytes(),
                "inactive document changed"
            );
        }
        let mut expected = content;
        expected.insert_str(offset, "RED_NAV_OK");
        ensure!(
            std::fs::read(edited)? == expected.as_bytes(),
            "saved document differs at final position"
        );
        println!(
            "{}",
            json!({"scenario": format!("{:?}", args.scenario), "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH, "rows": args.rows, "cols": args.cols, "input_count": args.presses,
            "batch_limit": args.batch, "interval_ms": args.interval_ms, "elapsed_us": elapsed.as_micros(), "bytes_through_last_observation": bytes,
            "final_line": line, "document_verified": true, "send_to_statusline_us": distribution(latency),
            "injection_lateness_us": distribution(lateness)})
        );
        Ok(())
    })();
    if result.is_err() {
        if let Some(status) = child.try_wait()? {
            return result.with_context(|| format!("editor exited with {status:?}"));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
