use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use husk_parser::parse_str;

const SMALL_SOURCE: &str = include_str!("../tests/fixtures/small.husk");
const MEDIUM_SOURCE: &str = include_str!("../tests/fixtures/medium.husk");
const LARGE_SOURCE: &str = include_str!("../tests/fixtures/large.husk");

fn bench_parse_small(c: &mut Criterion) {
    c.bench_function("parse_small_1kb", |b| {
        b.iter(|| parse_str(black_box(SMALL_SOURCE)))
    });
}

fn bench_parse_medium(c: &mut Criterion) {
    c.bench_function("parse_medium_30kb", |b| {
        b.iter(|| parse_str(black_box(MEDIUM_SOURCE)))
    });
}

fn bench_parse_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_large");
    group.sample_size(10); // Fewer samples for slow benchmark
    group.measurement_time(std::time::Duration::from_secs(60)); // Allow longer measurement
    group.throughput(Throughput::Bytes(LARGE_SOURCE.len() as u64));
    group.bench_function("parse_large_533kb", |b| {
        b.iter(|| parse_str(black_box(LARGE_SOURCE)))
    });
    group.finish();
}

/// Scaling benchmark to detect O(n) vs O(n²) behavior
fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_scaling");

    // Use progressively larger chunks of the medium file
    let sizes = [500, 1000, 2000, 4000, 8000];

    for &lines in &sizes {
        let source: String = MEDIUM_SOURCE
            .lines()
            .take(lines)
            .collect::<Vec<_>>()
            .join("\n");
        let bytes = source.len();

        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::new("lines", lines), &source, |b, s| {
            b.iter(|| parse_str(black_box(s)))
        });
    }

    group.finish();
}

/// Benchmark just the lexer to compare with parsing
fn bench_lexer_only(c: &mut Criterion) {
    use husk_lexer::Lexer;

    let mut group = c.benchmark_group("lexer_only");

    group.bench_function("lex_large_533kb", |b| {
        b.iter(|| {
            let tokens: Vec<_> = Lexer::new(black_box(LARGE_SOURCE)).collect();
            tokens
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_parse_small,
    bench_parse_medium,
    bench_parse_large,
    bench_scaling,
    bench_lexer_only,
);
criterion_main!(benches);
