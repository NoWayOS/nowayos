// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where a test's approved image is: `tests/snapshots/NAME`.

use std::path::PathBuf;

/// The path of the approved image `name`.
pub fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name)
}
