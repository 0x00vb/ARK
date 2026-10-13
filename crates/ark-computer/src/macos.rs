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

use crate::{
    Access, App, Control, Element, Frame, Look, Menu, MenuItem, Pointer, Screen, Shot,
    WindowMetadata,
};

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
    fn AXUIElementCopyActionNames(el: AxRef, names: *mut CFTypeRef) -> i32;
    fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
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
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
    fn CGEventSetLocation(event: *mut c_void, at: CGPoint);
    fn CGEventCreateMouseEvent(
        source: *const c_void,
        kind: u32,
        at: CGPoint,
        button: u32,
    ) -> *mut c_void;
    fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
    fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheels: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> *mut c_void;
    fn CGWarpMouseCursorPosition(at: CGPoint) -> i32;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CFRelease(cf: *const c_void);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

/// `kAXValueCGPointType`, `kAXValueCGSizeType`.
const AX_VALUE_CGPOINT: u32 = 1;
const AX_VALUE_CGSIZE: u32 = 2;
/// Mouse event kinds and the click count field.
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOUSE_MOVED: u32 = 5;
const CLICK_STATE: u32 = 1;
const PIXEL_UNITS: u32 = 0;

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

    /// The actions the element offers (`AXPress`, `AXShowMenu`, …).
    fn actions(&self) -> Vec<String> {
        let mut out: CFTypeRef = std::ptr::null();
        // SAFETY: a valid element; `out` follows the create rule when filled.
        let err = unsafe { AXUIElementCopyActionNames(self.raw(), &mut out) };
        if err != 0 || out.is_null() {
            return Vec::new();
        }
        let v = unsafe { CFType::wrap_under_create_rule(out) };
        let Some(arr) = v.downcast::<CFArray>() else {
            return Vec::new();
        };
        arr.get_all_values()
            .into_iter()
            // SAFETY: the array holds strings.
            .map(|p| unsafe { CFString::wrap_under_get_rule(p as CFStringRef) }.to_string())
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

/// Window parts that are never listed: the window's own buttons and scroll bar parts. The
/// agent closes, hides and scrolls with keys and `computer.scroll`.
const CHROME: [&str; 8] = [
    "AXCloseButton",
    "AXMinimizeButton",
    "AXZoomButton",
    "AXFullScreenButton",
    "AXIncrementArrow",
    "AXDecrementArrow",
    "AXIncrementPage",
    "AXDecrementPage",
];

/// What the list calls each kind of control, and which kinds are listed.
fn control_role(role: &str, subrole: Option<&str>) -> Option<&'static str> {
    // A search field is one by its subrole, whatever its role (Catalyst apps use static text).
    if subrole == Some("AXSearchField") {
        return Some("search field");
    }
    Some(match role {
        "AXButton" => "button",
        "AXCheckBox" => match subrole {
            Some("AXSwitch") => "switch",
            _ => "checkbox",
        },
        "AXRadioButton" => "radio button",
        "AXTextField" => match subrole {
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
        "AXRow" => "row",
        _ => return None,
    })
}

/// Roles whose children are the items of a list (files in Finder, rows of a table).
fn is_list(role: &str) -> bool {
    matches!(
        role,
        "AXList" | "AXOutline" | "AXTable" | "AXBrowser" | "AXGrid"
    )
}

/// Text as a person reads it: no invisible direction marks, plain spaces, and a phone number
/// read out digit by digit for a screen reader (`+ 5 4,9,3 4`) put back together.
pub(crate) fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => {}
            '\u{00a0}' | '\u{2009}' | '\u{200a}' | '\u{202f}' => out.push(' '),
            c => out.push(c),
        }
    }
    let t = out.trim();
    let digits = t.chars().filter(char::is_ascii_digit).count();
    if digits >= 7
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | ','))
        && t.contains(' ')
    {
        return t.chars().filter(|c| *c != ' ' && *c != ',').collect();
    }
    t.to_string()
}

/// How a listed control is acted on.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Pressed (a button, a link, a checkbox).
    Press,
    /// Focused, and its text replaced.
    Field,
    /// Selected (a row, an item of a list), pressed when it can't be selected.
    Select,
}

struct Walk {
    elements: Vec<Element>,
    controls: Vec<Arc<dyn Control>>,
    text: String,
    nodes: usize,
}

/// Where a walk is in the window: what it may show, and what it is inside.
#[derive(Clone, Copy)]
struct At {
    depth: usize,
    /// The parent is a list, so this is one of its items.
    in_list: bool,
    /// Inside an item whose text already is its name: its text is not repeated.
    quiet: bool,
    /// The part of the screen this element can show in (the window, cut by each scroll area
    /// it is in). What is scrolled out of it is not listed, so a look shows what a person sees
    /// and `computer.scroll` shows more.
    clip: Option<Frame>,
}

fn overlaps(a: Frame, b: Frame) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

fn cut(a: Option<Frame>, b: Option<Frame>) -> Option<Frame> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let x = a.x.max(b.x);
            let y = a.y.max(b.y);
            let w = (a.x + a.w).min(b.x + b.w) - x;
            let h = (a.y + a.h).min(b.y + b.h) - y;
            Some(Frame {
                x,
                y,
                w: w.max(0.0),
                h: h.max(0.0),
            })
        }
        (a, b) => a.or(b),
    }
}

impl Walk {
    fn new() -> Self {
        Walk {
            elements: Vec::new(),
            controls: Vec::new(),
            text: String::new(),
            nodes: 0,
        }
    }

    fn window(&mut self, window: Ax) {
        let clip = frame_of(&window);
        self.visit(
            window,
            At {
                depth: 0,
                in_list: false,
                quiet: false,
                clip,
            },
        );
    }

    /// Whether the element is scrolled out of sight (it says where it is, and that is outside).
    fn hidden(el: &Ax, at: At) -> bool {
        match (at.clip, frame_of(el)) {
            (Some(c), Some(f)) => c.w <= 0.0 || c.h <= 0.0 || !overlaps(c, f),
            _ => false,
        }
    }

    fn visit(&mut self, el: Ax, at: At) {
        let At {
            depth,
            in_list,
            quiet,
            ..
        } = at;
        if self.nodes >= MAX_NODES || depth > MAX_DEPTH {
            return;
        }
        self.nodes += 1;
        let role = el.text("AXRole").unwrap_or_default();
        let subrole = el.text("AXSubrole");
        if CHROME.contains(&subrole.as_deref().unwrap_or("")) || role == "AXScrollBar" {
            return;
        }
        let mut at = At {
            depth: depth + 1,
            ..at
        };
        if role == "AXScrollArea" {
            at.clip = cut(at.clip, frame_of(&el));
        }
        let kind = control_role(&role, subrole.as_deref())
            .or_else(|| (in_list && !is_list(&role)).then_some("item"))
            .or_else(|| {
                // Text or a group the app made pressable (a sidebar entry in a Catalyst app).
                (matches!(
                    role.as_str(),
                    "AXStaticText" | "AXImage" | "AXGroup" | "AXCell"
                ) && el.actions().iter().any(|a| a == "AXPress")
                    && own_name(&el).is_some())
                .then_some("button")
            });
        if (kind.is_some() || role == "AXStaticText") && Self::hidden(&el, at) {
            return;
        }
        // A row's editable name (Finder) repeats the row; the row is the control.
        let kind = kind.filter(|k| {
            !(quiet
                && matches!(*k, "text field" | "text area")
                && el.flag("AXFocused") != Some(true))
        });
        match kind {
            Some(kind) if self.controls.len() < MAX_CONTROLS && !self.unnamed_item(&el, kind) => {
                let field = matches!(
                    kind,
                    "text area" | "text field" | "search field" | "combo box" | "password field"
                );
                let item = matches!(kind, "row" | "item");
                let name = own_name(&el)
                    .or_else(|| {
                        (item || kind == "button")
                            .then(|| inner_text(&el, 120))
                            .flatten()
                    })
                    .unwrap_or_default();
                // A password field's value is never read out.
                let value = if kind == "password field" || item {
                    None
                } else {
                    el.text("AXValue")
                        .map(|v| clean(&v))
                        .filter(|v| !v.is_empty() && *v != name)
                        .map(|v| v.chars().take(200).collect())
                };
                let selected = (item || matches!(kind, "tab" | "radio button"))
                    && el.flag("AXSelected") == Some(true);
                self.elements.push(Element {
                    number: self.controls.len() + 1,
                    role: kind.to_string(),
                    name: name.chars().take(120).collect(),
                    value,
                    disabled: el.flag("AXEnabled") == Some(false),
                    selected,
                    identifier: el
                        .text("AXIdentifier")
                        .map(|v| clean(&v))
                        .filter(|v| !v.is_empty()),
                    description: el
                        .text("AXDescription")
                        .map(|v| clean(&v))
                        .filter(|v| !v.is_empty())
                        .map(|v| v.chars().take(1000).collect()),
                    focused: el.flag("AXFocused") == Some(true),
                    frame: frame_of(&el),
                });
                let el = Arc::new(el);
                self.controls.push(Arc::new(AxControl {
                    el: el.clone(),
                    kind: if field {
                        Kind::Field
                    } else if item {
                        Kind::Select
                    } else {
                        Kind::Press
                    },
                }));
                // A field's own text was listed as its value; its parts are not controls.
                if field {
                    return;
                }
                let inner = At {
                    in_list: is_list(&role),
                    quiet: quiet || item || kind == "button",
                    ..at
                };
                for c in el.children("AXChildren") {
                    self.visit(c, inner);
                }
            }
            _ => {
                if !quiet {
                    match role.as_str() {
                        "AXStaticText" => {
                            if let Some(v) = el.text("AXValue").or_else(|| el.text("AXDescription"))
                            {
                                self.push_text(&clean(&v));
                            }
                        }
                        "AXHeading" if el.children("AXChildren").is_empty() => {
                            if let Some(v) = own_name(&el) {
                                self.push_text(&v);
                            }
                        }
                        _ => {}
                    }
                }
                let inner = At {
                    in_list: is_list(&role),
                    ..at
                };
                for c in el.children("AXChildren") {
                    self.visit(c, inner);
                }
            }
        }
    }

    /// An item of a list with nothing to call it by (a section's header group) is not listed;
    /// what is inside it still is.
    fn unnamed_item(&self, el: &Ax, kind: &str) -> bool {
        matches!(kind, "item" | "row") && own_name(el).is_none() && inner_text(el, 1).is_none()
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

/// An element's own name: its title, description or placeholder, cleaned.
fn own_name(el: &Ax) -> Option<String> {
    ["AXTitle", "AXDescription", "AXPlaceholderValue"]
        .iter()
        .filter_map(|k| el.text(k))
        .map(|t| clean(&t))
        .find(|t| !t.is_empty())
}

/// The text inside an element (a row's cells, a button's label), joined, up to `max` chars.
fn inner_text(el: &Ax, max: usize) -> Option<String> {
    fn walk(el: &Ax, out: &mut Vec<String>, len: &mut usize, max: usize, n: &mut usize) {
        for c in el.children("AXChildren") {
            *n += 1;
            if *len >= max || *n > 60 {
                return;
            }
            let role = c.text("AXRole").unwrap_or_default();
            let t = match role.as_str() {
                "AXStaticText" => c.text("AXValue").or_else(|| c.text("AXDescription")),
                "AXTextField" => own_name(&c).or_else(|| c.text("AXValue")),
                // An icon's name ("clock") only when there is no text at all.
                "AXImage" if out.is_empty() => own_name(&c),
                _ => None,
            };
            if let Some(t) = t.map(|t| clean(&t)).filter(|t| !t.is_empty()) {
                *len += t.len();
                out.push(t);
            }
            walk(&c, out, len, max, n);
        }
    }
    let mut out = Vec::new();
    walk(el, &mut out, &mut 0, max, &mut 0);
    let joined = out.join(" · ");
    (!joined.is_empty()).then(|| joined.chars().take(max).collect())
}

fn frame_of(el: &Ax) -> Option<Frame> {
    let point = el.attr("AXPosition")?;
    let size = el.attr("AXSize")?;
    let mut p = CGPoint { x: 0.0, y: 0.0 };
    let mut s = CGSize {
        width: 0.0,
        height: 0.0,
    };
    // SAFETY: AXValues of the CGPoint and CGSize kinds, read into structs of those layouts.
    let ok = unsafe {
        AXValueGetValue(
            point.as_CFTypeRef(),
            AX_VALUE_CGPOINT,
            &mut p as *mut _ as *mut c_void,
        ) && AXValueGetValue(
            size.as_CFTypeRef(),
            AX_VALUE_CGSIZE,
            &mut s as *mut _ as *mut c_void,
        )
    };
    (ok && s.width > 0.0 && s.height > 0.0).then_some(Frame {
        x: p.x,
        y: p.y,
        w: s.width,
        h: s.height,
    })
}

struct AxControl {
    el: Arc<Ax>,
    kind: Kind,
}

impl Control for AxControl {
    fn press(&self) -> Result<(), String> {
        match self.kind {
            Kind::Field => self
                .el
                .set("AXFocused", &CFBoolean::true_value().as_CFType()),
            Kind::Select => self
                .el
                .set("AXSelected", &CFBoolean::true_value().as_CFType())
                .or_else(|_| self.el.act("AXPress")),
            Kind::Press => self.el.act("AXPress"),
        }
    }
    fn set_text(&self, text: &str) -> Result<(), String> {
        let _ = self
            .el
            .set("AXFocused", &CFBoolean::true_value().as_CFType());
        self.el.set("AXValue", &CFString::new(text).as_CFType())
    }
    fn frame(&self) -> Option<Frame> {
        frame_of(&self.el)
    }
}

/// The app's root element, with Electron's accessibility switched on (it builds no tree for
/// anyone until asked). Returns whether the switch was new to the app, so the caller can give it
/// a moment to build the tree.
fn app_element(pid: i32) -> (Ax, bool) {
    // SAFETY: creates an element we own for a running process.
    let root = Ax(unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) });
    unsafe { AXUIElementSetMessagingTimeout(root.raw(), 2.0) };
    let fresh = root.flag("AXManualAccessibility") == Some(false)
        && root
            .set(
                "AXManualAccessibility",
                &CFBoolean::true_value().as_CFType(),
            )
            .is_ok();
    (root, fresh)
}

/// The app's root for metadata-only reads. Unlike `app_element`, this never asks an app to
/// enable or build its accessibility tree.
fn metadata_element(pid: i32) -> Ax {
    // SAFETY: creates an element we own for a running process.
    let root = Ax(unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) });
    unsafe { AXUIElementSetMessagingTimeout(root.raw(), 2.0) };
    root
}

fn front_window(root: &Ax, app: &str) -> Result<Ax, String> {
    root.attr("AXFocusedWindow")
        .map(Ax)
        .or_else(|| root.children("AXWindows").into_iter().next())
        .ok_or_else(|| format!("{app} has no window open"))
}

/// A menu's items, as listed to an agent.
fn menu_items(menu: &Ax) -> Vec<(MenuItem, Ax)> {
    menu.children("AXChildren")
        .into_iter()
        .filter_map(|it| {
            let name = it.text("AXTitle").map(|t| clean(&t)).unwrap_or_default();
            if name.is_empty() {
                return None; // a separator
            }
            let submenu = !it.children("AXChildren").is_empty();
            let key = it.text("AXMenuItemCmdChar").and_then(|c| {
                let mods = it
                    .attr("AXMenuItemCmdModifiers")
                    .and_then(|v| v.downcast::<CFNumber>())
                    .and_then(|n| n.to_i64())
                    .unwrap_or(0);
                // The modifier mask: 1 shift, 2 option, 4 control, 8 no command.
                let mut k = String::new();
                if mods & 4 != 0 {
                    k.push_str("ctrl+");
                }
                if mods & 2 != 0 {
                    k.push_str("alt+");
                }
                if mods & 1 != 0 {
                    k.push_str("shift+");
                }
                if mods & 8 == 0 {
                    k.push_str("cmd+");
                }
                // No modifier at all: the item uses the globe key, which keys can't press.
                (!k.is_empty()).then(|| k + &c.to_lowercase())
            });
            Some((
                MenuItem {
                    name,
                    disabled: it.flag("AXEnabled") == Some(false),
                    submenu,
                    keys: key,
                },
                it,
            ))
        })
        .collect()
}

pub(crate) struct Mac;

/// A window on screen, as the window server lists it (front to back).
struct Win {
    owner: String,
    pid: i32,
    number: i64,
    bounds: Frame,
}

/// The on-screen windows of apps (layer 0), front to back.
fn windows() -> Result<Vec<Win>, String> {
    // SAFETY: returns an array we own (create rule) or null.
    let list = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN, 0) };
    if list.is_null() {
        return Err("the window list is not available".into());
    }
    let list = unsafe { CFType::wrap_under_create_rule(list) };
    let arr = list
        .downcast::<CFArray>()
        .ok_or("the window list is not a list")?;
    let mut out = Vec::new();
    for item in arr.get_all_values() {
        let info: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_get_rule(item as _) };
        let get = |k: &str| info.find(CFString::new(k)).map(|v| v.clone());
        let num = |k: &str| {
            get(k)
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_f64())
        };
        if num("kCGWindowLayer").unwrap_or(1.0) != 0.0 {
            continue;
        }
        let Some(owner) = get("kCGWindowOwnerName")
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| clean(&s.to_string()))
        else {
            continue;
        };
        let pid = num("kCGWindowOwnerPID").unwrap_or(0.0) as i32;
        if pid == 0 {
            continue;
        }
        let bounds = get("kCGWindowBounds")
            .and_then(|v| v.downcast::<CFDictionary>())
            .map(|d| {
                let d: CFDictionary<CFString, CFType> =
                    unsafe { CFDictionary::wrap_under_get_rule(d.as_concrete_TypeRef()) };
                let f = |k: &str| {
                    d.find(CFString::new(k))
                        .and_then(|v| v.downcast::<CFNumber>())
                        .and_then(|n| n.to_f64())
                        .unwrap_or(0.0)
                };
                Frame {
                    x: f("X"),
                    y: f("Y"),
                    w: f("Width"),
                    h: f("Height"),
                }
            })
            .unwrap_or(Frame {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            });
        out.push(Win {
            owner,
            pid,
            number: num("kCGWindowNumber").unwrap_or(0.0) as i64,
            bounds,
        });
    }
    Ok(out)
}

/// A PNG's width and height, from its header.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
    Some((be(16), be(20)))
}

fn same(listed: &str, asked: &str) -> bool {
    let norm = |x: &str| clean(x).trim_end_matches(['…', '.']).trim().to_lowercase();
    norm(listed) == norm(asked)
}

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
        let mut apps: Vec<App> = Vec::new();
        for w in windows()? {
            if apps.iter().any(|a| a.pid == w.pid) {
                continue;
            }
            // The list runs front to back: the first app is the one in front.
            let front = apps.is_empty();
            apps.push(App {
                name: w.owner,
                pid: w.pid,
                front,
            });
        }
        Ok(apps)
    }

    fn window_metadata(&self, app: &str) -> Result<WindowMetadata, String> {
        if self.access() != Access::Allowed {
            return Err("ARK is not allowed to use apps yet".into());
        }
        let found = self.find(app)?;
        let root = metadata_element(found.pid);
        let window = front_window(&root, &found.name)?;
        Ok(WindowMetadata {
            app: found.name,
            window: window
                .text("AXTitle")
                .map(|value| clean(&value))
                .unwrap_or_default(),
            window_identifier: window
                .text("AXIdentifier")
                .map(|value| clean(&value))
                .filter(|value| !value.is_empty()),
        })
    }

    fn capture(&self, app: &str, max: u32) -> Result<Shot, String> {
        // SAFETY: plain queries.
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            unsafe { CGRequestScreenCaptureAccess() };
            return Err(
                "ARK may not take pictures of windows yet. Allow it in System Settings › \
                 Privacy & Security › Screen & System Audio Recording, then try again."
                    .into(),
            );
        }
        let found = self.find(app)?;
        let w = windows()?
            .into_iter()
            .find(|w| w.pid == found.pid)
            .ok_or_else(|| format!("{} has no window on screen", found.name))?;
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ark-window-{}-{}.png",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // One window by its number, without its shadow or a sound.
        let shot = std::process::Command::new("screencapture")
            .args(["-x", "-o", "-t", "png"])
            .arg(format!("-l{}", w.number))
            .arg(&path)
            .output()
            .map_err(|e| format!("could not take a picture of the window: {e}"))?;
        let read = || -> Result<Shot, String> {
            if !shot.status.success() || !path.exists() {
                return Err(format!(
                    "could not take a picture of {}'s window",
                    found.name
                ));
            }
            // Made smaller for the model, its longer side at most `max` pixels.
            let fit = std::process::Command::new("sips")
                .args(["-Z", &max.to_string()])
                .arg(&path)
                .output()
                .map_err(|e| format!("could not resize the picture: {e}"))?;
            if !fit.status.success() {
                return Err("could not resize the picture".into());
            }
            let png = std::fs::read(&path).map_err(|e| e.to_string())?;
            let (width, height) = png_size(&png).ok_or("the picture is not a PNG")?;
            Ok(Shot {
                png,
                width,
                height,
                frame: w.bounds,
            })
        };
        let out = read();
        let _ = std::fs::remove_file(&path);
        out
    }

    fn look(&self, app: &str) -> Result<Look, String> {
        if self.access() != Access::Allowed {
            return Err("ARK is not allowed to use apps yet".into());
        }
        let found = self.find(app)?;
        let (root, fresh) = app_element(found.pid);
        let mut walk = Walk::new();
        let mut window = front_window(&root, &found.name)?;
        let mut window_identifier = window.text("AXIdentifier").map(|v| clean(&v));
        walk.window(window);
        if fresh && walk.elements.is_empty() && walk.text.is_empty() {
            // Electron builds its tree once asked; give it a moment and read again.
            std::thread::sleep(Duration::from_millis(700));
            walk = Walk::new();
            window = front_window(&root, &found.name)?;
            window_identifier = window.text("AXIdentifier").map(|v| clean(&v));
            walk.window(window);
        }
        let title = front_window(&root, &found.name)?
            .text("AXTitle")
            .map(|t| clean(&t))
            .unwrap_or_default();
        Ok(Look {
            app: found.name,
            window: title,
            window_identifier: window_identifier.filter(|v| !v.is_empty()),
            text: walk.text,
            elements: walk.elements,
            controls: walk.controls,
        })
    }

    fn menu(&self, app: &str, path: &[String]) -> Result<Menu, String> {
        let found = self.find(app)?;
        let (root, _) = app_element(found.pid);
        let bar = root
            .attr("AXMenuBar")
            .map(Ax)
            .ok_or_else(|| format!("{} has no menu bar", found.name))?;
        // The top level: the menu bar's titles, the app's own menu ("Apple") left out.
        let mut items: Vec<(MenuItem, Ax)> = bar
            .children("AXChildren")
            .into_iter()
            .filter_map(|it| {
                let name = it.text("AXTitle").map(|t| clean(&t))?;
                if name.is_empty() || name == "Apple" {
                    return None;
                }
                Some((
                    MenuItem {
                        name,
                        disabled: false,
                        submenu: true,
                        keys: None,
                    },
                    it,
                ))
            })
            .collect();
        let mut walked: Vec<String> = Vec::new();
        for (depth, want) in path.iter().enumerate() {
            let pos = items
                .iter()
                .position(|(m, _)| same(&m.name, want))
                .ok_or_else(|| {
                    let names: Vec<&str> = items.iter().map(|(m, _)| m.name.as_str()).collect();
                    format!(
                        "no “{want}” in {}; there is: {}",
                        if walked.is_empty() {
                            "the menu bar".into()
                        } else {
                            walked.join(" › ")
                        },
                        names.join(", ")
                    )
                })?;
            let (item, el) = items.swap_remove(pos);
            walked.push(item.name.clone());
            if item.disabled {
                return Err(format!(
                    "“{}” is not available now (greyed out)",
                    walked.join(" › ")
                ));
            }
            let last = depth + 1 == path.len();
            if !item.submenu {
                if !last {
                    return Err(format!("“{}” has no submenu", walked.join(" › ")));
                }
                el.act("AXPress")?;
                return Ok(Menu::Chosen(walked));
            }
            let menu = el
                .children("AXChildren")
                .into_iter()
                .next()
                .ok_or_else(|| format!("“{}” has no items", walked.join(" › ")))?;
            items = menu_items(&menu);
        }
        Ok(Menu::Items(items.into_iter().map(|(m, _)| m).collect()))
    }

    fn window_frame(&self, app: &str) -> Result<Frame, String> {
        let found = self.find(app)?;
        let (root, _) = app_element(found.pid);
        frame_of(&front_window(&root, &found.name)?)
            .ok_or_else(|| format!("{} did not say where its window is", found.name))
    }

    fn scroll(&self, at: Frame, pages: f64) -> Result<(), String> {
        let (x, y) = at.center();
        // Positive wheel values scroll up (content moves down), as a wheel turned away.
        let mut left = -(at.h * 0.8 * pages).round() as i32;
        while left != 0 {
            let step = left.clamp(-120, 120);
            left -= step;
            // SAFETY: a new event we own, placed, posted once and released.
            unsafe {
                let ev =
                    CGEventCreateScrollWheelEvent2(std::ptr::null(), PIXEL_UNITS, 1, step, 0, 0);
                if ev.is_null() {
                    return Err("the system made no scroll event".into());
                }
                CGEventSetLocation(ev, CGPoint { x, y });
                CGEventPost(HID_TAP, ev);
                CFRelease(ev as *const c_void);
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        Ok(())
    }

    fn pointer(&self, x: f64, y: f64, how: Pointer) -> Result<(), String> {
        let at = CGPoint { x, y };
        // SAFETY: new events we own, posted once and released; the pointer is put back after.
        unsafe {
            let probe = CGEventCreate(std::ptr::null());
            let was = (!probe.is_null()).then(|| CGEventGetLocation(probe));
            if !probe.is_null() {
                CFRelease(probe as *const c_void);
            }
            let send = |kind: u32, button: u32, clicks: i64| {
                let ev = CGEventCreateMouseEvent(std::ptr::null(), kind, at, button);
                if !ev.is_null() {
                    CGEventSetIntegerValueField(ev, CLICK_STATE, clicks);
                    CGEventPost(HID_TAP, ev);
                    CFRelease(ev as *const c_void);
                }
                std::thread::sleep(Duration::from_millis(30));
            };
            send(MOUSE_MOVED, 0, 0);
            match how {
                Pointer::Click => {
                    send(LEFT_DOWN, 0, 1);
                    send(LEFT_UP, 0, 1);
                }
                Pointer::Double => {
                    send(LEFT_DOWN, 0, 1);
                    send(LEFT_UP, 0, 1);
                    send(LEFT_DOWN, 0, 2);
                    send(LEFT_UP, 0, 2);
                }
                Pointer::Right => {
                    send(RIGHT_DOWN, 1, 1);
                    send(RIGHT_UP, 1, 1);
                }
            }
            if let Some(p) = was {
                CGWarpMouseCursorPosition(p);
            }
        }
        Ok(())
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

/// The raw accessibility tree of an app's front window, for working on the reader by hand.
#[doc(hidden)]
pub fn tree(app: &str, max_depth: usize) -> Result<String, String> {
    let found = Mac.find(app)?;
    let (root, _) = app_element(found.pid);
    let window = front_window(&root, &found.name)?;
    let mut out = String::new();
    fn walk(el: &Ax, depth: usize, max: usize, out: &mut String, n: &mut usize) {
        if depth > max || *n > 4000 {
            return;
        }
        *n += 1;
        let f = |k: &str| {
            el.text(k)
                .map(|v| format!(" {k}={:?}", v.chars().take(60).collect::<String>()))
                .unwrap_or_default()
        };
        out.push_str(&format!(
            "{}{}{}{}{}{}{}{}\n",
            "  ".repeat(depth),
            el.text("AXRole").unwrap_or_default(),
            f("AXSubrole"),
            f("AXTitle"),
            f("AXDescription"),
            f("AXValue"),
            f("AXIdentifier"),
            f("AXHelp"),
        ));
        for c in el.children("AXChildren") {
            walk(&c, depth + 1, max, out, n);
        }
    }
    let mut n = 0;
    walk(&window, 0, max_depth, &mut out, &mut n);
    Ok(out)
}
