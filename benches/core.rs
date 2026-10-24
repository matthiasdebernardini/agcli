use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde_json::json;
use tokio::runtime::Runtime;

use agcli::{
    AgentCli, Command, CommandOutput, ExitCode, Invocation, LogLevel, NextAction, StreamEvent,
    truncate_lines_with_file,
};

// ---------------------------------------------------------------------------
// Helpers: build CLI fixtures at various scales
// ---------------------------------------------------------------------------

fn build_flat_cli(n: usize) -> AgentCli {
    let mut cli = AgentCli::new("bench", "Benchmark CLI").version("0.1.0");
    for i in 0..n {
        let name = format!("cmd{i}");
        let desc = format!("Command number {i}");
        cli = cli.command(
            Command::new(name.clone(), desc)
                .usage(format!("bench {name} <arg>"))
                .handler(|req| {
                    let val = req.arg(0).unwrap_or("x").to_string();
                    async move {
                        Ok(CommandOutput::new(json!({ "echo": val }))
                            .next_action(NextAction::new("bench", "Inspect root")))
                    }
                }),
        );
    }
    cli
}

fn build_nested_cli(breadth: usize, depth: usize) -> AgentCli {
    fn add_children(parent: Command, breadth: usize, depth: usize, level: usize) -> Command {
        if level >= depth {
            return parent;
        }
        let mut p = parent;
        for i in 0..breadth {
            let name = format!("sub{i}");
            let desc = format!("Level {level} child {i}");
            let child = Command::new(name, desc);
            let child = add_children(child, breadth, depth, level + 1);
            p = p.subcommand(child);
        }
        p
    }

    let mut cli = AgentCli::new("bench", "Nested benchmark CLI").version("0.1.0");
    for i in 0..breadth {
        let name = format!("top{i}");
        let desc = format!("Top-level {i}");
        let cmd = Command::new(name, desc);
        let cmd = add_children(cmd, breadth, depth, 1);
        cli = cli.command(cmd);
    }
    cli
}

// ---------------------------------------------------------------------------
// Benchmark: root invocation (exercises root_result + root_actions)
// ---------------------------------------------------------------------------

fn bench_root_invocation(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("root_invocation");
    for n in [5, 20, 50] {
        let cli = build_flat_cli(n);
        group.bench_with_input(BenchmarkId::new("flat", n), &cli, |b, cli| {
            b.iter(|| {
                rt.block_on(async {
                    let run = cli.run(["bench"]).await;
                    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
                });
            });
        });
    }
    for (breadth, depth) in [(3, 3), (5, 2)] {
        let cli = build_nested_cli(breadth, depth);
        let label = format!("nested_{breadth}x{depth}");
        group.bench_with_input(BenchmarkId::new(&label, breadth * depth), &cli, |b, cli| {
            b.iter(|| {
                rt.block_on(async {
                    let run = cli.run(["bench"]).await;
                    assert_eq!(run.exit_code(), ExitCode::SUCCESS);
                });
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: command execution (exercises resolve_command + handler)
// ---------------------------------------------------------------------------

fn bench_command_execution(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("command_execution");
    for n in [5, 20, 50] {
        let cli = build_flat_cli(n);
        let target = format!("cmd{}", n - 1); // resolve last command (worst case)
        group.bench_with_input(
            BenchmarkId::new("flat", n),
            &(cli, target),
            |b, (cli, t)| {
                b.iter(|| {
                    rt.block_on(async {
                        let run = cli.run(["bench", t, "hello"]).await;
                        assert_eq!(run.exit_code(), ExitCode::SUCCESS);
                    });
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: Invocation::parse at various argv sizes
// ---------------------------------------------------------------------------

fn bench_parse_invocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_invocation");
    for n in [2, 5, 10, 20] {
        let mut argv: Vec<String> = vec!["prog".to_string()];
        for i in 0..n {
            if i % 3 == 0 {
                argv.push(format!("--flag{i}=val{i}"));
            } else {
                argv.push(format!("arg{i}"));
            }
        }
        group.bench_with_input(BenchmarkId::new("argv", n), &argv, |b, argv| {
            b.iter(|| {
                let _ = Invocation::parse(argv.clone(), |_| false);
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: truncate_lines_with_file
// ---------------------------------------------------------------------------

fn bench_truncate(c: &mut Criterion) {
    let mut group = c.benchmark_group("truncate_lines_with_file");

    // No truncation path
    let small: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
    group.bench_function("no_truncation_10", |b| {
        b.iter(|| {
            let _ = truncate_lines_with_file(small.clone(), 100, "bench");
        });
    });

    // Truncation path (writes temp file)
    for total in [100, 1000] {
        let lines: Vec<String> = (0..total)
            .map(|i| format!("log line {i}: some data"))
            .collect();
        group.bench_with_input(BenchmarkId::new("truncated", total), &lines, |b, lines| {
            b.iter(|| {
                let result =
                    truncate_lines_with_file(lines.clone(), 20, "bench").expect("must work");
                // Clean up temp file to avoid disk bloat during benchmarks
                result.cleanup().expect("must clean up");
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: a streaming command (event serialization + terminal line)
// ---------------------------------------------------------------------------

/// A CLI whose `stream` command writes `events` log events, then returns.
fn build_stream_cli(events: u8) -> AgentCli {
    AgentCli::new("bench", "Streaming benchmark CLI").command(
        Command::new("stream", "Stream log events")
            .usage("bench stream")
            .stream_handler(move |_req, out| async move {
                for i in 0..events {
                    out.emit(&StreamEvent::Log {
                        level: LogLevel::Info,
                        message: format!("event {i}"),
                    })?;
                }
                Ok(CommandOutput::new(json!({ "events": events })))
            }),
    )
}

fn bench_stream(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("stream");
    for events in [0u8, 5, 50] {
        let cli = build_stream_cli(events);
        group.bench_with_input(BenchmarkId::new("events", events), &cli, |b, cli| {
            b.iter(|| {
                rt.block_on(async {
                    // In-process: the events are captured, then rendered with
                    // the terminal line the way `print` would.
                    let run = cli.run(["bench", "stream"]).await;
                    assert_eq!(run.stream_events().len(), usize::from(events));
                    let _ = run.to_json();
                });
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_root_invocation,
    bench_command_execution,
    bench_parse_invocation,
    bench_truncate,
    bench_stream,
);
criterion_main!(benches);
