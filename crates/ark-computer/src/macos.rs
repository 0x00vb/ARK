use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

use crate::{Access, App, WindowMetadata};
use std::collections::BTreeSet;
use std::ffi::{c_char, c_void};
use std::time::Duration;
type AxRef = CFTypeRef;
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> AxRef;
    fn AXUIElementCopyAttributeValue(el: AxRef, attr: CFStringRef, value: *mut CFTypeRef) -> i32;
    fn AXUIElementSetMessagingTimeout(el: AxRef, seconds: f32) -> i32;
    fn AXUIElementGetPid(el: AxRef, pid: *mut i32) -> i32;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}
extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
}
const ON_SCREEN: u32 = (1 << 0) | (1 << 4);
struct Win {
    owner: String,
    pid: i32,
}
struct Ax(CFType);

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
}
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
        out.push(Win { owner, pid });
    }
    Ok(out)
}

fn find(app: &str) -> Result<App, String> {
    let apps = apps()?;
    let want = app.trim().to_lowercase();
    apps.iter()
        .find(|a| a.name.to_lowercase() == want)
        .cloned()
        .ok_or_else(|| format!("{app} has no open window; computer.open opens it"))
}
pub(crate) fn access() -> Access {
    // SAFETY: no arguments.
    if unsafe { AXIsProcessTrusted() } {
        Access::Allowed
    } else {
        Access::NotYet("Allow ARK in System Settings › Privacy & Security › Accessibility.".into())
    }
}

pub(crate) fn request_access() {
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

pub(crate) fn apps() -> Result<Vec<App>, String> {
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

pub(crate) fn window_metadata(app: &str) -> Result<WindowMetadata, String> {
    if access() != Access::Allowed {
        return Err("ARK is not allowed to use apps yet".into());
    }
    let found = find(app)?;
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

fn metadata_element(pid: i32) -> Ax {
    let root = Ax(unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) });
    unsafe { AXUIElementSetMessagingTimeout(root.raw(), 2.0) };
    root
}
fn front_window(root: &Ax, app: &str) -> Result<Ax, String> {
    root.attr("AXFocusedWindow")
        .map(Ax)
        .ok_or_else(|| format!("{app} has no focused window"))
}
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}
pub(crate) fn since_input() -> Option<Duration> {
    let seconds = unsafe { CGEventSourceSecondsSinceLastEventType(1, u32::MAX) };
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// Recovery must account for offscreen windows and failed AX enumeration.
pub(crate) fn complete_window_ids(pid: i32) -> Result<Vec<String>, String> {
    let root = metadata_element(pid);
    let key = CFString::new("AXWindows");
    let mut value = std::ptr::null();
    let status =
        unsafe { AXUIElementCopyAttributeValue(root.raw(), key.as_concrete_TypeRef(), &mut value) };
    if status != 0 || value.is_null() {
        return Err(format!(
            "Complete recovery window inventory is unavailable ({status})"
        ));
    }
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    let windows = value
        .downcast::<CFArray>()
        .ok_or("Recovery windows are not an array")?;
    let symbol = unsafe { dlsym((-2isize) as *mut c_void, c"_AXUIElementGetWindow".as_ptr()) };
    if symbol.is_null() {
        return Err("Exact recovery window identity is unavailable".into());
    }
    let window_id: unsafe extern "C" fn(AxRef, *mut u32) -> i32 =
        unsafe { std::mem::transmute(symbol) };
    let mut ax = BTreeSet::new();
    for item in windows.get_all_values() {
        let mut owner = 0;
        let mut id = 0;
        if unsafe { AXUIElementGetPid(item, &mut owner) } != 0
            || owner != pid
            || unsafe { window_id(item, &mut id) } != 0
            || id == 0
            || !ax.insert(format!("w-{id}"))
        {
            return Err("Recovery window identity is unknown or duplicated".into());
        }
    }
    let records = unsafe { CGWindowListCopyWindowInfo(1 << 4, 0) };
    if records.is_null() {
        return Err("Complete graphics window inventory is unavailable".into());
    }
    let records = unsafe { CFType::wrap_under_create_rule(records) };
    let records = records
        .downcast::<CFArray>()
        .ok_or("Graphics windows are not an array")?;
    let mut cg = BTreeSet::new();
    for item in records.get_all_values() {
        let info: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_get_rule(item as _) };
        let number = |name: &str| {
            info.find(CFString::new(name))
                .and_then(|value| value.downcast::<CFNumber>())
                .and_then(|number| number.to_i64())
        };
        if number("kCGWindowOwnerPID") == Some(i64::from(pid))
            && number("kCGWindowLayer") == Some(0)
        {
            let id = number("kCGWindowNumber")
                .filter(|id| *id > 0)
                .ok_or("Graphics window identity is unavailable")?;
            cg.insert(format!("w-{id}"));
        }
    }
    if ax != cg {
        return Err("ARK cannot verify every message window. Choose Review after checking the app yourself.".into());
    }
    Ok(ax.into_iter().collect())
}
