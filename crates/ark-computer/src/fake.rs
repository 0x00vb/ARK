//! A screen in memory, for tests: apps with one window of fields and buttons, a record of what
//! was pressed and typed, and a person who can be made to type.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Access, App, Control, Element, Look, Screen};

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
    person_typed: Option<Instant>,
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
            let i = self.screen.inner.lock().unwrap();
            let (_, controls) = i.apps.iter().find(|(a, _)| *a == self.app).ok_or("gone")?;
            controls[self.index].1.clone()
        };
        self.screen.record(format!("press {}/{name}", self.app));
        Ok(())
    }
    fn set_text(&self, text: &str) -> Result<(), String> {
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
        let i = self.inner.lock().unwrap();
        Ok(i.apps
            .iter()
            .enumerate()
            .map(|(n, (a, _))| App {
                name: a.clone(),
                pid: 100 + n as i32,
                front: i.front.as_deref() == Some(a),
            })
            .collect())
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
        i.front = Some(app.to_string());
        drop(i);
        self.record(format!("open {app}"));
        Ok(())
    }
    fn keys(&self, keys: &str) -> Result<(), String> {
        self.record(format!("keys {keys}"));
        Ok(())
    }
    fn type_text(&self, text: &str) -> Result<(), String> {
        self.record(format!("type {text:?}"));
        Ok(())
    }
    fn since_input(&self) -> Option<Duration> {
        self.inner.lock().unwrap().person_typed.map(|t| t.elapsed())
    }
}
