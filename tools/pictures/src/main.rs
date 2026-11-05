// SPDX-License-Identifier: MIT OR Apache-2.0
//! `pictures`: NoWayOS's build-time picture converter.
//!
//! `pictures [--out DIR] CRATE` reads `CRATE/pictures.toml` and the PNG and
//! JPEG files it pins, and writes each picture's pixels at each scale and
//! the module naming them into DIR (default: the module folder
//! `pictures.toml` names). `make atlases` runs it; `make atlases-check` runs
//! it into `build/` and compares. Exit status: 0 written; 1 refused (a pin,
//! a picture too small for a scale, a malformed input), with nothing
//! written; 2 a command line not understood.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match nowayos_pictures_tool::run(&args) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("pictures: FAIL - {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
