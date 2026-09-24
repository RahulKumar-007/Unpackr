use crate::cli::Cli;
use clap::CommandFactory;
use clap_complete::{generate, Shell};

/// Generates shell completion scripts to stdout for the specified shell.
pub fn run_completions(shell: Shell) {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
}
