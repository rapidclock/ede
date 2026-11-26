//! Disk-backed occurrence ranges with bounded, cancellable UI pages.
use super::*;
use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::fs::FileExt,
};

/// Maximum number of previews prepared at once.
pub(crate) const PAGE_SIZE: usize = 16;

/// A live match or an acknowledged edit in the search list.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub tab: TabId,
    pub line: u64,
    pub column: u64,
    pub before: String,
    pub matched: String,
    pub after: String,
    pub change: Option<&'static str>,
    found: Found,
    caret: u64,
}

/// Compact result metadata and the single page available to the UI.
#[derive(Clone, Debug, Default)]
pub(crate) struct Rows {
    pub generation: u64,
    pub count: u64,
    pub live: u64,
    pub current: Option<u64>,
    pub ordinal: Option<u64>,
    pub start: u64,
    pub page: Vec<Row>,
    pub error: Option<String>,
    pub feedback_incomplete: bool,
}

/// A page request and its optional navigation authority, assigned at admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageRequest {
    pub generation: u64,
    pub start: u64,
    pub jump: Option<u64>,
    pub navigation: Option<u64>,
}

/// Ranges written in canonical order. Feedback writes only actual text changes.
pub(super) struct Writer {
    file: Option<BufWriter<File>>,
    count: u64,
    first_match: u64,
    pub(super) failed: bool,
}

impl Writer {
    pub(super) fn new(enabled: bool) -> Self {
        let file = enabled.then(tempfile::tempfile).and_then(Result::ok);
        Self {
            failed: enabled && file.is_none(),
            file: file.map(BufWriter::new),
            count: 0,
            first_match: 0,
        }
    }

    pub(super) fn push(&mut self, range: &Range<u64>) {
        if let Some(file) = &mut self.file {
            let mut record = [0; 16];
            record[..8].copy_from_slice(&range.start.to_le_bytes());
            record[8..].copy_from_slice(&range.end.to_le_bytes());
            let available = !self.count.is_multiple_of(1 << 16)
                || nix::sys::statvfs::fstatvfs(file.get_ref()).is_ok_and(|space| {
                    u128::from(space.blocks_available()) * u128::from(space.fragment_size())
                        >= (513 << 20)
                });
            if available && file.write_all(&record).is_ok() {
                if self.count == 0 {
                    self.first_match = range.start;
                }
                self.count += 1;
            } else {
                self.file = None;
                self.failed = true;
            }
        }
    }

    pub(super) fn has_records(&self) -> bool {
        self.file.is_some() && self.count != 0
    }

    pub(super) fn finish(
        &mut self,
        tab: TabId,
        document: DocumentId,
        revision: document::Revision,
        original: Option<Snapshot>,
        change: Option<&'static str>,
    ) -> Option<Index> {
        let file = match self.file.take()?.into_inner() {
            Ok(file) => file,
            Err(_) => {
                self.failed = true;
                return None;
            }
        };
        (self.count != 0).then(|| Index {
            tab,
            document,
            revision,
            data: Arc::new(Data { file, original }),
            count: self.count,
            change,
            first: 0,
            first_match: self.first_match,
            before_change: None,
        })
    }
}

struct Data {
    file: File,
    original: Option<Snapshot>,
}

/// One tab's ordered segment. Receipt snapshots describe text before the write.
#[derive(Clone)]
pub(super) struct Index {
    pub(super) tab: TabId,
    document: DocumentId,
    pub(super) revision: document::Revision,
    data: Arc<Data>,
    count: u64,
    change: Option<&'static str>,
    first: u64,
    first_match: u64,
    pub(super) before_change: Option<u64>,
}

impl Index {
    fn range(&self, row: u64) -> Result<Range<u64>, Error> {
        let mut record = [0; 16];
        let offset = (self.first + row)
            .checked_mul(16)
            .ok_or(Error::InvalidRange)?;
        self.data.file.read_exact_at(&mut record, offset)?;
        let start = u64::from_le_bytes(record[..8].try_into().map_err(|_| Error::CorruptStorage)?);
        let end = u64::from_le_bytes(record[8..].try_into().map_err(|_| Error::CorruptStorage)?);
        Ok(start..end)
    }

    fn slice(&self, start: u64, end: u64) -> Self {
        Self {
            first: self.first + start,
            count: end - start,
            ..self.clone()
        }
    }

    /// Find the ordinal of a navigation candidate without scanning its document.
    fn ordinal(&self, start: u64) -> Result<Option<u64>, Error> {
        let (mut first, mut end) = (0, self.count);
        while first < end {
            let mid = first + (end - first) / 2;
            match self.range(mid)?.start.cmp(&start) {
                std::cmp::Ordering::Less => first = mid + 1,
                std::cmp::Ordering::Greater => end = mid,
                std::cmp::Ordering::Equal => return Ok(Some(mid)),
            }
        }
        Ok(None)
    }

    fn page(
        &self,
        snapshot: &Snapshot,
        range: Range<u64>,
        cancel: &Cancellation,
    ) -> Result<Vec<Row>, Error> {
        let mut text = Text::new(snapshot, cancel);
        let mut cache = Checkpoints::default();
        range
            .map(|index| {
                cancel.check()?;
                let range = self.range(index)?;
                let ceil = text.ceil(range.start)?;
                let caret = if ceil == range.start {
                    ceil
                } else {
                    text.previous(ceil)?
                };
                let (line, column) = text.coordinates(caret, &mut cache)?;
                let (line_start, line_end) = text.line_bounds(line)?;
                let preview_start = snapshot
                    .floor_scalar(ByteOffset(range.start.saturating_sub(24).max(line_start)))?;
                let window = snapshot.read_window_with_cancel(preview_start, 112, cancel)?;
                let value = window.text();
                let match_start = (range.start - preview_start.0) as usize;
                let match_end = value.floor_char_boundary(
                    (range.end - preview_start.0).min(value.len() as u64) as usize,
                );
                let preview_end = match_end.max(
                    value.floor_char_boundary(
                        line_end
                            .saturating_sub(preview_start.0)
                            .min(value.len() as u64) as usize,
                    ),
                );
                Ok(Row {
                    tab: self.tab,
                    line: line + 1,
                    column: column + 1,
                    before: format!(
                        "{}{}",
                        if preview_start.0 > line_start {
                            "…"
                        } else {
                            ""
                        },
                        crate::app::search::display(&value[..match_start])
                    ),
                    matched: crate::app::search::display(&value[match_start..match_end]),
                    after: format!(
                        "{}{}",
                        crate::app::search::display(&value[match_end..preview_end]),
                        if preview_start.0 + (preview_end as u64) < line_end
                            || preview_start.0 + (match_end as u64) < range.end
                        {
                            "…"
                        } else {
                            ""
                        }
                    ),
                    change: self.change,
                    found: Found {
                        tab: self.tab,
                        document: snapshot.document(),
                        revision: self.revision,
                        range,
                        extent: snapshot.len(),
                    },
                    caret,
                })
            })
            .collect()
    }
}

struct Pending {
    request: PageRequest,
    cancel: Cancellation,
    handle: JoinHandle<Result<Page, Error>>,
}

struct Page {
    rows: Vec<Row>,
    selected: Option<(u64, u64)>,
    jump_ordinal: Option<u64>,
    previous: Option<Found>,
}

/// A prepared and revalidated jump to an exact live match.
pub(super) struct Jump {
    pub(super) found: Found,
    pub(super) caret: u64,
    pub(super) row: u64,
    pub(super) ordinal: Option<u64>,
    pub(super) navigation: u64,
}

/// Bounded resident metadata for the submitted search and latest replacement.
#[derive(Default)]
pub(super) struct Results {
    pub(super) live: Vec<Index>,
    pub(super) changes: Vec<Index>,
    request: Option<PageRequest>,
    pending: Option<Pending>,
}

impl Results {
    pub(super) fn single_change_start(&self, tab: TabId) -> Option<u64> {
        self.changes
            .iter()
            .find(|index| index.tab == tab && index.count == 1)
            .map(|index| index.first_match)
    }

    fn ordered(&self, owner: &Coordinator) -> Vec<Index> {
        let mut ordered = Vec::new();
        for tab in &owner.tabs {
            let live = self.live.iter().find(|index| index.tab == tab.id);
            let change = self.changes.iter().find(|index| index.tab == tab.id);
            if let (Some(live), Some(change)) = (live, change)
                && let Some(split) = live.before_change
                && change.count == 1
            {
                ordered.push(live.slice(0, split));
                ordered.push(change.clone());
                ordered.push(live.slice(split, live.count));
            } else {
                ordered.extend(change.into_iter().chain(live).cloned());
            }
        }
        ordered.retain(|index| index.count != 0);
        ordered
    }

    fn valid(
        owner: &Coordinator,
        tab: TabId,
        document: DocumentId,
        revision: document::Revision,
    ) -> bool {
        owner.state(tab).is_some_and(|state| {
            !state.screen.loading
                && state.pending_edit.is_none()
                && state.screen.document == Some(document)
                && state
                    .info
                    .as_ref()
                    .is_some_and(|info| info.revision == revision)
        })
    }

    pub(super) fn prune(&mut self, owner: &Coordinator) -> bool {
        let before = self.live.len() + self.changes.len();
        for indices in [&mut self.live, &mut self.changes] {
            indices.retain(|index| {
                owner.state(index.tab).is_some_and(|state| {
                    !state.screen.loading
                        && state.screen.document == Some(index.document)
                        && state
                            .info
                            .as_ref()
                            .is_some_and(|info| info.revision == index.revision)
                })
            });
        }
        before != self.live.len() + self.changes.len()
    }

    pub(super) fn cancel(&mut self) {
        self.request = None;
        if let Some(pending) = &self.pending {
            pending.cancel.cancel();
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.cancel();
        if let Some(pending) = self.pending.take() {
            let _ = pending.handle.join();
        }
    }

    pub(super) fn reset_page(&mut self, rows: &mut Rows) {
        self.cancel();
        *rows = Rows {
            generation: rows.generation.wrapping_add(1),
            count: self
                .live
                .iter()
                .chain(&self.changes)
                .map(|index| index.count)
                .sum(),
            live: self.live.iter().map(|index| index.count).sum(),
            error: rows.error.take(),
            feedback_incomplete: rows.feedback_incomplete,
            ..Rows::default()
        };
    }

    pub(super) fn request(&mut self, request: PageRequest, rows: &Rows) {
        if request.generation != rows.generation
            || request.start >= rows.count
            || request.jump.is_some_and(|target| target >= rows.count)
        {
            return;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request == request)
        {
            return;
        }
        if request.jump.is_none() {
            if self.request.is_some_and(|queued| queued.jump.is_some()) {
                return;
            }
            if let Some(pending) = &self.pending
                && pending.request.jump.is_none()
            {
                pending.cancel.cancel();
            }
        } else {
            self.cancel();
        }
        self.request = Some(request);
    }

    pub(super) fn poll(
        &mut self,
        owner: &mut Coordinator,
        status: &mut Status,
        may_start: bool,
    ) -> Result<(bool, Option<Jump>), Error> {
        let mut dirty = false;
        let mut jump = None;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.handle.is_finished())
        {
            let pending = self.pending.take().ok_or(Error::CorruptStorage)?;
            let page = pending.handle.join().map_err(|_| Error::CorruptStorage)?;
            let request = pending.request;
            if !pending.cancel.is_cancelled() && request.generation == status.rows.generation {
                match page {
                    Ok(page) => {
                        status.rows.start = request.start;
                        status.rows.page = page.rows;
                        if page.previous == status.current {
                            status.rows.current = page.selected.map(|(selected, _)| selected);
                            status.rows.ordinal = page.selected.map(|(_, ordinal)| ordinal);
                        }
                        if let Some(navigation) = request.navigation
                            && navigation
                                == owner.view_cancel.search_navigation.load(Ordering::Acquire)
                            && let Some(index) = request.jump
                            && let Some(row) = index
                                .checked_sub(request.start)
                                .and_then(|local| status.rows.page.get(local as usize))
                            && row.change.is_none()
                            && Self::valid(owner, row.tab, row.found.document, row.found.revision)
                        {
                            jump = Some(Jump {
                                found: row.found.clone(),
                                caret: row.caret,
                                row: index,
                                ordinal: page.jump_ordinal,
                                navigation,
                            });
                        }
                    }
                    Err(error) => status.rows.error = Some(error.to_string()),
                }
                dirty = true;
            }
        }
        if may_start
            && self.pending.is_none()
            && let Some(request) = self.request.take()
        {
            let ordered = self.ordered(owner);
            let current = status.current.clone();
            let mut snapshots = Vec::new();
            let end = request.start.saturating_add(PAGE_SIZE as u64);
            let mut offset = 0;
            for index in &ordered {
                let next = offset + index.count;
                if index.change.is_none()
                    && offset < end
                    && next > request.start
                    && !snapshots.iter().any(|(tab, _)| *tab == index.tab)
                {
                    match owner.background_snapshot(index.tab, index.document) {
                        Ok(snapshot) if snapshot.revision() == index.revision => {
                            snapshots.push((index.tab, snapshot))
                        }
                        Ok(_) | Err(Error::StaleRevision | Error::Closed) => {
                            return Ok((true, None));
                        }
                        Err(error) => {
                            status.rows.error = Some(format!("Result page unavailable: {error}"));
                            return Ok((true, None));
                        }
                    }
                }
                offset = next;
            }
            let cancel = Cancellation::default();
            let token = cancel.clone();
            let handle = thread::Builder::new()
                .name("ede-search-page".into())
                .spawn(move || {
                    let mut rows = Vec::new();
                    let mut selected = None;
                    let mut jump_ordinal = None;
                    let mut offset = 0;
                    let mut live = 0;
                    for index in ordered {
                        token.check()?;
                        let next = offset + index.count;
                        if index.change.is_none()
                            && let Some(found) = &current
                            && found.tab == index.tab
                            && found.revision == index.revision
                            && let Some(local) = index.ordinal(found.range.start)?
                        {
                            selected = Some((offset + local, live + local + 1));
                        }
                        if let Some(target) = request.jump
                            && index.change.is_none()
                            && (offset..next).contains(&target)
                        {
                            jump_ordinal = Some(live + target - offset + 1);
                        }
                        if offset < end && next > request.start {
                            let snapshot = index
                                .data
                                .original
                                .as_ref()
                                .or_else(|| {
                                    snapshots
                                        .iter()
                                        .find(|(tab, _)| *tab == index.tab)
                                        .map(|(_, snapshot)| snapshot)
                                })
                                .ok_or(Error::CorruptStorage)?;
                            rows.extend(index.page(
                                snapshot,
                                request.start.saturating_sub(offset)..index.count.min(end - offset),
                                &token,
                            )?);
                        }
                        offset = next;
                        if index.change.is_none() {
                            live += index.count;
                        }
                    }
                    Ok(Page {
                        rows,
                        selected,
                        jump_ordinal,
                        previous: current,
                    })
                })?;
            self.pending = Some(Pending {
                request,
                cancel,
                handle,
            });
        }
        Ok((dirty, jump))
    }
}
