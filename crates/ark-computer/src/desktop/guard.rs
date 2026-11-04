use super::Authority;
use agent_desktop_core::*;
use agent_desktop_macos::MacOSAdapter;

pub(super) struct Guarded<'a> {
    pub native: &'a MacOSAdapter,
    pub authority: &'a Authority,
}

impl ObservationOps for Guarded<'_> {
    fn is_nonspatial_target(&self, _handle: &NativeHandle) -> bool {
        self.native.is_nonspatial_target(_handle)
    }
    fn observe_tree(
        &self,
        _root: ObservationRoot<'_>,
        _request: &ObservationRequest,
    ) -> Result<ObservedTree, AdapterError> {
        self.native.observe_tree(_root, _request)
    }
    fn list_windows(
        &self,
        _filter: &WindowFilter,
        _deadline: Deadline,
    ) -> Result<Vec<WindowInfo>, AdapterError> {
        self.native.list_windows(_filter, _deadline)
    }
    fn list_apps(&self, _deadline: Deadline) -> Result<Vec<AppInfo>, AdapterError> {
        self.native.list_apps(_deadline)
    }
    fn list_apps_inventory(
        &self,
        deadline: Deadline,
    ) -> Result<agent_desktop_core::AppInventory, AdapterError> {
        self.native.list_apps_inventory(deadline)
    }
    fn list_apps_scoped(
        &self,
        name: &str,
        bundle_id: Option<&str>,
        deadline: Deadline,
    ) -> Result<Vec<AppInfo>, AdapterError> {
        self.native.list_apps_scoped(name, bundle_id, deadline)
    }
    fn query_app_inventory(
        &self,
        query: agent_desktop_core::AppInventoryQuery<'_>,
        deadline: Deadline,
    ) -> Result<agent_desktop_core::AppInventory, AdapterError> {
        self.native.query_app_inventory(query, deadline)
    }
    fn get_tree(
        &self,
        _win: &WindowInfo,
        _opts: &TreeOptions,
        _deadline: Deadline,
    ) -> Result<AccessibilityNode, AdapterError> {
        self.native.get_tree(_win, _opts, _deadline)
    }
    fn resolve_element_strict(
        &self,
        _entry: &RefEntry,
        _deadline: Deadline,
    ) -> Result<NativeHandle, AdapterError> {
        self.native.resolve_element_strict(_entry, _deadline)
    }
    fn resolve_locator_anchor(
        &self,
        _entry: &RefEntry,
        _deadline: Deadline,
    ) -> Result<NativeHandle, AdapterError> {
        self.native.resolve_locator_anchor(_entry, _deadline)
    }
    fn list_surfaces(
        &self,
        _process: agent_desktop_core::ProcessIdentity,
        _deadline: Deadline,
    ) -> Result<Vec<SurfaceInfo>, AdapterError> {
        self.native.list_surfaces(_process, _deadline)
    }
    fn resolve_shell_surface(
        &self,
        _surface: SnapshotSurface,
        _deadline: Deadline,
    ) -> Result<WindowInfo, AdapterError> {
        self.native.resolve_shell_surface(_surface, _deadline)
    }
    fn get_live_value(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Result<Option<String>, AdapterError> {
        self.native.get_live_value(_handle, _deadline)
    }
    fn get_text_selection(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Result<Option<std::ops::Range<usize>>, AdapterError> {
        self.native.get_text_selection(_handle, _deadline)
    }
    fn get_live_state(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Result<Option<ElementState>, AdapterError> {
        self.native.get_live_state(_handle, _deadline)
    }
    fn get_live_actions(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Result<Option<Vec<String>>, AdapterError> {
        self.native.get_live_actions(_handle, _deadline)
    }
    fn get_live_element(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Result<LiveElement, AdapterError> {
        self.native.get_live_element(_handle, _deadline)
    }
    fn get_element_bounds(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Result<Option<Rect>, AdapterError> {
        self.native.get_element_bounds(_handle, _deadline)
    }
    fn observe_pointer(
        &self,
        handle: &NativeHandle,
        deadline: Deadline,
    ) -> Result<agent_desktop_core::PointerObservation, AdapterError> {
        self.native.observe_pointer(handle, deadline)
    }
    fn pointer_position_refusal(
        &self,
        _handle: &NativeHandle,
        _deadline: Deadline,
    ) -> Option<AdapterError> {
        self.native.pointer_position_refusal(_handle, _deadline)
    }
    fn hit_test(
        &self,
        handle: &NativeHandle,
        point: agent_desktop_core::Point,
        _deadline: Deadline,
    ) -> Result<agent_desktop_core::adapter::hit_test::HitTestResult, AdapterError> {
        self.native.hit_test(handle, point, _deadline)
    }
    fn confirm_pointer_receiver(
        &self,
        entry: &RefEntry,
        point: agent_desktop_core::Point,
        deadline: Deadline,
    ) -> Result<(), AdapterError> {
        self.native.confirm_pointer_receiver(entry, point, deadline)
    }
}

impl ActionOps for Guarded<'_> {
    fn execute_action(
        &self,
        _handle: &NativeHandle,
        _request: ActionRequest,
        _lease: &InteractionLease,
    ) -> Result<ActionResult, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.execute_action(_handle, _request, _lease)
    }
    fn scroll_into_view(
        &self,
        handle: &NativeHandle,
        lease: &InteractionLease,
    ) -> Result<(), AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.scroll_into_view(handle, lease)
    }
    fn scroll_ref_into_view(
        &self,
        _entry: &agent_desktop_core::RefEntry,
        handle: &NativeHandle,
        lease: &InteractionLease,
    ) -> Result<Option<NativeHandle>, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.scroll_ref_into_view(_entry, handle, lease)
    }
}

impl InputOps for Guarded<'_> {
    fn physical_pointer_refusal(&self) -> Option<AdapterError> {
        self.native.physical_pointer_refusal()
    }
    fn keyboard_needs_window_activation(&self) -> bool {
        self.native.keyboard_needs_window_activation()
    }
    fn mouse_event(
        &self,
        _event: MouseEvent,
        _lease: &InteractionLease,
    ) -> Result<OperationReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.mouse_event(_event, _lease)
    }
    fn key_event(
        &self,
        _combo: &KeyCombo,
        _down: bool,
        _lease: &InteractionLease,
    ) -> Result<(), AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.key_event(_combo, _down, _lease)
    }
    fn drag(
        &self,
        _params: DragParams,
        _lease: &InteractionLease,
    ) -> Result<OperationReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.drag(_params, _lease)
    }
    fn clear_clipboard(&self, _lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.clear_clipboard(_lease)
    }
    fn get_clipboard_content(
        &self,
        _format: ClipboardFormat,
        _deadline: Deadline,
    ) -> Result<Option<ClipboardContent>, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.get_clipboard_content(_format, _deadline)
    }
    fn set_clipboard_content(
        &self,
        _content: &ClipboardContent,
        _lease: &InteractionLease,
    ) -> Result<OperationReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.set_clipboard_content(_content, _lease)
    }
}

impl SystemOps for Guarded<'_> {
    fn update_cursor_overlay(&self, _control: &CursorOverlayControl) -> Result<(), AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.update_cursor_overlay(_control)
    }
    fn run_cursor_overlay_child(&self) -> Option<Result<(), AdapterError>> {
        self.native.run_cursor_overlay_child()
    }
    fn acquire_interaction_lease(
        &self,
        _deadline: Deadline,
    ) -> Result<InteractionLease, AdapterError> {
        self.native.acquire_interaction_lease(_deadline)
    }
    fn permission_report(&self, _deadline: Deadline) -> Result<PermissionReport, AdapterError> {
        self.native.permission_report(_deadline)
    }
    fn unknown_accessibility_means_unsupported(&self) -> bool {
        self.native.unknown_accessibility_means_unsupported()
    }
    fn activate_renderer_accessibility(
        &self,
        _process: ProcessIdentity,
        _lease: &InteractionLease,
    ) -> Result<(), AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native
            .activate_renderer_accessibility(_process, _lease)
    }
    fn request_permissions(
        &self,
        lease: &InteractionLease,
    ) -> Result<PermissionReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.request_permissions(lease)
    }
    fn start_permission_guide(&self) -> Result<&'static str, AdapterError> {
        self.native.start_permission_guide()
    }
    fn focus_window(
        &self,
        _win: &WindowInfo,
        _lease: &InteractionLease,
    ) -> Result<(), AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.focus_window(_win, _lease)
    }
    fn launch_app(
        &self,
        _id: &str,
        _options: &agent_desktop_core::system::launch_options::LaunchOptions,
        _lease: &InteractionLease,
    ) -> Result<agent_desktop_core::system::launch_result::LaunchResult, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.launch_app(_id, _options, _lease)
    }
    fn process_owns_tcp_listener(
        &self,
        _process: agent_desktop_core::ProcessIdentity,
        _port: u16,
        _deadline: Deadline,
    ) -> Result<bool, AdapterError> {
        self.native
            .process_owns_tcp_listener(_process, _port, _deadline)
    }
    fn process_state(
        &self,
        _process: ProcessIdentity,
        _deadline: Deadline,
    ) -> Result<agent_desktop_core::system::process_state::ProcessState, AdapterError> {
        self.native.process_state(_process, _deadline)
    }
    fn supported_surfaces(&self) -> Vec<agent_desktop_core::SnapshotSurface> {
        self.native.supported_surfaces()
    }
    fn check_surface_support(
        &self,
        requested: agent_desktop_core::SnapshotSurface,
    ) -> Result<(), AdapterError> {
        self.native.check_surface_support(requested)
    }
    fn subscribe_wait(
        &self,
        _filter: &SignalFilter,
        _deadline: Deadline,
    ) -> Result<Option<Box<dyn agent_desktop_core::WaitWakeup>>, AdapterError> {
        self.native.subscribe_wait(_filter, _deadline)
    }
    fn open_system_surface(
        &self,
        _surface: agent_desktop_core::SnapshotSurface,
        _policy: InteractionPolicy,
        _lease: &InteractionLease,
    ) -> Result<WindowInfo, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.open_system_surface(_surface, _policy, _lease)
    }
    fn open_session(
        &self,
        _affinity: &SessionAffinity,
        _deadline: Deadline,
    ) -> Result<Box<dyn AdapterSession>, AdapterError> {
        self.native.open_session(_affinity, _deadline)
    }
    fn capture_signal_baseline(
        &self,
        _filter: &SignalFilter,
        _deadline: Deadline,
    ) -> Result<SignalBaseline, AdapterError> {
        self.native.capture_signal_baseline(_filter, _deadline)
    }
    fn close_app(
        &self,
        _app: &AppInfo,
        _force: bool,
        _lease: &InteractionLease,
    ) -> Result<OperationReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.close_app(_app, _force, _lease)
    }
    fn is_protected_process(&self, _identifier: &str) -> bool {
        self.native.is_protected_process(_identifier)
    }
    fn is_blocked_combo(&self, _combo: &KeyCombo) -> bool {
        self.native.is_blocked_combo(_combo)
    }
    fn screenshot(
        &self,
        _target: ScreenshotTarget,
        _deadline: Deadline,
    ) -> Result<ImageBuffer, AdapterError> {
        self.native.screenshot(_target, _deadline)
    }
    fn screenshot_window_frame(
        &self,
        _window: &WindowInfo,
        _deadline: Deadline,
    ) -> Result<ImageBuffer, AdapterError> {
        self.native.screenshot_window_frame(_window, _deadline)
    }
    fn list_displays(&self, _deadline: Deadline) -> Result<Vec<DisplayInfo>, AdapterError> {
        self.native.list_displays(_deadline)
    }
    fn focused_window(&self, _deadline: Deadline) -> Result<Option<WindowInfo>, AdapterError> {
        self.native.focused_window(_deadline)
    }
    fn press_key_for_app(
        &self,
        _process: ProcessIdentity,
        _combo: &agent_desktop_core::KeyCombo,
        _policy: agent_desktop_core::InteractionPolicy,
        _lease: &InteractionLease,
    ) -> Result<ActionResult, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native
            .press_key_for_app(_process, _combo, _policy, _lease)
    }
    fn press_key_for_window(
        &self,
        window: &WindowInfo,
        combo: &KeyCombo,
        policy: InteractionPolicy,
        lease: &InteractionLease,
    ) -> Result<ActionResult, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native
            .press_key_for_window(window, combo, policy, lease)
    }
    fn wait_for_menu(
        &self,
        _process: ProcessIdentity,
        _open: bool,
        _deadline: Deadline,
    ) -> Result<(), AdapterError> {
        self.native.wait_for_menu(_process, _open, _deadline)
    }
    fn window_op(
        &self,
        _win: &WindowInfo,
        _op: WindowOp,
        _lease: &InteractionLease,
    ) -> Result<OperationReport, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.window_op(_win, _op, _lease)
    }
    fn resolve_window_strict(
        &self,
        _win: &WindowInfo,
        _deadline: Deadline,
    ) -> Result<WindowInfo, AdapterError> {
        self.native.resolve_window_strict(_win, _deadline)
    }
    fn check_notification_support(&self, _deadline: Deadline) -> Result<(), AdapterError> {
        self.native.check_notification_support(_deadline)
    }
    fn list_notifications(
        &self,
        _filter: &NotificationFilter,
        _policy: InteractionPolicy,
        _deadline: Deadline,
        _lease: Option<&InteractionLease>,
    ) -> Result<Vec<NotificationInfo>, AdapterError> {
        self.native
            .list_notifications(_filter, _policy, _deadline, _lease)
    }
    fn dismiss_notification(
        &self,
        _request: DismissNotificationRequest<'_>,
        _lease: &InteractionLease,
    ) -> Result<NotificationInfo, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.dismiss_notification(_request, _lease)
    }
    fn dismiss_all_notifications(
        &self,
        _request: DismissAllNotificationsRequest<'_>,
        _lease: &InteractionLease,
    ) -> Result<(Vec<NotificationInfo>, Vec<String>), AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.dismiss_all_notifications(_request, _lease)
    }
    fn notification_action(
        &self,
        _request: NotificationActionRequest<'_>,
        _lease: &InteractionLease,
    ) -> Result<ActionResult, AdapterError> {
        self.authority.check().map_err(|_| super::revoked())?;
        self.native.notification_action(_request, _lease)
    }
}
