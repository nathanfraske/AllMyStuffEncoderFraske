//! `amst` — the AllMyStuff terminal, as a standalone binary.
//!
//! A bare `amst` opens a shell on this machine; `amst <machine>` opens one on
//! another machine you own, over the mesh. The whole implementation lives in
//! the [`allmystuff_term`] library, which also backs `allmystuff term` — so the
//! command and the subcommand are one feature. See the crate docs for the full
//! reference.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    allmystuff_term::run(&args)
}
