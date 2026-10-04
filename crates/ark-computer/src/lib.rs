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

pub const APPS: &str = "computer.apps";
pub const LOOK: &str = "computer.look";
pub const OPEN: &str = "computer.open";
pub const CLICK: &str = "computer.click";
pub const TYPE: &str = "computer.type";
pub const KEYS: &str = "computer.keys";
pub const ACTIONS: [&str; 6] = [APPS, LOOK, OPEN, CLICK, TYPE, KEYS];

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
}

/// A control the platform can act on.
pub trait Control: Send + Sync {
    /// Press it (a button, a checkbox, a link).
    fn press(&self) -> std::result::Result<(), String>;
    /// Replace its text (a field).
    fn set_text(&self, text: &str) -> std::result::Result<(), String>;
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
}

#[derive(Default)]
struct State {
    /// Who holds the screen, and when they last acted.
    lease: Option<(String, Instant)>,
    /// Each owner's last look: the app and its controls by number.
    looks: HashMap<String, (String, Vec<Arc<dyn Control>>)>,
    /// When this process last sent keys, so its own input does not count as the person's.
    own_input: Option<Instant>,
    /// Owners the person took the screen from: refused until their turn or task ends.
    taken: std::collections::HashSet<String>,
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
            st.taken.insert(o.clone());
        }
        drop(st);
        self.holder.send_replace(None);
        had
    }

    fn release(&self, owner: &str) {
        let mut st = self.state.lock().unwrap();
        st.looks.remove(owner);
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

    /// The control numbered `n` in this owner's last look at `app`.
    fn control(&self, action: &str, app: &str, n: usize) -> Result<Arc<dyn Control>> {
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
        n.checked_sub(1)
            .and_then(|i| controls.get(i))
            .cloned()
            .ok_or_else(|| {
                AdapterError::bad_input(
                    action,
                    format!("no element [{n}] in {app}; computer.look lists them"),
                )
            })
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
        self.svc
            .state
            .lock()
            .unwrap()
            .looks
            .insert(self.owner.clone(), (look.app.clone(), look.controls));
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
        vec![
            ActionDescriptor::new(APPS, "list the apps open on this computer", RiskTier::Observe).volatile(),
            ActionDescriptor::new(
                LOOK,
                "see an app's front window: its text and the elements you can act on, each with a number",
                RiskTier::Mutate,
            )
            .volatile()
            .with_input_schema(json!({ "type": "object", "required": ["app"], "properties": { "app": app() } })),
            ActionDescriptor::new(OPEN, "start an app, or bring it to the front", RiskTier::Mutate)
                .with_input_schema(json!({ "type": "object", "required": ["app"], "properties": { "app": app() } })),
            ActionDescriptor::new(CLICK, "press an element of an app (a button, a checkbox, a link), by its number", RiskTier::Mutate)
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "ref"],
                    "properties": { "app": app(), "ref": el() }
                })),
            ActionDescriptor::new(TYPE, "type into a field of an app, by its number; submit presses Return", RiskTier::Mutate)
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "ref", "text"],
                    "properties": {
                        "app": app(), "ref": el(),
                        "text": { "type": "string", "description": "replaces what the field holds" },
                        "submit": { "type": "boolean" }
                    }
                })),
            ActionDescriptor::new(KEYS, "press a key or shortcut in an app: return, escape, tab, cmd+s, cmd+shift+n", RiskTier::Mutate)
                .with_input_schema(json!({
                    "type": "object", "required": ["app", "keys"],
                    "properties": { "app": app(), "keys": { "type": "string" } }
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
                let c = self.control(action, &app, n)?;
                self.svc.blocking(move |_| c.press()).await.map_err(tool)?;
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.look(&app).await
            }
            TYPE => {
                let n = number(action, &input)?;
                let c = self.control(action, &app, n)?;
                let text = input["text"].as_str().unwrap_or("").to_string();
                let t = text.clone();
                self.svc
                    .blocking(move |s| c.set_text(&t).or_else(|_| s.type_text(&t)))
                    .await
                    .map_err(tool)?;
                self.svc.sent_input();
                if input["submit"].as_bool().unwrap_or(false) {
                    self.svc
                        .blocking(|s| s.keys("return"))
                        .await
                        .map_err(tool)?;
                    self.svc.sent_input();
                }
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
                // Keys go to the app in front: bring this one forward first.
                let name = app.clone();
                self.svc
                    .blocking(move |s| {
                        s.open(&name)?;
                        std::thread::sleep(Duration::from_millis(300));
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
    let elements = out["elements"].as_array()?;
    let mut card = format!(
        "{} · \"{}\" ({} lines, {} elements)",
        s("app"),
        s("window"),
        out["lines"].as_u64().unwrap_or(0),
        elements.len()
    );
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
