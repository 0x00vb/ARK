//! # ark-computer
//!
//! The Computer capability: agents use the apps on this computer. An app's window is shown to
//! an agent as its text and a numbered list of the controls it can act on (buttons, fields,
//! checkboxes, links), read through the operating system's accessibility interface, as the
//! Browser shows a page. Any model can use it, also one that reads no images. Why this way and
//! how it is shared: ADR 0018.
//!
//! | Rule | How |
//! |---|---|
//! | One agent at a time on the screen | the first to act holds it; the others wait until it is done (its turn or task ends) or has left it idle for [`IDLE`] |
//! | Every action that reads or changes an app asks first | `look`, `open`, `click`, `type` and `keys` are `Mutate`, through the approval gate; only `apps` (the names of the open apps) runs on its own |
//! | The person comes first | an action waits until the person has left the keyboard and mouse alone for [`QUIET`] |
//! | The person can take it back | [`ComputerService::take_back`]: the holder's next action fails and says so |
//! | Elements are refused across apps | an element number is the agent's last `look`, in the app that `look` showed |
//!
//! A [`Screen`] is the platform underneath: macOS's accessibility API today
//! ([`ComputerService::native`]), a fake one in tests ([`fake`]). Other platforms have none yet,
//! and an app there offers no Computer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ark_core::{ActionDescriptor, RiskTier};
use ark_tools::{ActionOutput, Adapter, AdapterError, Result, RunCtx};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::watch;

pub mod fake;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use macos::tree;

pub const APPS: &str = "computer.apps";
pub const LOOK: &str = "computer.look";
pub const OPEN: &str = "computer.open";
pub const CLICK: &str = "computer.click";
pub const TYPE: &str = "computer.type";
pub const KEYS: &str = "computer.keys";
pub const MENU: &str = "computer.menu";
pub const SCROLL: &str = "computer.scroll";
pub const SEE: &str = "computer.see";
pub const POINT: &str = "computer.point";
pub const ACTIONS: [&str; 10] = [
    APPS, LOOK, OPEN, CLICK, TYPE, KEYS, MENU, SCROLL, SEE, POINT,
];
/// The longer side of a picture of a window, in pixels.
pub const PICTURE: u32 = 1280;

/// How long the screen stays with an agent after its last action, while another waits.
pub const IDLE: Duration = Duration::from_secs(30);
/// How long the person must have left the keyboard and mouse alone before an agent acts.
pub const QUIET: Duration = Duration::from_millis(1500);
/// How long an action waits for the person to pause before giving up.
const QUIET_WAIT: Duration = Duration::from_secs(60);
/// How long an agent waits for the screen before giving up.
const LEASE_WAIT: Duration = Duration::from_secs(600);
/// How long an action waits for the person to allow ARK in the system's settings.
const PERMISSION_WAIT: Duration = Duration::from_secs(120);
const CARD_TEXT: usize = 800;
const TAKEN: &str =
    "the person took the computer back; stop using it and tell them what is left to do";
const CARD_ELEMENTS: usize = 40;

/// An open app.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct App {
    pub name: String,
    pub pid: i32,
    /// The app in front.
    pub front: bool,
}

/// One control of a window, as an agent sees it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Element {
    #[serde(rename = "ref")]
    pub number: usize,
    /// `button`, `text field`, `checkbox`, …
    pub role: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// The selected row, item or tab.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub selected: bool,
}

/// Where something is on the screen, in points from the top left of the main display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Frame {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A picture of one app's front window.
#[derive(Clone, Debug)]
pub struct Shot {
    pub png: Vec<u8>,
    /// The picture's size in pixels.
    pub width: u32,
    pub height: u32,
    /// Where the window is on screen, so a point in the picture maps to a point on screen.
    pub frame: Frame,
}

impl Shot {
    /// The point on screen under pixel (`x`, `y`) of the picture.
    pub fn to_screen(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.frame.x + x * self.frame.w / self.width.max(1) as f64,
            self.frame.y + y * self.frame.h / self.height.max(1) as f64,
        )
    }
}

/// A press with the pointer, for what a control's own press does not do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pointer {
    Click,
    /// Open a file, a chat, an item.
    Double,
    /// The item's context menu.
    Right,
}

/// An item of an app's menu bar.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MenuItem {
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// It opens more items.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub submenu: bool,
    /// Its shortcut (`cmd+shift+s`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
}

/// What a walk down the menu bar found.
#[derive(Clone, Debug, PartialEq)]
pub enum Menu {
    /// The path ends at a menu: these are its items.
    Items(Vec<MenuItem>),
    /// The path ended at an item, and it was chosen. The names as the app spells them.
    Chosen(Vec<String>),
}

/// A control the platform can act on.
pub trait Control: Send + Sync {
    /// Press it (a button, a checkbox, a link), select it (a row, an item of a list), or focus
    /// it (a field).
    fn press(&self) -> std::result::Result<(), String>;
    /// Replace its text (a field).
    fn set_text(&self, text: &str) -> std::result::Result<(), String>;
    /// Where it is on the screen, for the pointer and for scrolling.
    fn frame(&self) -> Option<Frame> {
        None
    }
}

/// A window as read: its text, its controls in order, and a handle to act on each.
pub struct Look {
    pub app: String,
    pub window: String,
    pub text: String,
    pub elements: Vec<Element>,
    pub controls: Vec<Arc<dyn Control>>,
}

/// Whether ARK may use this computer's apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Allowed,
    /// The person has to allow it in the system's settings; the text says where.
    NotYet(String),
}

/// The platform underneath: reads windows and acts on them. Calls block; the service runs them
/// off the async runtime.
pub trait Screen: Send + Sync {
    fn access(&self) -> Access;
    /// Ask the system to show the person its permission prompt.
    fn request_access(&self);
    fn apps(&self) -> std::result::Result<Vec<App>, String>;
    /// The front window of the open app named `app`.
    fn look(&self, app: &str) -> std::result::Result<Look, String>;
    /// Start the app, or bring it to the front.
    fn open(&self, app: &str) -> std::result::Result<(), String>;
    /// A key or a shortcut (`return`, `cmd+s`) to the app in front.
    fn keys(&self, keys: &str) -> std::result::Result<(), String>;
    /// Type text into whatever has the focus.
    fn type_text(&self, text: &str) -> std::result::Result<(), String>;
    /// How long ago anyone used the keyboard or mouse (this process's own keys included).
    fn since_input(&self) -> Option<Duration>;
    /// Walk the menu bar of the app named `app` along `path` (empty: the menu bar itself). An
    /// item at the end is chosen; a menu at the end lists its items.
    fn menu(&self, app: &str, path: &[String]) -> std::result::Result<Menu, String>;
    /// Where the front window of `app` is.
    fn window_frame(&self, app: &str) -> std::result::Result<Frame, String>;
    /// Scroll what is at the middle of `at` by `pages` (negative: up), without moving the
    /// pointer.
    fn scroll(&self, at: Frame, pages: f64) -> std::result::Result<(), String>;
    /// Click at a point with the pointer, and put the pointer back where it was.
    fn pointer(&self, x: f64, y: f64, how: Pointer) -> std::result::Result<(), String>;
    /// A picture of the front window of `app` only (never the whole screen), its longer side at
    /// most `max` pixels.
    fn capture(&self, app: &str, max: u32) -> std::result::Result<Shot, String>;
}

/// A control as the owner's last look listed it, and the handle to act on it.
type Seen = (Element, Arc<dyn Control>);

#[derive(Default)]
struct State {
    /// Who holds the screen, and when they last acted.
    lease: Option<(String, Instant)>,
    /// Each owner's last look: the app and its controls by number.
    looks: HashMap<String, (String, Vec<Seen>)>,
    /// When this process last sent keys, so its own input does not count as the person's.
    own_input: Option<Instant>,
    /// Owners the person took the screen from: refused until their turn or task ends.
    taken: std::collections::HashSet<String>,
    /// Each owner's last picture: the app, its number, and how its pixels map to the screen.
    shots: HashMap<String, (String, u64, Shot)>,
    /// The newest picture of each app (by its name in lower case), and its number, for the
    /// window to show beside a question to click in it ([`ComputerService::picture`]).
    latest: HashMap<String, (u64, Shot)>,
    /// The number of the last picture taken.
    pictures: u64,
}

/// The computer of one ARK process. Cheap to share.
pub struct ComputerService {
    screen: Arc<dyn Screen>,
    idle: Duration,
    quiet: Duration,
    state: Mutex<State>,
    holder: watch::Sender<Option<String>>,
}

impl ComputerService {
    /// This computer, when ARK can use apps on this platform (macOS).
    pub fn native() -> Option<Arc<Self>> {
        #[cfg(target_os = "macos")]
        {
            Some(Self::with_screen(Arc::new(macos::Mac)))
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// A service over another [`Screen`] (tests, another platform).
    pub fn with_screen(screen: Arc<dyn Screen>) -> Arc<Self> {
        Arc::new(ComputerService {
            screen,
            idle: IDLE,
            quiet: QUIET,
            state: Mutex::new(State::default()),
            holder: watch::channel(None).0,
        })
    }

    /// Shorter waits (tests).
    pub fn timing(self: Arc<Self>, idle: Duration, quiet: Duration) -> Arc<Self> {
        let mut s = Arc::try_unwrap(self).unwrap_or_else(|_| panic!("set before sharing"));
        s.idle = idle;
        s.quiet = quiet;
        Arc::new(s)
    }

    /// The tools of one agent (or conversation), named `owner`. Dropping them gives the screen
    /// back if this owner holds it, so the screen is free when the turn or task ends.
    pub fn for_owner(self: &Arc<Self>, owner: &str) -> Computer {
        Computer {
            svc: self.clone(),
            owner: owner.to_string(),
        }
    }

    /// Who uses the screen now (an owner name: `session:<id>` or `<project>/<agent>`).
    pub fn holder(&self) -> Option<String> {
        self.holder.borrow().clone()
    }

    /// Follow who uses the screen.
    pub fn watch(&self) -> watch::Receiver<Option<String>> {
        self.holder.subscribe()
    }

    /// The platform underneath, for trying it by hand (`examples/probe.rs`). Calls on it skip
    /// the lease, the approvals and the quiet wait.
    pub fn screen(&self) -> Arc<dyn Screen> {
        self.screen.clone()
    }

    /// The newest picture of `app`'s window, as an agent saw it: what a question to click a spot
    /// in it is about. A click is refused once a newer picture of that app is taken, so the
    /// person always approves on the picture the click maps from.
    pub fn picture(&self, app: &str) -> Option<Shot> {
        let st = self.state.lock().unwrap();
        st.latest
            .get(&app.trim().to_lowercase())
            .map(|(_, s)| s.clone())
    }

    /// Whether ARK may use apps here.
    pub fn access(&self) -> Access {
        self.screen.access()
    }

    /// The person takes the screen back: whoever holds it loses it, and its actions fail until
    /// its turn or task ends. Returns who held it.
    pub fn take_back(&self) -> Option<String> {
        let mut st = self.state.lock().unwrap();
        let had = st.lease.take().map(|(o, _)| o);
        if let Some(o) = &had {
            st.looks.remove(o);
            st.shots.remove(o);
            st.taken.insert(o.clone());
        }
        drop(st);
        self.holder.send_replace(None);
        had
    }

    fn release(&self, owner: &str) {
        let mut st = self.state.lock().unwrap();
        st.looks.remove(owner);
        st.shots.remove(owner);
        st.taken.remove(owner);
        if st.lease.as_ref().is_some_and(|(o, _)| o == owner) {
            st.lease = None;
            drop(st);
            self.holder.send_replace(None);
        }
    }

    fn holds(&self, owner: &str) -> bool {
        let st = self.state.lock().unwrap();
        st.lease.as_ref().is_some_and(|(o, _)| o == owner)
    }

    /// Wait until `owner` holds the screen.
    async fn lease(&self, owner: &str) -> std::result::Result<(), String> {
        let until = Instant::now() + LEASE_WAIT;
        loop {
            {
                let mut st = self.state.lock().unwrap();
                if st.taken.contains(owner) {
                    return Err(TAKEN.into());
                }
                let free = match &st.lease {
                    None => true,
                    Some((o, last)) => o == owner || last.elapsed() >= self.idle,
                };
                if free {
                    let changed = st.lease.as_ref().map(|(o, _)| o.as_str()) != Some(owner);
                    if changed {
                        // A look of the one before is no use to anyone now.
                        if let Some((prev, _)) = st.lease.take() {
                            st.looks.remove(&prev);
                            st.shots.remove(&prev);
                        }
                    }
                    st.lease = Some((owner.to_string(), Instant::now()));
                    drop(st);
                    if changed {
                        self.holder.send_replace(Some(owner.to_string()));
                    }
                    return Ok(());
                }
                if Instant::now() >= until {
                    let who = st
                        .lease
                        .as_ref()
                        .map(|(o, _)| o.clone())
                        .unwrap_or_default();
                    return Err(format!(
                        "the computer stayed in use by {who}; try again later"
                    ));
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn touch(&self, owner: &str) {
        let mut st = self.state.lock().unwrap();
        if let Some((o, last)) = st.lease.as_mut() {
            if o == owner {
                *last = Instant::now();
            }
        }
    }

    /// Wait for ARK to be allowed to use apps (asking the system to prompt the person once).
    async fn allowed(&self) -> std::result::Result<(), String> {
        let Access::NotYet(how) = self.screen.access() else {
            return Ok(());
        };
        self.screen.request_access();
        let until = Instant::now() + PERMISSION_WAIT;
        while Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.screen.access() == Access::Allowed {
                return Ok(());
            }
        }
        Err(format!("ARK is not allowed to use apps yet. {how}"))
    }

    /// Wait until the person has left the keyboard and mouse alone for a moment.
    async fn person_quiet(&self) -> std::result::Result<(), String> {
        let until = Instant::now() + QUIET_WAIT;
        loop {
            let since = self.screen.since_input();
            let own = self.state.lock().unwrap().own_input;
            let quiet = match since {
                None => true,
                Some(s) => {
                    // The last input was this process's own keys.
                    s >= self.quiet
                        || own.is_some_and(|o| o.elapsed() <= s + Duration::from_millis(80))
                }
            };
            if quiet {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err("the person is using the computer; try again when they pause".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn sent_input(&self) {
        self.state.lock().unwrap().own_input = Some(Instant::now());
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn Screen) -> std::result::Result<T, String> + Send + 'static,
    ) -> std::result::Result<T, String> {
        let screen = self.screen.clone();
        tokio::task::spawn_blocking(move || f(screen.as_ref()))
            .await
            .map_err(|e| format!("the computer did not answer: {e}"))?
    }
}

/// One agent's computer tools ([`ComputerService::for_owner`]).
pub struct Computer {
    svc: Arc<ComputerService>,
    owner: String,
}

impl Drop for Computer {
    fn drop(&mut self) {
        if !self.owner.is_empty() {
            self.svc.release(&self.owner);
        }
    }
}

fn tool(e: String) -> AdapterError {
    AdapterError::Tool(e)
}

fn app_of(action: &str, input: &Value) -> Result<String> {
    let app = input["app"].as_str().unwrap_or("").trim();
    if app.is_empty() {
        return Err(AdapterError::bad_input(
            action,
            "`app` names the app, as computer.apps lists it",
        ));
    }
    Ok(app.to_string())
}

impl Computer {
    /// Hold the screen and have the person's go-ahead to act now.
    async fn ready(&self) -> Result<()> {
        self.svc.allowed().await.map_err(tool)?;
        self.svc.lease(&self.owner).await.map_err(tool)?;
        self.svc.person_quiet().await.map_err(tool)?;
        // The person may have taken it back while this waited.
        if !self.svc.holds(&self.owner) {
            return Err(tool(TAKEN.into()));
        }
        Ok(())
    }

    /// The control numbered `n` in this owner's last look at `app`, which must be the one named
    /// `name`: the approval showed that name, and a window that changed since is caught here.
    fn control(&self, action: &str, app: &str, n: usize, name: &str) -> Result<Arc<dyn Control>> {
        let st = self.svc.state.lock().unwrap();
        let Some((seen, controls)) = st.looks.get(&self.owner) else {
            return Err(AdapterError::bad_input(
                action,
                format!("look at {app} first (computer.look)"),
            ));
        };
        if !seen.eq_ignore_ascii_case(app) {
            return Err(AdapterError::bad_input(
                action,
                format!("element numbers are from your last look, at {seen}; look at {app} first"),
            ));
        }
        let (el, c) = n
            .checked_sub(1)
            .and_then(|i| controls.get(i))
            .ok_or_else(|| {
                AdapterError::bad_input(
                    action,
                    format!("no element [{n}] in {app}; computer.look lists them"),
                )
            })?;
        if !same_name(&el.name, name) {
            return Err(AdapterError::bad_input(
                action,
                format!(
                    "element [{n}] in {app} is the {} “{}”, not “{name}”; look again if the window changed",
                    el.role, el.name
                ),
            ));
        }
        Ok(c.clone())
    }

    /// A picture of the app's window, kept so `computer.point` can map its pixels to the screen.
    async fn see(&self, app: &str) -> Result<ActionOutput> {
        let name = app.to_string();
        let shot = self
            .svc
            .blocking(move |s| s.capture(&name, PICTURE))
            .await
            .map_err(tool)?;
        let out = json!({
            "app": app,
            "picture": { "width": shot.width, "height": shot.height },
        });
        let png = shot.png.clone();
        {
            let mut st = self.svc.state.lock().unwrap();
            st.pictures += 1;
            let n = st.pictures;
            st.latest
                .insert(app.trim().to_lowercase(), (n, shot.clone()));
            // A few apps' pictures at most stay in memory.
            while st.latest.len() > 8 {
                let oldest = st
                    .latest
                    .iter()
                    .min_by_key(|(_, (n, _))| *n)
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    st.latest.remove(&k);
                }
            }
            st.shots
                .insert(self.owner.clone(), (app.to_string(), n, shot));
        }
        self.svc.touch(&self.owner);
        Ok(ActionOutput::json(out).with_image("image/png", png))
    }

    async fn look(&self, app: &str) -> Result<ActionOutput> {
        let name = app.to_string();
        let look = self
            .svc
            .blocking(move |s| s.look(&name))
            .await
            .map_err(tool)?;
        let out = json!({
            "app": look.app,
            "window": look.window,
            "text": look.text,
            "lines": look.text.lines().count(),
            "elements": look.elements,
        });
        self.svc.state.lock().unwrap().looks.insert(
            self.owner.clone(),
            (
                look.app.clone(),
                look.elements.into_iter().zip(look.controls).collect(),
            ),
        );
        self.svc.touch(&self.owner);
        Ok(ActionOutput::json(out))
    }
}

#[async_trait]
impl Adapter for Computer {
    fn name(&self) -> &str {
        "computer"
    }

    fn actions(&self) -> Vec<ActionDescriptor> {
        let app = || json!({ "type": "string", "description": "the app's name, as computer.apps lists it" });
        let el = || json!({ "type": "integer", "description": "the element's number from your last computer.look" });
        let named = || json!({ "type": "string", "description": "the element's name as computer.look listed it (empty if it has none)" });
        vec![
            ActionDescriptor::new(APPS, "list the apps open on this computer", RiskTier::Observe).volatile(),
            ActionDescriptor::new(
                LOOK,
                "see an app's front window: its text and the elements you can act on, each with a number",
                RiskTier::Mutate,
            )
            .volatile()
            .ask_each_time()
            .with_input_schema(json!({ "type": "object", "required": ["app"], "properties": { "app": app() } })),
            ActionDescriptor::new(OPEN, "start an app, or bring it to the front", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({ "type": "object", "required": ["app"], "properties": { "app": app() } })),
            ActionDescriptor::new(CLICK, "press an element of an app (a button, a link, a row, an item), by its number and name; double opens it (a file, a chat), right shows its menu", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "ref", "name"],
                    "properties": {
                        "app": app(), "ref": el(), "name": named(),
                        "double": { "type": "boolean" }, "right": { "type": "boolean" }
                    }
                })),
            ActionDescriptor::new(TYPE, "type into a field of an app, by its number and name; submit presses Return", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "ref", "name", "text"],
                    "properties": {
                        "app": app(), "ref": el(), "name": named(),
                        "text": { "type": "string", "description": "replaces what the field holds" },
                        "submit": { "type": "boolean" }
                    }
                })),
            ActionDescriptor::new(KEYS, "press a key or shortcut in an app: return, escape, tab, cmd+s, cmd+shift+n", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "keys"],
                    "properties": { "app": app(), "keys": { "type": "string" } }
                })),
            ActionDescriptor::new(MENU, "use an app's menu bar: a path to an item (\"File > Export as PDF…\") chooses it; a path to a menu, or none, lists its items and their shortcuts", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({
                    "type": "object", "required": ["app"],
                    "properties": { "app": app(), "path": { "type": "string", "description": "menu names joined by >, e.g. File > Export as PDF…" } }
                })),
            ActionDescriptor::new(SEE, "a picture of an app's window, for when computer.look shows little (a canvas, a game, a video app); computer.point then clicks a spot in it", RiskTier::Mutate)
                .volatile()
                .ask_each_time()
                .with_input_schema(json!({ "type": "object", "required": ["app"], "properties": { "app": app() } })),
            ActionDescriptor::new(POINT, "click a spot in your last picture of an app (computer.see), by its pixel position; double opens, right shows a menu", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "x", "y"],
                    "properties": {
                        "app": app(),
                        "x": { "type": "number", "description": "pixels from the picture's left edge" },
                        "y": { "type": "number", "description": "pixels from the picture's top edge" },
                        "what": { "type": "string", "description": "what is at that spot, in a few words (the person sees it when asked)" },
                        "double": { "type": "boolean" }, "right": { "type": "boolean" }
                    }
                })),
            ActionDescriptor::new(SCROLL, "scroll an app's window, or the part of it holding an element, to see more", RiskTier::Mutate)
                .ask_each_time()
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "direction"],
                    "properties": {
                        "app": app(),
                        "direction": { "type": "string", "enum": ["up", "down"] },
                        "pages": { "type": "number", "description": "how far, in window heights (default 1)" },
                        "ref": el(), "name": named()
                    }
                })),
        ]
    }

    async fn run(&self, action: &str, input: Value, _ctx: &RunCtx<'_>) -> Result<ActionOutput> {
        if action == APPS {
            let apps = self.svc.blocking(|s| s.apps()).await.map_err(tool)?;
            return Ok(ActionOutput::json(json!({ "apps": apps })));
        }
        if !ACTIONS.contains(&action) {
            return Err(AdapterError::UnknownAction(action.into()));
        }
        let app = app_of(action, &input)?;
        self.ready().await?;
        match action {
            LOOK => self.look(&app).await,
            OPEN => {
                let name = app.clone();
                self.svc
                    .blocking(move |s| s.open(&name))
                    .await
                    .map_err(tool)?;
                tokio::time::sleep(Duration::from_millis(600)).await;
                self.look(&app).await
            }
            CLICK => {
                let n = number(action, &input)?;
                let c = self.control(action, &app, n, name_of(&input))?;
                let how = if input["right"] == json!(true) {
                    Some(Pointer::Right)
                } else if input["double"] == json!(true) {
                    Some(Pointer::Double)
                } else {
                    None
                };
                let name = app.clone();
                // The control's own press first; the pointer when asked for or when the control
                // takes no press (some web and custom controls).
                self.svc
                    .blocking(move |s| match how {
                        None => c.press().or_else(|e| {
                            point(s, &name, c.as_ref(), Pointer::Click).map_err(|_| e)
                        }),
                        Some(how) => point(s, &name, c.as_ref(), how),
                    })
                    .await
                    .map_err(tool)?;
                self.svc.sent_input();
                tokio::time::sleep(Duration::from_millis(400)).await;
                self.look(&app).await
            }
            MENU => {
                let path: Vec<String> = input["path"]
                    .as_str()
                    .unwrap_or("")
                    .split(['>', '›'])
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
                let name = app.clone();
                let menu = self
                    .svc
                    .blocking(move |s| {
                        if !path.is_empty() {
                            in_front(s, &name)?;
                        }
                        s.menu(&name, &path)
                    })
                    .await
                    .map_err(tool)?;
                self.svc.touch(&self.owner);
                match menu {
                    Menu::Items(items) => Ok(ActionOutput::json(json!({
                        "app": app,
                        "menu": input["path"].as_str().unwrap_or(""),
                        "items": items,
                    }))),
                    Menu::Chosen(walked) => {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let mut out = self.look(&app).await?;
                        out.output["chose"] = json!(walked.join(" > "));
                        Ok(out)
                    }
                }
            }
            SEE => self.see(&app).await,
            POINT => {
                let shot = {
                    let st = self.svc.state.lock().unwrap();
                    match st.shots.get(&self.owner) {
                        Some((seen, n, shot)) if same_app(seen, &app) => {
                            let newest = st.latest.get(&app.trim().to_lowercase()).map(|l| l.0);
                            if newest != Some(*n) {
                                return Err(AdapterError::bad_input(
                                    action,
                                    format!("a newer picture of {app} was taken since yours; take one again (computer.see)"),
                                ));
                            }
                            shot.clone()
                        }
                        Some((seen, _, _)) => {
                            return Err(AdapterError::bad_input(
                                action,
                                format!("your last picture is of {seen}; take one of {app} first (computer.see)"),
                            ))
                        }
                        None => {
                            return Err(AdapterError::bad_input(
                                action,
                                format!("take a picture of {app} first (computer.see)"),
                            ))
                        }
                    }
                };
                let (Some(x), Some(y)) = (input["x"].as_f64(), input["y"].as_f64()) else {
                    return Err(AdapterError::bad_input(
                        action,
                        "`x` and `y` are pixels in the picture",
                    ));
                };
                if x < 0.0 || y < 0.0 || x > shot.width as f64 || y > shot.height as f64 {
                    return Err(AdapterError::bad_input(
                        action,
                        format!(
                            "({x}, {y}) is outside the picture ({} by {})",
                            shot.width, shot.height
                        ),
                    ));
                }
                let how = if input["right"] == json!(true) {
                    Pointer::Right
                } else if input["double"] == json!(true) {
                    Pointer::Double
                } else {
                    Pointer::Click
                };
                let (sx, sy) = shot.to_screen(x, y);
                let name = app.clone();
                self.svc
                    .blocking(move |s| {
                        // The window may have moved since the picture: a click would land
                        // elsewhere.
                        let now = s.window_frame(&name)?;
                        if (now.x - shot.frame.x).abs() > 2.0
                            || (now.y - shot.frame.y).abs() > 2.0
                            || (now.w - shot.frame.w).abs() > 2.0
                            || (now.h - shot.frame.h).abs() > 2.0
                        {
                            return Err(format!(
                                "{name}'s window moved since the picture; take a new one"
                            ));
                        }
                        in_front(s, &name)?;
                        s.pointer(sx, sy, how)
                    })
                    .await
                    .map_err(tool)?;
                self.svc.sent_input();
                tokio::time::sleep(Duration::from_millis(400)).await;
                self.see(&app).await
            }
            SCROLL => {
                let down = match input["direction"].as_str().unwrap_or("") {
                    "down" => true,
                    "up" => false,
                    _ => return Err(AdapterError::bad_input(action, "`direction` is up or down")),
                };
                let pages = input["pages"].as_f64().unwrap_or(1.0).clamp(0.1, 10.0);
                let at = match number(action, &input) {
                    Ok(n) => Some(self.control(action, &app, n, name_of(&input))?),
                    Err(_) => None,
                };
                let name = app.clone();
                self.svc
                    .blocking(move |s| {
                        let frame = match at.and_then(|c| c.frame()) {
                            Some(f) => f,
                            None => s.window_frame(&name)?,
                        };
                        in_front(s, &name)?;
                        s.scroll(frame, if down { pages } else { -pages })
                    })
                    .await
                    .map_err(tool)?;
                self.svc.sent_input();
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.look(&app).await
            }
            TYPE => {
                let n = number(action, &input)?;
                let c = self.control(action, &app, n, name_of(&input))?;
                let text = input["text"].as_str().unwrap_or("").to_string();
                let submit = input["submit"].as_bool().unwrap_or(false);
                let name = app.clone();
                // Typed as a person types: the field focused, what it holds selected, the text
                // as keys, so the app hears every change (a text set from outside leaves some
                // apps' Send button off). Keys go to the app in front: only once the approved
                // app is that app. A field that can't take focus gets its text set directly.
                self.svc
                    .blocking(move |s| {
                        if c.press().is_ok() {
                            in_front(s, &name)?;
                            s.keys("cmd+a")?;
                            if text.is_empty() {
                                s.keys("delete")?;
                            } else {
                                s.type_text(&text)?;
                            }
                        } else {
                            c.set_text(&text)?;
                        }
                        if submit {
                            in_front(s, &name)?;
                            s.keys("return")?;
                        }
                        Ok(())
                    })
                    .await
                    .map_err(tool)?;
                self.svc.sent_input();
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.look(&app).await
            }
            KEYS => {
                let keys = input["keys"].as_str().unwrap_or("").trim().to_string();
                if keys.is_empty() {
                    return Err(AdapterError::bad_input(
                        action,
                        "`keys` is a key or shortcut, e.g. cmd+s",
                    ));
                }
                // Keys go to the app in front: only once the approved app is that app.
                let name = app.clone();
                self.svc
                    .blocking(move |s| {
                        in_front(s, &name)?;
                        s.keys(&keys)
                    })
                    .await
                    .map_err(tool)?;
                self.svc.sent_input();
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.look(&app).await
            }
            _ => unreachable!("checked above"),
        }
    }
}

/// Bring `app` to the front and make sure it is there, before keys that go to whatever is in
/// front. Nothing is sent when it is not.
fn in_front(s: &dyn Screen, app: &str) -> std::result::Result<(), String> {
    s.open(app)?;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(front) = s.apps()?.into_iter().find(|a| a.front) {
            if same_app(&front.name, app) {
                return Ok(());
            }
        }
    }
    Err(format!(
        "{app} did not come to the front, so nothing was typed"
    ))
}

/// Press `c` with the pointer, once `app` is in front: a click goes to whatever is at that point.
fn point(
    s: &dyn Screen,
    app: &str,
    c: &dyn Control,
    how: Pointer,
) -> std::result::Result<(), String> {
    let frame = c
        .frame()
        .ok_or("the app did not say where that element is, so it can't be clicked")?;
    in_front(s, app)?;
    let (x, y) = frame.center();
    s.pointer(x, y, how)
}

fn same_app(listed: &str, asked: &str) -> bool {
    let (l, a) = (listed.trim().to_lowercase(), asked.trim().to_lowercase());
    !a.is_empty() && (l == a || l.contains(&a))
}

fn same_name(listed: &str, given: &str) -> bool {
    let clean = |x: &str| {
        x.trim()
            .trim_matches(['"', '“', '”', '\''])
            .trim()
            .to_lowercase()
    };
    clean(listed) == clean(given)
}

fn name_of(input: &Value) -> &str {
    input["name"].as_str().unwrap_or("")
}

fn number(action: &str, input: &Value) -> Result<usize> {
    input["ref"]
        .as_u64()
        .or_else(|| {
            input["ref"]
                .as_str()
                .and_then(|s| s.trim_start_matches('[').trim_end_matches(']').parse().ok())
        })
        .map(|n| n as usize)
        .ok_or_else(|| {
            AdapterError::bad_input(
                action,
                "`ref` is the number of an element from computer.look",
            )
        })
}

/// The evidence card of a look: the app and window, its first elements, the start of its text.
pub fn distill(action: &str, out: &Value) -> Option<(String, Vec<String>)> {
    if action == APPS {
        let names: Vec<String> = out["apps"]
            .as_array()?
            .iter()
            .map(|a| {
                let n = a["name"].as_str().unwrap_or("").to_string();
                if a["front"] == json!(true) {
                    format!("{n} (in front)")
                } else {
                    n
                }
            })
            .collect();
        return Some((format!("open apps: {}", names.join(", ")), Vec::new()));
    }
    if !ACTIONS.contains(&action) {
        return None;
    }
    let s = |k: &str| out[k].as_str().unwrap_or("");
    if let Some(p) = out.get("picture") {
        return Some((
            format!(
                "{}: a picture of its window, {} by {} pixels (x from the left, y from the top); computer.point clicks a spot in it",
                s("app"),
                p["width"],
                p["height"]
            ),
            vec![s("app").to_string()],
        ));
    }
    if let Some(items) = out["items"].as_array() {
        let mut card = match s("menu") {
            "" => format!("{} menu bar:", s("app")),
            m => format!("{} › {m}:", s("app")),
        };
        for i in items {
            card.push_str(&format!("\n- {}", i["name"].as_str().unwrap_or("")));
            if i["submenu"] == json!(true) {
                card.push_str(" ›");
            }
            if let Some(k) = i["keys"].as_str() {
                card.push_str(&format!(" ({k})"));
            }
            if i["disabled"] == json!(true) {
                card.push_str(" (not available)");
            }
        }
        return Some((card, vec![s("app").to_string()]));
    }
    let elements = out["elements"].as_array()?;
    let mut card = String::new();
    if !s("chose").is_empty() {
        card.push_str(&format!("chose {}\n", s("chose")));
    }
    card.push_str(&format!(
        "{} · \"{}\" ({} lines, {} elements)",
        s("app"),
        s("window"),
        out["lines"].as_u64().unwrap_or(0),
        elements.len()
    ));
    for e in elements.iter().take(CARD_ELEMENTS) {
        let mut line = format!(
            "\n[{}] {} \"{}\"",
            e["ref"],
            e["role"].as_str().unwrap_or(""),
            e["name"].as_str().unwrap_or("")
        );
        if let Some(v) = e["value"].as_str() {
            line.push_str(&format!(" = \"{v}\""));
        }
        if e["disabled"] == json!(true) {
            line.push_str(" (disabled)");
        }
        if e["selected"] == json!(true) {
            line.push_str(" (selected)");
        }
        card.push_str(&line);
    }
    if elements.len() > CARD_ELEMENTS {
        card.push_str("\n(more elements: get_evidence on this result, path \"elements\")");
    }
    let text = s("text");
    let head: String = text.chars().take(CARD_TEXT).collect();
    if !head.is_empty() {
        card.push_str("\nText:\n");
        card.push_str(&head);
        if text.chars().count() > CARD_TEXT {
            card.push_str("\n(the rest: get_evidence on this result, path \"text\", with `match`)");
        }
    }
    Some((card, vec![s("app").to_string()]))
}

/// The computer as a per-agent tool: each owner its own tools, one screen between them.
pub struct Computers(pub Arc<ComputerService>);

impl ark_runtime::per_agent::PerAgent for Computers {
    fn listing(&self) -> Arc<dyn Adapter> {
        Arc::new(self.0.for_owner(""))
    }
    fn for_owner(&self, owner: &ark_runtime::per_agent::Owner) -> Arc<dyn Adapter> {
        Arc::new(self.0.for_owner(owner.name))
    }
    fn distiller(&self) -> Option<ark_runtime::Distiller> {
        Some(Arc::new(distill))
    }
}
