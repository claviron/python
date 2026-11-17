//! CPython embedded in a Claviron process, without a runner or listener.
//!
//! The interpreter is process-wide: one per process, with its installation or
//! a declared venv, prepared for the application that runs in it. With
//! `asgi`, a prepared [`asgi::PythonApplication`] boots its application loop
//! and awaits ASGI lifespan startup; its running owner drains accepted
//! exchanges and awaits lifespan shutdown before joining the loop thread. The
//! host owns registration, routing and process lifecycle.
//!
//! This crate does not provide isolated venvs or subinterpreters in one
//! process, so `python.yaml` names one application. The native Python module
//! is `claviron`.

#[cfg(feature = "asgi")]
pub mod asgi;
#[cfg(feature = "backend")]
pub mod backend;
#[cfg(feature = "broadcast")]
mod broadcast;
#[cfg(feature = "django")]
pub mod config;
#[cfg(feature = "asgi")]
mod counter;
#[cfg(feature = "django")]
mod django;
mod error;
pub mod interpreter;
#[cfg(feature = "log")]
mod log;
#[cfg(feature = "asgi")]
mod orbital;
#[cfg(feature = "asgi")]
pub mod routing;
#[cfg(feature = "backend")]
mod service;
#[cfg(feature = "websocket")]
pub mod websocket;
#[cfg(feature = "webtransport")]
mod webtransport;

#[cfg(feature = "backend")]
pub use service::{CONFIG_FILE, register};

pub use error::{Error, Result};
