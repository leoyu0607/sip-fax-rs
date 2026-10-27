//! `{placeholder}` templates for file names, e-mail subjects and bodies.
//!
//! Placeholders: `{date}` (YYYYMMDD of `started_at`), `{time}` (HHMMSS), `{ani}`, `{dnis}`, `{id}`,
//! `{trunk}`, `{pages}`, `{call_id}`, `{status}`, `{duration}` (seconds), `{started_at}` and
//! `{ended_at}` (RFC 3339), `{resolution}`, `{tsi}` (remote station id). `{{` and `}}` produce
//! literal braces. Missing values render as `unknown` (`{ani}`, `{dnis}`, ...) or an empty string
//! for the time stamps. `{date}`/`{time}` use the UTC offset given to [`Template::render`].

use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use store::FaxRecord;

/// How values are made safe for their destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// File name: every character outside `[alphanumeric - _ . + ( )]` becomes `_`, leading dots
    /// are replaced (no `..`, no hidden files), length is capped (path traversal protection).
    FileName,
    /// Free text (subject, body): control characters become spaces (header injection protection).
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Lit(String),
    Var(Var),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Var {
    Date,
    Time,
    Ani,
    Dnis,
    Id,
    Trunk,
    Pages,
    CallId,
    Status,
    Duration,
    StartedAt,
    EndedAt,
    Resolution,
    Tsi,
}

impl Var {
    fn parse(name: &str) -> Option<Var> {
        Some(match name {
            "date" => Var::Date,
            "time" => Var::Time,
            "ani" => Var::Ani,
            "dnis" => Var::Dnis,
            "id" => Var::Id,
            "trunk" => Var::Trunk,
            "pages" => Var::Pages,
            "call_id" => Var::CallId,
            "status" => Var::Status,
            "duration" => Var::Duration,
            "started_at" => Var::StartedAt,
            "ended_at" => Var::EndedAt,
            "resolution" => Var::Resolution,
            "tsi" => Var::Tsi,
            _ => return None,
        })
    }
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(Vec<Seg>);

impl Template {
    /// Parse `src`; errors name the offending placeholder.
    pub fn parse(src: &str) -> Result<Template, String> {
        let mut segs = Vec::new();
        let mut lit = String::new();
        let mut it = src.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '{' if it.peek() == Some(&'{') => {
                    it.next();
                    lit.push('{');
                }
                '}' if it.peek() == Some(&'}') => {
                    it.next();
                    lit.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    loop {
                        match it.next() {
                            Some('}') => break,
                            Some(ch) => name.push(ch),
                            None => return Err(format!("unterminated placeholder in {src:?}")),
                        }
                    }
                    let var = Var::parse(&name)
                        .ok_or_else(|| format!("unknown placeholder {{{name}}} in {src:?}"))?;
                    if !lit.is_empty() {
                        segs.push(Seg::Lit(std::mem::take(&mut lit)));
                    }
                    segs.push(Seg::Var(var));
                }
                '}' => return Err(format!("unmatched '}}' in {src:?}")),
                c => lit.push(c),
            }
        }
        if !lit.is_empty() {
            segs.push(Seg::Lit(lit));
        }
        Ok(Template(segs))
    }

    /// Expand the template for `rec`.
    pub fn render(&self, rec: &FaxRecord, offset: FixedOffset, mode: Mode) -> String {
        let mut out = String::new();
        for s in &self.0 {
            match s {
                Seg::Lit(l) => out.push_str(l),
                Seg::Var(v) => {
                    let val = value(*v, rec, offset);
                    match mode {
                        Mode::FileName => out.push_str(&sanitize_name(&val)),
                        // Values never contain line breaks (header injection); literal template
                        // text may (multi-line bodies).
                        Mode::Text => {
                            out.extend(val.chars().map(|c| if c.is_control() { ' ' } else { c }))
                        }
                    }
                }
            }
        }
        match mode {
            Mode::FileName => sanitize_name(&out),
            Mode::Text => out
                .chars()
                .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
                .collect(),
        }
    }
}

fn local(t: DateTime<Utc>, offset: FixedOffset) -> DateTime<FixedOffset> {
    t.with_timezone(&offset)
}

fn value(v: Var, r: &FaxRecord, offset: FixedOffset) -> String {
    let unknown = |o: &Option<String>| match o.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "unknown".to_string(),
    };
    match v {
        Var::Date => local(r.started_at, offset).format("%Y%m%d").to_string(),
        Var::Time => local(r.started_at, offset).format("%H%M%S").to_string(),
        Var::Ani => unknown(&r.ani),
        Var::Dnis => unknown(&r.dnis),
        Var::Id => r.id.to_string(),
        Var::Trunk => r.trunk.clone(),
        Var::Pages => r.pages.to_string(),
        Var::CallId => r.call_id.clone(),
        Var::Status => r.fax_status.as_str().to_string(),
        Var::Duration => r.duration_sec.map(|d| d.to_string()).unwrap_or_default(),
        Var::StartedAt => local(r.started_at, offset).to_rfc3339_opts(SecondsFormat::Secs, false),
        Var::EndedAt => r
            .ended_at
            .map(|t| local(t, offset).to_rfc3339_opts(SecondsFormat::Secs, false))
            .unwrap_or_default(),
        Var::Resolution => r.resolution.clone().unwrap_or_default(),
        Var::Tsi => r.remote_ident.clone().unwrap_or_default(),
    }
}

/// Maximum file name length in bytes (leaves room for `.part` and a numeric suffix).
pub const MAX_NAME_BYTES: usize = 200;

/// Make `s` safe to use as one path component.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '(' | ')') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    // No leading dot: rules out "." / ".." / hidden files.
    while out.starts_with('.') {
        out.replace_range(..1, "_");
    }
    // ".." inside the name is harmless as a component, but keep names free of it anyway.
    while out.contains("..") {
        out = out.replace("..", "._");
    }
    if out.len() > MAX_NAME_BYTES {
        let mut cut = MAX_NAME_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    if out.is_empty() {
        out.push_str("fax");
    }
    out
}

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(test)]
mod tests;
