//! Comma- and Enter-delimited custom value entry for metadata badges.

use gpui::{Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

/// Subscribe a text input so Enter or a delimiter commits custom badge values.
///
/// Commas and newlines commit every complete segment while leaving the final
/// unfinished segment in the input. Enter commits the whole input.
pub fn use_badge_input<T>(
    input: &Entity<InputState>,
    window: &Window,
    cx: &mut Context<T>,
    mut on_commit: impl FnMut(&mut T, Vec<String>, &mut Window, &mut Context<T>) + 'static,
) -> Subscription
where
    T: 'static,
{
    let listener_input = input.clone();
    cx.subscribe_in(input, window, move |owner, _, event, window, cx| {
        let commit_trailing = match event {
            InputEvent::PressEnter { .. } => true,
            InputEvent::Change => false,
            _ => return,
        };
        let input = listener_input.read(cx);
        let value = input.value();
        if !commit_trailing && !value.contains(',') && !value.contains('\n') {
            return;
        }

        let (values, remainder) = split_badge_input(value.as_ref(), commit_trailing);
        listener_input.update(cx, |input, cx| {
            input.set_value(remainder.as_str(), window, cx);
        });
        if !values.is_empty() {
            on_commit(owner, values, window, cx);
        }
    })
}

/// Return all non-empty values in comma- or newline-separated text.
pub fn badge_values(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn split_badge_input(text: &str, commit_trailing: bool) -> (Vec<String>, String) {
    let has_trailing_delimiter = text.ends_with([',', '\n']);
    let mut parts = text.split([',', '\n']).collect::<Vec<_>>();
    let remainder = if commit_trailing || has_trailing_delimiter {
        String::new()
    } else {
        parts
            .pop()
            .map_or_else(String::new, |value| value.trim().to_owned())
    };
    let values = parts
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    (values, remainder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_should_commit_complete_values_and_keep_the_unfinished_value() {
        assert_eq!(
            split_badge_input("drums, synth", false),
            (vec!["drums".to_owned()], "synth".to_owned())
        );
    }

    #[test]
    fn trailing_comma_should_commit_every_value() {
        assert_eq!(
            split_badge_input("drums, synth,", false),
            (vec!["drums".to_owned(), "synth".to_owned()], String::new())
        );
    }

    #[test]
    fn enter_should_commit_the_trailing_value() {
        assert_eq!(
            split_badge_input("drums, synth", true),
            (vec!["drums".to_owned(), "synth".to_owned()], String::new())
        );
    }

    #[test]
    fn empty_segments_should_not_create_badges() {
        assert_eq!(badge_values("drums, ,\n synth"), ["drums", "synth"]);
    }
}
