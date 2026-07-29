//! Opt-in semantic inspection for the GPUI desktop application.
//!
//! `gpui-mcp` deliberately owns neither GPUI nor application behavior. This
//! module is the narrow adapter between its framework-neutral transport and
//! Auru PM's foreground executor.

use gpui::{
    AnyWindowHandle, App, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase, Window, point, px,
};
use gpui_mcp::{
    ActionRequest, InspectionActionResult, InspectionRegistry, InspectionServer,
    NormalizedModifiers, SemanticBounds, SemanticNode, SemanticTree,
};

pub const ROOT_ID: &str = "auru-pm";

/// A top-level GPUI window/screen whose semantic tree is currently active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Surface {
    Library,
    Onboarding,
    Settings,
}

impl Surface {
    const fn value(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Onboarding => "onboarding",
            Self::Settings => "settings",
        }
    }
}

/// The two halves needed when inspection starts.
pub struct RunningInspection {
    pub publisher: InspectionPublisher,
    pub actions: async_channel::Receiver<ActionRequest>,
    pub address: std::net::SocketAddr,
    pub token: String,
}

/// Starts an authenticated listener on an ephemeral IPv4 loopback port.
pub fn start() -> Result<RunningInspection, String> {
    let registry = InspectionRegistry::default();
    let (action_tx, actions) = async_channel::bounded(16);
    let server = InspectionServer::start(0, registry.clone(), action_tx)?;
    let address = server.address();
    let token = server.token().to_owned();

    Ok(RunningInspection {
        publisher: InspectionPublisher {
            registry,
            _server: server,
            revision: 0,
            published_nodes: Vec::new(),
            active_window: None,
        },
        actions,
        address,
        token,
    })
}

/// Latest semantic snapshot plus the window actions should target.
pub struct InspectionPublisher {
    registry: InspectionRegistry,
    // The listener thread owns its socket, but retaining this value makes the
    // endpoint's lifetime visibly match the application's inspection state.
    _server: InspectionServer,
    revision: u64,
    published_nodes: Vec<SemanticNode>,
    active_window: Option<AnyWindowHandle>,
}

impl InspectionPublisher {
    /// Publishes the currently active surface. Background windows must not
    /// replace the root bounds used for resize, scroll, and screenshot actions.
    pub fn publish(
        &mut self,
        surface: Surface,
        window: &Window,
        root_focused: bool,
        mut nodes: Vec<SemanticNode>,
    ) {
        let window_handle = window.window_handle();
        if !window.is_window_active()
            && self
                .active_window
                .is_some_and(|active_window| active_window != window_handle)
        {
            return;
        }

        self.active_window = Some(window_handle);
        let viewport = window.viewport_size();
        let mut root = node(
            ROOT_ID,
            "application",
            "Auru PM",
            Some(surface.value().to_owned()),
            root_focused,
            &["focus"],
        );
        root.bounds = Some(SemanticBounds {
            x: 0.0,
            y: 0.0,
            width: viewport.width.into(),
            height: viewport.height.into(),
        });
        nodes.insert(0, root);

        if nodes == self.published_nodes {
            return;
        }
        self.revision = self.revision.saturating_add(1);
        self.published_nodes = nodes.clone();
        self.registry.publish(SemanticTree {
            revision: self.revision,
            nodes,
        });
    }

    pub fn active_window(&self) -> Option<AnyWindowHandle> {
        self.active_window
    }
}

/// Builder kept deliberately small so every semantic node uses the same action
/// vocabulary and optional-value shape.
pub fn node(
    id: impl Into<String>,
    role: impl Into<String>,
    label: impl Into<String>,
    value: Option<String>,
    focused: bool,
    actions: &[&str],
) -> SemanticNode {
    SemanticNode {
        id: id.into(),
        role: role.into(),
        label: label.into(),
        value,
        bounds: None,
        focused,
        actions: actions.iter().map(|action| (*action).to_owned()).collect(),
    }
}

/// Stable, opaque semantic ID for application identities that may contain
/// private filesystem paths or selector punctuation.
pub fn stable_id(prefix: &str, identity: &str) -> String {
    // FNV-1a is sufficient here: this is an automation locator, not a content
    // hash or security boundary. Keeping the tiny hash local avoids exposing
    // project paths and keeps IDs readable in MCP output.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in identity.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}-{hash:016x}")
}

pub fn gpui_modifiers(modifiers: NormalizedModifiers) -> Modifiers {
    Modifiers {
        platform: modifiers.platform,
        control: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        function: modifiers.function,
    }
}

pub fn press_key(
    key: String,
    modifiers: NormalizedModifiers,
    window: &mut Window,
    cx: &mut App,
) -> Result<InspectionActionResult, String> {
    let handled = window.dispatch_keystroke(
        Keystroke {
            modifiers: gpui_modifiers(modifiers),
            key,
            key_char: None,
        },
        cx,
    );
    if handled {
        Ok(InspectionActionResult::Complete)
    } else {
        Err("the active GPUI window did not handle that key".to_owned())
    }
}

pub fn type_text(
    text: &str,
    window: &mut Window,
    cx: &mut App,
) -> Result<InspectionActionResult, String> {
    for character in text.chars() {
        let character = character.to_string();
        let handled = window.dispatch_keystroke(
            Keystroke {
                modifiers: Modifiers::default(),
                key: character.clone(),
                key_char: Some(character),
            },
            cx,
        );
        if !handled {
            return Err("the active GPUI window has no editable focus target".to_owned());
        }
    }
    Ok(InspectionActionResult::Complete)
}

pub fn scroll(
    position: gpui_mcp::SemanticPoint,
    delta_x: f32,
    delta_y: f32,
    modifiers: NormalizedModifiers,
    window: &mut Window,
    cx: &mut App,
) -> InspectionActionResult {
    window.dispatch_event(
        PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: point(px(position.x), px(position.y)),
            delta: ScrollDelta::Pixels(point(px(delta_x), px(delta_y))),
            modifiers: gpui_modifiers(modifiers),
            touch_phase: TouchPhase::Moved,
        }),
        cx,
    );
    InspectionActionResult::Complete
}

pub fn drag(
    from: gpui_mcp::SemanticPoint,
    to: gpui_mcp::SemanticPoint,
    window: &mut Window,
    cx: &mut App,
) -> InspectionActionResult {
    let from = point(px(from.x), px(from.y));
    let to = point(px(to.x), px(to.y));
    let modifiers = Modifiers::default();

    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: from,
            pressed_button: None,
            modifiers,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: to,
            pressed_button: Some(MouseButton::Left),
            modifiers,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position: to,
            modifiers,
            click_count: 1,
        }),
        cx,
    );
    InspectionActionResult::Complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_modifiers_map_without_platform_guessing() {
        let mapped = gpui_modifiers(NormalizedModifiers {
            platform: true,
            control: true,
            alt: true,
            shift: true,
            function: true,
        });

        assert!(mapped.platform);
        assert!(mapped.control);
        assert!(mapped.alt);
        assert!(mapped.shift);
        assert!(mapped.function);
    }

    #[test]
    fn surface_values_are_stable_protocol_names() {
        assert_eq!(Surface::Library.value(), "library");
        assert_eq!(Surface::Onboarding.value(), "onboarding");
        assert_eq!(Surface::Settings.value(), "settings");
    }

    #[test]
    fn semantic_nodes_keep_the_declared_action_vocabulary() {
        let node = node(
            "save",
            "button",
            "Save",
            Some("ready".to_owned()),
            false,
            &["click", "focus"],
        );

        assert_eq!(node.id, "save");
        assert_eq!(node.role, "button");
        assert_eq!(node.value.as_deref(), Some("ready"));
        assert_eq!(node.actions, ["click", "focus"]);
    }

    #[test]
    fn stable_ids_do_not_expose_the_identity() {
        let id = stable_id("project", "/home/alice/Secret Project/song.als");

        assert!(id.starts_with("project-"));
        assert!(!id.contains("alice"));
        assert!(!id.contains('/'));
        assert_ne!(id, stable_id("project", "/home/alice/other.als"));
    }
}
