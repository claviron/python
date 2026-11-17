//! `claviron.orbital`: the fleet's Orbitals, from Python.
//!
//! An Orbital is one shared 64-bit value (`orbit-atomic`, through Claviron's
//! `OrbitalsProvider`): an `int`, a `float` or a `bool` in the fleet's shared
//! memory, the same for every worker of the application and for its Rust
//! routes. The Python object is a handle, not a copy: every read and write
//! goes to that memory, with an atomic, no lock and no message.
//!
//! ```python
//! import claviron
//!
//! progress = claviron.orbital.int(0)
//! progress.add(1)                              # the value after
//! same = claviron.orbital.open(progress.id)    # in any worker of the fleet
//! now = same.wait(1)                           # parks until it is not 1
//! ```
//!
//! Creating one is a declaration: it takes a slot of the fleet's table for as
//! long as the table lives, and nothing gives it back. Share it by passing
//! its id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use claviron::orbit::atomic::{Error, Kind};
use claviron::orbit::{Orbital, OrbitalId, Orbitals};
use pyo3::IntoPyObjectExt;
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyFloat, PyInt};

/// What ends a parked wait besides its value: the application's stop.
///
/// The word first, then a notify on every value something is parked on, so a
/// wait already parked is released and one about to park reads the word.
/// Nothing here measures time.
#[derive(Default)]
pub(crate) struct Waits {
    interrupt: AtomicBool,
    parked: Mutex<Parked>,
}

#[derive(Default)]
struct Parked {
    next: u64,
    waiting: HashMap<u64, Shared>,
}

impl Waits {
    /// End every wait, and every wait begun from now on.
    pub(crate) fn interrupt(&self) {
        self.interrupt.store(true, Ordering::SeqCst);
        let parked = self.parked.lock().expect("orbital waits lock");
        for shared in parked.waiting.values() {
            shared.notify();
        }
    }

    /// Announce a wait before it parks; the guard takes it back.
    fn park(
        &self,
        shared: &Shared,
    ) -> ParkedWait<'_> {
        let mut parked = self.parked.lock().expect("orbital waits lock");
        let id = parked.next;
        parked.next += 1;
        parked.waiting.insert(id, shared.clone());
        ParkedWait { waits: self, id }
    }
}

struct ParkedWait<'a> {
    waits: &'a Waits,
    id: u64,
}

impl Drop for ParkedWait<'_> {
    fn drop(&mut self) {
        self.waits
            .parked
            .lock()
            .expect("orbital waits lock")
            .waiting
            .remove(&self.id);
    }
}

/// A value of one of the three types, outside the interpreter: what a wait
/// carries while the GIL is released.
#[derive(Clone, Copy)]
enum Scalar {
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Scalar {
    /// A Python value as `kind`. An `int` is taken as a float where a float
    /// is held; nothing else is converted, and a `bool` is not an `int` here.
    fn from_python(
        kind: Kind,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        let boolean = value.is_instance_of::<PyBool>();
        let integer = !boolean && value.is_instance_of::<PyInt>();
        match kind {
            Kind::I64 if integer => value.extract().map(Self::Int),
            Kind::F64 if integer || value.is_instance_of::<PyFloat>() => {
                value.extract().map(Self::Float)
            }
            Kind::Bool if boolean => value.extract().map(Self::Bool),
            kind => Err(PyTypeError::new_err(format!(
                "Orbital: {} does not take {}",
                name(kind),
                value.get_type().name()?
            ))),
        }
    }

    fn into_python(
        self,
        py: Python<'_>,
    ) -> PyResult<Py<PyAny>> {
        match self {
            Self::Int(value) => value.into_py_any(py),
            Self::Float(value) => value.into_py_any(py),
            Self::Bool(value) => value.into_py_any(py),
        }
    }
}

/// The Python name of what an Orbital of `kind` holds.
fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::I64 => "an int Orbital",
        Kind::F64 => "a float Orbital",
        Kind::Bool => "a bool Orbital",
        Kind::U64 => "an unsigned Orbital",
    }
}

/// The typed handle. A Python `int` that fits is signed 64-bit here, so
/// there is no `u64` form.
#[derive(Clone)]
enum Shared {
    Int(Orbital<i64>),
    Float(Orbital<f64>),
    Bool(Orbital<bool>),
}

impl Shared {
    fn allocate(
        orbitals: &Orbitals,
        initial: Scalar,
    ) -> PyResult<Self> {
        match initial {
            Scalar::Int(value) => orbitals.allocate(value).map(Self::Int),
            Scalar::Float(value) => orbitals.allocate(value).map(Self::Float),
            Scalar::Bool(value) => orbitals.allocate(value).map(Self::Bool),
        }
        .map_err(error)
    }

    /// The id says which type it names, so this is one decode and one open.
    fn open(
        orbitals: &Orbitals,
        id: &str,
    ) -> PyResult<Self> {
        let id: OrbitalId = id.parse().map_err(error)?;
        match id.kind() {
            Kind::I64 => orbitals.open(id).map(Self::Int),
            Kind::F64 => orbitals.open(id).map(Self::Float),
            Kind::Bool => orbitals.open(id).map(Self::Bool),
            Kind::U64 => {
                return Err(PyValueError::new_err(format!(
                    "Orbital: {id} is unsigned; an Orbital here is an int, a float or a bool"
                )));
            }
        }
        .map_err(error)
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Int(_) => Kind::I64,
            Self::Float(_) => Kind::F64,
            Self::Bool(_) => Kind::Bool,
        }
    }

    fn id(&self) -> OrbitalId {
        match self {
            Self::Int(orbital) => orbital.id(),
            Self::Float(orbital) => orbital.id(),
            Self::Bool(orbital) => orbital.id(),
        }
    }

    fn load(&self) -> Scalar {
        match self {
            Self::Int(orbital) => Scalar::Int(orbital.load()),
            Self::Float(orbital) => Scalar::Float(orbital.load()),
            Self::Bool(orbital) => Scalar::Bool(orbital.load()),
        }
    }

    fn store(
        &self,
        value: Scalar,
    ) {
        match (self, value) {
            (Self::Int(orbital), Scalar::Int(value)) => orbital.store(value),
            (Self::Float(orbital), Scalar::Float(value)) => {
                orbital.store(value)
            }
            (Self::Bool(orbital), Scalar::Bool(value)) => orbital.store(value),
            // `Scalar::from_python` gives this Orbital's own type.
            _ => {}
        }
    }

    /// Atomic on the value; the value after comes back. An int wraps.
    fn add(
        &self,
        by: Scalar,
    ) -> Option<Scalar> {
        match (self, by) {
            (Self::Int(orbital), Scalar::Int(by)) => {
                Some(Scalar::Int(orbital.fetch_add(by).wrapping_add(by)))
            }
            (Self::Float(orbital), Scalar::Float(by)) => {
                Some(Scalar::Float(orbital.fetch_add(by) + by))
            }
            _ => None,
        }
    }

    /// Park until the value is no longer `current`: `Some` is what it is
    /// then, `None` is the interrupt.
    fn wait_until(
        &self,
        current: Scalar,
        interrupt: &AtomicBool,
    ) -> Result<Option<Scalar>, Error> {
        match (self, current) {
            (Self::Int(orbital), Scalar::Int(current)) => orbital
                .wait_until(current, interrupt)
                .map(|value| value.map(Scalar::Int)),
            (Self::Float(orbital), Scalar::Float(current)) => orbital
                .wait_until(current, interrupt)
                .map(|value| value.map(Scalar::Float)),
            (Self::Bool(orbital), Scalar::Bool(current)) => orbital
                .wait_until(current, interrupt)
                .map(|value| value.map(Scalar::Bool)),
            // `Scalar::from_python` gives this Orbital's own type.
            _ => Ok(None),
        }
    }

    fn notify(&self) {
        match self {
            Self::Int(orbital) => orbital.notify(),
            Self::Float(orbital) => orbital.notify(),
            Self::Bool(orbital) => orbital.notify(),
        }
    }
}

/// An id that names nothing here is the caller's mistake; a full table or an
/// I/O failure is the runtime's.
fn error(error: Error) -> PyErr {
    let message = format!("Orbital: {error}");
    match error {
        Error::Stale(_)
        | Error::KindMismatch { .. }
        | Error::UnknownKind(_)
        | Error::Malformed(_) => PyValueError::new_err(message),
        _ => PyRuntimeError::new_err(message),
    }
}

/// One shared value of the fleet. The id's kind and epoch are checked once,
/// where the handle is made; every operation after is the atomic one.
/// Collecting the object drops the handle, which changes nothing about the
/// value.
#[pyclass(name = "Orbital", module = "claviron", frozen)]
pub(crate) struct PythonOrbital {
    shared: Shared,
    waits: Arc<Waits>,
}

#[pymethods]
impl PythonOrbital {
    /// `<kind>:<slot>:<epoch>`: what another worker opens.
    #[getter]
    fn id(&self) -> String {
        self.shared.id().to_string()
    }

    fn get(
        &self,
        py: Python<'_>,
    ) -> PyResult<Py<PyAny>> {
        self.shared.load().into_python(py)
    }

    fn set(
        &self,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.shared.store(Scalar::from_python(self.shared.kind(), value)?);
        Ok(())
    }

    /// Add `by` and return the value after. An int wraps on overflow; a bool
    /// does not add.
    fn add(
        &self,
        py: Python<'_>,
        by: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let kind = self.shared.kind();
        let refused = || {
            PyTypeError::new_err(format!(
                "Orbital: {} does not add",
                name(kind)
            ))
        };
        if kind == Kind::Bool {
            return Err(refused());
        }
        match self.shared.add(Scalar::from_python(kind, by)?) {
            Some(after) => after.into_python(py),
            None => Err(refused()),
        }
    }

    /// Park until the value is no longer `current` and return what it is
    /// then; at once when it already differs. A float is compared by its
    /// bits.
    ///
    /// The calling thread parks with the GIL released, on the value itself,
    /// until a write changes it or the application is told to stop, which
    /// sets the word the wait also watches and then notifies the value. A
    /// wait ended by the stop raises `RuntimeError`.
    fn wait(
        &self,
        py: Python<'_>,
        current: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let current = Scalar::from_python(self.shared.kind(), current)?;
        let shared = &self.shared;
        let waits = &*self.waits;
        let outcome = py
            .detach(|| {
                let _parked = waits.park(shared);
                shared.wait_until(current, &waits.interrupt)
            })
            .map_err(error)?;
        match outcome {
            Some(value) => value.into_python(py),
            None => Err(PyRuntimeError::new_err(
                "Orbital: wait ended: the application is stopping",
            )),
        }
    }

    fn __repr__(&self) -> String {
        format!("claviron.orbital.open({:?})", self.shared.id().to_string())
    }
}

/// What `claviron.orbital` is bound to: the table, declaring and opening
/// values.
#[pyclass(name = "Orbitals", module = "claviron", frozen)]
pub(crate) struct PythonOrbitals {
    table: Arc<Orbitals>,
    waits: Arc<Waits>,
}

impl PythonOrbitals {
    pub(crate) fn new(
        table: Arc<Orbitals>,
        waits: Arc<Waits>,
    ) -> Self {
        Self { table, waits }
    }

    fn declare(
        &self,
        kind: Kind,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<PythonOrbital> {
        let initial = Scalar::from_python(kind, value)?;
        Ok(PythonOrbital {
            shared: Shared::allocate(&self.table, initial)?,
            waits: Arc::clone(&self.waits),
        })
    }
}

#[pymethods]
impl PythonOrbitals {
    /// Declare a signed 64-bit value. It takes a slot for the life of the
    /// fleet's table.
    fn int(
        &self,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<PythonOrbital> {
        self.declare(Kind::I64, value)
    }

    /// Declare a 64-bit float. It takes a slot for the life of the fleet's
    /// table.
    fn float(
        &self,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<PythonOrbital> {
        self.declare(Kind::F64, value)
    }

    /// Declare a bool. It takes a slot for the life of the fleet's table.
    fn bool(
        &self,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<PythonOrbital> {
        self.declare(Kind::Bool, value)
    }

    /// Open the value an id names. The id says which type it is.
    fn open(
        &self,
        id: &str,
    ) -> PyResult<PythonOrbital> {
        Ok(PythonOrbital {
            shared: Shared::open(&self.table, id)?,
            waits: Arc::clone(&self.waits),
        })
    }
}
