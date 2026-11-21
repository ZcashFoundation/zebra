//! The text and JSON log formats.
//!
//! The JSON format writes one object per line, with these keys, in this order:
//!
//! - `timestamp`: RFC 3339 UTC time, with microseconds
//! - `level`: `TRACE`, `DEBUG`, `INFO`, `WARN`, or `ERROR`
//! - `severity`: the same level as a Google Cloud Logging severity
//! - `message`
//! - `target`: the module that logged the event
//! - the event's fields
//! - `spans`: the spans the event happened in, outermost first, if any
//! - `code.file.path` and `code.line.number`, if known
//! - `trace_id` and `span_id`, if the event is in a sampled OpenTelemetry trace
//!
//! An event field whose name is one of these keys is written as `field.<name>`.

use std::{
    cell::RefCell,
    fmt::{self, Write as _},
};

use tracing::{
    field::{Field, Visit},
    span, Event, Level, Subscriber,
};
use tracing_log::NormalizeEvent;
use tracing_subscriber::{
    field::RecordFields,
    fmt::{
        format::{self, DefaultFields, Format, Full, Writer},
        time::SystemTime,
        FmtContext, FormatEvent, FormatFields, FormattedFields,
    },
    registry::LookupSpan,
};

/// The longest field value written, in bytes after JSON escaping. Longer values are truncated.
const MAX_VALUE_LEN: usize = 4 * 1024;

/// The longest line written, in bytes.
///
/// Container runtimes split longer lines, which breaks them as JSON.
const MAX_LINE_LEN: usize = 16 * 1024 - 1;

/// Marks a truncated value.
const TRUNCATED: &str = "...[truncated]";

/// Integers further from zero than this are written as strings, because many JSON parsers,
/// including Google Cloud Logging's, read numbers as 64-bit floats.
const MAX_EXACT_INTEGER: u64 = 1 << 53;

/// The keys of every line. Event fields with these names are renamed.
const RESERVED_KEYS: &[&str] = &[
    "timestamp",
    "level",
    "severity",
    "message",
    "target",
    "spans",
    "code.file.path",
    "code.line.number",
    "trace_id",
    "span_id",
];

/// Formats events in the configured [`LogFormat`](super::LogFormat).
#[derive(Debug)]
pub enum EventFormat {
    /// Human-readable text.
    Text(Format<Full, SystemTime>),

    /// One JSON object per line.
    Json,
}

impl EventFormat {
    /// Returns the event format and its matching field format.
    pub fn new(format: super::LogFormat) -> (Self, FieldFormat) {
        match format {
            super::LogFormat::Text => (Self::Text(format::format()), FieldFormat::Text),
            super::LogFormat::Json => (Self::Json, FieldFormat::Json),
        }
    }
}

/// Formats span fields to match the [`EventFormat`].
///
/// Layers that store span fields, such as `ErrorLayer`, must use this format too, so the
/// fields are formatted once and later updates reach every layer.
#[derive(Copy, Clone, Debug)]
pub enum FieldFormat {
    /// `name=value` pairs.
    Text,

    /// The inside of a JSON object, such as `"name":"value","other":1`.
    Json,
}

impl<'writer> FormatFields<'writer> for FieldFormat {
    fn format_fields<R: RecordFields>(&self, writer: Writer<'writer>, fields: R) -> fmt::Result {
        match self {
            Self::Text => DefaultFields::new().format_fields(writer, fields),
            Self::Json => {
                let mut writer = writer;
                let mut visitor = JsonVisitor::new(&mut writer, Fields::Span, true);
                fields.record(&mut visitor);
                visitor.result
            }
        }
    }

    fn add_fields(
        &self,
        current: &'writer mut FormattedFields<Self>,
        fields: &span::Record<'_>,
    ) -> fmt::Result {
        let first = current.fields.is_empty();

        match self {
            Self::Text => {
                if !first {
                    current.fields.push(' ');
                }
                DefaultFields::new().format_fields(current.as_writer(), fields)
            }
            Self::Json => {
                let mut writer = current.as_writer();
                let mut visitor = JsonVisitor::new(&mut writer, Fields::Span, first);
                fields.record(&mut visitor);
                visitor.result
            }
        }
    }
}

impl<S> FormatEvent<S, FieldFormat> for EventFormat
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, FieldFormat>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        match self {
            Self::Text(format) => format.format_event(ctx, writer, event),
            Self::Json => {
                thread_local! {
                    static LINE: RefCell<String> = const { RefCell::new(String::new()) };
                }

                LINE.with(|line| {
                    // A field's `Debug` implementation can log, which formats another line on
                    // this thread while the buffer is borrowed.
                    let mut reentrant_line = String::new();
                    let mut borrowed = line.try_borrow_mut();
                    let line = match borrowed.as_mut() {
                        Ok(line) => &mut **line,
                        Err(_) => &mut reentrant_line,
                    };
                    line.clear();

                    write_json_line(line, ctx, event)?;
                    if line.len() > MAX_LINE_LEN {
                        line.clear();
                        write_truncated_json_line(line, event)?;
                    }

                    writer.write_str(line)?;
                    writeln!(writer)
                })
            }
        }
    }
}

/// Writes `event` as a JSON object, without a line break.
fn write_json_line<S>(
    line: &mut String,
    ctx: &FmtContext<'_, S, FieldFormat>,
    event: &Event<'_>,
) -> fmt::Result
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let normalized = event.normalized_metadata();
    let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());

    write_fixed_keys(line, metadata.level())?;

    // `message` comes first, but tracing doesn't guarantee that it is the first field.
    line.push_str(",\"message\":");
    let mut message = MessageVisitor::new(line);
    event.record(&mut message);
    message.finish()?;

    line.push_str(",\"target\":");
    write_json_str(line, metadata.target())?;

    let mut fields = JsonVisitor::new(line, Fields::Event, false);
    event.record(&mut fields);
    fields.result?;

    write_spans(line, ctx)?;

    if let Some(file) = metadata.file() {
        line.push_str(",\"code.file.path\":");
        write_json_str(line, file)?;
    }
    if let Some(number) = metadata.line() {
        write!(line, ",\"code.line.number\":{number}")?;
    }

    #[cfg(feature = "opentelemetry")]
    write_trace_ids(line, ctx)?;

    line.push('}');

    Ok(())
}

/// Writes the event's spans from the root, as `"spans":[...]`, unless it has none apart from
/// the global span.
fn write_spans<S>(line: &mut String, ctx: &FmtContext<'_, S, FieldFormat>) -> fmt::Result
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let spans = ctx
        .event_scope()
        .into_iter()
        .flat_map(|scope| scope.from_root())
        .filter(|span| !is_global_span(span.metadata()));

    let mut first = true;
    for span in spans {
        line.push_str(if first { ",\"spans\":[" } else { "," });
        first = false;

        line.push_str("{\"name\":");
        write_json_str(line, span.name())?;
        let extensions = span.extensions();
        if let Some(fields) = extensions.get::<FormattedFields<FieldFormat>>() {
            if !fields.is_empty() {
                line.push(',');
                line.push_str(fields);
            }
        }
        line.push('}');
    }
    if !first {
        line.push(']');
    }

    Ok(())
}

/// Writes an event that is too long for one line, keeping its fixed keys and the start of its
/// message.
fn write_truncated_json_line(line: &mut String, event: &Event<'_>) -> fmt::Result {
    let normalized = event.normalized_metadata();
    let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());

    write_fixed_keys(line, metadata.level())?;

    line.push_str(",\"message\":");
    let mut message = MessageVisitor::new(line);
    event.record(&mut message);
    message.finish()?;

    line.push_str(",\"target\":");
    write_json_str(line, metadata.target())?;
    line.push_str(",\"truncated\":true}");

    Ok(())
}

/// Writes the opening brace, `timestamp`, `level`, and `severity`.
fn write_fixed_keys(line: &mut String, level: &Level) -> fmt::Result {
    write!(
        line,
        "{{\"timestamp\":\"{}\",\"level\":\"{}\",\"severity\":\"{}\"",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ"),
        level.as_str(),
        severity(level),
    )
}

/// Returns the Google Cloud Logging severity for `level`.
///
/// Google Cloud Logging reads `severity` and has no `TRACE` or `WARN` severity.
pub fn severity(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        Level::INFO => "INFO",
        Level::DEBUG | Level::TRACE => "DEBUG",
    }
}

/// Returns `true` for the span Zebra enters for the life of the process.
///
/// Every event happens in this span, and its fields don't change, so writing it on each line
/// would only make lines longer.
fn is_global_span(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.name().is_empty() && metadata.target() == "zebrad::application"
}

/// Writes the IDs of the exported span closest to the event, if that span is sampled.
///
/// The OpenTelemetry layer stores each exported span's ID, but not whether it is sampled, so
/// the IDs come from the context the layer activates when an exported span is entered. That
/// context belongs to the event only if it is the closest exported span's own context, which
/// isn't the case for an event with an explicit parent or `parent: None`.
#[cfg(feature = "opentelemetry")]
fn write_trace_ids<S>(line: &mut String, ctx: &FmtContext<'_, S, FieldFormat>) -> fmt::Result
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OtelData;

    let Some(exported_span_id) = ctx
        .event_scope()
        .into_iter()
        .flatten()
        .find_map(|span| span.extensions().get::<OtelData>().map(OtelData::span_id))
        .flatten()
    else {
        return Ok(());
    };

    opentelemetry::Context::map_current(|context| {
        let span = context.span();
        let span_context = span.span_context();

        if span_context.span_id() == exported_span_id && span_context.is_sampled() {
            write!(
                line,
                ",\"trace_id\":\"{}\",\"span_id\":\"{}\"",
                span_context.trace_id(),
                exported_span_id,
            )
        } else {
            Ok(())
        }
    })
}

/// Returns a panic as one JSON log line, without a line break.
///
/// The line has the same stable keys as other JSON log lines, with `level` `ERROR` and
/// `severity` `CRITICAL`, and the panic in `exception.type`, `exception.message`, and
/// `exception.stacktrace`.
pub(crate) fn json_panic_line(panic_info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = panic_info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic with a non-string payload");
    let thread = std::thread::current();
    let stacktrace = std::backtrace::Backtrace::force_capture();

    let mut line = String::new();
    // Writing to a `String` can't fail.
    let _ = (|| {
        write!(
            line,
            "{{\"timestamp\":\"{}\",\"level\":\"ERROR\",\"severity\":\"CRITICAL\",\"message\":",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ"),
        )?;
        write_json_str(&mut line, message)?;
        line.push_str(",\"target\":\"panic\",\"thread\":");
        write_json_str(&mut line, thread.name().unwrap_or("unnamed"))?;
        line.push_str(",\"exception.type\":\"panic\",\"exception.message\":");
        write_json_str(&mut line, message)?;

        let mut location = String::new();
        if let Some(panic_location) = panic_info.location() {
            location.push_str(",\"code.file.path\":");
            write_json_str(&mut location, panic_location.file())?;
            write!(location, ",\"code.line.number\":{}", panic_location.line())?;
        }

        // The stack trace gets the rest of the line.
        const STACKTRACE_KEY: &str = ",\"exception.stacktrace\":\"";
        let budget = MAX_LINE_LEN
            .saturating_sub(line.len() + STACKTRACE_KEY.len() + location.len())
            .saturating_sub("\"}".len() + TRUNCATED.len());
        line.push_str(STACKTRACE_KEY);
        let mut escaper = JsonEscaper::new(&mut line, budget);
        let _ = write!(escaper, "{stacktrace}");
        escaper.finish()?;
        line.push('"');
        line.push_str(&location);
        line.push('}');

        fmt::Result::Ok(())
    })();

    line
}

/// Which fields a [`JsonVisitor`] is writing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Fields {
    /// An event's fields, after its message.
    Event,

    /// A span's fields, inside its object in `spans`.
    Span,
}

/// Writes fields as `"name":value` pairs, separated by commas.
struct JsonVisitor<'a, W: fmt::Write> {
    writer: &'a mut W,
    fields: Fields,
    first: bool,
    result: fmt::Result,
}

impl<'a, W: fmt::Write> JsonVisitor<'a, W> {
    /// Returns a visitor that writes a comma before every field, or before every field but the
    /// first if `first` is `true`.
    fn new(writer: &'a mut W, fields: Fields, first: bool) -> Self {
        Self {
            writer,
            fields,
            first,
            result: Ok(()),
        }
    }

    /// Writes `value` with `write_value` as the value of `field`.
    fn write(&mut self, field: &Field, write_value: impl FnOnce(&mut W) -> fmt::Result) {
        let name = field.name();

        if self.result.is_err() || (self.fields == Fields::Event && name == "message") {
            return;
        }

        // Fields added by the `log` crate adapter are part of the normalized metadata.
        if name.starts_with("log.") {
            return;
        }

        let renamed = match self.fields {
            Fields::Event => RESERVED_KEYS.contains(&name),
            Fields::Span => name == "name",
        };

        self.result = (|| {
            if !self.first {
                self.writer.write_char(',')?;
            }
            self.first = false;

            self.writer.write_char('"')?;
            if renamed {
                self.writer.write_str("field.")?;
            }
            JsonEscaper::new(self.writer, usize::MAX).write_str(name)?;
            self.writer.write_str("\":")?;

            write_value(self.writer)
        })();
    }
}

impl<W: fmt::Write> Visit for JsonVisitor<'_, W> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.write(field, |writer| {
            if value.is_finite() {
                write!(writer, "{value}")
            } else {
                write!(writer, "\"{value}\"")
            }
        });
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.write(field, |writer| {
            if value.unsigned_abs() <= MAX_EXACT_INTEGER {
                write!(writer, "{value}")
            } else {
                write!(writer, "\"{value}\"")
            }
        });
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.write(field, |writer| {
            if value <= MAX_EXACT_INTEGER {
                write!(writer, "{value}")
            } else {
                write!(writer, "\"{value}\"")
            }
        });
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.write(field, |writer| write!(writer, "{value}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.write(field, |writer| write_json_str(writer, value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.write(field, |writer| write_json_display(writer, value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.write(field, |writer| write_json_debug(writer, value));
    }
}

/// Writes an event's `message` field as a JSON string, or `""` if it has none.
struct MessageVisitor<'a> {
    line: &'a mut String,
    written: bool,
    result: fmt::Result,
}

impl<'a> MessageVisitor<'a> {
    fn new(line: &'a mut String) -> Self {
        Self {
            line,
            written: false,
            result: Ok(()),
        }
    }

    fn finish(self) -> fmt::Result {
        self.result?;
        if !self.written {
            self.line.push_str("\"\"");
        }
        Ok(())
    }
}

impl Visit for MessageVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" && !self.written {
            self.written = true;
            self.result = write_json_str(self.line, value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && !self.written {
            self.written = true;
            self.result = write_json_debug(self.line, value);
        }
    }
}

/// Writes `value` as a JSON string, truncated to [`MAX_VALUE_LEN`].
fn write_json_str(writer: &mut impl fmt::Write, value: &str) -> fmt::Result {
    write_json_display(writer, &value)
}

/// Writes `value`'s `Display` output as a JSON string, truncated to [`MAX_VALUE_LEN`].
fn write_json_display(writer: &mut impl fmt::Write, value: &dyn fmt::Display) -> fmt::Result {
    writer.write_char('"')?;
    let mut escaper = JsonEscaper::new(writer, MAX_VALUE_LEN);
    // A truncated value stops the `Display` implementation with an error.
    let _ = write!(escaper, "{value}");
    escaper.finish()?;
    writer.write_char('"')
}

/// Writes `value`'s `Debug` output as a JSON string, truncated to [`MAX_VALUE_LEN`].
fn write_json_debug(writer: &mut impl fmt::Write, value: &dyn fmt::Debug) -> fmt::Result {
    writer.write_char('"')?;
    let mut escaper = JsonEscaper::new(writer, MAX_VALUE_LEN);
    let _ = write!(escaper, "{value:?}");
    escaper.finish()?;
    writer.write_char('"')
}

/// Escapes text for the inside of a JSON string, and stops once the escaped text reaches a
/// byte limit.
struct JsonEscaper<'a, W: fmt::Write> {
    writer: &'a mut W,
    remaining: usize,
    truncated: bool,
    result: fmt::Result,
}

impl<'a, W: fmt::Write> JsonEscaper<'a, W> {
    fn new(writer: &'a mut W, limit: usize) -> Self {
        Self {
            writer,
            remaining: limit,
            truncated: false,
            result: Ok(()),
        }
    }

    /// Returns any error from the underlying writer, and marks a truncated value.
    fn finish(self) -> fmt::Result {
        self.result?;
        if self.truncated {
            self.writer.write_str(TRUNCATED)?;
        }
        Ok(())
    }
}

impl<W: fmt::Write> fmt::Write for JsonEscaper<'_, W> {
    /// Writes `text` with JSON string escapes, counting the escaped bytes against the limit.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.truncated || self.result.is_err() {
            return Err(fmt::Error);
        }

        // Escaping makes text at most 6 times longer, so short text skips the limit checks.
        if text.len().saturating_mul(6) <= self.remaining {
            self.remaining -= text.len();
            self.result = escape_json(self.writer, text, &mut self.remaining);
            return self.result;
        }

        self.result = (|| {
            let mut start = 0;

            for (index, byte) in text.bytes().enumerate() {
                let escape = match byte {
                    b'"' => Some("\\\""),
                    b'\\' => Some("\\\\"),
                    b'\n' => Some("\\n"),
                    b'\r' => Some("\\r"),
                    b'\t' => Some("\\t"),
                    0x00..=0x1f => Some(""),
                    _ => None,
                };
                let len = match escape {
                    Some("") => 6,
                    Some(escape) => escape.len(),
                    None => 1,
                };

                if len > self.remaining {
                    let mut end = index;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.writer.write_str(&text[start..end])?;
                    self.truncated = true;
                    return Ok(());
                }
                self.remaining -= len;

                if let Some(escape) = escape {
                    self.writer.write_str(&text[start..index])?;
                    if escape.is_empty() {
                        write!(self.writer, "\\u{byte:04x}")?;
                    } else {
                        self.writer.write_str(escape)?;
                    }
                    start = index + 1;
                }
            }

            self.writer.write_str(&text[start..])
        })();

        if self.truncated {
            Err(fmt::Error)
        } else {
            self.result
        }
    }
}

/// Writes `text` with JSON string escapes, and subtracts the bytes that escapes add from
/// `remaining`.
fn escape_json(writer: &mut impl fmt::Write, text: &str, remaining: &mut usize) -> fmt::Result {
    let mut start = 0;

    for (index, byte) in text.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0x00..=0x1f => "",
            _ => continue,
        };

        writer.write_str(&text[start..index])?;
        if escape.is_empty() {
            write!(writer, "\\u{byte:04x}")?;
            *remaining -= 5;
        } else {
            writer.write_str(escape)?;
            *remaining -= 1;
        }
        start = index + 1;
    }

    writer.write_str(&text[start..])
}

#[cfg(test)]
mod tests;
