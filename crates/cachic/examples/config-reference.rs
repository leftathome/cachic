//! Emit the configuration reference on stdout.
//!
//! `cargo run --example config-reference > docs/configuration.md`

fn main() {
    print!("{}", cachic::config::reference::render());
}
