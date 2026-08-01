//! Form-like submission for `gpui-component` text inputs.

use gpui::{Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

/// Subscribe an input to a form-style submit callback.
///
/// The returned subscription must be retained for as long as the input should
/// submit. Keeping that ownership explicit mirrors GPUI's other subscriptions
/// and lets one input opt into exactly one form action.
pub fn use_input_submit<T>(
    input: &Entity<InputState>,
    window: &Window,
    cx: &mut Context<T>,
    mut on_submit: impl FnMut(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> Subscription
where
    T: 'static,
{
    cx.subscribe_in(input, window, move |owner, _, event, window, cx| {
        if is_submit(event) {
            on_submit(owner, window, cx);
        }
    })
}

fn is_submit(event: &InputEvent) -> bool {
    matches!(event, InputEvent::PressEnter { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_enter_should_submit_the_input() {
        assert!(is_submit(&InputEvent::PressEnter {
            secondary: false,
            shift: false,
        }));
    }

    #[test]
    fn input_changes_should_not_submit_the_input() {
        assert!(!is_submit(&InputEvent::Change));
    }
}
