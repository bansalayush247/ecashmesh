//! Runs deterministic routing scenarios and scalable-router benchmark fixtures.

use ecashmesh_core::{BenchmarkScale, LargeGraphGenerator};

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|argument| argument == "benchmark")
    {
        run_benchmark(arguments.get(1).map(String::as_str));
        return;
    }
    match ecashmesh_core::verify_all_scenarios() {
        Ok(results) => {
            for result in results {
                println!(
                    "{}: ranked={:?}; rejected={:?}",
                    result.name, result.ranked, result.rejected
                );
            }
            println!("all deterministic routing scenarios passed");
        }
        Err(error) => {
            eprintln!("deterministic routing scenario failed: {error}");
            std::process::exit(1);
        }
    }
}

fn run_benchmark(scale: Option<&str>) {
    let Some(scale) = scale.and_then(parse_scale) else {
        eprintln!("usage: ecashmesh-simulator benchmark <100k|500k|1m|5m> [queries]");
        std::process::exit(2);
    };
    let query_count = std::env::args()
        .nth(3)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10);
    match LargeGraphGenerator::default().benchmark(scale, query_count) {
        Ok(report) => println!(
            "scale={:?} connectors={} edges={} queries={} qps={} p50_us={} p95_us={} p99_us={} memory_bytes={} cpu_work_us={} candidates={} nodes={} edges_explored={} cache_hit_bps={} pruning_bps={}",
            report.scale,
            report.connector_count,
            report.edge_count,
            report.query_count,
            report.qps,
            report.p50_latency_micros,
            report.p95_latency_micros,
            report.p99_latency_micros,
            report.estimated_memory_bytes,
            report.cpu_work_micros,
            report.candidates_generated,
            report.nodes_explored,
            report.edges_explored,
            report.cache_hit_basis_points,
            report.pruning_basis_points,
        ),
        Err(error) => {
            eprintln!("benchmark graph generation failed: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_scale(value: &str) -> Option<BenchmarkScale> {
    match value {
        "100k" => Some(BenchmarkScale::Connectors100k),
        "500k" => Some(BenchmarkScale::Connectors500k),
        "1m" => Some(BenchmarkScale::Connectors1m),
        "5m" => Some(BenchmarkScale::Connectors5m),
        _ => None,
    }
}
