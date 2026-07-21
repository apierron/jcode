use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt;
use std::{collections::VecDeque, io, time::Duration};

const FRAGMENT_TIMEOUT: Duration = Duration::from_millis(25);
const MAX_SGR_MOUSE_TAIL_LEN: usize = 32;

/// Crossterm can emit a fragmented SGR mouse report as `Esc` followed by
/// printable key events on Unix (crossterm#668). Reassemble those reports at the
/// input boundary so their text never reaches the composer.
pub(super) struct EventStream {
    inner: crossterm::event::EventStream,
    replay: VecDeque<io::Result<Event>>,
}

impl EventStream {
    pub(super) fn new() -> Self {
        Self {
            inner: crossterm::event::EventStream::new(),
            replay: VecDeque::new(),
        }
    }

    pub(super) async fn next(&mut self) -> Option<io::Result<Event>> {
        if let Some(event) = self.replay.pop_front() {
            return Some(event);
        }

        let first = self.inner.next().await?;
        if !first.as_ref().is_ok_and(is_plain_escape) {
            return Some(first);
        }

        let mut tail = String::new();
        let mut captured = Vec::new();
        while tail.len() < MAX_SGR_MOUSE_TAIL_LEN {
            let next = match tokio::time::timeout(FRAGMENT_TIMEOUT, self.inner.next()).await {
                Ok(Some(event)) => event,
                _ => break,
            };
            let Some(ch) = next.as_ref().ok().and_then(sgr_tail_char) else {
                captured.push(next);
                break;
            };
            tail.push(ch);
            captured.push(next);

            if let Some(mouse) = parse_sgr_mouse_tail(&tail) {
                return Some(Ok(Event::Mouse(mouse)));
            }
            if !is_sgr_mouse_tail_prefix(&tail) {
                break;
            }
        }

        self.replay.extend(captured);
        Some(first)
    }
}

fn is_plain_escape(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press | KeyEventKind::Repeat,
            ..
        })
    )
}

fn sgr_tail_char(event: &Event) -> Option<char> {
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        || !(key.modifiers - KeyModifiers::SHIFT).is_empty()
    {
        return None;
    }
    match key.code {
        KeyCode::Char(ch) if ch.is_ascii() => Some(ch),
        _ => None,
    }
}

fn is_sgr_mouse_tail_prefix(tail: &str) -> bool {
    match tail {
        "[" | "[<" => true,
        _ => tail.strip_prefix("[<").is_some_and(|body| {
            body.bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b';')
        }),
    }
}

fn parse_sgr_mouse_tail(tail: &str) -> Option<MouseEvent> {
    let released = tail.ends_with('m');
    let body = tail.strip_prefix("[<")?.strip_suffix(['M', 'm'])?;
    let mut fields = body.split(';');
    let cb = fields.next()?.parse::<u8>().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    if fields.next().is_some() {
        return None;
    }

    let button = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    let dragging = cb & 0b0010_0000 != 0;
    let mut kind = match (button, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3..=5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };
    if released && let MouseEventKind::Down(button) = kind {
        kind = MouseEventKind::Up(button);
    }

    let mut modifiers = KeyModifiers::NONE;
    if cb & 0b0000_0100 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if cb & 0b0000_1000 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if cb & 0b0001_0000 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }

    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragmented_scroll_report() {
        assert_eq!(
            parse_sgr_mouse_tail("[<65;50;24M"),
            Some(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 49,
                row: 23,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[test]
    fn ordinary_text_is_not_an_sgr_mouse_report() {
        assert_eq!(parse_sgr_mouse_tail("[<65;50;24"), None);
        assert!(!is_sgr_mouse_tail_prefix("[hello"));
    }
}
