//! Where AI 助手's conversations run: a thread of its own with a tokio
//! runtime, the model services' HTTP client, and the conversations' file.
//! The UI only hands it work and polls for what came of it.

use std::path::Path;
use std::sync::{Arc, mpsc};

use gpui_kit::*;
use shellrs_assistant::{
    Env, FixedEnv, ProviderConfig, ProviderError, ReqwestTransport, SharedEnv, Store, SystemEnv,
    Transport, check_provider, list_models,
};

use crate::i18n::t;

/// What the assistant reaches the world with.
#[derive(Clone)]
pub struct AssistantServices {
    pub transport: Arc<dyn Transport>,
    pub store: Store,
    pub env: SharedEnv,
}

impl AssistantServices {
    /// ShellRS's own: the model services over HTTPS with the app's
    /// User-Agent, and the conversations in `assistant.db` beside the
    /// hosts' database.
    pub fn system(data_dir: &Path) -> Result<Self, String> {
        let user_agent = format!("ShellRS/{}", env!("CARGO_PKG_VERSION"));
        let transport = ReqwestTransport::new(&user_agent)?;
        let store =
            Store::open(&data_dir.join("assistant.db")).map_err(|error| error.to_string())?;
        Ok(Self {
            transport: Arc::new(transport),
            store,
            env: Arc::new(SystemEnv),
        })
    }

    /// For tests: `transport` answers, and the conversations are in memory.
    pub fn in_memory(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            store: Store::in_memory().expect("a database in memory"),
            env: Arc::new(FixedEnv::new(1_800_000_000_000)),
        }
    }

    pub fn env(&self) -> &dyn Env {
        self.env.as_ref()
    }
}

/// The thread the conversations run on: its runtime lives while the sender
/// does.
struct Runtime {
    handle: tokio::runtime::Handle,
    _stop: tokio::sync::oneshot::Sender<()>,
}

fn start_runtime() -> Option<Runtime> {
    let (handle_sender, handle) = mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    std::thread::Builder::new()
        .name("shellrs-assistant".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let _ = handle_sender.send(runtime.handle().clone());
            runtime.block_on(async {
                let _ = stopped.await;
            });
        })
        .ok()?;
    Some(Runtime {
        handle: handle.recv().ok()?,
        _stop: stop,
    })
}

/// AI 助手's services, shared by its panel, the model service dialog and
/// the settings page.
#[derive(Default)]
pub struct AssistantService {
    services: Option<AssistantServices>,
    runtime: Option<Runtime>,
    /// Why the services could not start: the conversations' file did not
    /// open, or the HTTP client could not be made.
    problem: Option<SharedString>,
}

impl AssistantService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use `services`, starting the thread they run on.
    pub fn set_services(
        &mut self,
        services: Result<AssistantServices, String>,
        cx: &mut Context<Self>,
    ) {
        match services {
            Ok(services) => {
                if self.runtime.is_none() {
                    self.runtime = start_runtime();
                }
                self.problem = self
                    .runtime
                    .is_none()
                    .then(|| t!("assistant.service.no_thread"));
                self.services = Some(services);
            }
            Err(error) => {
                self.problem = Some(t!("assistant.service.failed", error = error));
                self.services = None;
            }
        }
        cx.notify();
    }

    pub fn services(&self) -> Option<&AssistantServices> {
        self.services.as_ref()
    }

    pub fn store(&self) -> Option<&Store> {
        self.services.as_ref().map(|services| &services.store)
    }

    pub fn handle(&self) -> Option<&tokio::runtime::Handle> {
        self.runtime.as_ref().map(|runtime| &runtime.handle)
    }

    pub fn problem(&self) -> Option<SharedString> {
        self.problem.clone()
    }

    /// Ask `provider` one short question; the answer comes on the receiver.
    /// `None` without services.
    pub fn check(
        &self,
        provider: ProviderConfig,
    ) -> Option<mpsc::Receiver<Result<(), ProviderError>>> {
        let handle = self.handle()?;
        let transport = self.services.as_ref()?.transport.clone();
        let (answer, receiver) = mpsc::channel();
        handle.spawn(async move {
            let _ = answer.send(check_provider(&provider, transport).await);
        });
        Some(receiver)
    }

    /// Read the models `provider` lists; they come on the receiver. `None`
    /// without services.
    pub fn models(
        &self,
        provider: ProviderConfig,
    ) -> Option<mpsc::Receiver<Result<Vec<String>, ProviderError>>> {
        let handle = self.handle()?;
        let transport = self.services.as_ref()?.transport.clone();
        let (answer, receiver) = mpsc::channel();
        handle.spawn(async move {
            let _ = answer.send(list_models(&provider, transport).await);
        });
        Some(receiver)
    }
}
