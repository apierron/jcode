use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::{FutureExt, Stream, StreamExt};
use std::{collections::VecDeque, io, time::Duration};

const FRAGMENT_TIMEOUT: Duration = Duration::from_millis(25);
const MAX_SGR_MOUSE_TAIL_LEN: usize = 32;

/// Crossterm can emit a fragmented SGR mouse report as `Esc` followed by
/// printable key events on Unix (crossterm#668). Reassemble those reports at the
/// input boundary so their text never reaches the composer.
pub(super) struct EventStream<S = crossterm::event::EventStream> {
    inner: S,
    replay: VecDeque<io::Result<Event>>,
    pending_escape: Option<PendingEscape>,
}

impl EventStream {
    pub(super) fn new() -> Self {
        Self {
            inner: crossterm::event::EventStream::new(),
            replay: VecDeque::new(),
            pending_escape: None,
        }
    }
}

struct PendingEscape {
    first: io::Result<Event>,
    tail: String,
    captured: Vec<io::Result<Event>>,
    fragment_deadline: tokio::time::Instant,
}

impl<S> EventStream<S>
where
    S: Stream<Item = io::Result<Event>> + Unpin,
{
    #[cfg(test)]
    fn with_inner(inner: S) -> Self {
        Self {
            inner,
            replay: VecDeque::new(),
            pending_escape: None,
        }
    }

    pub(super) async fn next(&mut self) -> Option<io::Result<Event>> {
        if self.pending_escape.is_none() {
            if let Some(event) = self.replay.pop_front() {
                return Some(event);
            }

            let first = self.inner.next().await?;
            if !first.as_ref().is_ok_and(is_plain_escape) {
                return Some(first);
            }
            // Keep all partially consumed packet state on `self`. Callers poll
            // this future inside `tokio::select!`, so locals would be lost if a
            // bus event or redraw tick cancelled `next()` between fragments.
            self.pending_escape = Some(PendingEscape {
                first,
                tail: String::new(),
                captured: Vec::new(),
                fragment_deadline: tokio::time::Instant::now() + FRAGMENT_TIMEOUT,
            });
        }

        loop {
            if self
                .pending_escape
                .as_ref()
                .is_some_and(|pending| pending.tail.len() >= MAX_SGR_MOUSE_TAIL_LEN)
            {
                return self.finish_pending_escape();
            }

            let deadline = self.pending_escape.as_ref()?.fragment_deadline;
            let next = match tokio::time::timeout_at(deadline, self.inner.next()).await {
                Ok(Some(event)) => event,
                _ => return self.finish_pending_escape(),
            };
            let Some(ch) = next.as_ref().ok().and_then(sgr_tail_char) else {
                self.pending_escape.as_mut()?.captured.push(next);
                return self.finish_pending_escape();
            };

            let pending = self.pending_escape.as_mut()?;
            pending.tail.push(ch);
            pending.captured.push(next);
            pending.fragment_deadline = tokio::time::Instant::now() + FRAGMENT_TIMEOUT;

            if let Some(mouse) = parse_sgr_mouse_tail(&pending.tail) {
                self.pending_escape = None;
                return Some(Ok(Event::Mouse(mouse)));
            }
            if !is_sgr_mouse_tail_prefix(&pending.tail) {
                return self.finish_pending_escape();
            }
        }
    }

    /// Poll one event without waiting. Partially consumed Escape packets stay
    /// on `self`, so dropping the pending future is cancellation-safe.
    pub(super) fn next_ready(&mut self) -> Option<Option<io::Result<Event>>> {
        self.next().now_or_never()
    }

    pub(super) fn drain_ready<const N: usize>(&mut self) -> [Option<io::Result<Event>>; N] {
        let mut exhausted = false;
        std::array::from_fn(|_| {
            if exhausted {
                return None;
            }
            match self.next_ready() {
                Some(Some(event)) => Some(event),
                _ => {
                    exhausted = true;
                    None
                }
            }
        })
    }

    fn finish_pending_escape(&mut self) -> Option<io::Result<Event>> {
        let pending = self.pending_escape.take()?;
        self.replay.extend(pending.captured);
        Some(pending.first)
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
    use futures::channel::mpsc;

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

    #[tokio::test]
    async fn preserves_escape_and_fragments_when_next_is_cancelled() {
        let (tx, rx) = mpsc::unbounded();
        let mut stream = EventStream::with_inner(rx);
        tx.unbounded_send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .unwrap();

        // Poll through consuming Esc, then cancel while waiting for its tail.
        // The next call must resume the same packet instead of losing Esc.
        {
            let future = stream.next();
            tokio::pin!(future);
            assert!(futures::poll!(future.as_mut()).is_pending());
        }

        for ch in "[<65;50;24M".chars() {
            tx.unbounded_send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            ))))
            .unwrap();
        }

        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 49,
                row: 23,
                modifiers: KeyModifiers::NONE,
            })
        );
    }

    #[tokio::test]
    async fn cancellation_does_not_restart_escape_fragment_timeout() {
        let (tx, rx) = mpsc::unbounded();
        let mut stream = EventStream::with_inner(rx);
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        tx.unbounded_send(Ok(escape.clone())).unwrap();

        {
            let future = stream.next();
            tokio::pin!(future);
            assert!(futures::poll!(future.as_mut()).is_pending());
        }
        tokio::time::sleep(FRAGMENT_TIMEOUT + Duration::from_millis(5)).await;

        let resumed = tokio::time::timeout(Duration::from_millis(5), stream.next())
            .await
            .expect("an expired fragment deadline must not restart after cancellation")
            .unwrap()
            .unwrap();
        assert_eq!(resumed, escape);
    }

    #[tokio::test]
    async fn ready_events_can_be_coalesced_through_the_same_reader() {
        let (tx, rx) = mpsc::unbounded();
        let mut stream = EventStream::with_inner(rx);
        for ch in "abc".chars() {
            tx.unbounded_send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::NONE,
            ))))
            .unwrap();
        }

        let chars: Vec<_> = stream
            .drain_ready::<32>()
            .into_iter()
            .flatten()
            .filter_map(|event| match event {
                Ok(Event::Key(key)) => Some(key.code),
                _ => None,
            })
            .collect();
        assert_eq!(
            chars,
            [KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')]
        );
    }
}
