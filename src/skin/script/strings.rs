//! String and RegExp builtins. Strings are UTF-8 but JScript counts UTF-16
//! code units, so ASCII text takes a byte fast path and anything else goes
//! through a vector of units, which each method pays for in steps.

use std::sync::Arc;

use super::convert::{relative, to_integer, to_uint32};
use super::heap::{Kind, SLOT_BYTES};
use super::interp::{Abort, ErrorKind, Key, MAX_STRING_BYTES, R};
use super::regex::{Program, compile};
use super::{Engine, Host, Name, ObjectRef, Value, to_bool};

const MAX_CACHED_REGEXES: usize = 256;

pub(super) fn length(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

/// The UTF-16 form. Gone when the method returns, so it pays only in steps.
fn units(e: &mut Engine, s: &str) -> R<Vec<u16>> {
    e.charge(s.len() as u64 / 16)?;
    Ok(s.encode_utf16().collect())
}

/// Code units `from..to`, clipped to the string.
pub(super) fn slice(e: &mut Engine, s: &str, from: usize, to: usize) -> R<String> {
    if s.is_ascii() {
        let to = to.min(s.len());
        return Ok(s.get(from.min(to)..to).unwrap_or("").to_string());
    }
    let u = units(e, s)?;
    let to = to.min(u.len());
    Ok(String::from_utf16_lossy(
        u.get(from.min(to)..to).unwrap_or(&[]),
    ))
}

fn this_str(e: &mut Engine, h: &mut dyn Host, this: &Value) -> R<Arc<str>> {
    if matches!(this, Value::Undefined | Value::Null) {
        return e.error(ErrorKind::Type, "string method called on nothing");
    }
    let s = e.string_of(this, h)?;
    e.charge(1 + s.len() as u64 / 64)?;
    Ok(s)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undefined)
}

fn num_arg(e: &mut Engine, h: &mut dyn Host, args: &[Value], i: usize) -> R<f64> {
    let value = arg(args, i);
    e.number(&value, h)
}

fn str_arg(e: &mut Engine, h: &mut dyn Host, args: &[Value], i: usize) -> R<Arc<str>> {
    let value = arg(args, i);
    e.string_of(&value, h)
}

/// A naive scan, charged up front for its worst case.
fn find_units(e: &mut Engine, hay: &[u16], needle: &[u16], from: usize) -> R<Option<usize>> {
    if needle.len() > hay.len() {
        return Ok(None);
    }
    let last = hay.len() - needle.len();
    if from > last {
        return Ok(None);
    }
    let windows = (last - from + 1) as u64;
    e.charge(1 + windows.saturating_mul(1 + needle.len() as u64 / 32) / 16)?;
    Ok((from..=last).find(|i| hay.get(*i..*i + needle.len()) == Some(needle)))
}

fn index_of(e: &mut Engine, s: &str, needle: &str, from: usize) -> R<Option<usize>> {
    if s.is_ascii() && needle.is_ascii() {
        e.charge(s.len() as u64 / 64)?;
        let from = from.min(s.len());
        return Ok(s.get(from..).and_then(|t| t.find(needle)).map(|i| i + from));
    }
    let hay = units(e, s)?;
    let needle = units(e, needle)?;
    find_units(e, &hay, &needle, from.min(hay.len()))
}

fn last_index_of(e: &mut Engine, s: &str, needle: &str, from: f64) -> R<Option<usize>> {
    let hay = units(e, s)?;
    let needle = units(e, needle)?;
    if needle.len() > hay.len() {
        return Ok(None);
    }
    let last = hay.len() - needle.len();
    let start = if from.is_nan() {
        last
    } else {
        relative(from.max(0.0), last)
    };
    e.charge(1 + (start as u64 + 1).saturating_mul(1 + needle.len() as u64 / 32) / 16)?;
    Ok((0..=start)
        .rev()
        .find(|i| hay.get(*i..*i + needle.len()) == Some(needle.as_slice())))
}

fn regex_program(e: &Engine, value: &Value) -> Option<(ObjectRef, Arc<Program>)> {
    if let Value::Object(r) = value
        && let Kind::Regex(program) = &e.obj(*r).kind
    {
        return Some((*r, program.clone()));
    }
    None
}

fn run_regex(e: &mut Engine, program: &Program, input: &[u16], at: usize) -> R<Option<Vec<i32>>> {
    let mut steps = e.steps;
    let result = program.exec(input, at, &mut steps);
    e.steps = steps;
    result.map_err(|_| Abort::Budget)
}

fn flag(e: &mut Engine, r: ObjectRef, name: &str) -> R<bool> {
    let key = Key::Name(e.names.get(name));
    Ok(to_bool(&e.obj_get(r, &key)?))
}

fn unit_str(e: &mut Engine, input: &[u16], slots: &[i32], group: usize) -> R<Value> {
    let start = slots.get(group * 2).copied().unwrap_or(-1);
    let end = slots.get(group * 2 + 1).copied().unwrap_or(-1);
    if start < 0 || end < start {
        return Ok(Value::Undefined);
    }
    let text = input.get(start as usize..end as usize).unwrap_or(&[]);
    e.string_value(&String::from_utf16_lossy(text))
}

/// A new array from values, charged and checked like any other.
pub(super) fn new_array(e: &mut Engine, values: Vec<Value>) -> R<ObjectRef> {
    if values.len() > super::interp::MAX_ARRAY_LEN {
        return Err(Abort::Memory);
    }
    e.mem(values.len() * SLOT_BYTES)?;
    let items = values.into_iter().map(Some).collect();
    e.alloc(e.protos.array, Kind::Array(items))
}

fn match_array(
    e: &mut Engine,
    program: &Program,
    input: &[u16],
    slots: &[i32],
    s: &Arc<str>,
) -> R<Value> {
    let mut values = Vec::with_capacity(program.groups);
    for group in 0..program.groups {
        values.push(unit_str(e, input, slots, group)?);
    }
    let array = new_array(e, values)?;
    let index = f64::from(slots.first().copied().unwrap_or(0));
    e.define(array, "index", Value::Number(index));
    e.define(array, "input", Value::Str(s.clone()));
    Ok(Value::Object(array))
}

fn regex_exec(
    e: &mut Engine,
    h: &mut dyn Host,
    r: ObjectRef,
    program: &Program,
    s: &Arc<str>,
) -> R<Value> {
    let global = flag(e, r, "global")?;
    let key = Key::Name(e.names.get("lastIndex"));
    let at = if global {
        let last = e.obj_get(r, &key)?;
        to_integer(e.number(&last, h)?).max(0) as usize
    } else {
        0
    };
    let input = units(e, s)?;
    let found = if at > input.len() {
        None
    } else {
        run_regex(e, program, &input, at)?
    };
    let Some(slots) = found else {
        if global {
            e.obj_set(r, &key, Value::Number(0.0), h)?;
        }
        return Ok(Value::Null);
    };
    if global {
        let end = f64::from(slots.get(1).copied().unwrap_or(0));
        e.obj_set(r, &key, Value::Number(end), h)?;
    }
    match_array(e, program, &input, &slots, s)
}

impl Engine {
    pub(super) fn make_regex(&mut self, source: &Name, flags: &Name) -> R<Value> {
        self.charge(1 + source.len() as u64)?;
        let cache_key = (source.clone(), flags.clone());
        let program = match self.regex_cache.get(&cache_key) {
            Some(program) => program.clone(),
            None => {
                let program = match compile(source, flags) {
                    Ok(program) => Arc::new(program),
                    Err(message) => return self.error(ErrorKind::Syntax, &message),
                };
                self.charge(program.size() as u64 / 8)?;
                if self.regex_cache.len() >= MAX_CACHED_REGEXES {
                    self.regex_cache.clear();
                }
                self.regex_cache.insert(cache_key, program.clone());
                program
            }
        };
        let regex = self.alloc(self.protos.regexp, Kind::Regex(program))?;
        self.define(regex, "source", Value::Str(source.clone()));
        self.define(regex, "global", Value::Bool(flags.contains('g')));
        self.define(regex, "ignoreCase", Value::Bool(flags.contains('i')));
        self.define(regex, "multiline", Value::Bool(flags.contains('m')));
        self.define(regex, "lastIndex", Value::Number(0.0));
        Ok(Value::Object(regex))
    }

    pub(super) fn install_strings(&mut self) {
        let p = self.protos.string;
        let ctor = self.constructor("String", p, |e, h, _, a| {
            let is_new = e.is_new;
            let text = match a.first() {
                Some(value) => e.string_of(value, h)?,
                None => e.names.get(""),
            };
            if !is_new {
                return Ok(Value::Str(text));
            }
            let boxed = e.alloc(e.protos.string, Kind::Boxed(Value::Str(text)))?;
            Ok(Value::Object(boxed))
        });
        self.method(ctor, "fromCharCode", |e, h, _, a| {
            let mut unit_list = Vec::with_capacity(a.len());
            for value in a {
                unit_list.push(to_uint32(e.number(value, h)?) as u16);
            }
            e.string_value(&String::from_utf16_lossy(&unit_list))
        });
        for name in ["toString", "valueOf"] {
            self.method(p, name, |e, _, t, _| match t {
                Value::Str(_) => Ok(t.clone()),
                Value::Object(r) => match &e.obj(*r).kind {
                    Kind::Boxed(value @ Value::Str(_)) => Ok(value.clone()),
                    _ => e.error(ErrorKind::Type, "not a string"),
                },
                _ => e.error(ErrorKind::Type, "not a string"),
            });
        }
        self.method(p, "charAt", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let i = to_integer(num_arg(e, h, a, 0)?);
            if i < 0 {
                return Ok(Value::Str(e.names.get("")));
            }
            let text = slice(e, &s, i as usize, i as usize + 1)?;
            e.string_value(&text)
        });
        self.method(p, "charCodeAt", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let i = to_integer(num_arg(e, h, a, 0)?);
            if i < 0 {
                return Ok(Value::Number(f64::NAN));
            }
            let code = if s.is_ascii() {
                s.as_bytes().get(i as usize).map(|b| u16::from(*b))
            } else {
                units(e, &s)?.get(i as usize).copied()
            };
            Ok(Value::Number(code.map_or(f64::NAN, f64::from)))
        });
        self.method(p, "indexOf", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let needle = str_arg(e, h, a, 0)?;
            let from = to_integer(num_arg(e, h, a, 1)?).max(0) as usize;
            let found = index_of(e, &s, &needle, from)?;
            Ok(Value::Number(found.map_or(-1.0, |i| i as f64)))
        });
        self.method(p, "lastIndexOf", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let needle = str_arg(e, h, a, 0)?;
            let from = num_arg(e, h, a, 1)?;
            let found = last_index_of(e, &s, &needle, from)?;
            Ok(Value::Number(found.map_or(-1.0, |i| i as f64)))
        });
        self.method(p, "substring", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let len = length(&s);
            let clamp = |n: f64| to_integer(n).clamp(0, len as i64) as usize;
            let from = clamp(num_arg(e, h, a, 0)?);
            let to = match a.get(1) {
                None | Some(Value::Undefined) => len,
                Some(value) => clamp(e.number(value, h)?),
            };
            let text = slice(e, &s, from.min(to), from.max(to))?;
            e.string_value(&text)
        });
        self.method(p, "substr", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let len = length(&s);
            let from = relative(num_arg(e, h, a, 0)?, len);
            let count = match a.get(1) {
                None | Some(Value::Undefined) => len,
                Some(value) => to_integer(e.number(value, h)?).clamp(0, len as i64) as usize,
            };
            let text = slice(e, &s, from, from.saturating_add(count))?;
            e.string_value(&text)
        });
        self.method(p, "slice", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let len = length(&s);
            let from = relative(num_arg(e, h, a, 0)?, len);
            let to = match a.get(1) {
                None | Some(Value::Undefined) => len,
                Some(value) => relative(e.number(value, h)?, len),
            };
            let text = slice(e, &s, from, to.max(from))?;
            e.string_value(&text)
        });
        for name in ["toUpperCase", "toLocaleUpperCase"] {
            self.method(p, name, |e, h, t, _| {
                let s = this_str(e, h, t)?;
                e.string_value(&s.to_uppercase())
            });
        }
        for name in ["toLowerCase", "toLocaleLowerCase"] {
            self.method(p, name, |e, h, t, _| {
                let s = this_str(e, h, t)?;
                e.string_value(&s.to_lowercase())
            });
        }
        self.method(p, "concat", |e, h, t, a| {
            let mut result = Value::Str(this_str(e, h, t)?);
            for value in a {
                let left = e.string_of(&result, h)?;
                let right = e.string_of(value, h)?;
                result = e.concat(&left, &right)?;
            }
            Ok(result)
        });
        self.method(p, "localeCompare", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let other = str_arg(e, h, a, 0)?;
            e.charge(s.len().min(other.len()) as u64 / 64)?;
            Ok(Value::Number(f64::from(s.cmp(&other) as i8)))
        });
        self.method(p, "split", string_split);
        self.method(p, "replace", string_replace);
        self.method(p, "search", |e, h, t, a| {
            let s = this_str(e, h, t)?;
            let (_, program) = coerce_regex(e, h, &arg(a, 0))?;
            let input = units(e, &s)?;
            let found = run_regex(e, &program, &input, 0)?;
            let at = found.and_then(|slots| slots.first().copied());
            Ok(Value::Number(at.map_or(-1.0, f64::from)))
        });
        self.method(p, "match", string_match);

        let p = self.protos.regexp;
        self.constructor("RegExp", p, |e, h, _, a| {
            if let Some((r, _)) = regex_program(e, &arg(a, 0))
                && matches!(a.get(1), None | Some(Value::Undefined))
            {
                return Ok(Value::Object(r));
            }
            let source = match regex_program(e, &arg(a, 0)) {
                Some((r, _)) => {
                    let key = Key::Name(e.names.get("source"));
                    let value = e.obj_get(r, &key)?;
                    e.string_of(&value, h)?
                }
                None => match a.first() {
                    None | Some(Value::Undefined) => e.names.get(""),
                    Some(value) => e.string_of(value, h)?,
                },
            };
            let flags = match a.get(1) {
                None | Some(Value::Undefined) => e.names.get(""),
                Some(value) => e.string_of(value, h)?,
            };
            e.make_regex(&source, &flags)
        });
        self.method(p, "exec", |e, h, t, a| {
            let Some((r, program)) = regex_program(e, t) else {
                return e.error(ErrorKind::Type, "not a regular expression");
            };
            let s = str_arg(e, h, a, 0)?;
            regex_exec(e, h, r, &program, &s)
        });
        self.method(p, "test", |e, h, t, a| {
            let Some((r, program)) = regex_program(e, t) else {
                return e.error(ErrorKind::Type, "not a regular expression");
            };
            let s = str_arg(e, h, a, 0)?;
            let found = regex_exec(e, h, r, &program, &s)?;
            Ok(Value::Bool(found != Value::Null))
        });
        self.method(p, "toString", |e, h, t, _| {
            let Some((r, _)) = regex_program(e, t) else {
                return e.error(ErrorKind::Type, "not a regular expression");
            };
            let key = Key::Name(e.names.get("source"));
            let source = e.obj_get(r, &key)?;
            let source = e.string_of(&source, h)?;
            let mut flags = String::new();
            for (name, letter) in [("global", 'g'), ("ignoreCase", 'i'), ("multiline", 'm')] {
                if flag(e, r, name)? {
                    flags.push(letter);
                }
            }
            e.string_value(&format!("/{source}/{flags}"))
        });
    }
}

fn coerce_regex(e: &mut Engine, h: &mut dyn Host, value: &Value) -> R<(ObjectRef, Arc<Program>)> {
    if let Some(found) = regex_program(e, value) {
        return Ok(found);
    }
    let source = match value {
        Value::Undefined => e.names.get(""),
        other => e.string_of(other, h)?,
    };
    let flags = e.names.get("");
    let regex = e.make_regex(&source, &flags)?;
    match regex_program(e, &regex) {
        Some(found) => Ok(found),
        None => e.error(ErrorKind::Syntax, "invalid regular expression"),
    }
}

fn string_match(e: &mut Engine, h: &mut dyn Host, t: &Value, a: &[Value]) -> R<Value> {
    let s = this_str(e, h, t)?;
    let (r, program) = coerce_regex(e, h, &arg(a, 0))?;
    if !flag(e, r, "global")? {
        return regex_exec(e, h, r, &program, &s);
    }
    let key = Key::Name(e.names.get("lastIndex"));
    e.obj_set(r, &key, Value::Number(0.0), h)?;
    let input = units(e, &s)?;
    let mut matches = Vec::new();
    let mut at = 0;
    while at <= input.len() {
        let Some(slots) = run_regex(e, &program, &input, at)? else {
            break;
        };
        matches.push(unit_str(e, &input, &slots, 0)?);
        e.mem(SLOT_BYTES)?;
        let (start, end) = match slots.as_slice() {
            [start, end, ..] => (*start as usize, *end as usize),
            _ => break,
        };
        at = if end == start { end + 1 } else { end };
    }
    if matches.is_empty() {
        return Ok(Value::Null);
    }
    Ok(Value::Object(new_array(e, matches)?))
}

fn string_split(e: &mut Engine, h: &mut dyn Host, t: &Value, a: &[Value]) -> R<Value> {
    let s = this_str(e, h, t)?;
    let limit = match a.get(1) {
        None | Some(Value::Undefined) => u32::MAX as usize,
        Some(value) => to_uint32(e.number(value, h)?) as usize,
    };
    let separator = arg(a, 0);
    if separator == Value::Undefined {
        return Ok(Value::Object(new_array(e, vec![Value::Str(s)])?));
    }
    let input = units(e, &s)?;
    let mut parts = Vec::new();
    let mut add = |e: &mut Engine, part: &[u16]| -> R<()> {
        e.mem(SLOT_BYTES)?;
        parts.push(e.string_value(&String::from_utf16_lossy(part))?);
        Ok(())
    };
    if let Some((_, program)) = regex_program(e, &separator) {
        split_regex(e, &program, &input, &mut add)?;
    } else {
        let text = e.string_of(&separator, h)?;
        let needle = units(e, &text)?;
        if needle.is_empty() {
            for unit in input.iter().take(limit) {
                e.tick()?;
                add(e, std::slice::from_ref(unit))?;
            }
        } else {
            let mut from = 0;
            while let Some(at) = find_units(e, &input, &needle, from)? {
                add(e, input.get(from..at).unwrap_or(&[]))?;
                from = at + needle.len();
            }
            add(e, input.get(from..).unwrap_or(&[]))?;
        }
    }
    parts.truncate(limit);
    Ok(Value::Object(new_array(e, parts)?))
}

fn split_regex(
    e: &mut Engine,
    program: &Program,
    input: &[u16],
    add: &mut dyn FnMut(&mut Engine, &[u16]) -> R<()>,
) -> R<()> {
    if input.is_empty() {
        if run_regex(e, program, input, 0)?.is_none() {
            add(e, input)?;
        }
        return Ok(());
    }
    let (mut from, mut at) = (0, 0);
    while at < input.len() {
        let Some(slots) = run_regex(e, program, input, at)? else {
            break;
        };
        let (start, end) = match slots.as_slice() {
            [start, end, ..] => (*start as usize, *end as usize),
            _ => break,
        };
        if start >= input.len() {
            break;
        }
        if end == from || (end == start && start == from) {
            at = at.max(start) + 1;
            continue;
        }
        add(e, input.get(from..start).unwrap_or(&[]))?;
        for group in 1..slots.len() / 2 {
            if let (Some(a), Some(b)) = (slots.get(group * 2), slots.get(group * 2 + 1))
                && *a >= 0
            {
                add(e, input.get(*a as usize..*b as usize).unwrap_or(&[]))?;
            }
        }
        from = end;
        at = if end == start { end + 1 } else { end };
    }
    add(e, input.get(from..).unwrap_or(&[]))
}

/// Appends a replacement template, expanding `$1`, `$&`, `` $` ``, `$'`, `$$`.
fn expand(template: &[u16], slots: &[i32], input: &[u16], out: &mut Vec<u16>) {
    let dollar = '$' as u16;
    let groups = slots.len() / 2;
    let span = |group: usize| -> &[u16] {
        match (slots.get(group * 2), slots.get(group * 2 + 1)) {
            (Some(a), Some(b)) if *a >= 0 => input.get(*a as usize..*b as usize).unwrap_or(&[]),
            _ => &[],
        }
    };
    let mut i = 0;
    while let Some(c) = template.get(i).copied() {
        i += 1;
        let next = template.get(i).copied().filter(|_| c == dollar);
        match next.and_then(|n| char::from_u32(u32::from(n))) {
            Some('$') => {
                out.push(dollar);
                i += 1;
            }
            Some('&') => {
                out.extend_from_slice(span(0));
                i += 1;
            }
            Some('`') => {
                let start = slots.first().copied().unwrap_or(0) as usize;
                out.extend_from_slice(input.get(..start).unwrap_or(&[]));
                i += 1;
            }
            Some('\'') => {
                let end = slots.get(1).copied().unwrap_or(0) as usize;
                out.extend_from_slice(input.get(end..).unwrap_or(&[]));
                i += 1;
            }
            Some(d) if d.is_ascii_digit() => {
                let first = d as usize - 48;
                let second = template
                    .get(i + 1)
                    .and_then(|n| char::from_u32(u32::from(*n)))
                    .filter(char::is_ascii_digit)
                    .map(|d| first * 10 + (d as usize - 48));
                match second {
                    Some(n) if n >= 1 && n < groups => {
                        out.extend_from_slice(span(n));
                        i += 2;
                    }
                    _ if first >= 1 && first < groups => {
                        out.extend_from_slice(span(first));
                        i += 1;
                    }
                    _ => out.push(dollar),
                }
            }
            _ => out.push(c),
        }
    }
}

fn string_replace(e: &mut Engine, h: &mut dyn Host, t: &Value, a: &[Value]) -> R<Value> {
    let s = this_str(e, h, t)?;
    let input = units(e, &s)?;
    let pattern = arg(a, 0);
    let replacement = arg(a, 1);
    let is_function = e.is_callable(&replacement);
    let template = if is_function {
        Vec::new()
    } else {
        let text = e.string_of(&replacement, h)?;
        units(e, &text)?
    };
    let regex = regex_program(e, &pattern);
    let needle = match &regex {
        Some(_) => Vec::new(),
        None => {
            let text = e.string_of(&pattern, h)?;
            units(e, &text)?
        }
    };
    let global = match &regex {
        Some((r, _)) => flag(e, *r, "global")?,
        None => false,
    };
    let mut out: Vec<u16> = Vec::new();
    let (mut copied, mut at) = (0, 0);
    while at <= input.len() {
        let found = match &regex {
            Some((_, program)) => run_regex(e, program, &input, at)?,
            None => find_units(e, &input, &needle, at)?
                .map(|i| vec![i as i32, (i + needle.len()) as i32]),
        };
        let Some(slots) = found else { break };
        let (start, end) = match slots.as_slice() {
            [start, end, ..] => (*start as usize, *end as usize),
            _ => break,
        };
        out.extend_from_slice(input.get(copied..start).unwrap_or(&[]));
        if is_function {
            let mut call_args = Vec::with_capacity(slots.len() / 2 + 2);
            for group in 0..slots.len() / 2 {
                call_args.push(unit_str(e, &input, &slots, group)?);
            }
            call_args.push(Value::Number(start as f64));
            call_args.push(Value::Str(s.clone()));
            let result = e.call(&replacement, Value::Undefined, &call_args, h)?;
            let text = e.string_of(&result, h)?;
            out.extend(units(e, &text)?);
        } else {
            let before = out.len();
            expand(&template, &slots, &input, &mut out);
            e.charge((out.len() - before) as u64 / 16)?;
        }
        e.tick()?;
        if out.len() > MAX_STRING_BYTES {
            return Err(Abort::Memory);
        }
        copied = end;
        if !global {
            break;
        }
        at = if end == start { end + 1 } else { end };
    }
    out.extend_from_slice(input.get(copied..).unwrap_or(&[]));
    e.string_value(&String::from_utf16_lossy(&out))
}
