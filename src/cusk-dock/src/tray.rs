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

use crate::menu;
use zbus::zvariant::{OwnedValue, Value};

/// What an item says about how much it wants to be seen.
///
/// Values are the specification's own strings. The distinction matters because
/// **`Passive` means "do not show me"** — an application that has nothing to
/// report says so this way rather than by unregistering, and a tray that draws
/// every registered item regardless shows a row of icons for things that are
/// idle. That is the difference between a tray and a list of running programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Hidden. The application is registered but has nothing to say.
    Passive,
    /// Shown normally.
    Active,
    /// Shown, and asking to be noticed.
    NeedsAttention,
}

impl Status {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("Passive") => Status::Passive,
            Some("NeedsAttention") => Status::NeedsAttention,
            // Absent or unrecognised counts as Active. An item that registered
            // and then failed to describe itself is more likely to be a
            // half-implemented client than something that wants hiding, and a
            // visible icon can be clicked while an absent one cannot be
            // discovered at all.
            _ => Status::Active,
        }
    }

    /// Whether the tray should draw this item.
    pub fn visible(self) -> bool {
        self != Status::Passive
    }
}

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
    pub status: Status,
    /// Object path of this item's `com.canonical.dbusmenu`, if it has one.
    ///
    /// A *second protocol on a second object*, not a property of this one — see
    /// `menu.rs`. Absent for items that offer no menu, which is why right-click
    /// has to degrade rather than assume.
    pub menu_path: Option<String>,
    /// The item asking that a **left** click open its menu instead of
    /// activating.
    ///
    /// Set by applications whose icon has no primary action — a network applet
    /// where "activate" means nothing but the menu is the whole point. Honouring
    /// it is what stops a left click from doing nothing at all on those items.
    pub is_menu: bool,
}

impl Item {
    /// Whether this item is asking to be noticed.
    ///
    /// Read by the view to give the tile an accent. `NeedsAttention` is the
    /// specification's only way for an application to say "look at me" without
    /// stealing focus, and a tray that renders it identically to `Active` has
    /// discarded the distinction the application went out of its way to make.
    pub fn needs_attention(&self) -> bool {
        self.status == Status::NeedsAttention
    }
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

    let status = Status::parse(text(properties, "Status").as_deref());

    // The attention icon wins while the item is asking to be noticed. That is
    // the entire visible effect of `NeedsAttention` here — no pulse, no
    // animation — and an item that sets the status but ships no attention icon
    // falls back to its ordinary one rather than to nothing.
    let icon_name = if status == Status::NeedsAttention {
        text(properties, "AttentionIconName").or_else(|| text(properties, "IconName"))
    } else {
        text(properties, "IconName")
    };

    // Only decoded when there is no name to resolve. A theme icon matches the
    // rest of the desktop; a shipped pixmap is whatever the application drew,
    // often at the wrong size for this bar.
    let pixmap = if icon_name.is_some() {
        None
    } else {
        properties
            .get("IconPixmap")
            .and_then(pixmaps_from_value)
            .and_then(|list| largest(&list))
    };

    Item {
        service: service.to_string(),
        path: path.to_string(),
        title,
        icon_name,
        pixmap,
        status,
        // `Menu` is an object path, so it arrives as `ObjectPath` rather than
        // `Str` and `text` cannot read it. Read separately, and `/NO_DBUSMENU`
        // is filtered out because that is what applications with no menu
        // publish rather than omitting the property.
        menu_path: object_path(properties, "Menu"),
        is_menu: properties
            .get("ItemIsMenu")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false),
    }
}

/// Read an object-path property.
///
/// Separate from `text` because the wire type differs: `Menu` is an
/// `ObjectPath`, and downcasting it to `Str` fails — which read as "this item
/// has no menu" for every item that had one.
fn object_path(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let path = properties
        .get(key)
        .and_then(|v| v.downcast_ref::<zbus::zvariant::ObjectPath>().ok())
        .map(|p| p.to_string())?;
    // The convention for "I have no menu". Kept as a filter rather than trusted
    // as a real path, because calling `GetLayout` on it fails per right-click
    // and logs noise for a condition that is entirely normal.
    if path.is_empty() || path == "/NO_DBUSMENU" {
        return None;
    }
    Some(path)
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
    /// Middle click. Genuinely distinct from `Activate` — for a volume applet
    /// it is mute, for a music player it is play/pause — so it is wired rather
    /// than aliased to the left click.
    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;
}

/// The menu behind a tray icon, as a second protocol on a second object.
///
/// `GetLayout`'s reply is `(u32, (ia{sv}av))` and cannot be usefully typed, so
/// it comes back as a `Value` and `menu::parse_layout` walks it. See `menu.rs`
/// for why that walk is a pure function.
#[zbus::proxy(interface = "com.canonical.dbusmenu")]
trait DBusMenu {
    /// `parent_id` 0 is the root; `-1` depth means "everything".
    ///
    /// The whole tree is fetched at once rather than a level per submenu open.
    /// Menus are small, the round trip is the expensive part, and fetching
    /// lazily would mean a submenu that opens empty and fills in a moment later.
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, zbus::zvariant::OwnedValue)>;

    /// Tell the application a row was clicked.
    ///
    /// `event_id` is the string `"clicked"`; the timestamp is advisory.
    fn event(
        &self,
        id: i32,
        event_id: &str,
        data: &zbus::zvariant::Value<'_>,
        timestamp: u32,
    ) -> zbus::Result<()>;

    /// Give the application a chance to rebuild a submenu before it is shown.
    ///
    /// Applications that populate lazily — a device list, a recent-files menu —
    /// return their contents only after this. Skipping it yields a menu that is
    /// correct for applications that build eagerly and empty for the ones where
    /// the contents are the point.
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;
}

/// The service applications look for.
struct Watcher {
    items: Shared,
    /// Registration order, so the tray does not reshuffle when an item's
    /// properties are re-read.
    order: Arc<Mutex<Vec<(String, String)>>>,
    connection: Arc<Mutex<Option<zbus::Connection>>>,
    /// Consecutive failures per item, so a busy application is not mistaken
    /// for an exited one. Shared with the poll loop: both call `refresh`, and
    /// two independent strike counts would each forgive what the other struck.
    strikes: Arc<Mutex<HashMap<(String, String), u32>>>,
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
            refresh(&connection, &self.order, &self.items, &self.strikes).await;
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

/// How many consecutive failures before an item is forgotten.
///
/// Not one. The first version dropped an item the moment a single `GetAll` did
/// not answer, and **nothing ever re-registers it** — the watcher is told about
/// an item once, when the application starts. So one slow reply, one moment of a
/// busy single-threaded application not servicing its bus, and the icon was gone
/// for the rest of the session. That is the likeliest reason a tray looks empty
/// while the applications are plainly running.
///
/// Three strikes at the poll interval is several seconds of genuine silence
/// before anything disappears, which distinguishes "busy" from "exited" about as
/// well as polling can.
const STRIKES: u32 = 3;

/// Re-read every registered item and publish a fresh snapshot.
///
/// Items that stop answering are dropped only after `STRIKES` consecutive
/// failures. An application that has exited leaves its registration behind —
/// nothing tells the watcher — so the list is pruned by whether the process
/// still responds, but *not* by a single missed reply.
///
/// A failing item keeps its **last known good** properties in the meantime,
/// rather than being blanked. An icon that flickers to a lettered placeholder
/// every time its application is briefly busy is worse than one that holds its
/// last state for a second.
async fn refresh(
    connection: &zbus::Connection,
    order: &Arc<Mutex<Vec<(String, String)>>>,
    items: &Shared,
    strikes: &Arc<Mutex<HashMap<(String, String), u32>>>,
) {
    let registered = order.lock().unwrap().clone();
    // Indexed for the last-known-good fallback below.
    let previous: HashMap<(String, String), Item> = items
        .lock()
        .map(|held| {
            held.iter()
                .map(|i| ((i.service.clone(), i.path.clone()), i.clone()))
                .collect()
        })
        .unwrap_or_default();
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
                    Ok(interface) => proxy.get_all(interface).await.map_err(Into::into),
                    Err(e) => Err(zbus::Error::from(e)),
                },
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };

        let key = (service.clone(), path.clone());
        match properties {
            Ok(properties) => {
                // Answered, so any accumulated strikes are forgiven. Reset
                // rather than decremented: an item that answers is alive, and
                // carrying a strike forward would eventually drop something
                // that had recovered.
                strikes.lock().unwrap().remove(&key);
                alive.push(key);
                fresh.push(item_from_properties(&service, &path, &properties));
            }
            Err(e) => {
                // The liveness check, but forgiving. An application that has
                // exited keeps failing and is dropped after `STRIKES`; one that
                // was merely busy answers next time and is kept.
                let mut held = strikes.lock().unwrap();
                let count = held.entry(key.clone()).or_insert(0);
                *count += 1;
                let done = *count >= STRIKES;
                if done {
                    held.remove(&key);
                    drop(held);
                    eprintln!("tray: {service} stopped answering, dropping it ({e})");
                } else {
                    // Kept, with its last known properties. Blanking it here
                    // would make a busy application's icon flicker to a
                    // placeholder and back.
                    drop(held);
                    alive.push(key.clone());
                    if let Some(last) = previous.get(&key) {
                        fresh.push(last.clone());
                    }
                }
            }
        }
    }

    *order.lock().unwrap() = alive;
    // Only what should be seen. `Passive` means "do not show me", and an item
    // that says so is filtered here rather than in the view, so every consumer
    // of the snapshot agrees about what the tray contains.
    *items.lock().unwrap() = fresh.into_iter().filter(|i| i.status.visible()).collect();
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

/// Run a fire-and-forget D-Bus call on a throwaway connection.
///
/// Every UI-thread call into the bus goes through this. A dedicated connection
/// per click rather than the watcher's: that one lives on the tray thread and is
/// not reachable from the UI thread without plumbing a channel through iced, and
/// a click that spends a few milliseconds connecting is imperceptible where one
/// that blocked the frame would not be.
fn detached<F>(work: F)
where
    F: FnOnce(zbus::Connection) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>
        + Send
        + 'static,
{
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return;
        };
        runtime.block_on(async move {
            let Ok(builder) = zbus::connection::Builder::session() else { return };
            let Ok(connection) = builder.build().await else { return };
            work(connection).await;
        });
    });
}

/// Middle-click an item.
///
/// A separate action, not an alias for the left click: for a volume applet this
/// is mute and for a player it is play/pause, and wiring it to `Activate` would
/// silently do the wrong thing rather than nothing.
pub fn secondary_activate(item: &Item) {
    let (service, path) = (item.service.clone(), item.path.clone());
    detached(move |connection| {
        Box::pin(async move {
            let built = StatusNotifierItemProxy::builder(&connection)
                .destination(service.clone())
                .and_then(|b| b.path(path))
                .map(|b| b.build());
            let Ok(built) = built else { return };
            if let Ok(proxy) = built.await {
                if let Err(e) = proxy.secondary_activate(0, 0).await {
                    eprintln!("tray: {service} refused the middle click: {e}");
                }
            }
        })
    });
}

/// Fetch an item's menu, blocking the calling thread.
///
/// Blocking is deliberate and is the one place the dock does it. A right-click
/// has to *show something* — an empty popup that fills in a frame later reads as
/// a broken menu, and there is nothing sensible to draw in the meantime. The
/// call is one round trip on the session bus to a local process.
///
/// The timeout is the safeguard: an application that has wedged must not take
/// the dock's UI thread with it. A wedged tray icon is a nuisance; a frozen dock
/// is the whole desktop's furniture.
pub fn fetch_menu(item: &Item) -> Vec<menu::Entry> {
    let Some(menu_path) = item.menu_path.clone() else { return Vec::new() };
    let service = item.service.clone();

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Vec::new();
    };
    runtime.block_on(async move {
        let fetch = async {
            let builder = zbus::connection::Builder::session().ok()?;
            let connection = builder.build().await.ok()?;
            let proxy = DBusMenuProxy::builder(&connection)
                .destination(service.clone())
                .ok()?
                .path(menu_path)
                .ok()?
                .build()
                .await
                .ok()?;

            // Asked before reading, so applications that populate lazily have
            // built their contents. The reply says whether anything changed and
            // is deliberately ignored: the layout is read either way, and
            // trusting a `false` from an application that had not in fact
            // populated yet would produce an empty menu.
            let _ = proxy.about_to_show(0).await;

            // An empty property list means "all of them". Naming the ones this
            // understands would silently drop anything added to the
            // specification later, and the reply is small.
            let (_revision, layout) = proxy.get_layout(0, -1, &[]).await.ok()?;
            Some(menu::parse_layout(&layout))
        };

        match tokio::time::timeout(std::time::Duration::from_millis(700), fetch).await {
            Ok(Some(entries)) => entries,
            Ok(None) => {
                eprintln!("tray: {} offered a menu that could not be read", item.service);
                Vec::new()
            }
            Err(_) => {
                eprintln!("tray: {} did not answer for its menu in time", item.service);
                Vec::new()
            }
        }
    })
}

/// Tell an application one of its menu rows was clicked.
pub fn click_menu_entry(item: &Item, id: i32) {
    let Some(menu_path) = item.menu_path.clone() else { return };
    let service = item.service.clone();
    detached(move |connection| {
        Box::pin(async move {
            let built = DBusMenuProxy::builder(&connection)
                .destination(service.clone())
                .and_then(|b| b.path(menu_path))
                .map(|b| b.build());
            let Ok(built) = built else { return };
            let Ok(proxy) = built.await else { return };
            // `"clicked"` is the event id the specification defines; the data
            // argument is unused for it and the timestamp is advisory, so an
            // empty variant and a zero are correct rather than lazy.
            let data = zbus::zvariant::Value::from(0i32);
            if let Err(e) = proxy.event(id, "clicked", &data, 0).await {
                eprintln!("tray: {service} refused a menu click: {e}");
            }
        })
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
            let strikes = Arc::new(Mutex::new(HashMap::new()));

            // The outer loop exists because of a failure observed on a live
            // session, not a theoretical one.
            //
            // The dock had logged `watching for status notifier items` at
            // startup, the process was still running hours later, and **nothing
            // owned `org.kde.StatusNotifierWatcher`**. The tray strip rendered
            // empty for the rest of the session with no log line and no
            // recovery.
            //
            // The name cannot have been stolen: zbus requests it with
            // `DoNotQueue` and *without* `AllowReplacement`, so another process
            // asking for it with `ReplaceExisting` is refused. What is left is
            // that the **connection died** while the process lived — a bus
            // restart, or a broken socket.
            //
            // That distinction decides the shape of the fix. `receive_name_lost`
            // is delivered *on the connection*, so a dead connection reports
            // nothing at all: watching for the signal cannot detect the case
            // that actually happened. The connection has to be rebuilt, so
            // ownership is **verified** each tick rather than assumed from a
            // successful claim at startup.
            loop {
                let watcher = Watcher {
                    items: items.clone(),
                    order: order.clone(),
                    connection: connection_slot.clone(),
                    strikes: strikes.clone(),
                };

                // Claiming the name is the whole handshake. Exactly one process
                // may own it, so a failure here means another panel is already
                // the tray — reported, not fought over, because two watchers
                // would split the applications between them.
                let connection = match zbus::connection::Builder::session()
                    .and_then(|b| b.name("org.kde.StatusNotifierWatcher"))
                    .and_then(|b| b.serve_at("/StatusNotifierWatcher", watcher))
                {
                    Ok(builder) => builder.build().await,
                    Err(e) => {
                        eprintln!("tray: could not reach the session bus: {e}");
                        return;
                    }
                };

                let connection = match connection {
                    Ok(connection) => connection,
                    Err(e) => {
                        eprintln!(
                            "tray: could not become the StatusNotifierWatcher ({e}).\n      \
                             Another panel already owns it; the tray will stay empty."
                        );
                        return;
                    }
                };
                *connection_slot.lock().unwrap() = Some(connection.clone());
                // The unique name is what ownership is compared against below.
                // Comparing against the well-known name would always match and
                // check nothing.
                let me = connection.unique_name().map(|n| n.to_string());
                eprintln!("tray: watching for status notifier items");

                // Every registration belonged to the previous connection's
                // object, so nothing carries over. Cleared rather than kept:
                // icons for registrations that no longer exist cannot be
                // clicked, and applications re-register when a watcher appears —
                // which is the same mechanism that fills the tray at login.
                order.lock().unwrap().clear();
                items.lock().unwrap().clear();
                strikes.lock().unwrap().clear();

                // Polled rather than driven by PropertiesChanged. Items are few
                // and change slowly, and subscribing to every item's signals is
                // a second failure surface — this is honest about being a poll,
                // and the interval is long enough to be free.
                loop {
                    refresh(&connection, &order, &items, &strikes).await;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                    if !still_the_watcher(&connection, me.as_deref()).await {
                        eprintln!(
                            "tray: no longer the StatusNotifierWatcher — reconnecting"
                        );
                        break;
                    }
                }

                *connection_slot.lock().unwrap() = None;
                // A moment before retrying, so a bus that is genuinely gone
                // does not become a hot loop of failed connections.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    });

    published
}

/// Whether this connection still owns the watcher name.
///
/// Verified rather than assumed. A successful claim at startup says nothing
/// about an hour later, and the failure it misses is silent: the tray keeps
/// polling an item list that nothing will ever add to.
///
/// A failed *call* counts as "no longer the watcher", because a connection that
/// cannot ask the bus a question is a connection that cannot receive
/// registrations either. That is the case worth catching — it is what happened
/// on the live session — and treating an error as "probably fine" is what let it
/// go unnoticed.
async fn still_the_watcher(connection: &zbus::Connection, me: Option<&str>) -> bool {
    owns(connection, me, "org.kde.StatusNotifierWatcher").await
}

/// Whether `me` is the current owner of `name`, as the bus sees it.
///
/// Split from `still_the_watcher` only so the test can ask about a name of its
/// own; the watcher name is claimed by `start()` and two tests contending for
/// it on one bus is a race, not a check.
async fn owns(connection: &zbus::Connection, me: Option<&str>, name: &str) -> bool {
    let Some(me) = me else { return false };
    let proxy = match zbus::fdo::DBusProxy::new(connection).await {
        Ok(proxy) => proxy,
        Err(_) => return false,
    };
    let name = match zbus::names::BusName::try_from(name.to_string()) {
        Ok(name) => name,
        Err(_) => return false,
    };
    match proxy.get_name_owner(name).await {
        Ok(owner) => owner.as_str() == me,
        Err(_) => false,
    }
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
                    OwnedValue::from(zbus::zvariant::Str::from(*v)),
                )
            })
            .collect()
    }

    /// `Passive` means "do not show me". A tray that ignores it draws icons for
    /// applications that have deliberately said they have nothing to report,
    /// which turns a tray into a list of running programs.
    #[test]
    fn status_is_read_and_passive_is_the_only_hidden_one() {
        let of = |status: &str| {
            item_from_properties(":1.5", "/x", &props(&[("Status", status)])).status
        };
        assert_eq!(of("Passive"), Status::Passive);
        assert_eq!(of("Active"), Status::Active);
        assert_eq!(of("NeedsAttention"), Status::NeedsAttention);

        assert!(!Status::Passive.visible());
        assert!(Status::Active.visible());
        assert!(
            Status::NeedsAttention.visible(),
            "an item asking to be noticed must not be the one that is hidden"
        );
    }

    /// An item that registered and then failed to describe itself is more
    /// likely half-implemented than asking to be hidden, and a visible icon can
    /// be clicked where an absent one cannot even be discovered.
    #[test]
    fn an_absent_or_unknown_status_counts_as_active() {
        let absent = item_from_properties(":1.5", "/x", &props(&[]));
        assert_eq!(absent.status, Status::Active);
        let nonsense = item_from_properties(":1.5", "/x", &props(&[("Status", "Sideways")]));
        assert_eq!(nonsense.status, Status::Active);
    }

    /// The attention icon replaces the ordinary one only while the item is
    /// asking to be noticed — and an item that sets the status without shipping
    /// one falls back rather than losing its icon.
    #[test]
    fn the_attention_icon_wins_only_while_it_applies() {
        let shouting = item_from_properties(
            ":1.5",
            "/x",
            &props(&[
                ("Status", "NeedsAttention"),
                ("IconName", "ordinary"),
                ("AttentionIconName", "urgent"),
            ]),
        );
        assert_eq!(shouting.icon_name.as_deref(), Some("urgent"));
        assert!(shouting.needs_attention());

        let calm = item_from_properties(
            ":1.5",
            "/x",
            &props(&[
                ("Status", "Active"),
                ("IconName", "ordinary"),
                ("AttentionIconName", "urgent"),
            ]),
        );
        assert_eq!(calm.icon_name.as_deref(), Some("ordinary"));

        let no_attention_icon = item_from_properties(
            ":1.5",
            "/x",
            &props(&[("Status", "NeedsAttention"), ("IconName", "ordinary")]),
        );
        assert_eq!(
            no_attention_icon.icon_name.as_deref(),
            Some("ordinary"),
            "an item with no attention icon must keep the one it has"
        );
    }

    /// `Menu` is an `ObjectPath`, not a `Str`. Reading it with the string
    /// helper fails for every item that has a menu — which reads as "no item
    /// has a menu" and makes right-click universally dead.
    #[test]
    fn the_menu_path_is_read_as_an_object_path() {
        let mut properties = props(&[("Title", "Volume")]);
        properties.insert(
            "Menu".to_string(),
            OwnedValue::from(zbus::zvariant::ObjectPath::try_from("/MenuBar").unwrap()),
        );
        let item = item_from_properties(":1.5", "/x", &properties);
        assert_eq!(item.menu_path.as_deref(), Some("/MenuBar"));
    }

    /// `/NO_DBUSMENU` is what applications with no menu publish rather than
    /// omitting the property. Treated as a real path it produces a failing
    /// `GetLayout` on every right-click.
    #[test]
    fn the_no_menu_sentinel_is_not_a_menu() {
        let mut properties = props(&[]);
        properties.insert(
            "Menu".to_string(),
            OwnedValue::from(zbus::zvariant::ObjectPath::try_from("/NO_DBUSMENU").unwrap()),
        );
        assert_eq!(item_from_properties(":1.5", "/x", &properties).menu_path, None);
    }

    /// Items with no primary action set this so a left click opens the menu.
    /// Absent means false: assuming otherwise would send every ordinary item's
    /// left click to a menu it may not have.
    #[test]
    fn item_is_menu_defaults_to_false() {
        assert!(!item_from_properties(":1.5", "/x", &props(&[])).is_menu);

        let mut properties = props(&[]);
        properties.insert("ItemIsMenu".to_string(), OwnedValue::from(true));
        assert!(item_from_properties(":1.5", "/x", &properties).is_menu);
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

    /// Ownership is verified, not assumed.
    ///
    /// This is the check that would have caught the live-session failure: the
    /// dock had claimed the name at startup, still had it in its own head, and
    /// did not own it. A `true` here means "the bus agrees we are the watcher",
    /// and the reconnect loop turns a `false` into a fresh connection.
    ///
    /// Uses its **own** name rather than the real one. The other test in this
    /// module calls `start()`, which claims the watcher name, and cargo runs
    /// tests in threads sharing one bus — so both claiming it made whichever
    /// lost the race fail. Worth recording: the first version of this test did
    /// exactly that and failed on `the owner must recognise itself`, which
    /// looked like the check being broken and was the check being *right* about
    /// a connection that genuinely did not own the name.
    #[test]
    #[ignore = "needs a private session bus; see the module comment"]
    fn ownership_is_confirmed_against_the_bus() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let connection = zbus::connection::Builder::session()
                .unwrap()
                .name("org.hadalos.CuskTrayOwnershipTest")
                .unwrap()
                .build()
                .await
                .unwrap();
            let me = connection.unique_name().map(|n| n.to_string());
            assert!(
                owns(&connection, me.as_deref(), "org.hadalos.CuskTrayOwnershipTest").await,
                "the owner must recognise itself"
            );

            // A connection that owns nothing must report so rather than
            // assuming success — this is the branch that used to be missing.
            let bystander = zbus::connection::Builder::session()
                .unwrap()
                .build()
                .await
                .unwrap();
            let other = bystander.unique_name().map(|n| n.to_string());
            assert!(
                !owns(&bystander, other.as_deref(), "org.hadalos.CuskTrayOwnershipTest").await,
                "a connection that does not own the name must not claim to"
            );
        });
    }

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
