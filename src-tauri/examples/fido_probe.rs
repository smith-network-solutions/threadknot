//! Diagnostic: does Threadknot's own detector see a FIDO key right now?
//! Run with and without the key inserted: `cargo run --example fido_probe`.
//! This calls the exact per-platform code the vault's "require my security
//! key" gate uses, so its answer is the sheet's answer.
fn main() {
    println!(
        "key_present() = {}",
        threadknot_lib::security_key::key_present()
    );
}
