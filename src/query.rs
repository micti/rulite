//! Request parser: PostgREST query-string and header syntax → AST.
//! Pure functions, no DB access. Identifiers are kept as written; the SQL
//! builder (M3) validates every one against the schema cache.

// Consumed by the SQL builder (M3), writes (M4) and responses (M5).
#![allow(dead_code)]

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(ParseError(msg.into()))
}

#[derive(Debug, PartialEq)]
pub struct Query {
    /// `select`; `[Star]` when absent or empty.
    pub select: Vec<SelectItem>,
    /// Filters and modifiers per resource. The root scope (empty path) is
    /// always first; embedded scopes follow in order of first mention.
    pub scopes: Vec<Scope>,
    /// `on_conflict=cols` for upserts.
    pub on_conflict: Option<Vec<String>>,
    /// `columns=cols`, sent by supabase-js on bulk insert.
    pub columns: Option<Vec<String>>,
}

#[derive(Debug, PartialEq)]
pub enum SelectItem {
    Star,
    Column { alias: Option<String>, name: String },
    Embed(Embed),
}

#[derive(Debug, PartialEq)]
pub struct Embed {
    pub alias: Option<String>,
    /// Target table (or relation) name.
    pub relation: String,
    /// Disambiguation hint from `rel!hint`, e.g. a foreign key column.
    pub hint: Option<String>,
    /// `rel!inner`: drop parent rows without a match.
    pub inner: bool,
    /// `rel!one` / `rel!many`: force the to-one or to-many direction, e.g. on
    /// a self reference (`children:area!parent_id!many`).
    pub to_many: Option<bool>,
    /// Empty for `rel()`, which embeds nothing but can still filter with `!inner`.
    pub select: Vec<SelectItem>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Scope {
    /// Embed names (alias when given) from the root, e.g. `["edition"]`.
    pub path: Vec<String>,
    /// AND-ed together.
    pub filters: Vec<Condition>,
    pub order: Vec<OrderTerm>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub enum Condition {
    Filter { column: String, negated: bool, op: Op },
    Logic { negated: bool, op: LogicOp, conditions: Vec<Condition> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
}

#[derive(Debug, PartialEq)]
pub enum Op {
    /// Binary comparison against one value (bound as a parameter).
    Cmp(Cmp, String),
    Is(IsValue),
    In(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    /// Pattern with `*` already turned into `%`.
    Like,
    Ilike,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsValue {
    Null,
    True,
    False,
}

#[derive(Debug, PartialEq)]
pub struct OrderTerm {
    pub column: String,
    pub descending: bool,
    /// `None` leaves the placement to the SQL builder.
    pub nulls: Option<Nulls>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nulls {
    First,
    Last,
}

impl Query {
    pub fn root(&self) -> &Scope {
        &self.scopes[0]
    }

    pub fn scope(&self, path: &[&str]) -> Option<&Scope> {
        self.scopes.iter().find(|s| s.path.iter().map(String::as_str).eq(path.iter().copied()))
    }

    fn scope_mut(&mut self, path: &[&str]) -> &mut Scope {
        let i = match self.scopes.iter().position(|s| s.path.iter().map(String::as_str).eq(path.iter().copied())) {
            Some(i) => i,
            None => {
                let path = path.iter().map(|s| s.to_string()).collect();
                self.scopes.push(Scope { path, ..Scope::default() });
                self.scopes.len() - 1
            }
        };
        &mut self.scopes[i]
    }
}

/// Parses a raw (still percent-encoded) query string.
pub fn parse_query(raw: &str) -> Result<Query> {
    let mut q =
        Query { select: vec![SelectItem::Star], scopes: vec![Scope::default()], on_conflict: None, columns: None };
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let (key, value) = (decode(key)?, decode(value)?);
        match key.as_str() {
            "select" => q.select = parse_select(&value)?,
            "on_conflict" => q.on_conflict = Some(parse_column_list(&value)?),
            "columns" => q.columns = Some(parse_column_list(&value)?),
            _ => apply_param(&mut q, &key, &value)?,
        }
    }
    Ok(q)
}

/// A filter or modifier, optionally prefixed by an embed path:
/// `col`, `edition.language`, `edition.order`, `not.or`, `edition.not.and`.
fn apply_param(q: &mut Query, key: &str, value: &str) -> Result<()> {
    let mut path: Vec<&str> = key.split('.').collect();
    let last = path.pop().unwrap_or_default();
    let negated = matches!(last, "or" | "and") && path.last() == Some(&"not");
    if negated {
        path.pop();
    }
    if last.is_empty() || path.iter().any(|s| s.is_empty()) {
        return err(format!("invalid parameter name '{key}'"));
    }

    let scope = q.scope_mut(&path);
    match last {
        "order" => scope.order = parse_order(value)?,
        "limit" => scope.limit = Some(parse_u64(key, value)?),
        "offset" => scope.offset = Some(parse_u64(key, value)?),
        "or" | "and" => {
            let op = if last == "or" { LogicOp::Or } else { LogicOp::And };
            scope.filters.push(Condition::Logic { negated, op, conditions: parse_conditions(value)? });
        }
        column => {
            let (negated, op) = parse_op(value, false)?;
            scope.filters.push(Condition::Filter { column: column.to_string(), negated, op });
        }
    }
    Ok(())
}

fn parse_u64(key: &str, value: &str) -> Result<u64> {
    value.parse().or_else(|_| err(format!("invalid {key}: '{value}'")))
}

/// `[not.]op.value`. Inside logic trees a scalar value may be double-quoted.
fn parse_op(s: &str, in_tree: bool) -> Result<(bool, Op)> {
    let (negated, s) = match s.strip_prefix("not.") {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let Some((name, value)) = s.split_once('.') else {
        return err(format!("failed to parse filter '{s}', expected operator.value"));
    };
    let scalar = |v: &str| if in_tree { unquote(v) } else { Ok(v.to_string()) };
    let cmp = match name {
        "eq" => Cmp::Eq,
        "neq" => Cmp::Neq,
        "gt" => Cmp::Gt,
        "gte" => Cmp::Gte,
        "lt" => Cmp::Lt,
        "lte" => Cmp::Lte,
        "like" => Cmp::Like,
        "ilike" => Cmp::Ilike,
        "is" => {
            let v = match scalar(value)?.to_ascii_lowercase().as_str() {
                "null" => IsValue::Null,
                "true" => IsValue::True,
                "false" => IsValue::False,
                v => return err(format!("invalid value for is: '{v}', expected null, true or false")),
            };
            return Ok((negated, Op::Is(v)));
        }
        "in" => return Ok((negated, Op::In(parse_list(value)?))),
        _ => return err(format!("unknown operator '{name}'")),
    };
    let mut value = scalar(value)?;
    if matches!(cmp, Cmp::Like | Cmp::Ilike) {
        value = value.replace('*', "%");
    }
    Ok((negated, Op::Cmp(cmp, value)))
}

/// `(a,b,"c,d")` → `["a", "b", "c,d"]`.
fn parse_list(s: &str) -> Result<Vec<String>> {
    let inner = parenthesized(s)?;
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    split_top(inner)?.into_iter().map(unquote).collect()
}

/// `(cond,cond,...)` where each cond is `col.[not.]op.value` or a nested
/// `[not.]and(...)` / `[not.]or(...)`.
fn parse_conditions(s: &str) -> Result<Vec<Condition>> {
    let inner = parenthesized(s)?;
    if inner.is_empty() {
        return err("empty logic tree");
    }
    split_top(inner)?.into_iter().map(parse_condition).collect()
}

fn parse_condition(s: &str) -> Result<Condition> {
    let (negated, rest) = match s.strip_prefix("not.") {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    for (name, op) in [("or", LogicOp::Or), ("and", LogicOp::And)] {
        if let Some(tree) = rest.strip_prefix(name).filter(|t| t.starts_with('(')) {
            return Ok(Condition::Logic { negated, op, conditions: parse_conditions(tree)? });
        }
    }
    match s.split_once('.') {
        Some((column, op)) if !column.is_empty() => {
            let (negated, op) = parse_op(op, true)?;
            Ok(Condition::Filter { column: column.to_string(), negated, op })
        }
        _ => err(format!("failed to parse logic tree item '{s}'")),
    }
}

/// Strips one pair of outer parentheses.
fn parenthesized(s: &str) -> Result<&str> {
    s.strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| ParseError(format!("expected '(...)', got '{s}'")))
}

/// Splits on commas outside parentheses and double quotes.
fn split_top(s: &str) -> Result<Vec<&str>> {
    let (mut parts, mut start, mut depth) = (Vec::new(), 0, 0u32);
    let (mut quoted, mut escaped) = (false, false);
    for (i, c) in s.char_indices() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' => depth += 1,
            ')' if depth == 0 => return err(format!("unbalanced ')' in '{s}'")),
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if quoted || depth != 0 {
        return err(format!("unbalanced quotes or parentheses in '{s}'"));
    }
    parts.push(&s[start..]);
    Ok(parts)
}

/// `"a\"b"` → `a"b`; unquoted text is returned as is.
fn unquote(s: &str) -> Result<String> {
    let Some(body) = s.strip_prefix('"') else {
        return Ok(s.to_string());
    };
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next().ok_or_else(|| ParseError(format!("dangling escape in {s}")))?),
            '"' if chars.as_str().is_empty() => return Ok(out),
            '"' => return err(format!("unexpected text after closing quote in {s}")),
            c => out.push(c),
        }
    }
    err(format!("unterminated quote in {s}"))
}

/// `title.asc,created_at.desc.nullslast`.
fn parse_order(s: &str) -> Result<Vec<OrderTerm>> {
    s.split(',')
        .map(|term| {
            let mut parts = term.trim().split('.');
            let column = parts.next().unwrap_or_default();
            if column.is_empty() {
                return err(format!("invalid order term '{term}'"));
            }
            let mut t = OrderTerm { column: column.to_string(), descending: false, nulls: None };
            for p in parts {
                match p {
                    "asc" => t.descending = false,
                    "desc" => t.descending = true,
                    "nullsfirst" => t.nulls = Some(Nulls::First),
                    "nullslast" => t.nulls = Some(Nulls::Last),
                    _ => return err(format!("invalid order modifier '{p}' in '{term}'")),
                }
            }
            Ok(t)
        })
        .collect()
}

/// `a,b` or `"a","b"`.
fn parse_column_list(s: &str) -> Result<Vec<String>> {
    split_top(s)?
        .into_iter()
        .map(|c| match unquote(c.trim())? {
            c if c.is_empty() => err(format!("empty column name in '{s}'")),
            c => Ok(c),
        })
        .collect()
}

/// Percent-decoding with `+` as space (form encoding, as `URLSearchParams` sends).
fn decode(s: &str) -> Result<String> {
    if !s.bytes().any(|b| b == b'%' || b == b'+') {
        return Ok(s.to_string());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = |j: usize| b.get(j).and_then(|c| (*c as char).to_digit(16));
                match (hex(i + 1), hex(i + 2)) {
                    (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                    _ => return err(format!("invalid percent-encoding in '{s}'")),
                }
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).or_else(|_| err(format!("invalid UTF-8 in '{s}'")))
}

/// `select` value: `*`, `col`, `alias:col`, `[alias:]rel[!hint][!inner](...)`.
pub fn parse_select(s: &str) -> Result<Vec<SelectItem>> {
    if s.trim().is_empty() {
        return Ok(vec![SelectItem::Star]);
    }
    let mut c = Cursor { s, pos: 0 };
    let items = c.items()?;
    c.skip_ws();
    if c.pos != s.len() {
        return err(format!("unexpected '{}' at position {} in select", &s[c.pos..], c.pos));
    }
    Ok(items)
}

struct Cursor<'a> {
    s: &'a str,
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek().filter(|c| c.is_whitespace()) {
            self.pos += c.len_utf8();
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        let hit = self.peek() == Some(c);
        if hit {
            self.pos += c.len_utf8();
        }
        hit
    }

    fn items(&mut self) -> Result<Vec<SelectItem>> {
        let mut items = vec![self.item()?];
        while self.eat(',') {
            items.push(self.item()?);
        }
        Ok(items)
    }

    fn item(&mut self) -> Result<SelectItem> {
        if self.eat('*') {
            return Ok(SelectItem::Star);
        }
        let first = self.ident()?;
        let (alias, name) = if self.eat(':') {
            if self.peek() == Some(':') {
                return err(format!("casts are not supported ('{first}::')"));
            }
            (Some(first), self.ident()?)
        } else {
            (None, first)
        };

        let (mut hint, mut inner, mut to_many) = (None, false, None);
        while self.eat('!') {
            match self.ident()?.as_str() {
                "inner" => inner = true,
                "left" => inner = false,
                "one" => to_many = Some(false),
                "many" => to_many = Some(true),
                h if hint.is_none() => hint = Some(h.to_string()),
                h => return err(format!("more than one hint on '{name}' ('{h}')")),
            }
        }

        if !self.eat('(') {
            if hint.is_some() || inner || to_many.is_some() {
                return err(format!("'!' hints are only valid on embedded resources ('{name}')"));
            }
            return Ok(SelectItem::Column { alias, name });
        }
        let select = if self.eat(')') {
            Vec::new()
        } else {
            let items = self.items()?;
            if !self.eat(')') {
                return err(format!("expected ')' at position {} in select", self.pos));
            }
            items
        };
        Ok(SelectItem::Embed(Embed { alias, relation: name, hint, inner, to_many, select }))
    }

    /// Bare name (up to a delimiter) or a double-quoted one.
    fn ident(&mut self) -> Result<String> {
        self.skip_ws();
        let rest = &self.s[self.pos..];
        let name = if let Some(body) = rest.strip_prefix('"') {
            let Some(end) = body.find('"') else {
                return err(format!("unterminated quote at position {} in select", self.pos));
            };
            self.pos += end + 2;
            &body[..end]
        } else {
            let end = rest.find(|c: char| ",():!*\"".contains(c) || c.is_whitespace()).unwrap_or(rest.len());
            self.pos += end;
            &rest[..end]
        };
        if name.is_empty() {
            return err(format!("expected a name at position {} in select", self.pos));
        }
        Ok(name.to_string())
    }
}

/// Request headers that shape the response.
#[derive(Debug, Default, PartialEq)]
pub struct Headers {
    pub prefer: Prefer,
    pub range: Option<Range>,
    /// `Accept: application/vnd.pgrst.object+json`.
    pub single_object: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct Prefer {
    pub return_: Option<Return>,
    pub count: Option<Count>,
    pub resolution: Option<Resolution>,
    /// `missing=default`: absent columns in bulk inserts take their default.
    pub missing_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Return {
    Minimal,
    HeadersOnly,
    Representation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    Exact,
    Planned,
    Estimated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    MergeDuplicates,
    IgnoreDuplicates,
}

/// `Range: 0-9` (or `items=0-9`); `10-` means from offset 10, no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub offset: u64,
    pub limit: Option<u64>,
}

impl Headers {
    /// `prefer` takes every `Prefer` header value; the others take the first.
    pub fn parse<'a>(
        prefer: impl IntoIterator<Item = &'a str>,
        range: Option<&str>,
        accept: Option<&str>,
    ) -> Result<Headers> {
        Ok(Headers {
            prefer: parse_prefer(prefer),
            range: range.map(parse_range).transpose()?,
            single_object: accept.is_some_and(wants_object),
        })
    }
}

/// Unknown preferences are ignored, like PostgREST's lenient handling.
pub fn parse_prefer<'a>(values: impl IntoIterator<Item = &'a str>) -> Prefer {
    let mut p = Prefer::default();
    for token in values.into_iter().flat_map(|v| v.split(',')) {
        match token.trim() {
            "return=minimal" => p.return_ = Some(Return::Minimal),
            "return=headers-only" => p.return_ = Some(Return::HeadersOnly),
            "return=representation" => p.return_ = Some(Return::Representation),
            "count=exact" => p.count = Some(Count::Exact),
            "count=planned" => p.count = Some(Count::Planned),
            "count=estimated" => p.count = Some(Count::Estimated),
            "resolution=merge-duplicates" => p.resolution = Some(Resolution::MergeDuplicates),
            "resolution=ignore-duplicates" => p.resolution = Some(Resolution::IgnoreDuplicates),
            "missing=default" => p.missing_default = true,
            _ => {}
        }
    }
    p
}

pub fn parse_range(s: &str) -> Result<Range> {
    let s = s.trim();
    let spec = s.strip_prefix("items=").unwrap_or(s);
    let bad = || ParseError(format!("invalid Range '{s}'"));
    let (start, end) = spec.split_once('-').ok_or_else(bad)?;
    let offset: u64 = start.trim().parse().map_err(|_| bad())?;
    let limit = match end.trim() {
        "" => None,
        end => {
            let end: u64 = end.parse().map_err(|_| bad())?;
            // `0-<u64::MAX>` is every row; the max limit clamps it later.
            Some(end.checked_sub(offset).ok_or_else(bad)?.saturating_add(1))
        }
    };
    Ok(Range { offset, limit })
}

pub fn wants_object(accept: &str) -> bool {
    accept.split(',').any(|m| {
        let media = m.split(';').next().unwrap_or_default().trim();
        media.eq_ignore_ascii_case("application/vnd.pgrst.object+json")
            || media.eq_ignore_ascii_case("application/vnd.pgrst.object")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> SelectItem {
        SelectItem::Column { alias: None, name: name.into() }
    }

    fn embed(relation: &str, select: Vec<SelectItem>) -> SelectItem {
        SelectItem::Embed(Embed {
            alias: None,
            relation: relation.into(),
            hint: None,
            inner: false,
            to_many: None,
            select,
        })
    }

    fn hinted(alias: &str, relation: &str, hint: &str, select: Vec<SelectItem>) -> SelectItem {
        SelectItem::Embed(Embed {
            alias: Some(alias.into()),
            relation: relation.into(),
            hint: Some(hint.into()),
            inner: false,
            to_many: None,
            select,
        })
    }

    fn filter(column: &str, op: Op) -> Condition {
        Condition::Filter { column: column.into(), negated: false, op }
    }

    fn eq(v: &str) -> Op {
        Op::Cmp(Cmp::Eq, v.into())
    }

    fn q(raw: &str) -> Query {
        parse_query(raw).unwrap_or_else(|e| panic!("{raw}: {e}"))
    }

    // --- Target API examples (PLAN.md) ---

    #[test]
    fn target_flat_select_with_filter_order_limit() {
        let q = q("select=id,title&status=eq.completed&order=title.asc&limit=10");
        assert_eq!(q.select, [col("id"), col("title")]);
        let root = q.root();
        assert_eq!(root.filters, [filter("status", eq("completed"))]);
        assert_eq!(root.order, [OrderTerm { column: "title".into(), descending: false, nulls: None }]);
        assert_eq!((root.limit, root.offset), (Some(10), None));
        assert_eq!(q.scopes.len(), 1);
    }

    #[test]
    fn target_deep_nesting() {
        let q = q("select=title,edition(language,volume(number,chapter(title,part(content))))");
        let part = embed("part", vec![col("content")]);
        let chapter = embed("chapter", vec![col("title"), part]);
        let volume = embed("volume", vec![col("number"), chapter]);
        let edition = embed("edition", vec![col("language"), volume]);
        assert_eq!(q.select, [col("title"), edition]);
    }

    #[test]
    fn target_many_to_one_chain() {
        let q = q("select=content,chapter(title,volume(title))&word_count=gte.8");
        let volume = embed("volume", vec![col("title")]);
        assert_eq!(q.select, [col("content"), embed("chapter", vec![col("title"), volume])]);
        assert_eq!(q.root().filters, [filter("word_count", Op::Cmp(Cmp::Gte, "8".into()))]);
    }

    #[test]
    fn target_or() {
        let q = q("or=(genre.eq.fantasy,status.eq.hiatus)");
        assert_eq!(
            q.root().filters,
            [Condition::Logic {
                negated: false,
                op: LogicOp::Or,
                conditions: vec![filter("genre", eq("fantasy")), filter("status", eq("hiatus"))],
            }]
        );
    }

    #[test]
    fn target_self_reference_and_many_to_many() {
        let q = q("select=name,area(name,parent:area!parent_id(name)),sub_event(title,area(name),artist(name))");
        let area = embed("area", vec![col("name"), hinted("parent", "area", "parent_id", vec![col("name")])]);
        let sub_event = embed(
            "sub_event",
            vec![col("title"), embed("area", vec![col("name")]), embed("artist", vec![col("name")])],
        );
        assert_eq!(q.select, [col("name"), area, sub_event]);
    }

    #[test]
    fn target_many_to_many_reverse() {
        let q = q("select=name,sub_event(title,event(name))&country=eq.VN");
        let sub_event = embed("sub_event", vec![col("title"), embed("event", vec![col("name")])]);
        assert_eq!(q.select, [col("name"), sub_event]);
        assert_eq!(q.root().filters, [filter("country", eq("VN"))]);
    }

    #[test]
    fn target_self_reference_children() {
        let q = q("select=name,children:area!parent_id(name,capacity)&kind=eq.venue");
        let children = hinted("children", "area", "parent_id", vec![col("name"), col("capacity")]);
        assert_eq!(q.select, [col("name"), children]);
        assert_eq!(q.root().filters, [filter("kind", eq("venue"))]);
    }

    #[test]
    fn target_writes() {
        assert_eq!(q("on_conflict=title").on_conflict, Some(vec!["title".to_string()]));
        assert_eq!(q("id=eq.ff2a0f33").root().filters, [filter("id", eq("ff2a0f33"))]);
        assert_eq!(q("chapter_id=eq.ff2a0f33").root().filters, [filter("chapter_id", eq("ff2a0f33"))]);
    }

    #[test]
    fn target_empty_query() {
        let q = q("");
        assert_eq!(q.select, [SelectItem::Star]);
        assert_eq!(q.scopes, [Scope::default()]);
        assert_eq!((q.on_conflict, q.columns), (None, None));
    }

    // --- T2.1 select ---

    #[test]
    fn select_star_alias_inner_and_whitespace() {
        let q = q("select=*,%20t:title%20,edition!inner(*),vols:volume!edition_id!inner()");
        assert_eq!(
            q.select,
            [
                SelectItem::Star,
                SelectItem::Column { alias: Some("t".into()), name: "title".into() },
                SelectItem::Embed(Embed {
                    alias: None,
                    relation: "edition".into(),
                    hint: None,
                    inner: true,
                    to_many: None,
                    select: vec![SelectItem::Star],
                }),
                SelectItem::Embed(Embed {
                    alias: Some("vols".into()),
                    relation: "volume".into(),
                    hint: Some("edition_id".into()),
                    inner: true,
                    to_many: None,
                    select: vec![],
                }),
            ]
        );
    }

    #[test]
    fn select_direction_tokens() {
        let items = parse_select("parent:area!parent_id!one(name),kids:area!many!parent_id(name)").unwrap();
        let dirs: Vec<_> = items
            .iter()
            .map(|i| match i {
                SelectItem::Embed(e) => (e.hint.as_deref(), e.to_many),
                _ => panic!("expected embed"),
            })
            .collect();
        assert_eq!(dirs, [(Some("parent_id"), Some(false)), (Some("parent_id"), Some(true))]);
        assert!(parse_select("a!many").is_err());
    }

    #[test]
    fn select_quoted_name() {
        assert_eq!(
            parse_select(r#""my col",x:"a b""#).unwrap(),
            [col("my col"), SelectItem::Column { alias: Some("x".into()), name: "a b".into() },]
        );
    }

    #[test]
    fn select_errors() {
        for bad in ["a,", "a(b", "a(b))", "a!inner", "a::text", "a!x!y(b)", ",a", "a b", "a:"] {
            assert!(parse_select(bad).is_err(), "{bad}");
        }
    }

    // --- T2.2 filters ---

    #[test]
    fn filter_operators() {
        let cases = [
            ("a=neq.1", false, Op::Cmp(Cmp::Neq, "1".into())),
            ("a=gt.1", false, Op::Cmp(Cmp::Gt, "1".into())),
            ("a=lt.1", false, Op::Cmp(Cmp::Lt, "1".into())),
            ("a=lte.1.5", false, Op::Cmp(Cmp::Lte, "1.5".into())),
            ("a=like.*Road*", false, Op::Cmp(Cmp::Like, "%Road%".into())),
            ("a=ilike.%25road%25", false, Op::Cmp(Cmp::Ilike, "%road%".into())),
            ("a=is.null", false, Op::Is(IsValue::Null)),
            ("a=is.TRUE", false, Op::Is(IsValue::True)),
            ("a=not.is.false", true, Op::Is(IsValue::False)),
            ("a=not.eq.x", true, eq("x")),
            ("a=eq.", false, eq("")),
            ("a=eq.hello+world", false, eq("hello world")),
            ("a=eq.%22q%22", false, eq("\"q\"")),
        ];
        for (raw, negated, op) in cases {
            let q = q(raw);
            assert_eq!(q.root().filters, [Condition::Filter { column: "a".into(), negated, op }], "{raw}");
        }
    }

    #[test]
    fn filter_in_quoting() {
        let q = q(r#"id=in.(a,b,"c,d","e\"f",)&n=not.in.()"#);
        assert_eq!(
            q.root().filters,
            [
                filter("id", Op::In(vec!["a".into(), "b".into(), "c,d".into(), "e\"f".into(), "".into()])),
                Condition::Filter { column: "n".into(), negated: true, op: Op::In(vec![]) },
            ]
        );
    }

    #[test]
    fn repeated_filters_are_kept() {
        let q = q("n=gte.1&n=lte.5");
        assert_eq!(q.root().filters.len(), 2);
    }

    #[test]
    fn filter_errors() {
        for bad in ["a=1", "a=", "a=foo.1", "a=is.maybe", "a=in.1,2", r#"a=in.("x)"#, "a=eq.%zz", ".a=eq.1"] {
            assert!(parse_query(bad).is_err(), "{bad}");
        }
    }

    // --- T2.3 logic trees ---

    #[test]
    fn nested_and_negated_trees() {
        let q = q(r#"not.or=(a.eq.1,and(b.gt.2,c.not.in.(3,4)),not.or(d.is.null,e.eq."x,y"))"#);
        assert_eq!(
            q.root().filters,
            [Condition::Logic {
                negated: true,
                op: LogicOp::Or,
                conditions: vec![
                    filter("a", eq("1")),
                    Condition::Logic {
                        negated: false,
                        op: LogicOp::And,
                        conditions: vec![
                            filter("b", Op::Cmp(Cmp::Gt, "2".into())),
                            Condition::Filter {
                                column: "c".into(),
                                negated: true,
                                op: Op::In(vec!["3".into(), "4".into()])
                            },
                        ],
                    },
                    Condition::Logic {
                        negated: true,
                        op: LogicOp::Or,
                        conditions: vec![filter("d", Op::Is(IsValue::Null)), filter("e", eq("x,y"))],
                    },
                ],
            }]
        );
    }

    #[test]
    fn and_tree_at_root() {
        let q = q("and=(a.eq.1,b.eq.2)");
        assert!(
            matches!(&q.root().filters[0], Condition::Logic { op: LogicOp::And, negated: false, conditions } if conditions.len() == 2)
        );
    }

    #[test]
    fn tree_errors() {
        for bad in ["or=a.eq.1", "or=()", "or=(a.eq.1", "or=(a)", "or=(not.a.eq.1)", "or=(a.eq.1))"] {
            assert!(parse_query(bad).is_err(), "{bad}");
        }
    }

    // --- T2.4 modifiers ---

    #[test]
    fn multi_column_order() {
        let q = q("order=a.desc.nullslast,b,c.asc.nullsfirst");
        assert_eq!(
            q.root().order,
            [
                OrderTerm { column: "a".into(), descending: true, nulls: Some(Nulls::Last) },
                OrderTerm { column: "b".into(), descending: false, nulls: None },
                OrderTerm { column: "c".into(), descending: false, nulls: Some(Nulls::First) },
            ]
        );
        assert!(parse_query("order=a.up").is_err());
        assert!(parse_query("order=").is_err());
        assert!(parse_query("limit=-1").is_err());
        assert!(parse_query("offset=x").is_err());
    }

    #[test]
    fn embedded_scopes() {
        let q = q(concat!(
            "select=title,edition(language,volume(number))",
            "&edition.language=eq.vi&edition.order=language.desc&edition.limit=2&edition.offset=1",
            "&edition.volume.number=gt.1&edition.not.or=(a.eq.1,b.eq.2)&limit=5&offset=10",
        ));
        assert_eq!((q.root().limit, q.root().offset), (Some(5), Some(10)));
        assert!(q.root().filters.is_empty());

        let ed = q.scope(&["edition"]).unwrap();
        assert_eq!(ed.filters[0], filter("language", eq("vi")));
        assert!(matches!(ed.filters[1], Condition::Logic { negated: true, op: LogicOp::Or, .. }));
        assert_eq!(ed.order, [OrderTerm { column: "language".into(), descending: true, nulls: None }]);
        assert_eq!((ed.limit, ed.offset), (Some(2), Some(1)));

        let vol = q.scope(&["edition", "volume"]).unwrap();
        assert_eq!(vol.filters, [filter("number", Op::Cmp(Cmp::Gt, "1".into()))]);
        assert_eq!(q.scopes.len(), 3);
    }

    #[test]
    fn column_lists() {
        assert_eq!(q("columns=%22title%22,%22genre%22").columns, Some(vec!["title".into(), "genre".into()]));
        assert_eq!(q("on_conflict=novel_id,language").on_conflict, Some(vec!["novel_id".into(), "language".into()]));
        assert!(parse_query("on_conflict=a,,b").is_err());
    }

    // --- T2.5 headers ---

    #[test]
    fn prefer_header() {
        let p = parse_prefer([
            "return=representation, count=exact",
            "resolution=merge-duplicates,missing=default,tx=commit",
        ]);
        assert_eq!(
            p,
            Prefer {
                return_: Some(Return::Representation),
                count: Some(Count::Exact),
                resolution: Some(Resolution::MergeDuplicates),
                missing_default: true,
            }
        );
        let p = parse_prefer(["return=minimal,resolution=ignore-duplicates"]);
        assert_eq!(
            (p.return_, p.resolution, p.count),
            (Some(Return::Minimal), Some(Resolution::IgnoreDuplicates), None)
        );
        assert_eq!(parse_prefer([]), Prefer::default());
    }

    #[test]
    fn range_header() {
        assert_eq!(parse_range("0-9").unwrap(), Range { offset: 0, limit: Some(10) });
        assert_eq!(parse_range("items=5-5").unwrap(), Range { offset: 5, limit: Some(1) });
        assert_eq!(parse_range("10-").unwrap(), Range { offset: 10, limit: None });
        assert_eq!(parse_range("0-18446744073709551615").unwrap(), Range { offset: 0, limit: Some(u64::MAX) });
        for bad in ["", "5", "9-0", "a-b", "-5"] {
            assert!(parse_range(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn accept_header() {
        assert!(wants_object("application/vnd.pgrst.object+json"));
        assert!(wants_object("application/json, application/vnd.pgrst.object+json;q=0.9"));
        assert!(wants_object("Application/Vnd.Pgrst.Object"));
        assert!(!wants_object("application/json"));
        assert!(!wants_object("*/*"));
    }

    #[test]
    fn headers_combined() {
        let h = Headers::parse(["count=exact"], Some("0-24"), Some("application/vnd.pgrst.object+json")).unwrap();
        assert_eq!(h.prefer.count, Some(Count::Exact));
        assert_eq!(h.range, Some(Range { offset: 0, limit: Some(25) }));
        assert!(h.single_object);
        assert_eq!(Headers::parse([], None, None).unwrap(), Headers::default());
        assert!(Headers::parse([], Some("x"), None).is_err());
    }
}
