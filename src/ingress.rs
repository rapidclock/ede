//! Sole terminal reader: bounded VT decoding and streaming bracketed-paste staging.
//!
//! Crossterm's complete-string paste event is deliberately bypassed. One reader
//! thread owns stdin, parses keys/mouse/protocol replies, and stages paste bytes
//! directly to a private file. Bounded queues apply backpressure to the terminal.
use crate::{
    document::Cancellation,
    input::{self, Input},
    spool::Spool,
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use nix::poll::{PollFd, PollFlags, poll};
use std::{
    io::{self, Read},
    os::fd::AsFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Bounded reader owner. Stop is independent of event queue capacity.
pub(crate) struct Ingress {
    inbox: Receiver<io::Result<Input>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Ingress {
    /// Spawn after raw mode acquisition; no second terminal reader may run concurrently.
    pub fn new() -> io::Result<Self> {
        let (sender, inbox) = mpsc::sync_channel(256);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::Builder::new()
            .name("ede-input".into())
            .spawn(move || {
                let result = read_terminal(&sender, &stopping);
                if let Err(error) = result {
                    send(&sender, Err(error), &stopping);
                }
            })?;
        Ok(Self {
            inbox,
            stop,
            worker: Some(worker),
        })
    }
    /// Request shutdown before terminal restoration; this never waits for disk I/O.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    /// Join only after terminal restoration; a filesystem call may delay reader exit.
    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    /// Wait only for the event loop's bounded polling interval.
    pub fn input(&self, timeout: Duration) -> io::Result<Option<Input>> {
        match self.inbox.recv_timeout(timeout) {
            Ok(input) => input.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err(io::Error::other("Terminal input closed")),
        }
    }
}
impl Drop for Ingress {
    /// Panic paths request cancellation without waiting while terminal modes may still be held.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
/// A full queue blocks only this reader and checks shutdown every millisecond.
fn send(
    sender: &SyncSender<io::Result<Input>>,
    mut input: io::Result<Input>,
    stop: &AtomicBool,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(input) {
            Ok(()) => return true,
            Err(TrySendError::Full(value)) => {
                input = value;
                thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}
/// Poll stdin and terminal geometry without consuming capability replies elsewhere.
fn read_terminal(sender: &SyncSender<io::Result<Input>>, stop: &AtomicBool) -> io::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut parser = Parser::default();
    let mut bytes = [0; 8192];
    let mut size = crossterm::terminal::size()?;
    let mut last = Instant::now();
    while !stop.load(Ordering::Acquire) {
        parser.reclaim();
        let next = crossterm::terminal::size()?;
        if next != size {
            size = next;
            if !send(sender, Ok(Input::Resize(size.0, size.1)), stop) {
                break;
            }
        }
        let ready = {
            let mut fds = [PollFd::new(input.as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, 20u16) {
                Ok(count) => count > 0,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
        };
        if !ready {
            if last.elapsed() >= Duration::from_millis(40) {
                for event in parser.expire() {
                    if !send(sender, Ok(event), stop) {
                        return Ok(());
                    }
                }
            }
            continue;
        }
        let count = match input.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Terminal input closed",
            ));
        }
        last = Instant::now();
        for byte in &bytes[..count] {
            for event in parser.push(*byte) {
                if !send(sender, Ok(event), stop) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}
/// Paste state keeps only a fixed chunk, a possible terminator prefix, and a disk file.
struct Paste {
    spool: Option<Spool>,
    buffer: Vec<u8>,
    prefix: Vec<u8>,
    cancel: Cancellation,
    error: Option<String>,
}
/// Incremental VT parser. Escape sequences are bounded to 128 bytes.
#[derive(Default)]
struct Parser {
    pending: Vec<u8>,
    paste: Option<Paste>,
    /// Keep the last file owned by this I/O thread until every consumer releases it.
    /// This also limits completed terminal pastes independently of event-channel capacity.
    outstanding: Option<Arc<Spool>>,
}
impl Parser {
    /// Close completed spools here, so a rejected/dismissed UI event never closes a large file.
    fn reclaim(&mut self) {
        if let Some(paste) = &mut self.paste
            && paste.cancel.is_cancelled()
        {
            paste.spool = None;
            paste.buffer.clear();
        }
        if self
            .outstanding
            .as_ref()
            .is_some_and(|spool| Arc::strong_count(spool) == 1)
        {
            self.outstanding = None;
        }
    }
    /// Resolve a lone Escape; retain bounded partial protocol replies across slow transport gaps.
    fn expire(&mut self) -> Vec<Input> {
        self.reclaim();
        if self.paste.is_none() && self.pending == b"\x1b" {
            self.pending.clear();
            return vec![Input::Command(crate::command::Command::Cancel)];
        }
        vec![]
    }
    /// Consume one byte; bracketed paste is staged before any String is constructed.
    fn push(&mut self, byte: u8) -> Vec<Input> {
        if let Some(paste) = &mut self.paste {
            const END: &[u8] = b"\x1b[201~";
            paste.prefix.push(byte);
            while !END.starts_with(&paste.prefix) {
                let payload = paste.prefix.remove(0);
                if paste.spool.is_some() {
                    paste.buffer.push(payload);
                }
                if paste.buffer.len() == 64 << 10 {
                    paste.flush();
                }
            }
            if paste.prefix == END {
                paste.flush();
                let paste = self.paste.take().expect("paste ending");
                return if paste.cancel.is_cancelled() {
                    vec![Input::Notice("Paste cancelled".into())]
                } else if let Some(error) = paste.error {
                    vec![Input::PasteRejected(error)]
                } else {
                    let spool = Arc::new(paste.spool.expect("staged paste"));
                    self.outstanding = Some(spool.clone());
                    vec![Input::Paste(spool)]
                };
            }
            return vec![];
        }
        if self.pending == b"\x1b" && byte == 0x1b {
            // Two Escapes are two cancels, rather than an unbound Alt+Escape.
            return vec![Input::Command(crate::command::Command::Cancel)];
        }
        if !self.pending.is_empty() && self.pending[0] != 0x1b && byte & 0xc0 != 0x80 {
            // Discard a truncated UTF-8 scalar, then decode the fresh key. An
            // invalid prefix must not swallow the user's next printable/quit key.
            self.pending.clear();
        }
        // Keep bounded CSI/SS3 replies across slow reads. A fresh control key can
        // interrupt an incomplete sequence, so malformed input cannot swallow quit.
        if self.pending.len() > 1 && self.pending[0] == 0x1b && byte < 0x20 {
            self.pending.clear();
        }
        self.pending.push(byte);
        if self.pending == b"\x1b[200~" {
            self.pending.clear();
            self.reclaim();
            let cancel = Cancellation::default();
            let (spool, error) = if self.outstanding.is_some() {
                (
                    None,
                    Some("Busy: previous terminal paste is still pending; paste rejected".into()),
                )
            } else {
                match Spool::new() {
                    Ok(spool) => (Some(spool), None),
                    Err(error) => (None, Some(error.to_string())),
                }
            };
            self.paste = Some(Paste {
                spool,
                buffer: Vec::with_capacity(64 << 10),
                prefix: Vec::with_capacity(6),
                cancel: cancel.clone(),
                error,
            });
            return vec![Input::PasteStarted(cancel)];
        }
        if self.pending.len() > 128 {
            self.pending.clear();
            return vec![Input::Notice("Unsupported terminal sequence".into())];
        }
        let parsed = decode(&self.pending);
        match parsed {
            Decoded::More => vec![],
            Decoded::Ignore => {
                self.pending.clear();
                vec![]
            }
            Decoded::Input(input) => {
                self.pending.clear();
                vec![input]
            }
        }
    }
}
impl Paste {
    /// Flush a fixed chunk; after failure/cancellation drain to the terminator without storing.
    fn flush(&mut self) {
        if self.cancel.is_cancelled() {
            self.spool = None;
        }
        if let Some(spool) = &mut self.spool
            && let Err(error) = spool.append(&self.buffer)
        {
            self.error = Some(error.to_string());
            self.spool = None;
        }
        self.buffer.clear();
    }
}
/// Partial, ignored protocol response, or one normalized input event.
enum Decoded {
    More,
    Ignore,
    Input(Input),
}
/// Convert a decoded key through the shared exact binding table.
fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Decoded {
    input::normalize(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)))
        .map_or(Decoded::Ignore, Decoded::Input)
}
/// Parse xterm/Kitty modifiers (protocol value is bitset + one).
fn modifiers(value: u32) -> KeyModifiers {
    let value = value.saturating_sub(1);
    let mut result = KeyModifiers::NONE;
    for (bit, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ] {
        if value & bit != 0 {
            result |= modifier;
        }
    }
    result
}
/// Decode mouse flags once for SGR, urxvt and legacy X10 reporting.
fn mouse(b: u16, column: u16, row: u16, released: bool) -> Decoded {
    if b & !127 != 0 {
        return Decoded::Ignore;
    }
    let mut mods = KeyModifiers::NONE;
    if b & 4 != 0 {
        mods |= KeyModifiers::SHIFT;
    }
    if b & 8 != 0 {
        mods |= KeyModifiers::ALT;
    }
    if b & 16 != 0 {
        mods |= KeyModifiers::CONTROL;
    }
    let button = match b & 3 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        _ => MouseButton::Right,
    };
    let kind = if b & 64 != 0 {
        match b & 3 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if released {
        MouseEventKind::Up(button)
    } else if b & 32 != 0 {
        if b & 3 == 3 {
            MouseEventKind::Moved
        } else {
            MouseEventKind::Drag(button)
        }
    } else {
        MouseEventKind::Down(button)
    };
    Decoded::Input(Input::Pointer(MouseEvent {
        kind,
        column,
        row,
        modifiers: mods,
    }))
}
/// Decode bounded legacy VT, SGR mouse, CSI-u and capability replies.
fn decode(bytes: &[u8]) -> Decoded {
    let press = KeyEventKind::Press;
    if bytes[0] != 0x1b {
        let code = match bytes[0] {
            // Legacy Ctrl+Space is NUL, not a printable space.
            0 => return key(KeyCode::Char(' '), KeyModifiers::CONTROL, press),
            9 => KeyCode::Tab,
            10 | 13 => KeyCode::Enter,
            127 | 8 => KeyCode::Backspace,
            1..=26 => {
                return key(
                    KeyCode::Char((bytes[0] + b'a' - 1) as char),
                    KeyModifiers::CONTROL,
                    press,
                );
            }
            _ => match std::str::from_utf8(bytes) {
                Ok(text) => match text.chars().next() {
                    Some(ch) => KeyCode::Char(ch),
                    None => return Decoded::Ignore,
                },
                Err(error) if error.error_len().is_none() => return Decoded::More,
                Err(_) => return Decoded::Ignore,
            },
        };
        return key(code, KeyModifiers::NONE, press);
    }
    if bytes.len() == 1 {
        return Decoded::More;
    }
    if bytes[1] != b'[' && bytes[1] != b'O' {
        return match std::str::from_utf8(&bytes[1..]) {
            Ok(text) => key(
                KeyCode::Char(text.chars().next().unwrap_or(' ')),
                KeyModifiers::ALT,
                press,
            ),
            Err(error) if error.error_len().is_none() => Decoded::More,
            Err(_) => Decoded::Ignore,
        };
    }
    if bytes.starts_with(b"\x1b[M") {
        if bytes.len() < 6 {
            return Decoded::More;
        }
        let flags = bytes[3].saturating_sub(32) as u16;
        let released = flags & 3 == 3 && flags & 96 == 0;
        return mouse(
            if released { flags & !3 } else { flags },
            bytes[4].saturating_sub(33) as u16,
            bytes[5].saturating_sub(33) as u16,
            released,
        );
    }
    if bytes.len() < 3 || !(0x40..=0x7e).contains(bytes.last().expect("nonempty")) {
        return Decoded::More;
    }
    let final_byte = *bytes.last().expect("nonempty");
    let body = match std::str::from_utf8(&bytes[2..bytes.len() - 1]) {
        Ok(body) => body,
        Err(_) => return Decoded::Ignore,
    };
    if body.starts_with('?') {
        return if final_byte == b'u' {
            Decoded::Input(Input::KeyboardSupported)
        } else {
            Decoded::Ignore
        };
    }
    if let Some(body) = body.strip_prefix('<') {
        if !matches!(final_byte, b'M' | b'm') {
            return Decoded::Ignore;
        }
        let Ok(values) = body
            .split(';')
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>()
        else {
            return Decoded::Ignore;
        };
        if values.len() != 3 {
            return Decoded::Ignore;
        }
        return mouse(
            values[0],
            values[1].saturating_sub(1),
            values[2].saturating_sub(1),
            final_byte == b'm',
        );
    }
    if final_byte == b'M' && body.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        let Ok(values) = body
            .split(';')
            .map(str::parse::<u16>)
            .collect::<Result<Vec<_>, _>>()
        else {
            return Decoded::Ignore;
        };
        if values.len() != 3 {
            return Decoded::Ignore;
        }
        let flags = values[0].saturating_sub(32);
        let released = flags & 3 == 3 && flags & 96 == 0;
        return mouse(
            if released { flags & !3 } else { flags },
            values[1].saturating_sub(1),
            values[2].saturating_sub(1),
            released,
        );
    }
    if final_byte == b'I' {
        return Decoded::Input(Input::FocusGained);
    }
    if final_byte == b'O' && bytes[1] == b'[' {
        return Decoded::Input(Input::FocusLost);
    }
    let fields: Vec<&str> = body.split(';').collect();
    let first = fields
        .first()
        .and_then(|v| v.split(':').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);
    let mut mods = fields
        .get(1)
        .and_then(|v| v.split(':').next())
        .and_then(|v| v.parse().ok())
        .map(modifiers)
        .unwrap_or(KeyModifiers::NONE);
    let kind = match fields.get(1).and_then(|v| v.split(':').nth(1)) {
        Some("2") => KeyEventKind::Repeat,
        Some("3") => KeyEventKind::Release,
        _ => press,
    };
    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        b'Z' => {
            mods |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        b'~' => match first {
            1 | 7 => KeyCode::Home,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=15 => KeyCode::F((first - 10) as u8),
            17..=21 => KeyCode::F((first - 11) as u8),
            23..=24 => KeyCode::F((first - 12) as u8),
            _ => return Decoded::Ignore,
        },
        b'u' => match first {
            27 | 57344 => KeyCode::Esc,
            13 | 57345 => KeyCode::Enter,
            9 | 57346 if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            9 | 57346 => KeyCode::Tab,
            127 | 57347 => KeyCode::Backspace,
            57349 => KeyCode::Delete,
            57350 => KeyCode::Left,
            57351 => KeyCode::Right,
            57352 => KeyCode::Up,
            57353 => KeyCode::Down,
            57354 => KeyCode::PageUp,
            57355 => KeyCode::PageDown,
            57356 => KeyCode::Home,
            57357 => KeyCode::End,
            57364..=57398 => KeyCode::F((first - 57363) as u8),
            // This range denotes functional keys, never literal private-use text.
            // Ignore unsupported keys such as lock/modifier/media keys safely.
            57344..=63743 => return Decoded::Ignore,
            _ => match char::from_u32(first) {
                Some(ch) => KeyCode::Char(ch),
                None => return Decoded::Ignore,
            },
        },
        _ => return Decoded::Ignore,
    };
    let code = if final_byte == b'u' && mods == KeyModifiers::SHIFT {
        fields
            .first()
            .and_then(|v| v.split(':').nth(1))
            .and_then(|v| v.parse::<u32>().ok())
            .and_then(char::from_u32)
            .map_or(code, KeyCode::Char)
    } else {
        code
    };
    key(code, mods, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    /// Completed paste files cannot fill the input queue; rejected payload controls remain data.
    #[test]
    fn one_outstanding_paste_and_reader_owned_cleanup() {
        fn paste(parser: &mut Parser, payload: &[u8]) -> Vec<Input> {
            b"\x1b[200~"
                .iter()
                .chain(payload)
                .chain(b"\x1b[201~")
                .flat_map(|byte| parser.push(*byte))
                .collect()
        }
        let mut parser = Parser::default();
        let first = paste(&mut parser, b"first");
        let [Input::PasteStarted(_), Input::Paste(spool)] = first.as_slice() else {
            panic!("first paste");
        };
        let weak = Arc::downgrade(spool);
        let rejected = paste(&mut parser, b"\x11\x13not commands");
        assert!(
            matches!(rejected.as_slice(), [Input::PasteStarted(_), Input::PasteRejected(message)] if message.contains("Busy"))
        );
        drop(first);
        assert!(
            weak.upgrade().is_some(),
            "reader retains cleanup ownership after UI drops its event"
        );
        parser.expire();
        assert!(weak.upgrade().is_none());
        let next = paste(&mut parser, b"accepted after release");
        assert!(matches!(
            next.as_slice(),
            [Input::PasteStarted(_), Input::Paste(_)]
        ));
    }
    /// Control/functional keys cannot insert spaces or private-use characters; repeated Escape cancels.
    #[test]
    fn non_text_keys_and_repeated_escape() {
        let mut parser = Parser::default();
        let events: Vec<_> =
            b"\0\x1b[57348u\x1b[57358u\x1b[57441u\x1b[63743u\x1b[<128;1;1M\x1b\x1b"
                .iter()
                .flat_map(|byte| parser.push(*byte))
                .collect();
        assert!(matches!(
            events.as_slice(),
            [Input::Command(Command::Cancel)]
        ));
        assert!(matches!(
            parser.expire().as_slice(),
            [Input::Command(Command::Cancel)]
        ));
        assert!(matches!(parser.push(b'x').as_slice(), [Input::Text(text)] if text == "x"));
        assert!(parser.push(0xe7).is_empty());
        assert!(matches!(
            parser.push(0x11).as_slice(),
            [Input::Command(Command::Quit)]
        ));
        assert!(parser.push(0xf0).is_empty());
        assert!(matches!(parser.push(b'z').as_slice(), [Input::Text(text)] if text == "z"));
    }
    /// A multi-megabyte paste never builds a payload-sized buffer and preserves embedded controls.
    #[test]
    fn streaming_paste_arbitrary_splits_and_controls() {
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for byte in b"\x1b[200~" {
            events.extend(parser.push(*byte));
        }
        assert!(matches!(events.as_slice(), [Input::PasteStarted(_)]));
        events.clear();
        let block = "界\r\n\x11\x13\x1b[20x".as_bytes();
        for _ in 0..100_000 {
            for byte in block {
                assert!(parser.push(*byte).is_empty());
            }
            let paste = parser.paste.as_ref().unwrap();
            assert!(paste.buffer.len() <= 64 << 10);
            assert!(paste.prefix.len() <= 6);
        }
        for byte in b"\x1b[201~" {
            events.extend(parser.push(*byte));
        }
        let [Input::Paste(spool)] = events.as_slice() else {
            panic!("missing completed paste")
        };
        assert_eq!(spool.len, block.len() as u64 * 100_000);
        let mut reader = spool.reader();
        let mut bytes = vec![0; block.len()];
        for _ in 0..100_000 {
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(bytes, block);
        }
    }
    /// CSI-u modifiers/releases, legacy navigation/mouse and capability replies share one reader.
    #[test]
    fn keyboard_mouse_and_capability_normalization() {
        for (sequence, command) in [
            ("\x1b[115;9u", Command::Save),
            ("\x1b[115;10u", Command::SaveAs),
            ("\x1b[122;10u", Command::Redo),
            ("\x13", Command::Save),
        ] {
            let mut parser = Parser::default();
            let events: Vec<_> = sequence.bytes().flat_map(|b| parser.push(b)).collect();
            assert!(matches!(events.as_slice(),[Input::Command(found)] if *found==command));
        }
        let mut parser = Parser::default();
        let events: Vec<_> = b"\x1b[?1u\x1b[113;9:3u\x1b[<0;2;3M"
            .iter()
            .flat_map(|b| parser.push(*b))
            .collect();
        assert!(matches!(
            events.as_slice(),
            [
                Input::KeyboardSupported,
                Input::Pointer(MouseEvent {
                    column: 1,
                    row: 2,
                    ..
                })
            ]
        ));
    }
    /// Legacy mouse release, shifted Unicode keys, and slow split UTF-8 cannot become stray text.
    #[test]
    fn legacy_mouse_and_fragmented_utf8() {
        let mut parser = Parser::default();
        let events: Vec<_> = b"\x1b[M#!!\x1b[35;1;1M\x1b[97:65;2u"
            .iter()
            .flat_map(|byte| parser.push(*byte))
            .collect();
        assert!(
            matches!(events.as_slice(),[Input::Pointer(MouseEvent{kind:MouseEventKind::Up(MouseButton::Left),..}),Input::Pointer(MouseEvent{kind:MouseEventKind::Up(MouseButton::Left),..}),Input::Text(text)] if text=="A")
        );
        let mut reply = Parser::default();
        for byte in b"\x1b[?1" {
            assert!(reply.push(*byte).is_empty());
        }
        assert!(reply.expire().is_empty());
        assert!(matches!(
            reply.push(b'u').as_slice(),
            [Input::KeyboardSupported]
        ));
        let mut parser = Parser::default();
        assert!(parser.push(0xe7).is_empty());
        assert!(parser.expire().is_empty());
        assert!(parser.push(0x95).is_empty());
        assert!(matches!(parser.push(0x8c).as_slice(),[Input::Text(text)] if text=="界"));
    }

    /// Cancelled and invalid/unterminated pastes cannot become ordinary shortcut events.
    #[test]
    fn cancellation_drains_paste_without_emitting_text() {
        let mut parser = Parser::default();
        let events: Vec<_> = b"\x1b[200~".iter().flat_map(|b| parser.push(*b)).collect();
        let [Input::PasteStarted(cancel)] = events.as_slice() else {
            panic!()
        };
        cancel.cancel();
        let events: Vec<_> = b"abc\x11\x1b[201~"
            .iter()
            .flat_map(|b| parser.push(*b))
            .collect();
        assert!(matches!(events.as_slice(), [Input::Notice(_)]));
        assert!(parser.pending.is_empty());
        assert!(parser.paste.is_none());
    }
}
