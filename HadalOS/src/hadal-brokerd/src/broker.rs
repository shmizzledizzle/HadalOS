//! `org.hadal.Broker1` — the root object.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use zbus::object_server::ObjectServer;
use zbus::{fdo, interface, message::Header, Connection};

use crate::capability::Capability;
use crate::executor::Executor;
use crate::model::ModelClient;
use crate::policy::Policy;
use crate::session::Session;

pub const PATH: &str = "/org/hadal/Broker1";
pub const NAME: &str = "org.hadal.Broker1";

pub struct Broker {
    model: Arc<ModelClient>,
    policy: Arc<Policy>,
    executor: Arc<Executor>,
    next_session: AtomicU64,
}

impl Broker {
    pub fn new(model: Arc<ModelClient>, policy: Arc<Policy>, executor: Arc<Executor>) -> Self {
        Self { model, policy, executor, next_session: AtomicU64::new(1) }
    }
}

#[interface(name = "org.hadal.Broker1")]
impl Broker {
    async fn create_session(
        &self,
        options: HashMap<String, zvariant::OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<zvariant::OwnedObjectPath> {
        let sender = header
            .sender()
            .map(|s| s.to_string())
            .ok_or_else(|| fdo::Error::AccessDenied("no sender on message".into()))?;

        // The owning uid comes from the bus, not from the caller. A client
        // cannot claim to be someone else, which is what makes `Owner`
        // meaningful to anything reading it.
        let dbus = fdo::DBusProxy::new(conn)
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let owner = dbus
            .get_connection_unix_user(sender.as_str().try_into().map_err(|_| {
                fdo::Error::InvalidArgs("sender is not a valid bus name".into())
            })?)
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot determine caller: {e}")))?;

        let get = |k: &str| -> Option<String> {
            options.get(k).and_then(|v| <&str>::try_from(v).ok()).map(str::to_owned)
        };

        let tier = match get("tier").as_deref() {
            Some("deep") => "deep",
            Some("reflex") => "reflex",
            _ => "auto",
        }
        .to_string();

        // Advisory only: recorded for audit and used to shape prompts. Never
        // consulted for authorization — a caller claiming surface="settings"
        // gains nothing by it.
        let surface = get("surface").unwrap_or_else(|| "unknown".into());

        let id = self.next_session.fetch_add(1, Ordering::SeqCst);
        let path = format!("{PATH}/session/{id}");

        let session = Session::new(
            tier,
            owner,
            surface.clone(),
            Arc::clone(&self.model),
            Arc::clone(&self.policy),
            Arc::clone(&self.executor),
        );

        server
            .at(path.as_str(), session)
            .await
            .map_err(|e| fdo::Error::Failed(format!("cannot register session: {e}")))?;

        tracing::info!(%sender, owner, %surface, "session {id} opened at {path}");

        zvariant::OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn available_capabilities(&self) -> HashMap<String, String> {
        Capability::ALL
            .iter()
            .map(|c| (c.id().to_string(), c.advisory_disposition().to_string()))
            .collect()
    }

    #[zbus(property)]
    async fn available_tiers(&self) -> Vec<String> {
        vec!["reflex".into(), "deep".into()]
    }

    #[zbus(property)]
    async fn ready(&self) -> bool {
        self.model.ready().await
    }

    #[zbus(property)]
    async fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}
