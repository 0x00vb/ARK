//! A screen in memory, for tests: apps with one window of fields and buttons, a record of what
//! was pressed and typed, and a person who can be made to type.

use std::collections::HashMap;
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use crate::{
    Access, App, Control, Element, Frame, Look, Menu, MenuItem, Pointer, Screen, Shot,
    WindowMetadata,
};

/// What happened on the fake screen, in order: `open Notes`, `press Notes/Save`,
/// `set Notes/Title = "x"`, `keys cmd+s`.
pub type Log = Arc<Mutex<Vec<String>>>;

/// A control: (role, name, value).
type Ctl = (String, String, Option<String>);

#[derive(Default)]
struct Inner {
    /// Each app and its window's controls.
    apps: Vec<(String, Vec<Ctl>)>,
    front: Option<String>,
    allowed: bool,
    asked: bool,
    apps_error: bool,
    apps_started: Option<Arc<Barrier>>,
    apps_release: Option<Arc<Barrier>>,
    /// Fields refuse text set directly (it must be typed).
    no_set_text: bool,
    /// Opening an app does not bring it to the front.
    stuck_front: bool,
    person_typed: Option<Instant>,
    /// The field that has the focus: (app, control index).
    focus: Option<(String, usize)>,
    sends_on_return: bool,
    return_fails: bool,
}

/// A fake [`Screen`].
#[derive(Clone)]
pub struct FakeScreen {
    inner: Arc<Mutex<Inner>>,
    pub log: Log,
}

impl Default for FakeScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeScreen {
    pub fn new() -> Self {
        FakeScreen {
            inner: Arc::new(Mutex::new(Inner {
                allowed: true,
                ..Default::default()
            })),
            log: Arc::default(),
        }
    }

    /// An open app whose window has these controls: `("button", "Save")`, `("text field", "Title")`.
    pub fn app(self, name: &str, controls: &[(&str, &str)]) -> Self {
        self.inner.lock().unwrap().apps.push((
            name.to_string(),
            controls
                .iter()
                .map(|(r, n)| (r.to_string(), n.to_string(), None))
                .collect(),
        ));
        self
    }

    /// ARK is not allowed yet; the first request "prompts" and allows after `after`.
    pub fn not_allowed_until_asked(self, after: Duration) -> Self {
        self.inner.lock().unwrap().allowed = false;
        let inner = self.inner.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(20));
            let mut i = inner.lock().unwrap();
            if i.asked {
                drop(i);
                std::thread::sleep(after);
                inner.lock().unwrap().allowed = true;
                break;
            }
            i.allowed = false;
        });
        self
    }

    /// Fields take no text set directly: it is typed into the focus instead.
    pub fn fields_refuse_text(self) -> Self {
        self.inner.lock().unwrap().no_set_text = true;
        self
    }

    /// Opening an app leaves another one in front (another window took the focus).
    pub fn front_stays_elsewhere(self) -> Self {
        self.inner.lock().unwrap().stuck_front = true;
        self
    }

    /// Listing apps fails, for ambient-context recovery tests.
    pub fn apps_fail(self) -> Self {
        self.inner.lock().unwrap().apps_error = true;
        self
    }

    /// Hold an app listing between the two barriers, for deterministic revocation tests.
    pub fn apps_blocked(self, started: Arc<Barrier>, release: Arc<Barrier>) -> Self {
        let mut inner = self.inner.lock().unwrap();
        inner.apps_started = Some(started);
        inner.apps_release = Some(release);
        drop(inner);
        self
    }

    /// Another app takes the front and opening apps can no longer move it. Used to prove that
    /// global keys are never posted on stale accessibility focus.
    pub fn keep_front_on(&self, app: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.front = Some(app.to_string());
        inner.stuck_front = true;
    }

    /// Return sends the focused field: it clears the field and appends an outgoing item.
    pub fn sends_on_return(self) -> Self {
        self.inner.lock().unwrap().sends_on_return = true;
        self
    }

    /// Return reports an error, which cannot prove whether the app received it.
    pub fn return_fails(self) -> Self {
        self.inner.lock().unwrap().return_fails = true;
        self
    }

    /// The person touches the keyboard now.
    pub fn person_types(&self) {
        self.inner.lock().unwrap().person_typed = Some(Instant::now());
    }

    /// A control's current value.
    pub fn value(&self, app: &str, name: &str) -> Option<String> {
        let i = self.inner.lock().unwrap();
        i.apps
            .iter()
            .find(|(a, _)| a == app)?
            .1
            .iter()
            .find(|(_, n, _)| n == name)?
            .2
            .clone()
    }

    fn type_into_focus(&self, text: &str) {
        let mut i = self.inner.lock().unwrap();
        if let Some((app, k)) = i.focus.clone() {
            if let Some((_, controls)) = i.apps.iter_mut().find(|(a, _)| *a == app) {
                controls[k].2 = Some(text.to_string());
            }
        }
    }

    fn record(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }
}

struct FakeControl {
    screen: FakeScreen,
    app: String,
    index: usize,
}

impl Control for FakeControl {
    fn press(&self) -> Result<(), String> {
        let name = {
            let mut i = self.screen.inner.lock().unwrap();
            let (_, controls) = i.apps.iter().find(|(a, _)| *a == self.app).ok_or("gone")?;
            let (role, name, _) = controls[self.index].clone();
            // Pressing a field gives it the focus: typed keys land there.
            if role.contains("field") || role.contains("area") {
                i.focus = Some((self.app.clone(), self.index));
            }
            name
        };
        self.screen.record(format!("press {}/{name}", self.app));
        Ok(())
    }
    fn frame(&self) -> Option<Frame> {
        Some(Frame {
            x: 10.0,
            y: 20.0 + 30.0 * self.index as f64,
            w: 100.0,
            h: 20.0,
        })
    }
    fn set_text(&self, text: &str) -> Result<(), String> {
        if self.screen.inner.lock().unwrap().no_set_text {
            return Err("this field takes typing only".into());
        }
        let name = {
            let mut i = self.screen.inner.lock().unwrap();
            let (_, controls) = i
                .apps
                .iter_mut()
                .find(|(a, _)| *a == self.app)
                .ok_or("gone")?;
            controls[self.index].2 = Some(text.to_string());
            controls[self.index].1.clone()
        };
        self.screen
            .record(format!("set {}/{name} = {text:?}", self.app));
        Ok(())
    }
}

impl Screen for FakeScreen {
    fn access(&self) -> Access {
        if self.inner.lock().unwrap().allowed {
            Access::Allowed
        } else {
            Access::NotYet("Allow it in the test's settings.".into())
        }
    }
    fn request_access(&self) {
        self.inner.lock().unwrap().asked = true;
        self.record("asked for access".into());
    }
    fn apps(&self) -> Result<Vec<App>, String> {
        let (started, release) = {
            let i = self.inner.lock().unwrap();
            (i.apps_started.clone(), i.apps_release.clone())
        };
        if let Some(started) = started {
            started.wait();
        }
        if let Some(release) = release {
            release.wait();
        }
        let i = self.inner.lock().unwrap();
        if i.apps_error {
            return Err("the app list is unavailable".into());
        }
        Ok(i.apps
            .iter()
            .enumerate()
            .map(|(n, (a, _))| App {
                name: a.clone(),
                pid: 100 + n as i32,
                front: i
                    .front
                    .as_deref()
                    .is_some_and(|f| f.eq_ignore_ascii_case(a)),
            })
            .collect())
    }
    fn window_metadata(&self, app: &str) -> Result<WindowMetadata, String> {
        let name = self
            .inner
            .lock()
            .unwrap()
            .apps
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(app))
            .map(|(name, _)| name.clone())
            .ok_or_else(|| format!("{app} is not open; computer.open opens it"))?;
        self.record(format!("metadata {name}"));
        Ok(WindowMetadata {
            app: name.clone(),
            window: format!("{name} window"),
            window_identifier: Some(format!("fake-{name}-window")),
        })
    }
    fn look(&self, app: &str) -> Result<Look, String> {
        let i = self.inner.lock().unwrap();
        let (name, controls) = i
            .apps
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(app))
            .ok_or_else(|| format!("{app} is not open; computer.open opens it"))?;
        let mut names = HashMap::new();
        let elements = controls
            .iter()
            .enumerate()
            .map(|(k, (role, n, v))| {
                names.insert(k, n.clone());
                Element {
                    number: k + 1,
                    role: role.clone(),
                    name: n.clone(),
                    value: v.clone(),
                    disabled: false,
                    selected: false,
                    identifier: Some(format!("fake-{k}")),
                    description: Some(n.clone()),
                    focused: i
                        .focus
                        .as_ref()
                        .is_some_and(|(a, at)| a == name && *at == k),
                    frame: Some(Frame {
                        x: 10.0,
                        y: 20.0 + 30.0 * k as f64,
                        w: 100.0,
                        h: 20.0,
                    }),
                }
            })
            .collect();
        let handles = (0..controls.len())
            .map(|index| {
                Arc::new(FakeControl {
                    screen: self.clone(),
                    app: name.clone(),
                    index,
                }) as Arc<dyn Control>
            })
            .collect();
        Ok(Look {
            app: name.clone(),
            window: format!("{name} window"),
            window_identifier: Some(format!("fake-{name}-window")),
            text: format!("The {name} window."),
            elements,
            controls: handles,
        })
    }
    fn open(&self, app: &str) -> Result<(), String> {
        let mut i = self.inner.lock().unwrap();
        if !i.apps.iter().any(|(a, _)| a.eq_ignore_ascii_case(app)) {
            i.apps.push((app.to_string(), Vec::new()));
        }
        if !i.stuck_front {
            i.front = Some(app.to_string());
        }
        drop(i);
        self.record(format!("open {app}"));
        Ok(())
    }
    fn keys(&self, keys: &str) -> Result<(), String> {
        if keys == "return" && self.inner.lock().unwrap().return_fails {
            return Err("Return had no acknowledgement".into());
        }
        if keys == "delete" {
            self.type_into_focus("");
        }
        if keys == "return" {
            let mut i = self.inner.lock().unwrap();
            if i.sends_on_return {
                if let Some((app, k)) = i.focus.clone() {
                    if let Some((_, controls)) = i.apps.iter_mut().find(|(a, _)| *a == app) {
                        let text = controls[k].2.take().unwrap_or_default();
                        controls.push(("button".into(), format!("Outgoing {text}"), None));
                    }
                }
            }
        }
        self.record(format!("keys {keys}"));
        Ok(())
    }
    /// Typed text replaces the focused field's (an agent selects all first).
    fn type_text(&self, text: &str) -> Result<(), String> {
        self.type_into_focus(text);
        self.record(format!("type {text:?}"));
        Ok(())
    }
    fn since_input(&self) -> Option<Duration> {
        self.inner.lock().unwrap().person_typed.map(|t| t.elapsed())
    }
    /// Every app has a menu bar of File (Save, cmd+s; Export › PDF) and Edit (Undo, greyed).
    fn menu(&self, app: &str, path: &[String]) -> Result<Menu, String> {
        let item = |name: &str, submenu: bool, keys: Option<&str>, disabled: bool| MenuItem {
            name: name.into(),
            disabled,
            submenu,
            keys: keys.map(Into::into),
        };
        let p: Vec<String> = path.iter().map(|x| x.to_lowercase()).collect();
        let p: Vec<&str> = p.iter().map(String::as_str).collect();
        match p.as_slice() {
            [] => Ok(Menu::Items(vec![
                item("File", true, None, false),
                item("Edit", true, None, false),
            ])),
            ["file"] => Ok(Menu::Items(vec![
                item("Save", false, Some("cmd+s"), false),
                item("Export", true, None, false),
            ])),
            ["file", "export"] => Ok(Menu::Items(vec![item("PDF…", false, None, false)])),
            ["edit"] => Ok(Menu::Items(vec![item("Undo", false, Some("cmd+z"), true)])),
            ["edit", "undo"] => Err("“Edit › Undo” is not available now (greyed out)".into()),
            ["file", "save"] | ["file", "export", "pdf…"] | ["file", "export", "pdf"] => {
                self.record(format!("menu {app}/{}", path.join(" > ")));
                Ok(Menu::Chosen(path.to_vec()))
            }
            _ => Err(format!("no {} in the menu bar", path.join(" > "))),
        }
    }
    fn window_frame(&self, _app: &str) -> Result<Frame, String> {
        Ok(Frame {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        })
    }
    fn scroll(&self, at: Frame, pages: f64) -> Result<(), String> {
        let (x, y) = at.center();
        self.record(format!("scroll {pages} at {x},{y}"));
        Ok(())
    }
    fn pointer(&self, x: f64, y: f64, how: Pointer) -> Result<(), String> {
        self.record(format!("pointer {how:?} at {x},{y}"));
        Ok(())
    }
    /// A 400 by 300 picture of an 800 by 600 window at the screen's corner: a pixel is two
    /// points.
    fn capture(&self, app: &str, _max: u32) -> Result<Shot, String> {
        self.record(format!("capture {app}"));
        Ok(Shot {
            png: b"\x89PNG fake".to_vec(),
            width: 400,
            height: 300,
            frame: Frame {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 600.0,
            },
        })
    }
}
