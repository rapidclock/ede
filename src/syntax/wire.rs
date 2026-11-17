//! Private syntax subprocess protocol. All lengths are checked before allocation.

use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    Category, Fallback, LANGUAGES, Language,
    config::{Rules, SOURCE_LIMIT},
    engine::{self, Analysis},
    limits::DOCUMENT,
    projection::{GlyphRun, MAX_RANGES, Styled},
};

pub(super) const READY: u8 = b'R';
pub(super) const VALIDATE: u8 = b'V';
pub(super) const OPEN: u8 = b'O';
pub(super) const PROJECT: u8 = b'P';
pub(super) const DONE: u8 = b'D';
pub(super) const INVALID: u8 = b'I';
pub(super) const TIMEOUT: u8 = b'T';
pub(super) const BUDGET: u8 = b'B';
pub(super) const ANALYSIS: Duration = Duration::from_millis(250);

/// Serve dedicated pipe handles without acquiring the terminal or opening document paths.
pub fn serve(
    input: impl Read,
    output: impl Write,
    counters: impl Fn() -> (usize, usize),
) -> io::Result<()> {
    let mut input = input;
    let mut output = io::BufWriter::with_capacity(16 * 1024, output);
    send(&mut output, READY)?;
    let mut document = None;
    let mut parsed = None;
    loop {
        let operation = match byte(&mut input) {
            Ok(op) => op,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        match operation {
            VALIDATE => {
                let rules = read_rules(&mut input)?;
                send(&mut output, READY)?;
                if byte(&mut input)? != VALIDATE {
                    return Err(invalid());
                }
                let deadline = Instant::now() + ANALYSIS;
                if let Err((language, reason)) = engine::validate(&rules, deadline) {
                    output.write_all(&[INVALID, query_code(language)])?;
                    send_string(
                        &mut output,
                        reason.chars().take(256).collect::<String>().as_str(),
                    )?;
                    output.flush()?;
                } else {
                    send(&mut output, DONE)?;
                }
                return Ok(());
            }
            OPEN if parsed.is_none() && document.is_none() => {
                let language = language(byte(&mut input)?)?.ok_or_else(invalid)?;
                let rules = read_rules(&mut input)?;
                let len = number(&mut input)? as usize;
                if len as u64 > DOCUMENT {
                    return Err(invalid());
                }
                let mut data = vec![0; len];
                input.read_exact(&mut data)?;
                let source = String::from_utf8(data).map_err(|_| invalid())?;
                document = Some((source, language, rules));
                send(&mut output, READY)?;
            }
            PROJECT => {
                let timeout = Duration::from_millis(u64::from(number(&mut input)?));
                if timeout.is_zero() || timeout > ANALYSIS {
                    return Err(invalid());
                }
                let deadline = Instant::now() + timeout;
                let sequence = number(&mut input)?;
                let Ok(runs) = read_runs(&mut input) else {
                    return Err(invalid());
                };
                let result = (|| {
                    if Instant::now() >= deadline {
                        return Err(Fallback::Timeout);
                    }
                    if let Some((source, language, rules)) = document.take() {
                        if runs
                            .last()
                            .is_some_and(|run| run.end as usize > source.len())
                        {
                            return Err(Fallback::Output);
                        }
                        parsed = Some((
                            source.len(),
                            Analysis::new(source, language, &rules, deadline)?,
                        ));
                    }
                    let (len, analysis) = parsed.as_ref().ok_or(Fallback::Output)?;
                    if runs.last().is_some_and(|run| run.end as usize > *len) {
                        return Err(Fallback::Output);
                    }
                    analysis.project(&runs, deadline)
                })();
                match result {
                    Ok(spans) => {
                        let (peak, native_calls) = counters();
                        output.write_all(&[DONE])?;
                        send_number(&mut output, sequence)?;
                        send_number(&mut output, peak as u32)?;
                        send_number(&mut output, native_calls.min(u32::MAX as usize) as u32)?;
                        send_number(&mut output, spans.len() as u32)?;
                        for span in spans {
                            send_number(&mut output, span.start)?;
                            send_number(&mut output, span.end)?;
                            output.write_all(&[span.category as u8])?;
                        }
                        output.flush()?;
                    }
                    Err(error) => {
                        send(
                            &mut output,
                            match error {
                                Fallback::Timeout => TIMEOUT,
                                Fallback::Output => BUDGET,
                                _ => INVALID,
                            },
                        )?;
                        return Ok(());
                    }
                }
            }
            _ => return Err(invalid()),
        }
    }
}

/// Contract violation or truncated header, kept distinct from the user's document data.
pub(super) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid syntax worker message")
}

pub(super) fn byte(input: &mut impl Read) -> io::Result<u8> {
    let mut value = [0];
    input.read_exact(&mut value)?;
    Ok(value[0])
}

pub(super) fn number(input: &mut impl Read) -> io::Result<u32> {
    let mut value = [0; 4];
    input.read_exact(&mut value)?;
    Ok(u32::from_le_bytes(value))
}

pub(super) fn send_number(output: &mut impl Write, value: u32) -> io::Result<()> {
    output.write_all(&value.to_le_bytes())
}

pub(super) fn send(output: &mut impl Write, value: u8) -> io::Result<()> {
    output.write_all(&[value])?;
    output.flush()
}

fn read_string(input: &mut impl Read, available: &mut usize) -> io::Result<String> {
    let len = number(input)? as usize;
    *available = available.checked_sub(len).ok_or_else(invalid)?;
    let mut value = vec![0; len];
    input.read_exact(&mut value)?;
    String::from_utf8(value).map_err(|_| invalid())
}

fn send_string(output: &mut impl Write, value: &str) -> io::Result<()> {
    send_number(output, value.len().try_into().map_err(|_| invalid())?)?;
    output.write_all(value.as_bytes())
}

pub(super) fn language_code(language: Option<&Language>) -> u8 {
    language
        .and_then(|language| LANGUAGES.iter().position(|entry| entry == language))
        .map_or(0, |index| index as u8 + 1)
}

pub(super) fn language(code: u8) -> io::Result<Option<&'static Language>> {
    if code == 0 {
        Ok(None)
    } else {
        LANGUAGES
            .get(usize::from(code - 1))
            .map(Some)
            .ok_or_else(invalid)
    }
}

fn query_code(id: &str) -> u8 {
    if id == engine::INLINE {
        LANGUAGES.len() as u8 + 1
    } else {
        language_code(Language::by_id(id))
    }
}

fn query_id(code: u8) -> io::Result<&'static str> {
    if usize::from(code) == LANGUAGES.len() + 1 {
        Ok(engine::INLINE)
    } else {
        language(code)?
            .map(|language| language.id)
            .ok_or_else(invalid)
    }
}

pub(super) fn send_rules(output: &mut impl Write, rules: &Rules) -> io::Result<()> {
    send_number(output, rules.fences.len() as u32)?;
    for (alias, target) in &rules.fences {
        send_string(output, alias)?;
        output.write_all(&[language_code(*target)])?;
    }
    send_number(output, rules.queries.len() as u32)?;
    for (id, text) in &rules.queries {
        output.write_all(&[query_code(id)])?;
        send_string(output, text)?;
    }
    Ok(())
}

fn read_rules(input: &mut impl Read) -> io::Result<Arc<Rules>> {
    let mut available = SOURCE_LIMIT;
    let mut rules = Rules::default();
    let aliases = number(input)? as usize;
    if aliases > SOURCE_LIMIT {
        return Err(invalid());
    }
    for _ in 0..aliases {
        let alias = read_string(input, &mut available)?;
        let target = language(byte(input)?)?;
        if alias.is_empty() || rules.fences.insert(alias, target).is_some() {
            return Err(invalid());
        }
    }
    let queries = number(input)? as usize;
    if queries > LANGUAGES.len() + 1 {
        return Err(invalid());
    }
    for _ in 0..queries {
        let id = query_id(byte(input)?)?;
        if rules
            .queries
            .insert(id, read_string(input, &mut available)?)
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(Arc::new(rules))
}

pub(super) fn send_runs(output: &mut impl Write, runs: &[GlyphRun]) -> io::Result<()> {
    send_number(output, runs.len() as u32)?;
    for run in runs {
        send_number(output, run.start)?;
        send_number(output, run.end)?;
        send_number(output, run.stride)?;
    }
    output.flush()
}

fn read_runs(input: &mut impl Read) -> io::Result<Vec<GlyphRun>> {
    let count = number(input)? as usize;
    if count > MAX_RANGES {
        return Err(invalid());
    }
    let mut runs = Vec::with_capacity(count);
    let mut previous = 0;
    for _ in 0..count {
        let start = number(input)?;
        let end = number(input)?;
        let stride = number(input)?;
        if start < previous || start >= end || stride == 0 || !(end - start).is_multiple_of(stride)
        {
            return Err(invalid());
        }
        runs.push(GlyphRun { start, end, stride });
        previous = end;
    }
    Ok(runs)
}

pub(super) fn read_projection(
    input: &mut impl Read,
    expected: u32,
) -> io::Result<(Vec<Styled>, u32, u32)> {
    if number(input)? != expected {
        return Err(invalid());
    }
    let peak = number(input)?;
    let calls = number(input)?;
    let count = number(input)? as usize;
    if count > MAX_RANGES || peak > 64 * 1024 * 1024 {
        return Err(invalid());
    }
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let start = number(input)?;
        let end = number(input)?;
        let category = Category::from_byte(byte(input)?).ok_or_else(invalid)?;
        result.push(Styled {
            start,
            end,
            category,
        });
    }
    Ok((result, peak, calls))
}

pub(super) fn read_error(input: &mut impl Read) -> io::Result<(&'static str, String)> {
    let id = query_id(byte(input)?)?;
    Ok((id, read_string(input, &mut 1024)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_enforces_supplied_deadline_during_dispatch() {
        let mut start = Vec::new();
        start.extend_from_slice(&[OPEN, language_code(Some(&LANGUAGES[0]))]);
        send_rules(&mut start, &Rules::default()).unwrap();
        send_string(&mut start, "").unwrap();
        start.push(PROJECT);
        send_number(&mut start, 1).unwrap();

        // Block the child at descriptor reception after its configured deadline starts.
        let delayed = std::io::repeat(0).take(8);
        let delayed = Delayed(Some(delayed));
        let mut out = Vec::new();
        serve(start.as_slice().chain(delayed), &mut out, || (0, 0)).unwrap();
        assert_eq!(out, [READY, READY, TIMEOUT]);
    }

    struct Delayed<R>(Option<R>);

    impl<R: Read> Read for Delayed<R> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if let Some(reader) = &mut self.0 {
                std::thread::sleep(Duration::from_millis(10));
                reader.read(bytes).inspect(|&count| {
                    if count == 0 {
                        self.0 = None;
                    }
                })
            } else {
                Ok(0)
            }
        }
    }

    #[test]
    fn duplicate_viewport_reply_with_same_ranges_cannot_be_relabelled() {
        let mut bytes = Vec::new();
        for number in [4, 24, 9, 1, 0, 4] {
            send_number(&mut bytes, number).unwrap();
        }
        bytes.push(Category::Keyword as u8);
        assert!(read_projection(&mut bytes.as_slice(), 4).is_ok());
        assert!(read_projection(&mut bytes.as_slice(), 5).is_err());
        *bytes.last_mut().unwrap() = 255;
        assert!(read_projection(&mut bytes.as_slice(), 4).is_err());
    }
}
