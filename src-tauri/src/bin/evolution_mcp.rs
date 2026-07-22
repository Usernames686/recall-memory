fn main() {
    if let Err(err) = recall_evolution_lib::mcp::run_stdio() {
        eprintln!("evolution-mcp: {err}");
        std::process::exit(1);
    }
}
