//! macOS: windows read and pressed through the accessibility API (`AXUIElement`), keys sent as
//! keyboard events, apps listed from the window server and opened with `open -a`. Using it needs
//! the person's permission once (System Settings › Privacy & Security › Accessibility).

use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

use crate::{Access, App, Control, Element, Look, Screen};

type AxRef = CFTypeRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> AxRef;
    fn AXUIElementCopyAttributeValue(el: AxRef, attr: CFStringRef, value: *mut CFTypeRef) -> i32;
    fn AXUIElementSetAttributeValue(el: AxRef, attr: CFStringRef, value: CFTypeRef) -> i32;
    fn AXUIElementPerformAction(el: AxRef, action: CFStringRef) -> i32;
    fn AXUIElementSetMessagingTimeout(el: AxRef, seconds: f32) -> i32;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventKeyboardSetUnicodeString(event: *mut c_void, len: usize, chars: *const u16);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CFRelease(cf: *const c_void);
}

/// Window list options: on screen only, without the desktop.
const ON_SCREEN: u32 = (1 << 0) | (1 << 4);
const HID_STATE: i32 = 1;
const ANY_INPUT: u32 = u32::MAX;
const HID_TAP: u32 = 0;

/// Elements visited per look, controls listed, characters of text kept.
const MAX_NODES: usize = 3000;
const MAX_CONTROLS: usize = 150;
const MAX_TEXT: usize = 8000;
const MAX_DEPTH: usize = 40;

/// An accessibility element, retained.
struct Ax(CFType);
// SAFETY: AXUIElement references may be used from any thread; each call is one IPC message.
unsafe impl Send for Ax {}
unsafe impl Sync for Ax {}

impl Ax {
    fn raw(&self) -> AxRef {
        self.0.as_CFTypeRef()
    }

    fn attr(&self, name: &str) -> Option<CFType> {
        let key = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        // SAFETY: a valid element and key; `out` follows the create rule when filled.
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.raw(), key.as_concrete_TypeRef(), &mut out)
        };
        (err == 0 && !out.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(out) })
    }

    fn text(&self, name: &str) -> Option<String> {
        let v = self.attr(name)?;
        if let Some(s) = v.downcast::<CFString>() {
            let s = s.to_string();
            return (!s.trim().is_empty()).then_some(s);
        }
        if let Some(n) = v.downcast::<CFNumber>() {
            return n.to_f64().map(|f| {
                if f.fract() == 0.0 {
                    format!("{}", f as i64)
                } else {
                    format!("{f}")
                }
            });
        }
        None
    }

    fn flag(&self, name: &str) -> Option<bool> {
        self.attr(name)?.downcast::<CFBoolean>().map(bool::from)
    }

    fn children(&self, name: &str) -> Vec<Ax> {
        let Some(v) = self.attr(name) else {
            return Vec::new();
        };
        let Some(arr) = v.downcast::<CFArray>() else {
            return Vec::new();
        };
        arr.get_all_values()
            .into_iter()
            // SAFETY: the array's items are elements; take our own reference to each.
            .map(|p| Ax(unsafe { CFType::wrap_under_get_rule(p) }))
            .collect()
    }

    fn set(&self, name: &str, value: &CFType) -> Result<(), String> {
        let key = CFString::new(name);
        // SAFETY: a valid element, key and value.
        let err = unsafe {
            AXUIElementSetAttributeValue(
                self.raw(),
                key.as_concrete_TypeRef(),
                value.as_CFTypeRef(),
            )
        };
        if err == 0 {
            Ok(())
        } else {
            Err(format!("the app refused to change {name} ({err})"))
        }
    }

    fn act(&self, action: &str) -> Result<(), String> {
        let key = CFString::new(action);
        // SAFETY: a valid element and action name.
        let err = unsafe { AXUIElementPerformAction(self.raw(), key.as_concrete_TypeRef()) };
        if err == 0 {
            Ok(())
        } else {
            Err(format!("the app refused the {action} action ({err})"))
        }
    }
}

/// What the list calls each kind of control, and which kinds are listed.
fn control_role(role: &str, subrole: Option<&str>) -> Option<&'static str> {
    Some(match role {
        "AXButton" => match subrole {
            Some("AXCloseButton") => "close button",
            _ => "button",
        },
        "AXCheckBox" => "checkbox",
        "AXRadioButton" => "radio button",
        "AXTextField" => match subrole {
            Some("AXSearchField") => "search field",
            Some("AXSecureTextField") => "password field",
            _ => "text field",
        },
        "AXTextArea" => "text area",
        "AXComboBox" => "combo box",
        "AXPopUpButton" => "pop-up button",
        "AXMenuButton" => "menu button",
        "AXLink" => "link",
        "AXMenuItem" => "menu item",
        "AXDisclosureTriangle" => "disclosure",
        "AXSlider" => "slider",
        "AXIncrementor" => "stepper",
        "AXTab" => "tab",
        _ => return None,
    })
}

struct Walk {
    elements: Vec<Element>,
    controls: Vec<Arc<dyn Control>>,
    text: String,
    nodes: usize,
}

impl Walk {
    fn visit(&mut self, el: Ax, depth: usize) {
        if self.nodes >= MAX_NODES || depth > MAX_DEPTH {
            return;
        }
        self.nodes += 1;
        let role = el.text("AXRole").unwrap_or_default();
        let subrole = el.text("AXSubrole");
        if role == "AXStaticText" {
            if let Some(v) = el.text("AXValue") {
                self.push_text(&v);
            }
        } else if let Some(kind) = control_role(&role, subrole.as_deref()) {
            if self.controls.len() < MAX_CONTROLS {
                let name = el
                    .text("AXTitle")
                    .or_else(|| el.text("AXDescription"))
                    .or_else(|| el.text("AXPlaceholderValue"))
                    .or_else(|| el.text("AXHelp"))
                    .unwrap_or_default();
                // A password field's value is never read out.
                let value = if kind == "password field" {
                    None
                } else {
                    el.text("AXValue").map(|v| v.chars().take(200).collect())
                };
                self.elements.push(Element {
                    number: self.controls.len() + 1,
                    role: kind.to_string(),
                    name: name.chars().take(120).collect(),
                    value,
                    disabled: el.flag("AXEnabled") == Some(false),
                });
                let fields = matches!(
                    kind,
                    "text area" | "text field" | "search field" | "combo box"
                );
                let el = Arc::new(el);
                self.controls.push(Arc::new(AxControl {
                    el: el.clone(),
                    text: fields,
                }));
                // A text area's own text was listed as its value; its parts are not controls.
                if fields {
                    return;
                }
                for c in el.children("AXChildren") {
                    self.visit(c, depth + 1);
                }
                return;
            }
        }
        for c in el.children("AXChildren") {
            self.visit(c, depth + 1);
        }
    }

    fn push_text(&mut self, t: &str) {
        let t = t.trim();
        if t.is_empty() || self.text.len() >= MAX_TEXT {
            return;
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        let room = MAX_TEXT - self.text.len();
        self.text.extend(t.chars().take(room));
    }
}

struct AxControl {
    el: Arc<Ax>,
    text: bool,
}

impl Control for AxControl {
    fn press(&self) -> Result<(), String> {
        if self.text {
            return self
                .el
                .set("AXFocused", &CFBoolean::true_value().as_CFType());
        }
        self.el.act("AXPress")
    }
    fn set_text(&self, text: &str) -> Result<(), String> {
        let _ = self
            .el
            .set("AXFocused", &CFBoolean::true_value().as_CFType());
        self.el.set("AXValue", &CFString::new(text).as_CFType())
    }
}

pub(crate) struct Mac;

impl Mac {
    fn find(&self, app: &str) -> Result<App, String> {
        let apps = self.apps()?;
        let want = app.trim().to_lowercase();
        apps.iter()
            .find(|a| a.name.to_lowercase() == want)
            .or_else(|| apps.iter().find(|a| a.name.to_lowercase().contains(&want)))
            .cloned()
            .ok_or_else(|| format!("{app} has no open window; computer.open opens it"))
    }
}

fn key_code(k: &str) -> Option<u16> {
    Some(match k {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "=" => 24,
        "9" => 25,
        "7" => 26,
        "-" => 27,
        "8" => 28,
        "0" => 29,
        "]" => 30,
        "o" => 31,
        "u" => 32,
        "[" => 33,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "'" => 39,
        "k" => 40,
        ";" => 41,
        "\\" => 42,
        "," => 43,
        "/" => 44,
        "n" => 45,
        "m" => 46,
        "." => 47,
        "`" => 50,
        "return" | "enter" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "forwarddelete" => 117,
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    })
}

fn post(code: u16, flags: u64, text: Option<&[u16]>) {
    for down in [true, false] {
        // SAFETY: a new event we own, posted once and released.
        unsafe {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
            if ev.is_null() {
                continue;
            }
            if let Some(t) = text {
                CGEventKeyboardSetUnicodeString(ev, t.len(), t.as_ptr());
            }
            CGEventSetFlags(ev, flags);
            CGEventPost(HID_TAP, ev);
            CFRelease(ev as *const c_void);
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

impl Screen for Mac {
    fn access(&self) -> Access {
        // SAFETY: no arguments.
        if unsafe { AXIsProcessTrusted() } {
            Access::Allowed
        } else {
            Access::NotYet(
                "Allow ARK in System Settings › Privacy & Security › Accessibility.".into(),
            )
        }
    }

    fn request_access(&self) {
        // SAFETY: a dictionary of one CF key to a CF boolean.
        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let opts = CFDictionary::from_CFType_pairs(&[(
                key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            )]);
            AXIsProcessTrustedWithOptions(opts.as_CFTypeRef());
        }
    }

    fn apps(&self) -> Result<Vec<App>, String> {
        // SAFETY: returns an array we own (create rule) or null.
        let list = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN, 0) };
        if list.is_null() {
            return Err("the window list is not available".into());
        }
        let list = unsafe { CFType::wrap_under_create_rule(list) };
        let arr = list
            .downcast::<CFArray>()
            .ok_or("the window list is not a list")?;
        let mut apps: Vec<App> = Vec::new();
        for item in arr.get_all_values() {
            let info: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(item as _) };
            let get = |k: &str| info.find(CFString::new(k)).map(|v| v.clone());
            let layer = get("kCGWindowLayer")
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
                .unwrap_or(1);
            if layer != 0 {
                continue;
            }
            let Some(name) = get("kCGWindowOwnerName")
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string())
            else {
                continue;
            };
            let pid = get("kCGWindowOwnerPID")
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i32())
                .unwrap_or(0);
            if pid == 0 || apps.iter().any(|a| a.pid == pid) {
                continue;
            }
            // The list runs front to back: the first app is the one in front.
            let front = apps.is_empty();
            apps.push(App { name, pid, front });
        }
        Ok(apps)
    }

    fn look(&self, app: &str) -> Result<Look, String> {
        if self.access() != Access::Allowed {
            return Err("ARK is not allowed to use apps yet".into());
        }
        let found = self.find(app)?;
        // SAFETY: creates an element we own for a running process.
        let root =
            Ax(unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(found.pid)) });
        unsafe { AXUIElementSetMessagingTimeout(root.raw(), 2.0) };
        let window = root
            .attr("AXFocusedWindow")
            .map(Ax)
            .or_else(|| root.children("AXWindows").into_iter().next())
            .ok_or_else(|| format!("{} has no window open", found.name))?;
        let title = window.text("AXTitle").unwrap_or_default();
        let mut walk = Walk {
            elements: Vec::new(),
            controls: Vec::new(),
            text: String::new(),
            nodes: 0,
        };
        walk.visit(window, 0);
        Ok(Look {
            app: found.name,
            window: title,
            text: walk.text,
            elements: walk.elements,
            controls: walk.controls,
        })
    }

    fn open(&self, app: &str) -> Result<(), String> {
        let out = std::process::Command::new("open")
            .arg("-a")
            .arg(app.trim())
            .output()
            .map_err(|e| format!("could not open {app}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("there is no app named {app} on this computer"))
        }
    }

    fn keys(&self, keys: &str) -> Result<(), String> {
        let mut flags = 0u64;
        let mut key = None;
        for part in keys.to_lowercase().split('+').map(str::trim) {
            match part {
                "cmd" | "command" | "⌘" => flags |= 1 << 20,
                "shift" | "⇧" => flags |= 1 << 17,
                "ctrl" | "control" | "⌃" => flags |= 1 << 18,
                "alt" | "option" | "opt" | "⌥" => flags |= 1 << 19,
                k => key = Some(key_code(k).ok_or_else(|| format!("no key named `{k}`"))?),
            }
        }
        let code = key.ok_or("say which key, e.g. cmd+s")?;
        post(code, flags, None);
        Ok(())
    }

    fn type_text(&self, text: &str) -> Result<(), String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        for chunk in units.chunks(20) {
            post(0, 0, Some(chunk));
        }
        Ok(())
    }

    fn since_input(&self) -> Option<Duration> {
        // SAFETY: plain query.
        let s = unsafe { CGEventSourceSecondsSinceLastEventType(HID_STATE, ANY_INPUT) };
        (s.is_finite() && s >= 0.0).then(|| Duration::from_secs_f64(s))
    }
}
