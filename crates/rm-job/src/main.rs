//! rm-job binary: parse, run, print, exit.

use clap::Parser;

fn main() {
    let cli = rm_job::Cli::parse();
    let (code, report) = rm_job::run(cli);
    println!("{}", report.out);
    eprint!("{}", report.err);
    std::process::exit(code);
}
