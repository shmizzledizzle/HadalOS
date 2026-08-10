//! The system tray, over D-Bus.
//!
//! A tray is not a drawing problem. There is no Wayland protocol for it: the
//! desktop-wide convention is **StatusNotifierItem**, where the panel hosts a
//! D-Bus service called `org.kde.StatusNotifierWatcher`, applications register
//! themselves against it, and the panel then reads each one's icon, title and
//! status back over IPC.
//!
//! So the dock is both a D-Bus **server** (the watcher applications find) and a
//! **client** (of every item that registers). Nothing appears until an
//! application volunteers.
//!
//! # Why the watcher must be claimed, not shared
//!
//! Exactly one process on the session bus may own the watcher name. If KDE's
//! own panel is running it already owns it, and cusk's dock will fail to claim
//! it — correctly, because two watchers would mean applications registering
//! with one and being displayed by the other. That case is reported rather than
//! worked around: an empty tray with a line saying who owns the name is honest,
//! and a second watcher fighting for it would be neither.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedValue, Value};

/// One tray icon, as far as drawing is concerned.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The bus name to talk back to when clicked.
    pub service: String,
    pub path: String,
    pub title: String,
    /// A theme icon name, resolved the same way desktop entries are.
    pub icon_name: Option<String>,
    /// Raw pixels, when the application ships its icon rather than naming one.
    pub pixmap: Option<Pixmap>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    /// RGBA, already converted from the wire's ARGB.
    pub rgba: Vec<u8>,
}

/// The shared snapshot the interface writes and the UI reads.
pub type Shared = Arc<Mutex<Vec<Item>>>;

/// Convert one `IconPixmap` entry to RGBA.
///
/// The wire format is **ARGB32 in network byte order**, which is not what any
/// renderer wants and not what the name suggests to a reader who skips the
/// specification. Getting this wrong does not fail — it produces an icon with
/// its channels rotated, which reads as a themeing bug rather than a decoding
/// one.
///
/// Rows are returned untouched: the protocol carries no stride, so the data is
/// tightly packed by definition.
pub fn argb_to_rgba(width: i32, height: i32, argb: &[u8]) -> Option<Pixmap> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let expected = w.checked_mul(h)?.checked_mul(4)?;
    // Truncated payloads happen; drawing whatever arrived would read past the
    // end or paint garbage, and both are worse than showing no icon.
    if argb.len() < expected {
        return None;
    }

    let mut rgba = Vec::with_capacity(expected);
    for px in argb[..expected].chunks_exact(4) {
        // ARGB -> RGBA.
        rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
    }
    Some(Pixmap { width: width as u32, height: height as u32, rgba })
}

/// Choose the largest pixmap offered.
///
/// Applications commonly send several sizes in one array. Taking the first
/// gives whichever the toolkit happened to list first — often 16x16 — and the
/// result is a visibly soft icon beside crisp ones.
pub fn largest(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<Pixmap> {
    pixmaps
        .iter()
        .max_by_key(|(w, h, _)| (*w as i64) * (*h as i64))
        .and_then(|(w, h, data)| argb_to_rgba(*w, *h, data))
}

/// Split a registration argument into a bus name and an object path.
///
/// The specification allows either form, and applications disagree about which
/// they send. A bare path means "me, at this path" — the bus name is the
/// sender's. Reading only one form loses every application that chose the
/// other, silently, because a registration that is not understood simply never
/// appears.
pub fn split_registration(argument: &str, sender: Option<&str>) -> Option<(String, String)> {
    const DEFAULT_PATH: &str = "/StatusNotifierItem";

    if let Some(rest) = argument.strip_prefix('/') {
        // A path: the service is whoever sent it.
        let sender = sender?;
        return Some((sender.to_string(), format!("/{rest}")));
    }
    if argument.is_empty() {
        return None;
    }
    // A bus name, possibly with a path appended after it.
    match argument.split_once('/') {
        Some((name, path)) if !name.is_empty() => {
            Some((name.to_string(), format!("/{path}")))
        }
        _ => Some((argument.to_string(), DEFAULT_PATH.to_string())),
    }
}

/// Read a property that may be absent or of an unexpected type.
fn text(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match properties.get(key).map(|v| v.downcast_ref::<zbus::zvariant::Str>()) {
        Some(Ok(s)) => {
            let s = s.to_string();
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

/// Build an `Item` from a bag of StatusNotifierItem properties.
///
/// Separated from the IPC so the decisions in it — which title wins, when a
/// pixmap beats a name — can be tested without a bus.
pub fn item_from_properties(
    service: &str,
    path: &str,
    properties: &HashMap<String, OwnedValue>,
) -> Item {
    // `Title` is the human name; `Id` is the application's own identifier and
    // is the fallback rather than the first choice, because it is frequently
    // something like "chrome_status_icon_1".
    let title = text(properties, "Title")
        .or_else(|| text(properties, "Id"))
        .unwrap_or_else(|| service.to_string());

    let icon_name = text(properties, "IconName");

    // Only decoded when there is no name to resolve. A theme icon matches the
    // rest of the desktop; a shipped pixmap is whatever the application drew,
    // often at the wrong size for this bar.
    let pixmap = if icon_name.is_some() {
        None
    } else {
        properties
            .get("IconPixmap")
            .and_then(|v| pixmaps_from_value(v))
            .and_then(|list| largest(&list))
    };

    Item {
        service: service.to_string(),
        path: path.to_string(),
        title,
        icon_name,
        pixmap,
    }
}

fn pixmaps_from_value(value: &OwnedValue) -> Option<Vec<(i32, i32, Vec<u8>)>> {
    let array = match value.downcast_ref::<zbus::zvariant::Array>() {
        Ok(a) => a,
        Err(_) => return None,
    };
    let mut out = Vec::new();
    for entry in array.iter() {
        let Value::Structure(s) = entry else { continue };
        let fields = s.fields();
        let (Some(Value::I32(w)), Some(Value::I32(h)), Some(Value::Array(bytes))) =
            (fields.first(), fields.get(1), fields.get(2))
        else {
            continue;
        };
        let data: Vec<u8> = bytes
            .iter()
            .filter_map(|b| match b {
                Value::U8(byte) => Some(*byte),
                _ => None,
            })
            .collect();
        out.push((*w, *h, data));
    }
    (!out.is_empty()).then_some(out)
}

/// The `org.kde.StatusNotifierItem` side: what the dock reads from each item.
#[zbus::proxy(
    interface = "org.kde.StatusNotifierItem",
    default_path = "/StatusNotifierItem"
)]
trait StatusNotifierItem {
    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;
    /// Left click. The coordinates are advisory and applications largely
    /// ignore them, but the signature requires them.
    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;
}

/// The service applications look for.
struct Watcher {
    items: Shared,
    /// Registration order, so the tray does not reshuffle when an item's
    /// properties are re-read.
    order: Arc<Mutex<Vec<(String, String)>>>,
    connection: Arc<Mutex<Option<zbus::Connection>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        let sender = header.sender().map(|s| s.to_string());
        let Some((service, path)) = split_registration(service, sender.as_deref()) else {
            eprintln!("tray: ignoring a registration that names no service");
            return;
        };
        {
            let mut order = self.order.lock().unwrap();
            if order.iter().any(|(s, p)| s == &service && p == &path) {
                return;
            }
            order.push((service.clone(), path.clone()));
        }
        eprintln!("tray: {service} registered");
        let connection = self.connection.lock().unwrap().clone();
        if let Some(connection) = connection {
            refresh(&connection, &self.order, &self.items).await;
        }
    }

    async fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> {
        self.order
            .lock()
            .unwrap()
            .iter()
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// Always true: the dock *is* the host. An application that checks this
    /// and finds it false will not bother registering, so answering honestly
    /// here is what makes anything appear at all.
    #[zbus(property)]
    async fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    async fn protocol_version(&self) -> i32 {
        0
    }
}

/// Re-read every registered item and publish a fresh snapshot.
///
/// Items that no longer answer are dropped. An application that has exited
/// leaves its registration behind — nothing tells the watcher — so the list is
/// pruned by whether the process still responds rather than by any event.
async fn refresh(
    connection: &zbus::Connection,
    order: &Arc<Mutex<Vec<(String, String)>>>,
    items: &Shared,
) {
    let registered = order.lock().unwrap().clone();
    let mut fresh = Vec::new();
    let mut alive = Vec::new();

    for (service, path) in registered {
        // `GetAll` rather than a property at a time: one round trip instead of
        // three, and — more importantly — it hands back the same map
        // `item_from_properties` is tested against, so the decoding that runs
        // is the decoding that is tested. Reading each property through the
        // typed proxy left those tests proving nothing about this path.
        let properties = match zbus::fdo::PropertiesProxy::builder(connection)
            .destination(service.clone())
            .and_then(|b| b.path(path.clone()))
        {
            Ok(builder) => match builder.build().await {
                Ok(proxy) => match zbus::names::InterfaceName::try_from(
                    "org.kde.StatusNotifierItem",
                ) {
                    Ok(interface) => proxy.get_all(interface).await,
                    Err(_) => continue,
                },
                Err(_) => continue,
            },
            Err(_) => continue,
        };

        // Doubles as the liveness check: an application that has exited fails
        // here and is dropped, rather than left as a permanent blank icon.
        let Ok(properties) = properties else { continue };

        alive.push((service.clone(), path.clone()));
        fresh.push(item_from_properties(&service, &path, &properties));
    }

    *order.lock().unwrap() = alive;
    *items.lock().unwrap() = fresh;
}

/// Left-click an item.
///
/// Fire-and-forget on a throwaway connection rather than reusing the watcher's.
/// The watcher's connection lives on the tray thread and is not reachable from
/// the UI thread without plumbing a channel through iced — and a click that
/// takes a few milliseconds to open its own connection is imperceptible, where
/// a click that blocked the frame would not be.
///
/// Failures are logged and dropped. An application that has exited between the
/// last poll and the click cannot be activated, and there is nothing useful to
/// say to the user about it.
pub fn activate(item: &Item) {
    let (service, path) = (item.service.clone(), item.path.clone());
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return;
        };
        runtime.block_on(async move {
            let proxy = zbus::connection::Builder::session()
                .map(|b| b.build())
                .ok();
            let Some(connection) = proxy else { return };
            let Ok(connection) = connection.await else { return };
            let built = StatusNotifierItemProxy::builder(&connection)
                .destination(service.clone())
                .and_then(|b| b.path(path))
                .map(|b| b.build());
            let Ok(built) = built else { return };
            if let Ok(proxy) = built.await {
                // The coordinates are advisory; applications largely ignore
                // them, but the signature requires them.
                if let Err(e) = proxy.activate(0, 0).await {
                    eprintln!("tray: {service} did not accept the click: {e}");
                }
            }
        });
    });
}

/// Start the tray, on its own thread.
///
/// Returns the shared snapshot immediately; it fills in once applications
/// register. The runtime lives on a dedicated thread because iced owns the
/// main one, and the two event loops cannot be interleaved.
pub fn start() -> Shared {
    let items: Shared = Arc::new(Mutex::new(Vec::new()));
    let published = items.clone();

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(e) => {
                eprintln!("tray: no runtime, tray disabled: {e}");
                return;
            }
        };
        runtime.block_on(async move {
            let order = Arc::new(Mutex::new(Vec::new()));
            let connection_slot = Arc::new(Mutex::new(None));
            let watcher = Watcher {
                items: items.clone(),
                order: order.clone(),
                connection: connection_slot.clone(),
            };

            // Claiming the name is the whole handshake. Exactly one process may
            // own it, so a failure here means another panel is already the
            // tray — reported, not retried, because two watchers would split
            // the applications between them.
            let connection = match zbus::connection::Builder::session()
                .and_then(|b| b.name("org.kde.StatusNotifierWatcher"))
                .and_then(|b| b.serve_at("/StatusNotifierWatcher", watcher))
            {
                Ok(builder) => match builder.build().await {
                    Ok(connection) => connection,
                    Err(e) => {
                        eprintln!(
                            "tray: could not become the StatusNotifierWatcher ({e}).\n      \
                             Another panel already owns it; the tray will stay empty."
                        );
                        return;
                    }
                },
                Err(e) => {
                    eprintln!("tray: could not reach the session bus: {e}");
                    return;
                }
            };
            *connection_slot.lock().unwrap() = Some(connection.clone());
            eprintln!("tray: watching for status notifier items");

            // Polled rather than driven by PropertiesChanged. Items are few and
            // change slowly, and subscribing to every item's signals is a
            // second failure surface for a first version — this is honest about
            // being a poll, and the interval is long enough to be free.
            loop {
                refresh(&connection, &order, &items).await;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    });

    published
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire carries ARGB; renderers want RGBA. Getting this wrong does not
    /// fail, it rotates the channels — which looks like a theme problem.
    #[test]
    fn argb_becomes_rgba_in_the_right_order() {
        // One opaque red pixel: A=255 R=255 G=0 B=0.
        let pixmap = argb_to_rgba(1, 1, &[255, 255, 0, 0]).unwrap();
        assert_eq!(pixmap.rgba, vec![255, 0, 0, 255], "R G B A");
    }

    #[test]
    fn alpha_survives_the_conversion() {
        let pixmap = argb_to_rgba(1, 1, &[128, 10, 20, 30]).unwrap();
        assert_eq!(pixmap.rgba, vec![10, 20, 30, 128]);
    }

    /// A short payload must produce nothing rather than be drawn: the
    /// alternative is reading past the end or painting garbage.
    #[test]
    fn a_truncated_pixmap_is_refused() {
        assert!(argb_to_rgba(4, 4, &[0; 8]).is_none());
        assert!(argb_to_rgba(0, 0, &[]).is_none());
        assert!(argb_to_rgba(-1, 4, &[0; 64]).is_none());
    }

    /// Toolkits send several sizes at once. Taking the first gives whichever
    /// they listed first, often 16x16, and it is visibly soft beside the rest.
    #[test]
    fn the_largest_pixmap_wins() {
        let small = (2, 2, vec![255u8; 2 * 2 * 4]);
        let big = (8, 8, vec![128u8; 8 * 8 * 4]);
        let chosen = largest(&[small.clone(), big.clone()]).unwrap();
        assert_eq!((chosen.width, chosen.height), (8, 8));
        // Order must not matter.
        let chosen = largest(&[big, small]).unwrap();
        assert_eq!((chosen.width, chosen.height), (8, 8));
    }

    #[test]
    fn no_pixmaps_means_no_icon() {
        assert!(largest(&[]).is_none());
    }

    /// Applications disagree about which form they register with, and a form
    /// that is not understood never appears — silently.
    #[test]
    fn a_registration_may_be_a_bus_name_or_a_path() {
        assert_eq!(
            split_registration("org.example.App", None),
            Some(("org.example.App".into(), "/StatusNotifierItem".into())),
            "a bare bus name gets the default path"
        );
        assert_eq!(
            split_registration("/org/example/Item", Some(":1.42")),
            Some((":1.42".into(), "/org/example/Item".into())),
            "a bare path belongs to the sender"
        );
        assert_eq!(
            split_registration("org.example.App/Custom", None),
            Some(("org.example.App".into(), "/Custom".into())),
        );
    }

    /// A path with no sender cannot be attributed to anyone, and guessing
    /// would mean talking to the wrong process.
    #[test]
    fn a_path_without_a_sender_is_refused() {
        assert_eq!(split_registration("/org/example/Item", None), None);
        assert_eq!(split_registration("", Some(":1.1")), None);
    }

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    OwnedValue::try_from(zbus::zvariant::Str::from(*v)).unwrap(),
                )
            })
            .collect()
    }

    /// `Id` is the application's own identifier and is often something like
    /// `chrome_status_icon_1`, so it is the fallback and not the first choice.
    #[test]
    fn the_human_title_is_preferred_over_the_id() {
        let item = item_from_properties(
            ":1.5",
            "/StatusNotifierItem",
            &props(&[("Title", "Volume"), ("Id", "pulse_icon_1")]),
        );
        assert_eq!(item.title, "Volume");

        let item = item_from_properties(":1.5", "/x", &props(&[("Id", "pulse_icon_1")]));
        assert_eq!(item.title, "pulse_icon_1", "Id when there is no Title");
    }

    /// Something has to be shown, or an item with neither is invisible and
    /// unclickable while still occupying the bus.
    #[test]
    fn an_item_with_no_names_falls_back_to_its_bus_name() {
        let item = item_from_properties(":1.9", "/x", &props(&[]));
        assert_eq!(item.title, ":1.9");
    }

    /// An empty string is not a name. Trusting it gives a blank tooltip and,
    /// worse, an icon lookup for "".
    #[test]
    fn empty_properties_count_as_absent() {
        let item = item_from_properties(":1.5", "/x", &props(&[("Title", ""), ("IconName", "")]));
        assert_eq!(item.title, ":1.5");
        assert_eq!(item.icon_name, None);
    }
}

/// End-to-end over a real bus.
///
/// Ignored by default because it needs a session bus to itself — this machine's
/// KDE already owns the watcher name, and a second claim correctly fails. Run
/// it with:
///
/// ```text
/// dbus-run-session -- cargo test --  --ignored tray_
/// ```
///
/// Worth having rather than trusting the unit tests: every bug found in this
/// project's Wayland work came from running a real client against it, and the
/// D-Bus half has exactly the same shape — a handshake that compiles perfectly
/// and never completes.
#[cfg(test)]
mod e2e {
    use super::*;

    struct FakeItem;

    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl FakeItem {
        #[zbus(property)]
        async fn title(&self) -> String {
            "Fake Volume".to_string()
        }
        #[zbus(property)]
        async fn id(&self) -> String {
            "fake_1".to_string()
        }
        #[zbus(property)]
        async fn icon_name(&self) -> String {
            "audio-volume-high".to_string()
        }
    }

    #[test]
    #[ignore = "needs a private session bus; see the module comment"]
    fn tray_shows_an_item_that_registers() {
        let items = start();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            // The watcher runs on its own thread and claims the name there.
            // Waiting for the name rather than sleeping a fixed time is what
            // keeps this from being flaky on a loaded machine.
            let client = zbus::Connection::session().await.unwrap();
            let dbus = zbus::fdo::DBusProxy::new(&client).await.unwrap();
            let mut claimed = false;
            for _ in 0..50 {
                if dbus
                    .get_name_owner(
                        zbus::names::BusName::try_from("org.kde.StatusNotifierWatcher").unwrap(),
                    )
                    .await
                    .is_ok()
                {
                    claimed = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(claimed, "the dock never claimed the watcher name");

            // Serve a fake item and register it, exactly as an application does.
            let item = zbus::connection::Builder::session()
                .unwrap()
                .serve_at("/StatusNotifierItem", FakeItem)
                .unwrap()
                .build()
                .await
                .unwrap();
            let me = item.unique_name().unwrap().to_string();

            let watcher = zbus::Proxy::new(
                &client,
                "org.kde.StatusNotifierWatcher",
                "/StatusNotifierWatcher",
                "org.kde.StatusNotifierWatcher",
            )
            .await
            .unwrap();
            let _: () = watcher
                .call("RegisterStatusNotifierItem", &(me.as_str()))
                .await
                .unwrap();

            // The poll is on a two-second cycle; allow several.
            let mut seen = None;
            for _ in 0..80 {
                let snapshot = items.lock().unwrap().clone();
                if let Some(found) = snapshot.into_iter().next() {
                    seen = Some(found);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }

            let seen = seen.expect("the registered item never reached the snapshot");
            assert_eq!(seen.title, "Fake Volume", "Title must win over Id");
            assert_eq!(seen.icon_name.as_deref(), Some("audio-volume-high"));
        });
    }
}
