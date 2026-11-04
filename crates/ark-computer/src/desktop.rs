use crate::{
    Access, App, Control, Element, Frame, Look, Menu, MenuItem, OperationPermit, Pointer, Screen,
    Shot, WindowMetadata,
};
use agent_desktop_core::{self as ad, Action, ActionRequest, InputOps, ObservationOps, SystemOps};
use agent_desktop_macos::{MacOSAdapter, RetainedRefSession};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Weak,
};
use std::time::Duration;
mod guard;
#[cfg(test)]
mod queue_tests;

const TIMEOUT: Duration = Duration::from_secs(12);
const LIMIT_CONTROLS: usize = 4096;
const LIMIT_TEXT: usize = 1024 * 1024;
type Reply<T> = Result<T, String>;
type Job = Box<dyn FnOnce(&mut Worker) + Send>;
type Pending<T> = (mpsc::Receiver<Reply<T>>, Arc<AtomicBool>);

#[derive(Clone)]
struct Epoch {
    generation: Arc<AtomicU64>,
    expected: u64,
}
impl Epoch {
    fn check(&self) -> Reply<()> {
        if self.generation.load(Ordering::Acquire) == self.expected {
            Ok(())
        } else {
            Err(error(revoked()))
        }
    }
}
struct Authority {
    permit: OperationPermit,
    timeout_active: Arc<AtomicBool>,
    snapshot_epoch: Option<Epoch>,
}
impl Authority {
    fn check(&self) -> Reply<()> {
        self.permit.check()?;
        if let Some(epoch) = &self.snapshot_epoch {
            epoch.check()?;
        }
        if self.timeout_active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(error(revoked()))
        }
    }
}
fn revoked() -> ad::AdapterError {
    ad::AdapterError::new(
        ad::ErrorCode::PolicyDenied,
        "Computer authorization expired before native dispatch",
    )
    .with_disposition(ad::DeliverySemantics::not_delivered())
}
fn error(error: ad::AdapterError) -> String {
    format!("{} [delivery={:?}]", error, error.disposition)
}
fn deadline() -> Reply<ad::Deadline> {
    ad::Deadline::after(8000).map_err(error)
}

#[derive(Clone)]
pub(crate) struct Desktop {
    sender: mpsc::SyncSender<Job>,
    permit: OperationPermit,
}
impl Desktop {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(16);
        if let Err(err) = std::thread::Builder::new()
            .name("ark-computer-native".into())
            .spawn(move || {
                let retained = agent_desktop_macos::ensure_cocoa_multithreaded()
                    .map_err(error)
                    .and_then(|()| RetainedRefSession::start().map_err(error));
                let mut worker = Worker {
                    native: MacOSAdapter::new(),
                    retained,
                    live: Vec::new(),
                    target: None,
                    input_target: None,
                    focused: None,
                    root: None,
                    observation_epoch: None,
                };
                while let Ok(job) = receiver.recv() {
                    job(&mut worker);
                }
            })
        {
            tracing::error!(error = %err, "could not start Computer native thread");
        }
        Self {
            sender,
            permit: OperationPermit {
                generation: Arc::new(AtomicU64::new(0)),
                expected: 0,
                active: Arc::new(AtomicBool::new(true)),
            },
        }
    }
    fn epoch(&self) -> Epoch {
        Epoch {
            generation: self.permit.generation.clone(),
            expected: self.permit.expected,
        }
    }
    fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Worker, &Authority) -> Reply<T> + Send + 'static,
    ) -> Reply<T> {
        self.call_with_timeout(TIMEOUT, operation)
    }
    fn call_with_timeout<T: Send + 'static>(
        &self,
        timeout: Duration,
        operation: impl FnOnce(&mut Worker, &Authority) -> Reply<T> + Send + 'static,
    ) -> Reply<T> {
        let (receive, active) = self.enqueue(operation)?;
        let result = receive.recv_timeout(timeout).map_err(|err| format!("Computer native response unavailable: {err} [delivery=DeliveryUncertain]; do not retry blindly"));
        active.store(false, Ordering::Release);
        result?
    }
    fn enqueue<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Worker, &Authority) -> Reply<T> + Send + 'static,
    ) -> Reply<Pending<T>> {
        let (reply, receive) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicBool::new(true));
        let authority = Authority {
            permit: self.permit.clone(),
            timeout_active: active.clone(),
            snapshot_epoch: None,
        };
        self.sender
            .try_send(Box::new(move |worker| {
                let result = authority
                    .check()
                    .and_then(|()| operation(worker, &authority));
                let _receiver_dropped = reply.send(result);
            }))
            .map_err(|err| {
                format!("Computer native queue unavailable: {err} [delivery=NotDelivered]")
            })?;
        Ok((receive, active))
    }
    fn act(
        &self,
        entry: Arc<ad::RefEntry>,
        epoch: Epoch,
        actions: Vec<Action>,
        expected_value: Option<String>,
    ) -> Reply<()> {
        self.call(move |worker, authority| {
            epoch.check()?;
            let authority = Authority {
                permit: authority.permit.clone(),
                timeout_active: authority.timeout_active.clone(),
                snapshot_epoch: Some(epoch.clone()),
            };
            for (completed, action) in actions.into_iter().enumerate() {
                if expected_value.is_some() && matches!(action, Action::TypeText(_)) {
                    worker.require_full_selection(&authority, &entry)?;
                }
                epoch.check().and_then(|()| worker.action(&authority, &entry, action)).map_err(|err| {
                    if completed == 0 { err } else { format!("Native sequence stopped after {completed} completed steps [sequence_delivery=DeliveredUnverified]: {err}") }
                })?;
            }
            if let Some(expected) = expected_value {
                let actual = worker.field_value(&authority, &entry).map_err(|_| "Native replacement could not read back the field [delivery=DeliveredUnverified]; do not retry".to_string())?;
                if actual != expected {
                    return Err("Native replacement did not produce the exact requested value [delivery=DeliveredUnverified]; do not retry".into());
                }
            }
            worker.focused = Some(entry);
            Ok(())
        })
    }
}
struct Worker {
    native: MacOSAdapter,
    retained: Reply<RetainedRefSession>,
    live: Vec<Weak<ad::RefEntry>>,
    target: Option<ad::WindowInfo>,
    input_target: Option<ad::WindowInfo>,
    focused: Option<Arc<ad::RefEntry>>,
    root: Option<Arc<ad::RefEntry>>,
    observation_epoch: Option<Epoch>,
}
impl Worker {
    fn field_value(&self, authority: &Authority, entry: &ad::RefEntry) -> Reply<String> {
        authority.check()?;
        let adapter = guard::Guarded {
            native: &self.native,
            authority,
        };
        let deadline = deadline()?;
        let handle = adapter
            .resolve_element_strict(entry, deadline)
            .map_err(error)?;
        adapter
            .get_live_value(&handle, deadline)
            .map_err(error)?
            .ok_or_else(|| "The field value is unavailable".into())
    }

    fn require_full_selection(&self, authority: &Authority, entry: &ad::RefEntry) -> Reply<()> {
        authority.check()?;
        let adapter = guard::Guarded {
            native: &self.native,
            authority,
        };
        let deadline = deadline()?;
        let handle = adapter
            .resolve_element_strict(entry, deadline)
            .map_err(error)?;
        let value = adapter
            .get_live_value(&handle, deadline)
            .map_err(error)?
            .ok_or(
                "Field value unavailable after select-all [sequence_delivery=DeliveredUnverified]",
            )?;
        let selection = adapter
            .get_text_selection(&handle, deadline)
            .map_err(error)?;
        if !value.is_empty() && selection != Some(0..value.encode_utf16().count()) {
            return Err("Select-all could not be proved on the bound field [sequence_delivery=DeliveredUnverified]; text was not inserted".into());
        }
        authority.check()
    }
    fn snapshot(
        &mut self,
        authority: &Authority,
        app: &str,
        surface: ad::SnapshotSurface,
        max_depth: u8,
    ) -> Reply<ad::snapshot::SnapshotResult> {
        self.snapshot_in_window(authority, app, surface, max_depth, None)
    }
    fn snapshot_in_window(
        &mut self,
        authority: &Authority,
        app: &str,
        surface: ad::SnapshotSurface,
        max_depth: u8,
        window_id: Option<&str>,
    ) -> Reply<ad::snapshot::SnapshotResult> {
        authority.check()?;
        let retained = self.retained.as_ref().map_err(Clone::clone)?;
        self.live.retain(|entry| entry.strong_count() > 0);
        if self.live.len() > 16384 {
            return Err(
                "Too many live native references; release old observations before continuing"
                    .into(),
            );
        }
        let tokens: HashSet<String> = self
            .live
            .iter()
            .filter_map(Weak::upgrade)
            .filter_map(|entry| entry.identity.retained_object.clone())
            .collect();
        retained.retain(&tokens);
        let adapter = guard::Guarded {
            native: &self.native,
            authority,
        };
        let opts = ad::TreeOptions {
            include_bounds: true,
            max_depth,
            surface,
            ..Default::default()
        };
        let windows = adapter
            .list_windows(
                &ad::WindowFilter {
                    app: Some(app.into()),
                    focused_only: false,
                },
                deadline()?,
            )
            .map_err(error)?;
        let selected = match window_id {
            Some(id) => windows
                .into_iter()
                .find(|window| window.id == id)
                .ok_or("The recovery window disappeared")?,
            None => choose_window(windows)?,
        };
        let captures_before = retained.capture_count();
        let snapshot =
            ad::snapshot::build(&adapter, &opts, Some(app), Some(&selected.id), deadline()?)
                .map_err(|err| err.to_string())?;
        if retained.capture_count().saturating_sub(captures_before) > 6000 {
            retained.retain(&tokens);
            return Err("Native retained-object observation limit exceeded".into());
        }
        if !snapshot.complete {
            return Err(
                "The native observation is incomplete; no complete window evidence is available"
                    .into(),
            );
        }
        Ok(snapshot)
    }
    fn action(
        &mut self,
        authority: &Authority,
        entry: &Arc<ad::RefEntry>,
        action: Action,
    ) -> Reply<()> {
        authority.check()?;
        let adapter = guard::Guarded {
            native: &self.native,
            authority,
        };
        // A second headed focus would collapse the selection made by select-all.
        let request = if matches!(action, Action::TypeText(_))
            || matches!(
                entry.source.source_surface,
                ad::SnapshotSurface::Menubar | ad::SnapshotSurface::Menu
            ) {
            ActionRequest::headless(action)
        } else {
            ActionRequest::headed(action)
        };
        ad::ref_action::execute_entry_with_context(
            &adapter,
            entry,
            request.with_timeout_ms(Some(8000)),
            &ad::CommandContext::default(),
        )
        .map_err(error)?;
        Ok(())
    }
    fn look(&mut self, desktop: Desktop, authority: &Authority, app: &str) -> Reply<Look> {
        self.look_in_window(desktop, authority, app, None, None)
    }
    fn look_in_window(
        &mut self,
        desktop: Desktop,
        authority: &Authority,
        app: &str,
        window_id: Option<&str>,
        recovery: Option<(&str, &str)>,
    ) -> Reply<Look> {
        let snapshot =
            self.snapshot_in_window(authority, app, ad::SnapshotSurface::Window, 40, window_id)?;
        let mut look = Look {
            app: snapshot.window.app.clone(),
            window: snapshot.window.title.clone(),
            window_identifier: Some(snapshot.window.id.clone()),
            text: String::new(),
            elements: Vec::new(),
            controls: Vec::new(),
        };
        let aliases = match recovery {
            Some((conversation, composer)) => {
                self.recovery_aliases(authority, &snapshot.window, [conversation, composer])?
            }
            None => HashMap::new(),
        };
        project_with_aliases(
            &snapshot.tree,
            &snapshot.refmap,
            &desktop,
            &mut look,
            &mut self.live,
            &aliases,
        )?;
        if recovery.is_some() {
            return Ok(look);
        }
        self.observation_epoch = Some(desktop.epoch());
        self.root = Some(root_entry(&snapshot));
        self.live.push(Arc::downgrade(
            self.root.as_ref().ok_or("Native root unavailable")?,
        ));
        self.target = Some(snapshot.window);
        self.focused = look
            .controls
            .iter()
            .zip(&look.elements)
            .find(|(_, element)| element.focused)
            .and_then(|_| snapshot_entry_focused(&snapshot.tree, &snapshot.refmap))
            .map(Arc::new);
        if let Some(entry) = &self.focused {
            self.live.push(Arc::downgrade(entry));
        }
        Ok(look)
    }
    fn recovery_aliases(
        &self,
        authority: &Authority,
        window: &ad::WindowInfo,
        identifiers: [&str; 2],
    ) -> Reply<HashMap<String, String>> {
        let adapter = guard::Guarded {
            native: &self.native,
            authority,
        };
        recovery_aliases_with(&adapter, window, identifiers)
    }
}
fn recovery_aliases_with(
    adapter: &dyn ad::PlatformAdapter,
    window: &ad::WindowInfo,
    identifiers: [&str; 2],
) -> Reply<HashMap<String, String>> {
    let mut aliases = HashMap::new();
    for identifier in identifiers {
        let mut query = ad::LocatorQuery::default();
        query.identity.native_id = Some(identifier.into());
        let result = ad::resolve_query(
            adapter,
            &query,
            ad::ObservationRoot::Window(window),
            &ad::LocatorResolveRequest {
                selection: ad::LocatorSelection::All { limit: None },
                deadline: deadline()?,
                max_raw_depth: 50,
                materialization: ad::LocatorMaterialization::SelectedMatches,
                surface: Some(ad::SnapshotSurface::Window),
            },
        )
        .map_err(|error| error.to_string())?;
        if !result.meta.complete || !result.meta.selection_complete || result.matches.len() > 1 {
            return Err("Legacy message identity is incomplete or ambiguous".into());
        }
        if let Some(found) = result.matches.into_iter().next() {
            let token = found
                .into_entry()
                .identity
                .retained_object
                .ok_or("Legacy message identity has no retained object")?;
            aliases.insert(token, identifier.to_string());
        }
    }
    Ok(aliases)
}

fn snapshot_entry_focused(node: &ad::AccessibilityNode, refs: &ad::RefMap) -> Option<ad::RefEntry> {
    if node.presentation.states.iter().any(|s| s == "focused") {
        if let Some(entry) = node.ref_id.as_deref().and_then(|id| refs.get(id)) {
            return Some(entry.clone());
        }
    }
    node.children
        .iter()
        .find_map(|node| snapshot_entry_focused(node, refs))
}
fn frame(bounds: ad::Rect) -> Frame {
    Frame {
        x: bounds.x,
        y: bounds.y,
        w: bounds.width,
        h: bounds.height,
    }
}
fn role(role: &str) -> &str {
    match role {
        "textfield" => "text field",
        "textarea" => "text area",
        "combobox" => "combo box",
        "treeitem" => "row",
        "menuitem" => "menu item",
        "radiobutton" => "radio button",
        "tabitem" => "tab",
        "listitem" => "item",
        "slider" => "slider",
        other => other,
    }
}
fn identity_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => {}
            '\u{00a0}' | '\u{2009}' | '\u{200a}' | '\u{202f}' => out.push(' '),
            c => out.push(c),
        }
    }
    let text = out.trim();
    if text.chars().filter(char::is_ascii_digit).count() >= 7
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | ','))
        && text.contains(' ')
    {
        text.chars().filter(|c| *c != ' ' && *c != ',').collect()
    } else {
        text.to_string()
    }
}
#[cfg(test)]
fn project(
    node: &ad::AccessibilityNode,
    refs: &ad::RefMap,
    desktop: &Desktop,
    look: &mut Look,
    live: &mut Vec<Weak<ad::RefEntry>>,
) -> Reply<()> {
    project_with_aliases(node, refs, desktop, look, live, &HashMap::new())
}
fn project_with_aliases(
    node: &ad::AccessibilityNode,
    refs: &ad::RefMap,
    desktop: &Desktop,
    look: &mut Look,
    live: &mut Vec<Weak<ad::RefEntry>>,
    aliases: &HashMap<String, String>,
) -> Reply<()> {
    if node.subtree_truncated {
        return Err("Native subtree truncated; window evidence refused".into());
    }
    for text in [
        &node.identity.name,
        &node.identity.value,
        &node.identity.description,
    ]
    .into_iter()
    .flatten()
    {
        if look.text.len() + text.len() + 1 > LIMIT_TEXT {
            return Err("Window text exceeds Computer's private evidence limit".into());
        }
        look.text.push_str(text);
        look.text.push('\n');
    }
    let informative = node.identity.name.is_some()
        || node.identity.value.is_some()
        || node.identity.description.is_some()
        || node.identity.native_id.is_some();
    if informative || node.ref_id.is_some() {
        if look.elements.len() >= LIMIT_CONTROLS {
            return Err("Window exceeds Computer's private evidence element limit".into());
        }
        let entry = node
            .ref_id
            .as_deref()
            .and_then(|id| refs.get(id))
            .cloned()
            .map(Arc::new);
        if let Some(entry) = &entry {
            live.push(Arc::downgrade(entry));
        }
        let bounds = node.presentation.bounds.map(frame);
        look.elements.push(Element {
            number: look.elements.len() + 1,
            role: role(&node.role).into(),
            name: node
                .identity
                .name
                .as_deref()
                .map(identity_text)
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect(),
            value: node.identity.value.clone(),
            disabled: node.presentation.states.iter().any(|s| s == "disabled"),
            selected: node.presentation.states.iter().any(|s| s == "selected"),
            identifier: node
                .identity
                .retained_object
                .as_ref()
                .and_then(|token| aliases.get(token))
                .cloned()
                .or_else(|| {
                    node.identity
                        .native_id
                        .as_ref()
                        .map(|id| identity_text(&id.value))
                }),
            description: node
                .identity
                .description
                .as_deref()
                .map(identity_text)
                .map(|text| text.chars().take(1000).collect()),
            focused: node.presentation.states.iter().any(|s| s == "focused"),
            frame: bounds,
        });
        look.controls.push(Arc::new(NativeControl {
            sender: desktop.sender.clone(),
            epoch: desktop.epoch(),
            entry,
            bounds,
        }));
    }
    for child in &node.children {
        project_with_aliases(child, refs, desktop, look, live, aliases)?;
    }
    Ok(())
}
struct NativeControl {
    sender: mpsc::SyncSender<Job>,
    epoch: Epoch,
    entry: Option<Arc<ad::RefEntry>>,
    bounds: Option<Frame>,
}
impl NativeControl {
    fn desktop(&self, permit: Option<OperationPermit>) -> Reply<Desktop> {
        self.epoch.check()?;
        let permit = permit.unwrap_or_else(|| OperationPermit {
            generation: self.epoch.generation.clone(),
            expected: self.epoch.expected,
            active: Arc::new(AtomicBool::new(true)),
        });
        Ok(Desktop {
            sender: self.sender.clone(),
            permit,
        })
    }
    fn execute(&self, permit: Option<OperationPermit>, actions: Vec<Action>) -> Reply<()> {
        let entry = self.entry.clone().ok_or(
            "This informative element has no actionable native reference [delivery=NotDelivered]",
        )?;
        self.desktop(permit)?
            .act(entry, self.epoch.clone(), actions, None)
    }
    fn replace(&self, permit: Option<OperationPermit>, text: &str) -> Reply<()> {
        let entry = self
            .entry
            .clone()
            .ok_or("This element has no native field reference")?;
        self.desktop(permit)?.act(
            entry,
            self.epoch.clone(),
            replacement(text)?,
            Some(text.into()),
        )
    }
}
impl Control for NativeControl {
    fn press(&self) -> Reply<()> {
        self.execute(None, vec![Action::Click])
    }
    fn set_text(&self, text: &str) -> Reply<()> {
        self.replace(None, text)
    }
    fn frame(&self) -> Option<Frame> {
        self.bounds
    }
    fn click(&self, how: Pointer, screen: &dyn Screen, _app: &str) -> Reply<()> {
        let permit = screen
            .operation_permit()
            .ok_or("No current Computer operation authorizes this click")?;
        self.execute(Some(permit), vec![pointer_action(how)])
    }
    fn replace_text(&self, text: &str, screen: &dyn Screen, _app: &str) -> Reply<()> {
        let permit = screen
            .operation_permit()
            .ok_or("No current Computer operation authorizes this replacement")?;
        self.replace(Some(permit), text)
    }
}
fn pointer_action(how: Pointer) -> Action {
    match how {
        Pointer::Click => Action::Click,
        Pointer::Double => Action::DoubleClick,
        Pointer::Right => Action::RightClick,
    }
}
fn combo(keys: &str) -> Reply<ad::KeyCombo> {
    let mut parts: Vec<&str> = keys.split('+').collect();
    let key = parts
        .pop()
        .filter(|key| !key.is_empty())
        .ok_or("A key is required")?
        .to_string();
    let modifiers = parts
        .into_iter()
        .map(|part| match part.to_lowercase().as_str() {
            "cmd" | "meta" => Ok(ad::Modifier::Meta),
            "ctrl" | "control" => Ok(ad::Modifier::Ctrl),
            "alt" | "option" => Ok(ad::Modifier::Alt),
            "shift" => Ok(ad::Modifier::Shift),
            _ => Err(format!("Unknown key modifier: {part}")),
        })
        .collect::<Reply<_>>()?;
    Ok(ad::KeyCombo { key, modifiers })
}
fn audit_recovery_windows(
    native: &MacOSAdapter,
    app: &str,
    windows: &[ad::WindowInfo],
) -> Reply<()> {
    let apps = native
        .list_apps_scoped(app, None, deadline()?)
        .map_err(error)?;
    let mut expected = HashSet::new();
    for app in apps {
        let pid =
            i32::try_from(app.pid.get()).map_err(|_| "Recovery process identity is invalid")?;
        for id in crate::macos::complete_window_ids(pid)? {
            expected.insert((app.pid.get(), id));
        }
    }
    let observed: HashSet<_> = windows
        .iter()
        .map(|window| (window.pid.get(), window.id.clone()))
        .collect();
    require_complete_inventory(&expected, &observed, windows.len())
}
fn require_complete_inventory(
    expected: &HashSet<(u32, String)>,
    observed: &HashSet<(u32, String)>,
    count: usize,
) -> Reply<()> {
    if expected.is_empty() || expected != observed || observed.len() != count {
        return Err(
            "ARK cannot verify every message window. Choose Review after checking the app yourself."
                .into(),
        );
    }
    Ok(())
}
fn require_input_window(target: &ad::WindowInfo, expected: &str) -> Reply<()> {
    if !expected.is_empty() && target.id != expected {
        return Err("The approved input window differs from the proved message window [delivery=NotDelivered]".into());
    }
    Ok(())
}
fn replacement(text: &str) -> Reply<Vec<Action>> {
    Ok(vec![
        Action::SetFocus,
        Action::PressKey(combo("cmd+a")?),
        Action::TypeText(text.into()),
    ])
}
impl Screen for Desktop {
    fn scoped(
        &self,
        generation: Arc<AtomicU64>,
        expected: u64,
        active: Arc<AtomicBool>,
    ) -> Option<Arc<dyn Screen>> {
        Some(Arc::new(Self {
            sender: self.sender.clone(),
            permit: OperationPermit {
                generation,
                expected,
                active,
            },
        }))
    }
    fn operation_permit(&self) -> Option<OperationPermit> {
        Some(self.permit.clone())
    }
    fn access(&self) -> Access {
        crate::macos::access()
    }
    fn request_access(&self) {
        crate::macos::request_access()
    }
    fn apps(&self) -> Reply<Vec<App>> {
        crate::macos::apps()
    }
    fn window_metadata(&self, app: &str) -> Reply<WindowMetadata> {
        crate::macos::window_metadata(app)
    }
    fn look(&self, app: &str) -> Reply<Look> {
        let app = app.to_owned();
        let desktop = self.clone();
        self.call(move |worker, authority| worker.look(desktop, authority, &app))
    }
    fn recovery_looks(
        &self,
        app: &str,
        _original_window: &str,
        conversation: &str,
        composer: &str,
    ) -> Reply<Vec<Look>> {
        let app = app.to_owned();
        let conversation = conversation.to_owned();
        let composer = composer.to_owned();
        let desktop = self.clone();
        self.call(move |worker, authority| {
            let filter = ad::WindowFilter {
                app: Some(app.clone()),
                focused_only: false,
            };
            let windows = worker
                .native
                .list_windows(&filter, deadline()?)
                .map_err(error)?;
            if windows.is_empty() {
                return Err("Open the message app before checking its draft".into());
            }
            audit_recovery_windows(&worker.native, &app, &windows)?;
            let looks: Reply<Vec<Look>> = windows
                .iter()
                .map(|window| {
                    worker.look_in_window(
                        desktop.clone(),
                        authority,
                        &app,
                        Some(&window.id),
                        Some((&conversation, &composer)),
                    )
                })
                .collect();
            let looks = looks?;
            let after = worker
                .native
                .list_windows(&filter, deadline()?)
                .map_err(error)?;
            audit_recovery_windows(&worker.native, &app, &after)?;
            if !same_window_inventory(&windows, &after) {
                return Err(
                    "The message app's windows changed during recovery; check again".into(),
                );
            }
            Ok(looks)
        })
    }
    fn open(&self, app: &str) -> Reply<()> {
        let app = app.to_owned();
        self.call(move |worker, authority| {
            let adapter = guard::Guarded {
                native: &worker.native,
                authority,
            };
            let lease = adapter
                .acquire_interaction_lease(deadline()?)
                .map_err(error)?;
            let filter = ad::WindowFilter {
                app: Some(app.clone()),
                focused_only: false,
            };
            let mut windows = adapter
                .list_windows(&filter, lease.deadline())
                .map_err(error)?;
            if windows.is_empty() {
                adapter
                    .launch_app(
                        &app,
                        &ad::system::launch_options::LaunchOptions {
                            activate: true,
                            ..Default::default()
                        },
                        &lease,
                    )
                    .map_err(error)?;
                windows = adapter
                    .list_windows(&filter, lease.deadline())
                    .map_err(error)?;
            }
            let window = choose_window(windows)?;
            adapter.focus_window(&window, &lease).map_err(error)?;
            if worker.target.as_ref().is_some_and(|previous| {
                previous.id != window.id || previous.process_instance != window.process_instance
            }) {
                worker.focused = None;
                worker.root = None;
                worker.target = None;
                worker.observation_epoch = None;
            }
            worker.input_target = Some(window.clone());
            Ok(())
        })
    }
    fn keys(&self, keys: &str) -> Reply<()> {
        self.keys_in_window(keys, "")
    }
    fn keys_in_window(&self, keys: &str, expected_window: &str) -> Reply<()> {
        let combo = combo(keys)?;
        let expected_window = expected_window.to_owned();
        self.call(move |worker, authority| {
            let target = worker
                .input_target
                .as_ref()
                .ok_or("Open the target app before sending keys [delivery=NotDelivered]")?;
            require_input_window(target, &expected_window)?;
            let adapter = guard::Guarded {
                native: &worker.native,
                authority,
            };
            let lease = adapter
                .acquire_interaction_lease(deadline()?)
                .map_err(error)?;
            let window = adapter
                .resolve_window_strict(target, lease.deadline())
                .map_err(error)?;
            adapter
                .press_key_for_window(&window, &combo, ad::InteractionPolicy::headed(), &lease)
                .map_err(error)?;
            Ok(())
        })
    }
    fn type_text(&self, text: &str) -> Reply<()> {
        let text = text.to_owned();
        self.call(move |worker, authority| {
            let entry = worker.focused.clone().ok_or(
                "Take a fresh look and focus a native field before typing [delivery=NotDelivered]",
            )?;
            let target = worker.input_target.as_ref().ok_or("Open the approved app before typing [delivery=NotDelivered]")?;
            if entry.source.source_window_id.as_deref() != Some(target.id.as_str())
                || entry.process.pid != target.pid
                || entry.process.process_instance != target.process_instance
            {
                return Err("The focused field does not belong to the approved input window [delivery=NotDelivered]".into());
            }
            worker.action(authority, &entry, Action::TypeText(text))
        })
    }
    fn since_input(&self) -> Option<Duration> {
        crate::macos::since_input()
    }
    fn menu(&self, app: &str, path: &[String]) -> Reply<Menu> {
        let app = app.to_owned();
        let path = path.to_vec();
        self.call(move |worker, authority| {
            let mut snapshot =
                worker.snapshot(authority, &app, ad::SnapshotSurface::Menubar, 40)?;
            let mut chosen = Vec::new();
            for name in &path {
                let scope = menu_scope(&snapshot.tree, &chosen)?;
                let node = unique_menu(scope, name)?;
                let entry = Arc::new(
                    node.ref_id
                        .as_deref()
                        .and_then(|id| snapshot.refmap.get(id))
                        .cloned()
                        .ok_or("Menu item has no strict native reference")?,
                );
                chosen.push(node.identity.name.clone().unwrap_or_default());
                let submenu = node.children.iter().any(|child| child.role == "menu")
                    || node
                        .presentation
                        .available_actions
                        .iter()
                        .any(|a| a.to_lowercase().contains("showmenu"));
                if !submenu && chosen.len() != path.len() {
                    return Err("Menu path continues past a command [delivery=NotDelivered]".into());
                }
                worker.action(authority, &entry, Action::Click)?;
                if !submenu {
                    return Ok(Menu::Chosen(chosen));
                }
                snapshot = worker.snapshot(authority, &app, ad::SnapshotSurface::Menubar, 40)?;
            }
            let root = menu_scope(&snapshot.tree, &chosen)?;
            let mut items = Vec::new();
            menu_children(root, &mut items);
            if items.len() > LIMIT_CONTROLS {
                return Err("Menu exceeds Computer's element limit".into());
            }
            Ok(Menu::Items(items))
        })
    }
    fn window_frame(&self, app: &str) -> Reply<Frame> {
        let app = app.to_owned();
        self.call(move |worker, authority| {
            let snapshot = worker.snapshot(authority, &app, ad::SnapshotSurface::Window, 40)?;
            let bounds = snapshot
                .window
                .bounds
                .ok_or("Native window has no bounds")?;
            Ok(frame(bounds))
        })
    }
    fn scroll(&self, at: Frame, pages: f64) -> Reply<()> {
        if !pages.is_finite() || pages.abs() > 100.0 {
            return Err("Scroll amount is invalid".into());
        }
        self.call(move |worker, authority| {
            let (x, y) = at.center();
            worker.pointer(
                authority,
                x,
                y,
                ad::MouseEventKind::Wheel {
                    delta_x: 0.0,
                    delta_y: pages * 30.0,
                },
                ad::MouseButton::Left,
            )
        })
    }
    fn pointer(&self, x: f64, y: f64, how: Pointer) -> Reply<()> {
        self.call(move |worker, authority| {
            worker.pointer(
                authority,
                x,
                y,
                ad::MouseEventKind::Click {
                    count: if how == Pointer::Double { 2 } else { 1 },
                },
                if how == Pointer::Right {
                    ad::MouseButton::Right
                } else {
                    ad::MouseButton::Left
                },
            )
        })
    }
    fn capture(&self, app: &str, max: u32) -> Reply<Shot> {
        if max == 0 || max > 4096 {
            return Err("Picture limit must be between 1 and 4096 pixels".into());
        }
        let app = app.to_owned();
        self.call(move |worker, authority| {
            let snapshot = worker.snapshot(authority, &app, ad::SnapshotSurface::Window, 40)?;
            worker.observation_epoch = Some(Epoch {
                generation: authority.permit.generation.clone(),
                expected: authority.permit.expected,
            });
            worker.root = Some(root_entry(&snapshot));
            worker.live.push(Arc::downgrade(
                worker.root.as_ref().ok_or("Native root unavailable")?,
            ));
            let window = snapshot.window;
            worker.focused = None;
            worker.target = Some(window.clone());
            let adapter = guard::Guarded {
                native: &worker.native,
                authority,
            };
            let bounds = window.bounds.ok_or("Native window has no bounds")?;
            let image = adapter
                .screenshot(ad::ScreenshotTarget::ExactWindow(window), deadline()?)
                .map_err(error)?;
            if image.data.len() > ad::MAX_PNG_INPUT_BYTES {
                return Err("Native picture exceeds input limit".into());
            }
            if ad::parse_png_dimensions(&image.data).is_none() {
                return Err("Native picture has invalid or excessive PNG geometry".into());
            }
            let decoded = image::load_from_memory_with_format(&image.data, image::ImageFormat::Png)
                .map_err(|err| format!("Native picture decode failed: {err}"))?;
            let resized = decoded.thumbnail(max, max);
            let (width, height) = (resized.width(), resized.height());
            let mut png = std::io::Cursor::new(Vec::new());
            resized
                .write_to(&mut png, image::ImageFormat::Png)
                .map_err(|err| format!("Native picture encode failed: {err}"))?;
            Ok(Shot {
                png: png.into_inner(),
                width,
                height,
                frame: frame(bounds),
            })
        })
    }
}
fn choose_window(mut windows: Vec<ad::WindowInfo>) -> Reply<ad::WindowInfo> {
    if windows.len() == 1 {
        return Ok(windows.remove(0));
    }
    let mut focused: Vec<_> = windows
        .into_iter()
        .filter(|window| window.state.is_focused)
        .collect();
    if focused.len() == 1 {
        return Ok(focused.remove(0));
    }
    Err("No unique focused native window; choose the target window first".into())
}
fn same_window_inventory(before: &[ad::WindowInfo], after: &[ad::WindowInfo]) -> bool {
    let identities = |windows: &[ad::WindowInfo]| {
        windows
            .iter()
            .map(|window| {
                (
                    window.pid,
                    window.process_instance.clone(),
                    window.id.clone(),
                )
            })
            .collect::<HashSet<_>>()
    };
    let expected = identities(before);
    let actual = identities(after);
    expected.len() == before.len() && actual.len() == after.len() && expected == actual
}
fn unique_menu<'a>(
    root: &'a ad::AccessibilityNode,
    name: &str,
) -> Reply<&'a ad::AccessibilityNode> {
    fn collect<'a>(
        node: &'a ad::AccessibilityNode,
        name: &str,
        matches: &mut Vec<&'a ad::AccessibilityNode>,
    ) {
        if matches!(node.role.as_str(), "menuitem" | "menubaritem") {
            if node
                .identity
                .name
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(name))
            {
                matches.push(node);
            }
            return;
        }
        for child in &node.children {
            collect(child, name, matches);
        }
    }
    let mut matches = Vec::new();
    for child in &root.children {
        collect(child, name, &mut matches);
    }
    if matches.len() != 1 {
        return Err(format!(
            "Menu item '{name}' is missing or ambiguous [delivery=NotDelivered]"
        ));
    }
    Ok(matches[0])
}
fn menu_scope<'a>(
    root: &'a ad::AccessibilityNode,
    path: &[String],
) -> Reply<&'a ad::AccessibilityNode> {
    let mut scope = root;
    for name in path {
        scope = unique_menu(scope, name)?;
    }
    Ok(scope)
}
fn menu_children(node: &ad::AccessibilityNode, items: &mut Vec<MenuItem>) {
    for child in &node.children {
        if matches!(child.role.as_str(), "menuitem" | "menubaritem") {
            items.push(MenuItem {
                name: child.identity.name.clone().unwrap_or_default(),
                disabled: child.presentation.states.iter().any(|s| s == "disabled"),
                submenu: child.children.iter().any(|node| node.role == "menu"),
                keys: None,
            });
        } else {
            menu_children(child, items);
        }
    }
}
pub fn tree(app: &str, max_depth: usize) -> Reply<String> {
    let app = app.to_owned();
    let depth = u8::try_from(max_depth.min(40)).map_err(|err| err.to_string())?;
    Desktop::new().call(move |worker, authority| {
        let snapshot = worker.snapshot(authority, &app, ad::SnapshotSurface::Window, depth)?;
        serde_json::to_string_pretty(&snapshot.tree).map_err(|err| err.to_string())
    })
}
fn root_entry(snapshot: &ad::snapshot::SnapshotResult) -> Arc<ad::RefEntry> {
    let node = &snapshot.tree;
    let bounds = node.presentation.bounds;
    Arc::new(ad::RefEntry {
        process: ad::RefProcess {
            pid: snapshot.window.pid,
            process_instance: snapshot.window.process_instance.clone(),
        },
        identity: ad::RefEntryIdentity {
            retained_object: node.identity.retained_object.clone(),
            role: node.role.clone(),
            name: node.identity.name.clone(),
            value: node.identity.value.clone(),
            description: node.identity.description.clone(),
            native_id: node.identity.native_id.clone(),
        },
        geometry: ad::RefGeometry {
            bounds,
            bounds_hash: bounds.and_then(|bounds| bounds.bounds_hash()),
            bounds_space: None,
        },
        capabilities: ad::RefCapabilities {
            states: node.presentation.states.clone(),
            available_actions: node.presentation.available_actions.clone(),
        },
        source: ad::RefSource {
            source_app: Some(snapshot.window.app.clone()),
            source_window_id: Some(snapshot.window.id.clone()),
            source_window_title: Some(snapshot.window.title.clone()),
            source_window_bounds_hash: snapshot
                .window
                .bounds
                .and_then(|bounds| bounds.bounds_hash()),
            source_surface: ad::SnapshotSurface::Window,
        },
        scope: ad::RefScope {
            root_ref: None,
            path_is_absolute: false,
            path: Default::default(),
        },
    })
}
impl Worker {
    fn pointer(
        &mut self,
        authority: &Authority,
        x: f64,
        y: f64,
        kind: ad::MouseEventKind,
        button: ad::MouseButton,
    ) -> Reply<()> {
        let point = ad::Point { x, y };
        point.validate().map_err(error)?;
        self.observation_epoch
            .as_ref()
            .ok_or("Take a fresh native observation before pointer input")?
            .check()?;
        let target = self
            .target
            .as_ref()
            .ok_or("Take a fresh look or picture before pointer input [delivery=NotDelivered]")?;
        let root = self
            .root
            .as_ref()
            .ok_or("Take a fresh look or picture before pointer input [delivery=NotDelivered]")?;
        let adapter = guard::Guarded {
            native: &self.native,
            authority,
        };
        let lease = adapter
            .acquire_interaction_lease(deadline()?)
            .map_err(error)?;
        let window = adapter
            .resolve_window_strict(target, lease.deadline())
            .map_err(error)?;
        if window.bounds != target.bounds {
            return Err("Window moved since observation [delivery=NotDelivered]".into());
        }
        let bounds = window
            .bounds
            .ok_or("Window geometry unavailable [delivery=NotDelivered]")?;
        if x < bounds.x
            || y < bounds.y
            || x >= bounds.x + bounds.width
            || y >= bounds.y + bounds.height
        {
            return Err("Point is outside the observed window [delivery=NotDelivered]".into());
        }
        let focused = adapter
            .focused_window(lease.deadline())
            .map_err(error)?
            .ok_or("No focused native window [delivery=NotDelivered]")?;
        if focused.id != window.id
            || focused.pid != window.pid
            || focused.process_instance != window.process_instance
        {
            return Err("Observed window no longer has focus [delivery=NotDelivered]".into());
        }
        let handle = adapter
            .resolve_element_strict(root, lease.deadline())
            .map_err(error)?;
        if adapter
            .hit_test(&handle, point.clone(), lease.deadline())
            .map_err(error)?
            != ad::HitTestResult::ReachesTarget
        {
            return Err(
                "Exact window pointer receiver could not be proved [delivery=NotDelivered]".into(),
            );
        }
        adapter
            .mouse_event(
                ad::MouseEvent {
                    kind,
                    point,
                    button,
                    modifiers: Vec::new(),
                },
                &lease,
            )
            .map_err(error)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_inventory_uses_exact_identity_and_refuses_changed_or_duplicate_windows() {
        let first: ad::WindowInfo = serde_json::from_value(serde_json::json!({"id":"w-1","title":"Same title","app_name":"Fixture","pid":1,"process_instance":"generation-1","is_focused":true})).unwrap();
        let mut recreated = first.clone();
        recreated.id = "w-2".into();
        assert!(same_window_inventory(
            std::slice::from_ref(&first),
            std::slice::from_ref(&first)
        ));
        assert!(!same_window_inventory(
            std::slice::from_ref(&first),
            std::slice::from_ref(&recreated)
        ));
        assert!(!same_window_inventory(
            std::slice::from_ref(&first),
            &[first.clone(), first.clone()]
        ));
        recreated.id = first.id.clone();
        recreated.process_instance = Some("generation-2".into());
        assert!(!same_window_inventory(
            std::slice::from_ref(&first),
            std::slice::from_ref(&recreated)
        ));
    }
    #[test]
    fn send_window_binding_and_hidden_window_coverage_fail_closed() {
        let window: ad::WindowInfo = serde_json::from_value(serde_json::json!({"id":"w-1","title":"Same title","app_name":"Fixture","pid":1,"process_instance":"generation-1","is_focused":true})).unwrap();
        assert!(require_input_window(&window, "w-1").is_ok());
        assert!(require_input_window(&window, "w-2")
            .unwrap_err()
            .contains("NotDelivered"));
        let visible = HashSet::from([(1, "w-1".into())]);
        let complete = HashSet::from([(1, "w-1".into()), (1, "w-2".into())]);
        assert!(require_complete_inventory(&complete, &visible, 1).is_err());
        assert!(require_complete_inventory(&complete, &complete, 2).is_ok());
        assert!(require_complete_inventory(&complete, &complete, 3).is_err());
    }
    #[test]
    fn legacy_names_and_ax_aliases_survive_without_cleaning_field_values() {
        assert_eq!(
            identity_text("\u{200e}+ 5 4,9,3 4 5 6 7\u{200f}"),
            "+54934567"
        );
        let (sender, _receiver) = mpsc::sync_channel(1);
        let desktop = Desktop {
            sender,
            permit: permit(),
        };
        let mut source = node();
        source.identity.name = Some("\u{200e}Composer\u{00a0}".into());
        source.identity.value = Some("  raw\n\u{200e}".into());
        source.identity.retained_object = Some("retained-1".into());
        source.identity.native_id = Some(ad::ElementIdentifier {
            kind: ad::IdentifierKind::AxDomIdentifier,
            value: "dom-composer".into(),
        });
        let mut look = empty_look();
        project_with_aliases(
            &source,
            &ad::RefMap::new(),
            &desktop,
            &mut look,
            &mut Vec::new(),
            &HashMap::from([("retained-1".into(), "_NS:old-composer".into())]),
        )
        .unwrap();
        assert_eq!(look.elements[0].name, "Composer");
        assert_eq!(
            look.elements[0].identifier.as_deref(),
            Some("_NS:old-composer")
        );
        assert_eq!(look.elements[0].value.as_deref(), Some("  raw\n\u{200e}"));
    }
    #[test]
    fn upstream_alias_resolution_keeps_ax_identity_when_dom_is_preferred() {
        struct DualIdentifiers;
        impl ObservationOps for DualIdentifiers {
            fn observe_tree(
                &self,
                root: ad::ObservationRoot<'_>,
                _request: &ad::ObservationRequest,
            ) -> Result<ad::ObservedTree, ad::AdapterError> {
                use ad::LocatorField::{Absent, Known};
                let evidence = ad::LocatorEvidence {
                    role: Known("textfield".into()),
                    name: Known("Message".into()),
                    description: Absent,
                    value: Known(String::new()),
                    states: Known(vec!["focused".into()]),
                    identifiers: ad::IdentifierEvidence::typed(
                        [
                            ad::ElementIdentifier {
                                kind: ad::IdentifierKind::AxIdentifier,
                                value: "_NS:legacy-field".into(),
                            },
                            ad::ElementIdentifier {
                                kind: ad::IdentifierKind::AxDomIdentifier,
                                value: "dom-field".into(),
                            },
                        ],
                        Some(1),
                        true,
                    )
                    .with_retained_object("same-native-object".into()),
                    ref_evidence: ad::LocatorRefEvidence {
                        bounds: Absent,
                        window_bounds: Absent,
                        available_actions: Known(vec!["AXSetValue".into()]),
                        descriptors: Default::default(),
                    },
                };
                ad::ObservedTree::from_roots(
                    vec![ad::ObservedSubtree::new(evidence, vec![], true, None)],
                    ad::ObservationSource::from_root(&root, ad::SnapshotSurface::Window),
                    ad::LocatorStats::default(),
                    true,
                )
            }
        }
        impl ad::ActionOps for DualIdentifiers {}
        impl InputOps for DualIdentifiers {}
        impl SystemOps for DualIdentifiers {}
        let window: ad::WindowInfo = serde_json::from_value(serde_json::json!({"id":"w-1","title":"Fixture","app_name":"Fixture","pid":1,"process_instance":"generation-1","is_focused":true})).unwrap();
        let aliases = recovery_aliases_with(
            &DualIdentifiers,
            &window,
            ["_NS:legacy-field", "missing-conversation"],
        )
        .unwrap();
        assert_eq!(
            aliases.get("same-native-object").map(String::as_str),
            Some("_NS:legacy-field")
        );
    }
    #[test]
    fn private_evidence_keeps_large_transcripts_complete() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let desktop = Desktop {
            sender,
            permit: permit(),
        };
        let mut source = node();
        source.children = (0..220).map(|_| node()).collect();
        let mut look = empty_look();
        project(
            &source,
            &ad::RefMap::new(),
            &desktop,
            &mut look,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(look.elements.len(), 221);
    }
    fn permit() -> OperationPermit {
        OperationPermit {
            generation: Arc::new(AtomicU64::new(7)),
            expected: 7,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
    fn authority() -> Authority {
        Authority {
            permit: permit(),
            timeout_active: Arc::new(AtomicBool::new(true)),
            snapshot_epoch: None,
        }
    }
    #[test]
    fn dispatch_checks_caller_lifetime_timeout_and_epoch() {
        let authority = authority();
        assert!(authority.check().is_ok());
        authority.permit.active.store(false, Ordering::Release);
        assert!(authority.check().is_err());
        authority.permit.active.store(true, Ordering::Release);
        authority.timeout_active.store(false, Ordering::Release);
        assert!(authority.check().is_err());
        authority.timeout_active.store(true, Ordering::Release);
        authority.permit.generation.fetch_add(1, Ordering::AcqRel);
        assert!(authority.check().is_err());
    }
    #[test]
    fn old_snapshot_epoch_cannot_borrow_new_operation_authority() {
        let mut authority = authority();
        let generation = Arc::new(AtomicU64::new(1));
        authority.snapshot_epoch = Some(Epoch {
            generation: generation.clone(),
            expected: 1,
        });
        assert!(authority.check().is_ok());
        generation.store(2, Ordering::Release);
        assert!(authority.check().is_err());
    }
    #[test]
    fn guarded_mutation_refuses_before_native_dispatch_after_wait() {
        let authority = authority();
        let native = MacOSAdapter::new();
        let adapter = guard::Guarded {
            native: &native,
            authority: &authority,
        };
        let lease = ad::InteractionLease::guarded(ad::Deadline::after(1000).unwrap(), ()).unwrap();
        authority.permit.generation.fetch_add(1, Ordering::AcqRel);
        let result = ad::ActionOps::execute_action(
            &adapter,
            &ad::NativeHandle::null(),
            ActionRequest::headed(Action::Click),
            &lease,
        );
        let err = result.unwrap_err();
        assert_eq!(err.disposition, ad::DeliverySemantics::NotDelivered);
        assert_eq!(err.code, ad::ErrorCode::PolicyDenied);
    }
    fn node() -> ad::AccessibilityNode {
        serde_json::from_value(serde_json::json!({ "role": "textfield", "name": "Composer", "value": "draft", "description": "Recipient", "native_id": { "kind": "ax_identifier", "value": "stable-composer" }, "states": ["focused", "selected", "disabled"], "bounds": { "x": 10, "y": 20, "width": 100, "height": 30 } })).unwrap()
    }
    fn empty_look() -> Look {
        Look {
            app: "app".into(),
            window: "window".into(),
            window_identifier: Some("w-1".into()),
            text: String::new(),
            elements: Vec::new(),
            controls: Vec::new(),
        }
    }
    #[test]
    fn informative_nodes_preserve_message_evidence_and_refuse_actions() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let desktop = Desktop {
            sender,
            permit: permit(),
        };
        let mut look = empty_look();
        project(
            &node(),
            &ad::RefMap::new(),
            &desktop,
            &mut look,
            &mut Vec::new(),
        )
        .unwrap();
        let element = &look.elements[0];
        assert_eq!(element.role, "text field");
        assert_eq!(element.name, "Composer");
        assert_eq!(element.value.as_deref(), Some("draft"));
        assert_eq!(element.description.as_deref(), Some("Recipient"));
        assert_eq!(element.identifier.as_deref(), Some("stable-composer"));
        assert!(element.disabled && element.selected && element.focused);
        assert_eq!(
            element.frame,
            Some(Frame {
                x: 10.0,
                y: 20.0,
                w: 100.0,
                h: 30.0
            })
        );
        assert_eq!(look.text, "Composer\ndraft\nRecipient\n");
        assert!(look.controls[0]
            .press()
            .unwrap_err()
            .contains("informative"));
    }
    #[test]
    fn projection_overflow_and_incomplete_subtree_are_explicit_failures() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let desktop = Desktop {
            sender,
            permit: permit(),
        };
        let mut look = empty_look();
        look.text = "a".repeat(LIMIT_TEXT);
        assert!(project(
            &node(),
            &ad::RefMap::new(),
            &desktop,
            &mut look,
            &mut Vec::new()
        )
        .unwrap_err()
        .contains("limit"));
        let mut incomplete = node();
        incomplete.subtree_truncated = true;
        assert!(project(
            &incomplete,
            &ad::RefMap::new(),
            &desktop,
            &mut empty_look(),
            &mut Vec::new()
        )
        .unwrap_err()
        .contains("truncated"));
    }
    #[test]
    fn duplicate_menu_labels_are_scoped_to_the_selected_parent() {
        let menu: ad::AccessibilityNode = serde_json::from_value(serde_json::json!({ "role": "menu", "children": [
            { "role": "menuitem", "name": "File", "children": [{ "role": "menu", "children": [{ "role": "menuitem", "name": "Open", "value": "file" }] }] },
            { "role": "menuitem", "name": "Edit", "children": [{ "role": "menu", "children": [{ "role": "menuitem", "name": "Open", "value": "edit" }] }] }
        ] })).unwrap();
        assert!(unique_menu(&menu, "Open").is_err());
        let selected = menu_scope(&menu, &["File".into(), "Open".into()]).unwrap();
        assert_eq!(selected.identity.value.as_deref(), Some("file"));
    }
    #[test]
    fn replacement_uses_same_target_steps_without_submit_or_fallback() {
        let actions = replacement("new text").unwrap();
        assert!(matches!(actions[0], Action::SetFocus));
        assert!(
            matches!(&actions[1], Action::PressKey(combo) if combo.key == "a" && combo.modifiers == [ad::Modifier::Meta])
        );
        assert!(matches!(&actions[2], Action::TypeText(text) if text == "new text"));
        assert_eq!(actions.len(), 3);
        assert!(matches!(&replacement("").unwrap()[2], Action::TypeText(text) if text.is_empty()));
    }
}
