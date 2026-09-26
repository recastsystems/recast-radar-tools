//! Link arguments for a Python extension module: on macOS the interpreter's
//! symbols resolve when the module is loaded (`-undefined dynamic_lookup`).
//! maturin adds the same flags; this makes a plain `cargo build` link too.

fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
