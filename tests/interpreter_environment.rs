//! A process of its own: the interpreter and its `os.environ` are
//! process-wide, and an application booting beside this test would replace
//! them.

use std::collections::HashMap;

use claviron_python::interpreter::{self, Prepare};
use pyo3::prelude::*;

/// The application sees what it declares and the runtime essentials, none of
/// the host's other variables, and declaring them writes nothing into the
/// process environment.
#[test]
fn an_application_environment_is_its_own() {
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    interpreter::initialize(None).unwrap();
    let environment = HashMap::from([(
        "CLAVIRON_PYTHON_DECLARED".to_owned(),
        "declared".to_owned(),
    )]);
    Python::attach(|py| {
        interpreter::prepare(
            py,
            &Prepare {
                module_paths: &[],
                virtual_environment: None,
                environment: &environment,
            },
        )
        .unwrap();
        let environ = py.import("os").unwrap().getattr("environ").unwrap();
        let declared: String = environ
            .get_item("CLAVIRON_PYTHON_DECLARED")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(declared, "declared");
        assert!(!environ.contains("CARGO_MANIFEST_DIR").unwrap());
        assert!(environ.contains("PATH").unwrap());
    });
    assert!(std::env::var_os("CLAVIRON_PYTHON_DECLARED").is_none());
}
