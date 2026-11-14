//! The one Python application of this process, in the host's lifecycle.
//!
//! The interpreter is process-wide and has one environment, so `python.yaml`
//! names one application rather than a list. A second application runs in a
//! process of its own and comes over Orbit.
//!
//! The application manager sees it as `python:<name>`. Stopping it runs ASGI
//! lifespan shutdown and its routes answer 503 until it starts again; starting
//! it boots it from the `python.yaml` published now. Its modules stay imported
//! in the interpreter, so a restart is a new lifespan, not new code: changed
//! code takes a new worker.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use claviron::app::{
    ApplicationControlResult, ApplicationController, ApplicationDescription,
    ApplicationId, ApplicationLifecycle, ApplicationManager, ApplicationName,
    ApplicationRuntime, ApplicationSnapshot,
};
use claviron::runtime::congif::{self, ConfigFile, Configurable};
use claviron::runtime::{
    AppState, Bootable, Component, ConfigCheck, Finalizable, registry,
};
use serde::Deserialize;

use crate::asgi::{
    PythonApplication, PythonApplicationHandle, RunningPythonApplication,
};

/// Where a host's application is written down, in its configuration directory.
pub const CONFIG_FILE: &str = "python.yaml";

/// The runtime the application manager lists the application under.
const RUNTIME: &str = "python";

#[derive(Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The application's name in the application manager.
    name: String,
    /// The application, as `module:attribute`.
    target: String,
    /// The directory the module is imported from.
    module_path: PathBuf,
    /// The Python environment. Unwritten, the installation selected at build
    /// time. Read at boot: an interpreter does not change its environment, so
    /// changing this takes a restart.
    venv: Option<PathBuf>,
    /// The application's environment, as `os.environ` shows it: these
    /// values, with `${NAME}` expanded from the host's environment and its
    /// env files, beside only the runtime essentials the host keeps. The
    /// host's own variables are not the application's. Read at boot.
    #[serde(default, alias = "environment")]
    env: BTreeMap<String, String>,
    /// Files of further values, read before `env` with their deployment
    /// variants; relative to the working directory.
    #[serde(default, deserialize_with = "claviron::helpers::de::one_or_many")]
    env_file: Vec<PathBuf>,
    /// Frameworks a management client shows, such as `django`. Informational.
    #[serde(
        default,
        alias = "framework",
        deserialize_with = "claviron::helpers::de::one_or_many"
    )]
    frameworks: Vec<String>,
    /// A symbolic icon code for management clients, never a path or URL.
    icon: Option<String>,
}

impl Config {
    fn id(&self) -> Result<ApplicationId, String> {
        let name =
            ApplicationName::new(self.name.as_str()).map_err(|error| {
                format!("{CONFIG_FILE} name {:?}: {error}", self.name)
            })?;
        Ok(ApplicationId::new(runtime().clone(), name))
    }

    fn description(&self) -> ApplicationDescription {
        let description = ApplicationDescription::new(RUNTIME, "asgi")
            .runner("claviron")
            .frameworks(self.frameworks.iter().map(String::as_str));
        match &self.icon {
            Some(icon) => description.icon(icon.as_str()),
            None => description,
        }
    }
}

fn runtime() -> &'static ApplicationRuntime {
    static RUNTIME_ID: std::sync::OnceLock<ApplicationRuntime> =
        std::sync::OnceLock::new();
    RUNTIME_ID.get_or_init(|| {
        ApplicationRuntime::new(RUNTIME).expect("static Python runtime name")
    })
}

pub(crate) type Handle = Arc<RwLock<Option<PythonApplicationHandle>>>;

/// What the application manager reads beside the running application, which
/// itself says whether its loop still runs.
struct Control {
    /// The identity and description of the last boot; none before the first,
    /// and none in a process that serves nothing.
    booted: Option<(ApplicationId, ApplicationDescription)>,
    starting: bool,
    stopping: bool,
    enabled: bool,
    generation: u64,
    last_error: Option<String>,
}

pub(crate) struct PythonService {
    source: PathBuf,
    handle: Handle,
    running: Mutex<Option<RunningPythonApplication>>,
    control: Mutex<Control>,
    /// One boot, start, stop, restart or finalization at a time.
    commands: tokio::sync::Mutex<()>,
    /// Retires the running application's sessions when the process begins to
    /// go away, before its listeners wait for their connections.
    retire: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PythonService {
    pub(crate) fn handle(&self) -> Handle {
        Arc::clone(&self.handle)
    }

    /// Boot the application `python.yaml` names now, where none runs.
    async fn load(
        &self,
        state: &AppState,
    ) -> Result<(), String> {
        let config = state
            .config_reader()
            .snapshot::<Self>()
            .map_err(|error| error.to_string())?;
        let id = config.id()?;
        {
            let mut control = self.control.lock().expect("python control lock");
            control.booted = Some((id.clone(), config.description()));
            control.starting = true;
            control.enabled = true;
        }
        let booted = boot(state, &config).await;
        let mut control = self.control.lock().expect("python control lock");
        control.starting = false;
        match booted {
            Ok(running) => {
                let sessions = running.handle();
                let shutdown = state.shutdown_token();
                let retire = tokio::spawn(async move {
                    shutdown.cancelled().await;
                    sessions.retire_sessions();
                });
                if let Some(previous) = self
                    .retire
                    .lock()
                    .expect("python retire lock")
                    .replace(retire)
                {
                    previous.abort();
                }
                *self.handle.write().expect("python handle lock") =
                    Some(running.handle());
                *self.running.lock().expect("python owner lock") =
                    Some(running);
                control.generation += 1;
                control.last_error = None;
                tracing::info!(
                    application = %id,
                    target = %config.target,
                    generation = control.generation,
                    "🐍 Python application started"
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(application = %id, %error, "🐍 Python application failed to start");
                control.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Stop routing to the application, then run its lifespan shutdown.
    async fn unload(&self) -> Result<(), String> {
        *self.handle.write().expect("python handle lock") = None;
        if let Some(retire) =
            self.retire.lock().expect("python retire lock").take()
        {
            retire.abort();
        }
        let running = self.running.lock().expect("python owner lock").take();
        let Some(running) = running else {
            return Ok(());
        };
        self.control.lock().expect("python control lock").stopping = true;
        let stopped = tokio::task::spawn_blocking(move || running.shutdown())
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
        let mut control = self.control.lock().expect("python control lock");
        control.stopping = false;
        // A running application was booted, so it has its identity.
        let application = control.booted.as_ref().map(|(id, _)| id);
        match &stopped {
            Ok(()) => tracing::info!(
                application = application.map(tracing::field::display),
                generation = control.generation,
                "🐍 Python application stopped"
            ),
            Err(error) => {
                tracing::warn!(
                    application = application.map(tracing::field::display),
                    %error,
                    "🐍 Python application failed to stop"
                );
                control.last_error = Some(error.clone());
            }
        }
        stopped
    }

    fn snapshot(&self) -> Option<ApplicationSnapshot> {
        let looping = self
            .running
            .lock()
            .expect("python owner lock")
            .as_ref()
            .map(|running| !running.is_finished());
        let control = self.control.lock().expect("python control lock");
        let (id, description) = control.booted.clone()?;
        let lifecycle = if control.stopping {
            ApplicationLifecycle::Stopping
        } else if control.starting {
            ApplicationLifecycle::Starting
        } else {
            match (looping, control.enabled) {
                (Some(true), _) => ApplicationLifecycle::Running,
                // Its loop returned without being stopped.
                (Some(false), _) => ApplicationLifecycle::Failed,
                (None, false) => ApplicationLifecycle::Stopped,
                // Enabled with nothing running: its boot failed.
                (None, true) => ApplicationLifecycle::Failed,
            }
        };
        Some(ApplicationSnapshot {
            id,
            description,
            lifecycle,
            enabled: control.enabled,
            generation: control.generation,
            // The application loop exposes no pool counters.
            pool: None,
            last_error: control.last_error.clone(),
        })
    }
}

/// Prepare and boot the application `config` names.
async fn boot(
    state: &AppState,
    config: &Config,
) -> Result<RunningPythonApplication, String> {
    let mut application = PythonApplication::new(config.target.clone())
        .map_err(|error| error.to_string())?
        .module_path(&config.module_path);
    if let Some(venv) = &config.venv {
        application = application.virtual_environment(venv);
    }
    let environment = state
        .read_program_environment(&config.env_file, config.env.clone())
        .map_err(|error| format!("{CONFIG_FILE} env: {error}"))?;
    application = application.environment(environment.as_ref().clone());
    // Booting waits for ASGI lifespan startup; not on an async reactor.
    tokio::task::spawn_blocking(move || application.boot())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Bring up the application `python.yaml` names, in every process that serves,
/// and hand it to the application manager as the `python` runtime. With
/// `edge`, also install the `python:` route factory on Edge.
pub fn register(state: &AppState) -> registry::Result<()> {
    let service = Arc::new(PythonService {
        source: state.config_dir().join(CONFIG_FILE),
        handle: Arc::new(RwLock::new(None)),
        running: Mutex::new(None),
        control: Mutex::new(Control {
            booted: None,
            starting: false,
            stopping: false,
            enabled: false,
            generation: 0,
            last_error: None,
        }),
        commands: tokio::sync::Mutex::new(()),
        retire: Mutex::new(None),
    });
    #[cfg(feature = "webtransport")]
    let handle = service.handle();
    if let Some(manager) = state.registry_ref().resolve::<ApplicationManager>()
    {
        manager
            .add(Arc::new(PythonApplicationController {
                state: state.clone(),
                service: Arc::clone(&service),
            }))
            .map_err(|error| registry::Error::msg(error.to_string()))?;
    }
    state.registry_ref().register(service)?;
    #[cfg(feature = "edge")]
    if let Some(hosts) =
        state.registry_ref().resolve::<claviron::edge::HostAppRegistry>()
    {
        crate::backend::register(hosts.site())?;
        // The paths `python:` routes declare for native WebTransport sessions.
        #[cfg(feature = "webtransport")]
        crate::webtransport::register(state, handle)?;
    }
    Ok(())
}

#[async_trait]
impl Configurable for PythonService {
    type Config = Config;

    fn source(&self) -> ConfigFile {
        ConfigFile::yaml(&self.source)
    }

    async fn configure(
        &self,
        config: &Config,
    ) -> congif::Result<()> {
        config.id().map_err(congif::Error::msg)?;
        PythonApplication::new(config.target.clone())
            .map_err(congif::Error::msg)?;
        if !config.module_path.is_dir() {
            return Err(congif::Error::msg(
                "Python module_path must be an existing directory",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl Component<AppState> for PythonService {
    type Deps = ();

    fn name(&self) -> &'static str {
        "PythonService"
    }

    fn as_configurable(&self) -> Option<&dyn congif::ConfigRegistration> {
        Some(self)
    }

    fn as_bootable(&self) -> Option<&dyn Bootable<AppState, Self::Deps>> {
        Some(self)
    }

    fn as_finalizable(&self) -> Option<&dyn Finalizable<AppState>> {
        Some(self)
    }
}

#[async_trait]
impl Bootable<AppState, ()> for PythonService {
    async fn boot(
        &self,
        _: (),
        state: &AppState,
    ) -> registry::Result<()> {
        // A master serves nothing and a check runs nothing: neither needs an
        // interpreter or a copy of the application's memory.
        if state.is_master()
            || state.registry_ref().resolve::<ConfigCheck>().is_some()
        {
            return Ok(());
        }
        let _command = self.commands.lock().await;
        self.load(state).await.map_err(registry::Error::msg)
    }
}

#[async_trait]
impl Finalizable<AppState> for PythonService {
    async fn finalize(
        &self,
        _: &AppState,
    ) -> registry::Result<()> {
        let _command = self.commands.lock().await;
        self.unload().await.map_err(registry::Error::msg)
    }
}

/// The `python` runtime's side of the application manager.
struct PythonApplicationController {
    state: AppState,
    service: Arc<PythonService>,
}

impl PythonApplicationController {
    /// Refuse a name other than the application booted here.
    fn named(
        &self,
        name: &ApplicationName,
    ) -> ApplicationControlResult<()> {
        let control = self.service.control.lock().expect("python control lock");
        match &control.booted {
            Some((id, _)) if id.name() == name => Ok(()),
            _ => {
                Err(format!("no Python application {name} in this process")
                    .into())
            }
        }
    }
}

#[async_trait]
impl ApplicationController for PythonApplicationController {
    fn runtime(&self) -> &ApplicationRuntime {
        runtime()
    }

    fn list(&self) -> Vec<ApplicationSnapshot> {
        self.service.snapshot().into_iter().collect()
    }

    fn status(
        &self,
        name: &ApplicationName,
    ) -> Option<ApplicationSnapshot> {
        self.service.snapshot().filter(|snapshot| snapshot.id.name() == name)
    }

    async fn start(
        &self,
        name: &ApplicationName,
    ) -> ApplicationControlResult<()> {
        self.named(name)?;
        let _command = self.service.commands.lock().await;
        let looping = self
            .service
            .running
            .lock()
            .expect("python owner lock")
            .as_ref()
            .map(|running| !running.is_finished());
        match looping {
            Some(true) => return Ok(()),
            // A loop that returned by itself is joined before a new boot.
            Some(false) => self.service.unload().await?,
            None => {}
        }
        self.service.load(&self.state).await.map_err(Into::into)
    }

    async fn stop(
        &self,
        name: &ApplicationName,
    ) -> ApplicationControlResult<()> {
        self.named(name)?;
        let _command = self.service.commands.lock().await;
        self.service.control.lock().expect("python control lock").enabled =
            false;
        self.service.unload().await.map_err(Into::into)
    }

    /// Lifespan shutdown, then a new boot: requests in between answer 503.
    async fn restart(
        &self,
        name: &ApplicationName,
    ) -> ApplicationControlResult<()> {
        self.named(name)?;
        let _command = self.service.commands.lock().await;
        self.service.unload().await?;
        self.service.load(&self.state).await.map_err(Into::into)
    }
}

#[cfg(test)]
#[path = "../tests/service.rs"]
mod tests;
