//! `macp-conform`: run the MACP conformance fixtures against an
//! implementation and report the level it reaches. See [`macp_conform::cli`].

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    macp_conform::cli::main("macp-conform", std::env::args().skip(1)).await
}
