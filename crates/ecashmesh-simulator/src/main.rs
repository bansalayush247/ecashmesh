//! Runs and verifies all deterministic `EcashMesh` routing scenarios.

fn main() {
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
