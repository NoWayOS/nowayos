// SPDX-License-Identifier: MIT OR Apache-2.0
//! `glyph-atlas`: NoWayOS's build-time glyph atlas generator.
//!
//! `glyph-atlas [--out DIR] CRATE` reads `CRATE/glyphs.toml` and the font
//! files it pins, and writes the atlases into DIR (default
//! `CRATE/src/atlas`). `make atlases` runs it for `crates/nowayos-glyphs`;
//! `make atlases-check` runs it into `build/` and compares. Exit status: 0
//! written; 1 refused (a pin, a missing character, a malformed input),
//! with nothing written; 2 a command line not understood.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match nowayos_glyph_atlas::run(&args) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("glyph-atlas: FAIL - {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
