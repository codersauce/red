fn main() -> std::process::ExitCode {
    match husk_lsp::run_stdio(husk_lsp::ServerOptions::default()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("husk-lsp: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
