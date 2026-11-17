//! Validate the CPython this crate is built against.
//!
//! Python is selected the PyO3 way, with `PYO3_PYTHON` or
//! `PYO3_CONFIG_FILE`. Finding a shared libpython at run time is the host
//! binary's concern, and PyO3 has the standard tool for it: the host's own
//! `build.rs` calls `pyo3_build_config::add_libpython_rpath_link_args()`.
fn main() {
    let python = pyo3_build_config::get();
    assert_eq!(
        python.implementation(),
        pyo3_build_config::PythonImplementation::CPython,
        "claviron-python embeds the selected CPython build, not an alternative interpreter"
    );
    assert_eq!(
        python.version(),
        pyo3_build_config::PythonVersion { major: 3, minor: 15 },
        "claviron-python requires CPython 3.15; select the owned interpreter with PYO3_PYTHON"
    );
    assert!(
        matches!(
            python.target_abi().kind(),
            pyo3_build_config::PythonAbiKind::VersionSpecific(_)
        ),
        "claviron-python uses the selected CPython 3.15 ABI, not a multi-version abi3/abi3t build"
    );
}
