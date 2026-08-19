//! Build-time patcher for the vendored Hunspell dictionary.
//!
//! The vendored `assets/dict/en_US.dic` stays byte-identical to upstream
//! SCOWL, so re-vendoring is a drop-in file swap. Local fixes live in
//! `assets/dict/en_US.patches` — one exact-line replacement per record,
//! `old -> new` — and are applied here into `$OUT_DIR/en_US.dic`, which
//! `src/sysdict.rs` embeds. A patch whose left-hand line is absent from the
//! dictionary fails the build: upstream either changed the line or already
//! ships the fix, and the manifest must be updated to match.
//!
//! The patching logic lives in `src/dictpatch.rs` (shared source via
//! `#[path]`) so that `cargo test` — which does not run build-script tests
//! — exercises it as a test-only module of the main crate.

#[path = "src/dictpatch.rs"]
mod dictpatch;

use dictpatch::apply_patches;
use std::env;
use std::fs;
use std::path::Path;

const DIC: &str = "assets/dict/en_US.dic";
const PATCHES: &str = "assets/dict/en_US.patches";

fn main() {
    println!("cargo:rerun-if-changed={DIC}");
    println!("cargo:rerun-if-changed={PATCHES}");

    let dic = fs::read_to_string(DIC).expect("read vendored dictionary");
    let patches = fs::read_to_string(PATCHES).expect("read patch manifest");

    let patched = apply_patches(&dic, &patches).unwrap_or_else(|msg| panic!("{PATCHES}: {msg}"));

    let out = Path::new(&env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("en_US.dic");
    fs::write(out, patched).expect("write patched dictionary");
}
