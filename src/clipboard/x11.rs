//! UTF-8 X11 CLIPBOARD transport for local Xorg and GNOME's Xwayland bridge.
use std::{
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::FileExt,
    },
    time::{Duration, Instant},
};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE,
    connection::Connection,
    protocol::{Event, xproto::*},
    rust_connection::{DefaultStream, RustConnection},
    wrapper::ConnectionExt as _,
};
use x11rb_protocol::{
    parse_display::{ConnectAddress, parse_display},
    xauth::get_auth,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const READ_WORDS: u32 = 16 << 10;
const MAX_REQUESTS: usize = 8;
const TRANSFER_IDLE: Duration = Duration::from_secs(15);

x11rb::atom_manager! {
    Atoms: AtomCookies {
        CLIPBOARD,
        UTF8_STRING,
        INCR,
        TARGETS,
        TEXT,
        TIMESTAMP,
        EDE_CLIPBOARD_TRANSFER,
        text_utf8: b"text/plain;charset=utf-8",
        text_plain: b"text/plain",
    }
}

/// A single helper connection and its unmapped selection window.
struct Clipboard {
    connection: RustConnection,
    window: Window,
    atoms: Atoms,
}

impl Clipboard {
    fn connect() -> Result<Self> {
        let display = parse_display(None)?;
        let address = display
            .connect_instruction()
            .find(|address| matches!(address, ConnectAddress::Socket(_)))
            .ok_or("The display has no local socket")?;
        let (stream, (family, address)) = DefaultStream::connect(&address)?;
        let peer = std::env::var("EDE_X11_PEER")?.parse::<i32>()?;
        if super::linux::socket_peer(stream.as_raw_fd()).map(|(pid, _)| pid) != Some(peer) {
            return Err("The local display changed".into());
        }
        let screen = display.screen.into();
        let (auth_name, auth_data) = get_auth(family, &address, display.display)
            .ok()
            .flatten()
            .unwrap_or_default();
        let connection =
            RustConnection::connect_to_stream_with_auth_info(stream, screen, auth_name, auth_data)?;
        let window = connection.generate_id()?;
        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                connection.setup().roots[screen].root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )?
            .check()?;
        let atoms = Atoms::new(&connection)?.reply()?;
        Ok(Self {
            connection,
            window,
            atoms,
        })
    }

    fn timestamp(&self) -> Result<Timestamp> {
        self.connection
            .change_property8(
                PropMode::REPLACE,
                self.window,
                self.atoms.EDE_CLIPBOARD_TRANSFER,
                AtomEnum::INTEGER,
                &[0],
            )?
            .check()?;
        loop {
            if let Event::PropertyNotify(event) = self.connection.wait_for_event()?
                && event.window == self.window
                && event.atom == self.atoms.EDE_CLIPBOARD_TRANSFER
                && event.state == Property::NEW_VALUE
            {
                self.connection
                    .delete_property(self.window, event.atom)?
                    .check()?;
                return Ok(event.time);
            }
        }
    }

    fn text_types(&self) -> [Atom; 3] {
        [
            self.atoms.UTF8_STRING,
            self.atoms.text_utf8,
            self.atoms.text_plain,
        ]
    }

    /// Acknowledge only after a server round trip confirms ownership.
    /// Keep the private source alive until another owner takes over.
    fn serve(&self, file: File, len: u64) -> Result<()> {
        let timestamp = self.timestamp()?;
        self.connection
            .set_selection_owner(self.window, self.atoms.CLIPBOARD, timestamp)?
            .check()?;
        if self
            .connection
            .get_selection_owner(self.atoms.CLIPBOARD)?
            .reply()?
            .owner
            != self.window
        {
            return Err("Could not claim desktop clipboard".into());
        }
        {
            let mut output = io::stdout().lock();
            output.write_all(&[1])?;
            output.flush()?;
        }
        let chunk = (usize::from(self.connection.setup().maximum_request_length) * 4)
            .saturating_sub(128)
            .clamp(1024, 64 << 10);
        let mut buffer = vec![0u8; chunk];
        let mut transfers = Vec::<Transfer>::new();
        let mut owns_clipboard = true;
        loop {
            // Watchers are bounded and released when clients abandon their transfer.
            let now = Instant::now();
            let mut index = 0;
            while index < transfers.len() {
                if transfers[index].expires <= now {
                    let window = transfers.swap_remove(index).window;
                    self.stop_watching(window, &transfers);
                } else {
                    index += 1;
                }
            }
            if !owns_clipboard && transfers.is_empty() {
                break;
            }
            let Some(event) = self.connection.poll_for_event()? else {
                let mut socket = [PollFd::new(
                    self.connection.stream().as_fd(),
                    PollFlags::POLLIN,
                )];
                let timeout = if transfers.is_empty() {
                    PollTimeout::NONE
                } else {
                    PollTimeout::from(1000u16)
                };
                match poll(&mut socket, timeout) {
                    Ok(_) | Err(nix::errno::Errno::EINTR) => continue,
                    Err(error) => return Err(error.into()),
                }
            };
            match event {
                Event::SelectionClear(clear) if clear.selection == self.atoms.CLIPBOARD => {
                    owns_clipboard = false;
                }
                Event::SelectionRequest(request) if owns_clipboard => {
                    let property = if request.property == NONE {
                        request.target
                    } else {
                        request.property
                    };
                    let published = request.selection == self.atoms.CLIPBOARD
                        && (request.time == CURRENT_TIME
                            || request.time.wrapping_sub(timestamp) < 1 << 31)
                        && self
                            .respond(
                                request,
                                property,
                                &file,
                                len,
                                timestamp,
                                &mut buffer,
                                &mut transfers,
                            )
                            .is_ok();
                    let notify = SelectionNotifyEvent {
                        response_type: SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: request.time,
                        requestor: request.requestor,
                        selection: request.selection,
                        target: request.target,
                        property: if published { property } else { NONE },
                    };
                    // A closed requestor does not end ownership for other clients.
                    let _ = self
                        .connection
                        .send_event(false, request.requestor, EventMask::NO_EVENT, notify)?
                        .check();
                }
                Event::DestroyNotify(event) => {
                    transfers.retain(|transfer| transfer.window != event.window)
                }
                Event::PropertyNotify(event) if event.state == Property::DELETE => {
                    if let Some(index) = transfers.iter().position(|transfer| {
                        transfer.window == event.window && transfer.property == event.atom
                    }) {
                        let finished = self
                            .serve_chunk(&mut transfers[index], &file, len, &mut buffer)
                            .unwrap_or(true);
                        if finished {
                            self.stop_watching(transfers.swap_remove(index).window, &transfers);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Each response shares the single immutable source and bounded transfer state"
    )]
    fn respond(
        &self,
        request: SelectionRequestEvent,
        property: Atom,
        file: &File,
        len: u64,
        timestamp: Timestamp,
        buffer: &mut [u8],
        transfers: &mut Vec<Transfer>,
    ) -> Result<()> {
        if transfers
            .iter()
            .any(|transfer| transfer.window == request.requestor && transfer.property == property)
        {
            return Err("Clipboard request already active".into());
        }
        let target = request.target;
        let window = request.requestor;
        if target == self.atoms.TARGETS {
            let [utf8, text_utf8, text_plain] = self.text_types();
            let advertised = [
                self.atoms.TARGETS,
                self.atoms.TIMESTAMP,
                self.atoms.TEXT,
                utf8,
                text_utf8,
                text_plain,
            ];
            self.connection
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    property,
                    AtomEnum::ATOM,
                    &advertised,
                )?
                .check()?;
        } else if target == self.atoms.TIMESTAMP {
            self.connection
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    property,
                    AtomEnum::INTEGER,
                    &[timestamp],
                )?
                .check()?;
        } else if self.text_types().contains(&target) || target == self.atoms.TEXT {
            let format = if target == self.atoms.TEXT {
                self.atoms.UTF8_STRING
            } else {
                target
            };
            if len <= buffer.len() as u64 {
                let bytes = &mut buffer[..len as usize];
                file.read_exact_at(bytes, 0)?;
                self.connection
                    .change_property8(PropMode::REPLACE, window, property, format, bytes)?
                    .check()?;
            } else {
                if transfers.len() >= MAX_REQUESTS {
                    return Err("Clipboard request limit reached".into());
                }
                self.connection
                    .change_window_attributes(
                        window,
                        &ChangeWindowAttributesAux::new()
                            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
                    )?
                    .check()?;
                if let Err(error) = self
                    .connection
                    .change_property32(
                        PropMode::REPLACE,
                        window,
                        property,
                        self.atoms.INCR,
                        &[len.min(u64::from(u32::MAX)) as u32],
                    )?
                    .check()
                {
                    self.stop_watching(window, transfers);
                    return Err(error.into());
                }
                transfers.push(Transfer {
                    window,
                    property,
                    format,
                    offset: 0,
                    expires: Instant::now() + TRANSFER_IDLE,
                });
            }
        } else {
            return Err("Clipboard format unavailable".into());
        }
        Ok(())
    }

    fn serve_chunk(
        &self,
        transfer: &mut Transfer,
        file: &File,
        len: u64,
        buffer: &mut [u8],
    ) -> Result<bool> {
        let count = (len - transfer.offset).min(buffer.len() as u64) as usize;
        let bytes = &mut buffer[..count];
        file.read_exact_at(bytes, transfer.offset)?;
        self.connection
            .change_property8(
                PropMode::REPLACE,
                transfer.window,
                transfer.property,
                transfer.format,
                bytes,
            )?
            .check()?;
        transfer.offset += count as u64;
        transfer.expires = Instant::now() + TRANSFER_IDLE;
        Ok(count == 0)
    }

    fn stop_watching(&self, window: Window, transfers: &[Transfer]) {
        if !transfers.iter().any(|transfer| transfer.window == window)
            && let Ok(cookie) = self.connection.change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
            )
        {
            // The requestor can close between the final property and unwatch.
            let _ = cookie.check();
        }
    }

    fn get(&self, offset: u32) -> Result<GetPropertyReply> {
        Ok(self
            .connection
            .get_property(
                true,
                self.window,
                self.atoms.EDE_CLIPBOARD_TRANSFER,
                AtomEnum::ANY,
                offset,
                READ_WORDS,
            )?
            .reply()?)
    }

    fn emit(&self, mut data: GetPropertyReply, output: &mut impl Write) -> Result<u64> {
        let format = data.type_;
        let mut written = 0u64;
        loop {
            if data.type_ != format || !self.text_types().contains(&format) || data.format != 8 {
                return Err("Desktop clipboard did not return UTF-8 text".into());
            }
            output.write_all(&data.value)?;
            written = written
                .checked_add(data.value.len() as u64)
                .ok_or("Clipboard overflow")?;
            if data.bytes_after == 0 {
                return Ok(written);
            }
            if data.value.is_empty() || !written.is_multiple_of(4) {
                return Err("Desktop clipboard property changed".into());
            }
            data = self.get(u32::try_from(written / 4)?)?;
        }
    }

    /// Request text only for explicit Paste. Read the server property in bounded
    /// slices and acknowledge each INCR property by deleting it after consumption.
    fn read(&self) -> Result<bool> {
        let property = self.atoms.EDE_CLIPBOARD_TRANSFER;
        let timestamp = self.timestamp()?;
        let mut output = io::stdout().lock();
        for target in self.text_types() {
            self.connection
                .convert_selection(
                    self.window,
                    self.atoms.CLIPBOARD,
                    target,
                    property,
                    timestamp,
                )?
                .check()?;
            let response = loop {
                if let Event::SelectionNotify(event) = self.connection.wait_for_event()?
                    && event.requestor == self.window
                    && event.selection == self.atoms.CLIPBOARD
                    && event.target == target
                    && event.time == timestamp
                {
                    break event;
                }
            };
            if response.property == NONE {
                continue;
            }
            if response.property != property {
                return Err("Desktop clipboard reported an unexpected property".into());
            }
            let first = self.get(0)?;
            if first.type_ != self.atoms.INCR {
                self.emit(first, &mut output)?;
                output.flush()?;
                return Ok(true);
            }
            let declared = first
                .value32()
                .and_then(|mut sizes| sizes.next())
                .filter(|_| first.bytes_after == 0 && first.value_len == 1)
                .ok_or("Desktop clipboard did not start a valid transfer")?;
            let mut received = 0u64;
            loop {
                if let Event::PropertyNotify(event) = self.connection.wait_for_event()?
                    && event.window == self.window
                    && event.atom == property
                    && event.state == Property::NEW_VALUE
                {
                    let count = self.emit(self.get(0)?, &mut output)?;
                    if count == 0 {
                        if received < u64::from(declared) {
                            return Err("Desktop clipboard transfer ended early".into());
                        }
                        output.flush()?;
                        return Ok(true);
                    }
                    received = received.checked_add(count).ok_or("Clipboard overflow")?;
                }
            }
        }
        Ok(false)
    }
}

/// One requestor's progress in the ICCCM INCR handshake.
struct Transfer {
    window: Window,
    property: Atom,
    format: Atom,
    offset: u64,
    expires: Instant,
}

/// Run a separately cancellable helper against the positively associated peer.
pub fn worker(write: bool) -> u8 {
    let result = (|| {
        let clipboard = Clipboard::connect()?;
        if !write {
            return Ok(if clipboard.read()? { 0 } else { 3 });
        }
        let file = File::from(nix::unistd::dup(io::stdin())?);
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Ok(1);
        }
        clipboard.serve(file, metadata.len())?;
        Ok::<_, Box<dyn std::error::Error>>(0)
    })();
    result.unwrap_or(1)
}
