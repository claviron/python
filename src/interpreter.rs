//! The process's one CPython interpreter: started once, with its own
//! installation or a declared venv, and prepared for the application that
//! runs in it: its import paths and its environment. Every way this crate
//! runs Python stands on it.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyListMethods};

use crate::error::{Error, Result};

/// What an application asks of the interpreter before its code is imported.
pub struct Prepare<'a> {
    /// Directories put first on `sys.path`, in order.
    pub module_paths: &'a [PathBuf],
    /// The venv the interpreter was started with, checked rather than chosen.
    pub virtual_environment: Option<&'a Path>,
    /// The application's `os.environ` beside the runtime essentials.
    pub environment: &'a HashMap<String, String>,
}

/// Start the linked CPython 3.15, with its installation or the declared venv.
/// Never let the runner executable or ambient PYTHONHOME select another stdlib.
pub fn initialize(virtual_environment: Option<&Path>) -> Result<()> {
    static INITIALIZATION: Mutex<()> = Mutex::new(());
    static ENVIRONMENT: OnceLock<Option<PathBuf>> = OnceLock::new();
    let _initialization = INITIALIZATION.lock().map_err(|_| {
        Error::Python("interpreter initialization lock poisoned".to_owned())
    })?;
    let executable = match virtual_environment {
        Some(venv) => {
            let config = std::fs::read_to_string(venv.join("pyvenv.cfg"))
                .map_err(|error| {
                    Error::Python(format!(
                        "cannot read virtual environment pyvenv.cfg: {error}"
                    ))
                })?;
            if !is_python_315_environment(&config) {
                return Err(Error::Python(
                    "virtual environment must be created from CPython 3.15"
                        .to_owned(),
                ));
            }
            if cfg!(windows) {
                venv.join("Scripts").join("python.exe")
            } else {
                venv.join("bin").join("python")
            }
        }
        None => installation_executable()?,
    };
    if !executable.is_file() {
        return Err(Error::Python(format!(
            "selected CPython executable does not exist: {}",
            executable.display()
        )));
    }
    let executable = CString::new(path_text(&executable)?)
        .map_err(|error| Error::Python(error.to_string()))?;
    let environment = virtual_environment
        .map(Path::canonicalize)
        .transpose()
        .map_err(|error| Error::Python(error.to_string()))?;

    // SAFETY: the config is initialised before use and cleared on every path;
    // the interpreter is started at most once, before any other Python call,
    // and the GIL it hands this thread is released as `Python::initialize`
    // releases it.
    unsafe {
        if pyo3::ffi::Py_IsInitialized() != 0 {
            return if ENVIRONMENT.get() == Some(&environment) {
                Ok(())
            } else {
                Err(Error::Python(
                    "interpreter ownership/environment differs; replace the worker instead of switching interpreters".to_owned(),
                ))
            };
        }
        let mut config = std::mem::MaybeUninit::<pyo3::ffi::PyConfig>::uninit();
        pyo3::ffi::PyConfig_InitPythonConfig(config.as_mut_ptr());
        let mut config = config.assume_init();
        // What `Py_InitializeEx(0)` leaves alone: the process owns its
        // signals, its argv and its stdio.
        config.install_signal_handlers = 0;
        config.parse_argv = 0;
        config.configure_c_stdio = 0;
        config.use_environment = 0;
        config.user_site_directory = 0;
        config.safe_path = 1;

        let config_ptr: *mut pyo3::ffi::PyConfig = &mut config;
        let status = pyo3::ffi::PyConfig_SetBytesString(
            config_ptr,
            &raw mut (*config_ptr).executable,
            executable.as_ptr(),
        );
        if pyo3::ffi::PyStatus_Exception(status) != 0 {
            pyo3::ffi::PyConfig_Clear(&mut config);
            return Err(Error::Python(
                "could not set the interpreter's executable".to_owned(),
            ));
        }
        let status = pyo3::ffi::Py_InitializeFromConfig(&config);
        pyo3::ffi::PyConfig_Clear(&mut config);
        if pyo3::ffi::PyStatus_Exception(status) != 0 {
            return Err(Error::Python(
                "the interpreter failed to start".to_owned(),
            ));
        }
        pyo3::ffi::PyEval_SaveThread();
        ENVIRONMENT.set(environment).map_err(|_| {
            Error::Python(
                "interpreter environment was already initialized".to_owned(),
            )
        })?;
    }
    Ok(())
}

/// The interpreter of the installation this process's libpython belongs to.
///
/// Found from the library the process actually loaded, the one its rpath
/// names, so the standard library always matches it, whatever `PATH`, the
/// working directory or the host binary's location say. Walking up from the
/// library's directory finds `bin/python3.15` for a prefix layout
/// (`<prefix>/lib`), a multiarch one (`<prefix>/lib/<triple>`) and a macOS
/// framework (`Versions/3.15/Python`). A static libpython lives inside the
/// host binary, so there is no installation to find: it needs a venv.
#[cfg(unix)]
fn installation_executable() -> Result<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let symbol = pyo3::ffi::Py_InitializeFromConfig as *const libc::c_void;
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    // SAFETY: dladdr reads the address and fills `info`; it does not retain
    // the pointer.
    let found = unsafe { libc::dladdr(symbol, info.as_mut_ptr()) };
    // SAFETY: zero-initialised, and filled by dladdr when it succeeds.
    let info = unsafe { info.assume_init() };
    if found == 0 || info.dli_fname.is_null() {
        return Err(Error::Python(
            "cannot locate the loaded libpython; declare a venv".to_owned(),
        ));
    }
    // SAFETY: dladdr returned a NUL-terminated path owned by the loader.
    let name = unsafe { CStr::from_ptr(info.dli_fname) };
    let library = PathBuf::from(OsStr::from_bytes(name.to_bytes()));
    let is_libpython = library.file_name().is_some_and(|file| {
        let file = file.to_string_lossy();
        file.starts_with("libpython") || file == "Python"
    });
    if !is_libpython {
        return Err(Error::Python(format!(
            "libpython is linked statically into {}; declare a venv",
            library.display()
        )));
    }
    library
        .ancestors()
        .skip(1)
        .map(|directory| directory.join("bin").join("python3.15"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            Error::Python(format!(
                "no bin/python3.15 belongs to the loaded {}; declare a venv",
                library.display()
            ))
        })
}

#[cfg(not(unix))]
fn installation_executable() -> Result<PathBuf> {
    Err(Error::Python(
        "declare a venv: this platform has no loaded-library lookup".to_owned(),
    ))
}

/// Prepare the interpreter for an application before its code is imported:
/// check the environment it runs in, put its directories first on
/// `sys.path` and give it its own `os.environ`.
pub fn prepare(
    py: Python<'_>,
    setup: &Prepare<'_>,
) -> Result<()> {
    let sys = py.import("sys").map_err(|error| python_error(py, error))?;
    let version: (u8, u8) = sys
        .getattr("version_info")
        .and_then(|value| {
            Ok((value.get_item(0)?.extract()?, value.get_item(1)?.extract()?))
        })
        .map_err(|error| python_error(py, error))?;
    if version != (3, 15) {
        return Err(Error::Python(
            "claviron-python requires CPython 3.15".to_owned(),
        ));
    }
    if let Some(environment) = setup.virtual_environment {
        let prefix: PathBuf = sys
            .getattr("prefix")
            .and_then(|value| value.extract())
            .map_err(|error| python_error(py, error))?;
        let requested = environment
            .canonicalize()
            .map_err(|error| Error::Python(error.to_string()))?;
        let current = prefix
            .canonicalize()
            .map_err(|error| Error::Python(error.to_string()))?;
        if requested != current {
            return Err(Error::Python(
                "the initialized interpreter belongs to a different environment; replace the worker to change environments".to_owned(),
            ));
        }
    }
    let sys_path =
        sys.getattr("path").map_err(|error| python_error(py, error))?;
    let sys_path = sys_path
        .cast_into::<PyList>()
        .map_err(|error| Error::Python(error.to_string()))?;
    for path in setup.module_paths.iter().rev() {
        let path = path_text(path)?;
        if !sys_path.contains(path).map_err(|error| python_error(py, error))? {
            sys_path
                .insert(0, path)
                .map_err(|error| python_error(py, error))?;
        }
    }
    // CPython processed the selected environment's site directories during
    // initialization, including PEP 829 .start files. Replaying addsitedir here
    // would run startup hooks again on every ASGI lifespan restart.
    application_environment(py, setup.environment)
        .map_err(|error| python_error(py, error))
}

/// `os.environ` as the application's: the runtime essentials the host keeps
/// and the declared values, nothing else of the host's. Its mapping is
/// replaced in place, so `putenv` never runs and the process environment,
/// which the host scrubs after boot, is left as it is.
fn application_environment(
    py: Python<'_>,
    values: &HashMap<String, String>,
) -> PyResult<()> {
    let environ = py.import("os")?.getattr("environ")?;
    let data = environ.getattr("_data")?;
    let encode_key = environ.getattr("encodekey")?;
    let encode_value = environ.getattr("encodevalue")?;
    let kept = PyDict::new(py);
    for key in claviron::runtime::env::RUNTIME_ESSENTIALS {
        let encoded = encode_key.call1((*key,))?;
        if data.contains(&encoded)? {
            kept.set_item(&encoded, data.get_item(&encoded)?)?;
        }
    }
    for (key, value) in values {
        kept.set_item(
            encode_key.call1((key,))?,
            encode_value.call1((value,))?,
        )?;
    }
    data.call_method0("clear")?;
    data.call_method1("update", (kept,))?;
    Ok(())
}

fn is_python_315_environment(config: &str) -> bool {
    let mut declared = false;
    for (key, value) in config.lines().filter_map(|line| line.split_once('=')) {
        if matches!(key.trim(), "version" | "version_info") {
            if !value.trim().split('.').take(2).eq(["3", "15"]) {
                return false;
            }
            declared = true;
        }
    }
    declared
}

#[cfg(test)]
#[path = "../tests/environment.rs"]
mod environment_tests;

pub(crate) fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Python(format!(
            "Python path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

pub(crate) fn python_error(
    py: Python<'_>,
    error: PyErr,
) -> Error {
    let rendered = py
        .import("traceback")
        .and_then(|traceback| traceback.call_method1("format_exception", (error.value(py),)))
        .and_then(|lines| lines.call_method0("__iter__"))
        .and_then(|iterator| {
            let mut rendered = String::new();
            loop {
                match iterator.call_method0("__next__") {
                    Ok(line) => rendered.push_str(&line.extract::<String>()?),
                    Err(stop) if stop.is_instance_of::<pyo3::exceptions::PyStopIteration>(py) => {
                        break;
                    }
                    Err(error) => return Err(error)
                }
            }
            Ok(rendered)
        })
        .unwrap_or_else(|_| error.to_string());
    Error::Python(rendered.trim().to_owned())
}
