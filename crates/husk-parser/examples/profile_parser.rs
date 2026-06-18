use husk_lexer::Lexer;
use husk_parser::parse_str;
use std::env;
use std::fs;
use std::time::Instant;

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("tests/fixtures/small.husk");

    let source = fs::read_to_string(path).expect("Failed to read file");
    let bytes = source.len();

    println!("File: {} ({} bytes)", path, bytes);

    // Time lexing separately
    let lex_start = Instant::now();
    let tokens: Vec<_> = Lexer::new(&source).collect();
    let lex_time = lex_start.elapsed();
    println!("Lexing: {:?} ({} tokens)", lex_time, tokens.len());

    // Time parsing
    let parse_start = Instant::now();
    let result = parse_str(&source);
    let parse_time = parse_start.elapsed();

    println!("Parsing: {:?}", parse_time);
    println!("Errors: {}", result.errors.len());

    // Calculate throughput
    let total_time = lex_time + parse_time;
    let tokens_per_sec = tokens.len() as f64 / total_time.as_secs_f64();
    let bytes_per_sec = bytes as f64 / total_time.as_secs_f64();

    println!("\nThroughput:");
    println!("  {:.0} tokens/sec", tokens_per_sec);
    println!("  {:.2} KB/sec", bytes_per_sec / 1024.0);
}
