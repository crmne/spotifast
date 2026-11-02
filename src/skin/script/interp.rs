//! The tree-walking evaluator.
//!
//! Limits are enforced here. `steps` pays for time, `Engine::mem` for space.
//! `depth` counts native recursion: script recursion alone is not enough,
//! since one call can sit on top of a deeply nested expression. Budget, memory
//! and depth aborts pass straight through `catch` and `finally`, so a script
//! cannot trap them and keep running.

use std::sync::Arc;

use super::ast::{BinOp, Expr, FuncDef, Link, Stmt, UnOp};
use super::convert::{number_to_string, string_to_number, to_int32, to_uint32};
use super::heap::{Kind, OBJECT_BYTES, PROP_BYTES, SLOT_BYTES, find_var};
use super::{Engine, Host, HostId, Name, ObjectRef, ScriptError, Value, parser, stack, to_bool};

/// Native recursion ceiling, a backstop to what `stack` measures. Ordinary
/// recursion uses about seven units a call.
pub(super) const MAX_NATIVE_DEPTH: u32 = 900;
/// What a script or builtin call adds to `depth`, for its extra native frames.
const CALL_WEIGHT: u32 = 3;

pub(super) const MAX_STRING_BYTES: usize = 4 << 20;
pub(super) const MAX_ARRAY_LEN: usize = 1 << 22;

/// Bytes of copying that one step pays for.
pub(super) const BYTES_PER_STEP: u64 = 32;

pub(super) type R<T> = Result<T, Abort>;
pub(super) type NativeFn = fn(&mut Engine, &mut dyn Host, &Value, &[Value]) -> R<Value>;

#[derive(Debug)]
pub(super) enum Abort {
    /// A script `throw`: the only one `catch` can see.
    Throw(Value),
    Budget,
    Memory,
    Depth,
}

pub(super) enum Flow {
    Normal,
    Break(Option<Name>),
    Continue(Option<Name>),
    Return(Value),
}

/// A property key: arrays want the integer, everything else the name.
#[derive(Clone)]
pub(super) enum Key {
    Idx(u32),
    Name(Name),
}

impl Key {
    pub fn name(&self) -> Name {
        match self {
            Key::Idx(i) => Arc::from(i.to_string()),
            Key::Name(n) => n.clone(),
        }
    }
}

pub(super) enum ErrorKind {
    Error = 0,
    Type = 1,
    Reference = 2,
    Range = 3,
    Syntax = 4,
    Uri = 6,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Hint {
    Default,
    Number,
    String,
}

/// Where a bare name lives.
enum Slot {
    Var(u32, usize),
    /// Found on a `with` object or a handler's scope object.
    Holder(Value),
    Global(usize),
    /// A name only the host knows: readable, but never assigned.
    Host(Value),
    Missing,
}

enum Place {
    Name(Name),
    Prop(Value, Key),
}

fn is_length(name: &str) -> bool {
    name.len() == 6 && name.eq_ignore_ascii_case("length")
}

fn missing(name: &str) -> String {
    format!("'{name}' is undefined")
}

impl Engine {
    pub(super) fn begin(&mut self) {
        stack::mark();
        self.steps = self.limits.steps;
        self.depth = 0;
        self.calls = 0;
        self.scope = None;
        self.this = Value::Object(self.global);
        self.last = Value::Undefined;
        self.track = false;
    }

    pub(super) fn finish(
        &mut self,
        result: R<Value>,
        host: &mut dyn Host,
    ) -> Result<Value, ScriptError> {
        result.map_err(|abort| self.script_error(abort, host))
    }

    fn script_error(&mut self, abort: Abort, host: &mut dyn Host) -> ScriptError {
        match abort {
            Abort::Budget => ScriptError::Budget,
            Abort::Memory => ScriptError::Memory,
            Abort::Depth => ScriptError::Depth,
            Abort::Throw(value) => {
                // The script may have spent everything; describing its error
                // gets a small allowance of its own.
                self.begin();
                self.steps = self.steps.min(10_000);
                let text = self.string_of(&value, host);
                ScriptError::Thrown(text.map_or_else(|_| "script error".into(), |s| s.to_string()))
            }
        }
    }

    #[inline]
    pub(super) fn charge(&mut self, steps: u64) -> R<()> {
        if self.steps < steps {
            self.steps = 0;
            return Err(Abort::Budget);
        }
        self.steps -= steps;
        Ok(())
    }

    #[inline]
    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_NATIVE_DEPTH || stack::exceeded() {
            self.depth -= 1;
            return Err(Abort::Depth);
        }
        Ok(())
    }

    fn enter_call(&mut self) -> R<()> {
        if self.calls >= self.limits.max_depth
            || self.depth + CALL_WEIGHT > MAX_NATIVE_DEPTH
            || stack::exceeded()
        {
            return Err(Abort::Depth);
        }
        self.calls += 1;
        self.depth += CALL_WEIGHT;
        Ok(())
    }

    fn exit_call(&mut self) {
        self.calls -= 1;
        self.depth -= CALL_WEIGHT;
    }

    pub(super) fn error<T>(&mut self, kind: ErrorKind, message: &str) -> R<T> {
        let error = self.new_error(kind as usize, message)?;
        Err(Abort::Throw(Value::Object(error)))
    }

    pub(super) fn new_error(&mut self, kind: usize, message: &str) -> R<ObjectRef> {
        let proto = self.protos.errors.get(kind).copied();
        let error = self.alloc(proto.unwrap_or(self.protos.object), Kind::Error)?;
        let text = Value::Str(Arc::from(message));
        self.define(error, "message", text.clone());
        self.define(error, "description", text);
        self.define(error, "number", Value::Number(0.0));
        Ok(error)
    }

    /// A hidden property on a builtin or error object.
    pub(super) fn define(&mut self, object: ObjectRef, name: &str, value: Value) {
        let key = self.names.get(name);
        let props = &mut self.obj_mut(object).props;
        match props.find(&key) {
            Some(i) => props.list[i].value = value,
            None => props.insert(key, value, true),
        }
    }

    pub(super) fn string_value(&mut self, text: &str) -> R<Value> {
        Ok(Value::Str(self.new_string(text)?))
    }

    pub(super) fn new_string(&mut self, text: &str) -> R<Arc<str>> {
        if text.len() > MAX_STRING_BYTES {
            return Err(Abort::Memory);
        }
        self.charge(text.len() as u64 / BYTES_PER_STEP)?;
        self.mem(text.len())?;
        Ok(Arc::from(text))
    }

    // Running programs and handlers.

    pub(super) fn run_program(&mut self, def: &Arc<FuncDef>, host: &mut dyn Host) -> R<()> {
        self.hoist_vars(def)?;
        self.hoist_funcs(def)?;
        let mut first = None;
        for stmt in &def.body {
            match self.exec(stmt, host) {
                Ok(Flow::Return(_)) => break,
                Ok(_) => {}
                Err(Abort::Throw(value)) => first = first.or(Some(value)),
                Err(limit) => return Err(first.map_or(limit, Abort::Throw)),
            }
        }
        first.map_or(Ok(()), |value| Err(Abort::Throw(value)))
    }

    pub(super) fn run_handler(
        &mut self,
        def: &Arc<FuncDef>,
        scope: Option<HostId>,
        args: &[(&str, Value)],
        host: &mut dyn Host,
    ) -> R<Value> {
        let frame = self.new_scope(None);
        let vars: Vec<_> = args
            .iter()
            .map(|(name, value)| (self.names.get(name), value.clone()))
            .collect();
        self.scopes[frame as usize].vars = vars;
        self.scopes[frame as usize].host = scope;
        self.scope = Some(frame);
        if let Some(id) = scope {
            self.this = Value::Host(id);
        }
        self.track = true;
        let result = self
            .hoist_vars(def)
            .and_then(|()| self.hoist_funcs(def))
            .and_then(|()| self.exec_list(&def.body, host));
        self.track = false;
        self.scope = None;
        self.release_scope(frame);
        match result? {
            Flow::Return(value) => Ok(value),
            _ => Ok(std::mem::replace(&mut self.last, Value::Undefined)),
        }
    }

    /// `eval`: runs in the caller's scope, which natives leave current.
    pub(super) fn eval_source(&mut self, source: &str, host: &mut dyn Host) -> R<Value> {
        self.charge(source.len() as u64 / 4)?;
        self.mem(source.len().saturating_mul(2))?;
        // The parser shares the stack with whatever called `eval`, and
        // `stack` holds it to what is left.
        let def = match parser::parse(source, &mut self.names, parser::MAX_NESTING) {
            Ok(def) => def,
            Err(ScriptError::Depth) => return Err(Abort::Depth),
            Err(error) => return self.error(ErrorKind::Syntax, &error.to_string()),
        };
        self.enter_call()?;
        let track = std::mem::replace(&mut self.track, true);
        let last = std::mem::replace(&mut self.last, Value::Undefined);
        let result = self
            .hoist_vars(&def)
            .and_then(|()| self.hoist_funcs(&def))
            .and_then(|()| self.exec_list(&def.body, host));
        let value = std::mem::replace(&mut self.last, last);
        self.track = track;
        self.exit_call();
        match result? {
            Flow::Return(returned) => Ok(returned),
            _ => Ok(value),
        }
    }

    fn hoist_vars(&mut self, def: &FuncDef) -> R<()> {
        for name in &def.vars {
            self.tick()?;
            match self.scope {
                Some(i) => {
                    let cost = self.scopes[i as usize].vars.len() as u64 / 16;
                    self.charge(cost)?;
                    let scope = &mut self.scopes[i as usize];
                    if find_var(&scope.vars, name).is_none() {
                        scope.vars.push((name.clone(), Value::Undefined));
                    }
                }
                None => {
                    if self.obj(self.global).props.find(name).is_none() {
                        self.insert_global(name.clone(), Value::Undefined)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn hoist_funcs(&mut self, def: &FuncDef) -> R<()> {
        for inner in &def.funcs {
            self.tick()?;
            let function = Value::Object(self.make_closure(inner)?);
            let Some(name) = &inner.name else { continue };
            match self.scope {
                Some(i) => {
                    let cost = self.scopes[i as usize].vars.len() as u64 / 16;
                    self.charge(cost)?;
                    let vars = &mut self.scopes[i as usize].vars;
                    match find_var(vars, name) {
                        Some(at) => vars[at].1 = function,
                        None => vars.push((name.clone(), function)),
                    }
                }
                None => {
                    let props = &self.obj(self.global).props;
                    match props.find(name) {
                        Some(at) => {
                            // A script's own `escape` is a script global.
                            self.folded.insert(name.to_lowercase(), name.clone());
                            self.obj_mut(self.global).props.list[at].value = function;
                        }
                        None => self.insert_global(name.clone(), function)?,
                    }
                }
            }
        }
        Ok(())
    }

    fn insert_global(&mut self, name: Name, value: Value) -> R<()> {
        self.mem(PROP_BYTES + name.len())?;
        self.folded.insert(name.to_lowercase(), name.clone());
        let global = self.global;
        self.obj_mut(global).props.insert(name, value, false);
        Ok(())
    }

    /// The real name of a script global spelled in another case.
    fn fold_global(&self, name: &str) -> Option<usize> {
        let real = self.folded.get(&name.to_lowercase())?;
        self.obj(self.global).props.find(real)
    }

    pub(super) fn global_value(&self, name: &str) -> Value {
        let props = &self.obj(self.global).props;
        props
            .find(name)
            .or_else(|| self.fold_global(name))
            .and_then(|i| props.list.get(i))
            .map_or(Value::Undefined, |p| p.value.clone())
    }

    fn make_closure(&mut self, def: &Arc<FuncDef>) -> R<ObjectRef> {
        self.capture_scopes(self.scope)?;
        self.alloc(
            self.protos.function,
            Kind::Function(def.clone(), self.scope),
        )
    }

    // Names.

    fn resolve(&mut self, name: &Name, host: &mut dyn Host) -> R<Slot> {
        let mut current = self.scope;
        while let Some(i) = current {
            let scope = &self.scopes[i as usize];
            if let Some(at) = find_var(&scope.vars, name) {
                return Ok(Slot::Var(i, at));
            }
            let holder = match (&scope.with, scope.host) {
                (Some(object), _) => Some(object.clone()),
                (None, Some(id)) => Some(Value::Host(id)),
                (None, None) => None,
            };
            current = scope.parent;
            let cost = 1 + scope.vars.len() as u64 / 16;
            self.charge(cost)?;
            if let Some(holder) = holder
                && self.has_name(&holder, name, host)?
            {
                return Ok(Slot::Holder(holder));
            }
        }
        let globals = &self.obj(self.global).props;
        if let Some(at) = globals.find(name).or_else(|| self.fold_global(name)) {
            return Ok(Slot::Global(at));
        }
        Ok(host.global(name).map_or(Slot::Missing, Slot::Host))
    }

    fn get_name(&mut self, name: &Name, host: &mut dyn Host) -> R<Value> {
        match self.lookup(name, host)? {
            Some(value) => Ok(value),
            None => self.error(ErrorKind::Reference, &missing(name)),
        }
    }

    fn lookup(&mut self, name: &Name, host: &mut dyn Host) -> R<Option<Value>> {
        Ok(Some(match self.resolve(name, host)? {
            Slot::Var(i, at) => self.scopes[i as usize].vars[at].1.clone(),
            Slot::Holder(holder) => self.get(&holder, &Key::Name(name.clone()), host)?,
            Slot::Global(at) => self.obj(self.global).props.list[at].value.clone(),
            Slot::Host(value) => value,
            Slot::Missing => return Ok(None),
        }))
    }

    fn set_name(&mut self, name: &Name, value: Value, host: &mut dyn Host) -> R<()> {
        match self.resolve(name, host)? {
            Slot::Var(i, at) => self.scopes[i as usize].vars[at].1 = value,
            Slot::Holder(holder) => self.set(&holder, &Key::Name(name.clone()), value, host)?,
            Slot::Global(at) => {
                let global = self.global;
                self.obj_mut(global).props.list[at].value = value;
            }
            Slot::Host(_) => {}
            Slot::Missing => self.insert_global(name.clone(), value)?,
        }
        Ok(())
    }

    fn call_name(&mut self, name: &Name, args: &[Expr], host: &mut dyn Host) -> R<Value> {
        let function = match self.resolve(name, host)? {
            Slot::Var(i, at) => self.scopes[i as usize].vars[at].1.clone(),
            Slot::Holder(holder) => {
                return self.method_link(&holder, &Key::Name(name.clone()), args, host);
            }
            Slot::Global(at) => self.obj(self.global).props.list[at].value.clone(),
            Slot::Host(function) => function,
            Slot::Missing => return self.error(ErrorKind::Reference, &missing(name)),
        };
        self.call_link(&function, args, host)
    }

    // Properties.

    pub(super) fn key_of(&mut self, value: &Value, host: &mut dyn Host) -> R<Key> {
        if let Value::Number(n) = value
            && *n >= 0.0
            && *n < 4_294_967_295.0
            && n.fract() == 0.0
        {
            return Ok(Key::Idx(*n as u32));
        }
        let text = self.string_of(value, host)?;
        let bytes = text.as_bytes();
        if bytes.len() <= 10
            && bytes.first().is_some_and(u8::is_ascii_digit)
            && (bytes.len() == 1 || bytes[0] != b'0')
            && let Ok(i) = text.parse::<u32>()
            && i != u32::MAX
        {
            return Ok(Key::Idx(i));
        }
        Ok(Key::Name(text))
    }

    pub(super) fn get(&mut self, base: &Value, key: &Key, host: &mut dyn Host) -> R<Value> {
        let proto = match base {
            Value::Object(r) => return self.obj_get(*r, key),
            Value::Host(id) => {
                return Ok(match key {
                    Key::Idx(i) => host.index(*id, f64::from(*i)),
                    Key::Name(name) => host.get(*id, name),
                });
            }
            Value::Str(s) => {
                match key {
                    Key::Name(name) if is_length(name) => {
                        self.charge(1 + s.len() as u64 / 64)?;
                        return Ok(Value::Number(super::strings::length(s) as f64));
                    }
                    Key::Idx(i) => {
                        let i = *i as usize;
                        let text = super::strings::slice(self, s, i, i + 1)?;
                        return Ok(if text.is_empty() {
                            Value::Undefined
                        } else {
                            Value::Str(Arc::from(text))
                        });
                    }
                    Key::Name(_) => {}
                }
                self.protos.string
            }
            Value::Number(_) => self.protos.number,
            Value::Bool(_) => self.protos.boolean,
            Value::Undefined | Value::Null => {
                let message = format!("'{}' is null or not an object", key.name());
                return self.error(ErrorKind::Type, &message);
            }
        };
        self.obj_get(proto, key)
    }

    pub(super) fn obj_get(&mut self, r: ObjectRef, key: &Key) -> R<Value> {
        let name = match key {
            Key::Name(name) => name.clone(),
            Key::Idx(i) => {
                if let Kind::Array(items) = &self.obj(r).kind
                    && let Some(Some(value)) = items.get(*i as usize)
                {
                    return Ok(value.clone());
                }
                key.name()
            }
        };
        if is_length(&name) {
            match &self.obj(r).kind {
                Kind::Array(items) => return Ok(Value::Number(items.len() as f64)),
                Kind::Boxed(Value::Str(s)) => {
                    return Ok(Value::Number(super::strings::length(s) as f64));
                }
                Kind::Function(def, _) => return Ok(Value::Number(def.params.len() as f64)),
                _ => {}
            }
        }
        if &*name == "prototype"
            && matches!(self.obj(r).kind, Kind::Function(..))
            && self.obj(r).props.find(&name).is_none()
        {
            let proto = self.alloc(self.protos.object, Kind::Plain)?;
            self.define(proto, "constructor", Value::Object(r));
            self.define(r, "prototype", Value::Object(proto));
            return Ok(Value::Object(proto));
        }
        let mut current = Some(r);
        while let Some(c) = current {
            self.charge(1)?;
            let obj = self.obj(c);
            if let Some(at) = obj.props.find(&name) {
                return Ok(obj.props.list[at].value.clone());
            }
            current = obj.proto;
        }
        if matches!(key, Key::Name(_)) {
            return self.obj_get_folded(r, &name);
        }
        Ok(Value::Undefined)
    }

    /// The case-insensitive fallback. A scan, so it pays for what it reads.
    fn obj_get_folded(&mut self, r: ObjectRef, name: &str) -> R<Value> {
        let mut current = Some(r);
        while let Some(c) = current {
            let obj = self.obj(c);
            let found = obj.props.list.iter();
            let found = found
                .filter(|p| p.key.len() == name.len() && p.key.eq_ignore_ascii_case(name))
                .map(|p| p.value.clone())
                .next();
            let (next, count) = (obj.proto, obj.props.list.len());
            self.charge(1 + count as u64 / 8)?;
            if let Some(value) = found {
                return Ok(value);
            }
            current = next;
        }
        Ok(Value::Undefined)
    }

    pub(super) fn set(
        &mut self,
        base: &Value,
        key: &Key,
        value: Value,
        host: &mut dyn Host,
    ) -> R<()> {
        match base {
            Value::Object(r) => self.obj_set(*r, key, value, host),
            Value::Host(id) => {
                host.set(*id, &key.name(), value);
                Ok(())
            }
            Value::Undefined | Value::Null => {
                let message = format!("'{}' is null or not an object", key.name());
                self.error(ErrorKind::Type, &message)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn obj_set(
        &mut self,
        r: ObjectRef,
        key: &Key,
        value: Value,
        host: &mut dyn Host,
    ) -> R<()> {
        if matches!(self.obj(r).kind, Kind::Array(_)) {
            match key {
                Key::Idx(i) => return self.array_set(r, *i as usize, value),
                Key::Name(name) if is_length(name) => {
                    let n = self.number(&value, host)?;
                    if n < 0.0 || n.fract() != 0.0 || n >= 4_294_967_296.0 || n.is_nan() {
                        return self.error(ErrorKind::Range, "invalid array length");
                    }
                    return self.array_resize(r, n as usize);
                }
                Key::Name(_) => {}
            }
        }
        let name = key.name();
        if let Some(at) = self.obj(r).props.find(&name) {
            self.obj_mut(r).props.list[at].value = value;
            return Ok(());
        }
        if r == self.global {
            return self.insert_global(name, value);
        }
        self.mem(PROP_BYTES + name.len())?;
        let hidden = &*name == "prototype" && matches!(self.obj(r).kind, Kind::Function(..));
        self.obj_mut(r).props.insert(name, value, hidden);
        Ok(())
    }

    fn array_set(&mut self, r: ObjectRef, i: usize, value: Value) -> R<()> {
        let len = self.array_len(r);
        if i >= len {
            self.array_resize(r, i + 1)?;
        }
        if let Kind::Array(items) = &mut self.obj_mut(r).kind
            && let Some(slot) = items.get_mut(i)
        {
            *slot = Some(value);
        }
        Ok(())
    }

    pub(super) fn array_len(&self, r: ObjectRef) -> usize {
        match &self.obj(r).kind {
            Kind::Array(items) => items.len(),
            _ => 0,
        }
    }

    pub(super) fn array_resize(&mut self, r: ObjectRef, len: usize) -> R<()> {
        if len > MAX_ARRAY_LEN {
            return Err(Abort::Memory);
        }
        let old = self.array_len(r);
        if len > old {
            self.charge((len - old) as u64 / 16)?;
            self.mem((len - old).saturating_mul(SLOT_BYTES))?;
        }
        if let Kind::Array(items) = &mut self.obj_mut(r).kind {
            items.resize(len, None);
        }
        Ok(())
    }

    fn has_name(&mut self, base: &Value, name: &Name, host: &mut dyn Host) -> R<bool> {
        let start = match base {
            Value::Host(id) => return Ok(host.has(*id, name)),
            Value::Object(r) => *r,
            Value::Str(_) => self.protos.string,
            Value::Number(_) => self.protos.number,
            Value::Bool(_) => self.protos.boolean,
            Value::Undefined | Value::Null => return Ok(false),
        };
        self.has_property(start, &Key::Name(name.clone()))
    }

    pub(super) fn has_own(&self, r: ObjectRef, key: &Key) -> bool {
        let obj = self.obj(r);
        match (&obj.kind, key) {
            (Kind::Array(items), Key::Idx(i)) => {
                matches!(items.get(*i as usize), Some(Some(_)))
            }
            (Kind::Array(_), Key::Name(name)) if &**name == "length" => true,
            _ => obj.props.find(&key.name()).is_some(),
        }
    }

    fn has_property(&mut self, r: ObjectRef, key: &Key) -> R<bool> {
        let mut current = Some(r);
        while let Some(c) = current {
            self.charge(1)?;
            if self.has_own(c, key) {
                return Ok(true);
            }
            current = self.obj(c).proto;
        }
        Ok(false)
    }

    fn delete_key(&mut self, base: &Value, key: &Key) -> R<bool> {
        let Value::Object(r) = base else {
            return Ok(!matches!(base, Value::Host(_)));
        };
        let r = *r;
        if let (Key::Idx(i), Kind::Array(items)) = (key, &mut self.obj_mut(r).kind) {
            if let Some(slot) = items.get_mut(*i as usize) {
                *slot = None;
            }
            return Ok(true);
        }
        let name = key.name();
        if let Some(at) = self.obj(r).props.find(&name) {
            self.charge(1 + self.obj(r).props.list.len() as u64 / 8)?;
            self.obj_mut(r).props.remove(at);
        }
        Ok(true)
    }

    // Conversions.

    pub(super) fn primitive(&mut self, v: &Value, hint: Hint, host: &mut dyn Host) -> R<Value> {
        let r = match v {
            Value::Object(r) => *r,
            Value::Host(_) => return Ok(Value::Str(Arc::from("[object]"))),
            other => return Ok(other.clone()),
        };
        let is_date = matches!(self.obj(r).kind, Kind::Date(_));
        let string_first = hint == Hint::String || (hint == Hint::Default && is_date);
        let order = if string_first {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        };
        for name in order {
            let key = Key::Name(self.names.get(name));
            let function = self.obj_get(r, &key)?;
            if self.is_callable(&function) {
                let result = self.call(&function, v.clone(), &[], host)?;
                if !matches!(result, Value::Object(_)) {
                    return Ok(result);
                }
            }
        }
        self.error(ErrorKind::Type, "cannot convert object to a primitive")
    }

    pub(super) fn number(&mut self, v: &Value, host: &mut dyn Host) -> R<f64> {
        Ok(match v {
            Value::Number(n) => *n,
            Value::Undefined | Value::Host(_) => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Str(s) => {
                self.charge(s.len() as u64 / 64)?;
                string_to_number(s)
            }
            Value::Object(_) => {
                let primitive = self.primitive(v, Hint::Number, host)?;
                return self.number(&primitive, host);
            }
        })
    }

    pub(super) fn string_of(&mut self, v: &Value, host: &mut dyn Host) -> R<Arc<str>> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            Value::Number(n) => self.new_string(&number_to_string(*n)),
            Value::Undefined => Ok(self.names.get("undefined")),
            Value::Null => Ok(self.names.get("null")),
            Value::Bool(b) => Ok(self.names.get(if *b { "true" } else { "false" })),
            Value::Host(_) => Ok(self.names.get("[object]")),
            Value::Object(_) => {
                let primitive = self.primitive(v, Hint::String, host)?;
                match primitive {
                    Value::Object(_) => Ok(self.names.get("[object Object]")),
                    other => self.string_of(&other, host),
                }
            }
        }
    }

    // Calls.

    pub(super) fn is_callable(&self, v: &Value) -> bool {
        match v {
            Value::Object(r) => {
                matches!(self.obj(*r).kind, Kind::Function(..) | Kind::Native(_))
            }
            _ => false,
        }
    }

    pub(super) fn call(
        &mut self,
        function: &Value,
        this: Value,
        args: &[Value],
        host: &mut dyn Host,
    ) -> R<Value> {
        self.invoke(function, this, args, false, host)
    }

    fn invoke(
        &mut self,
        function: &Value,
        this: Value,
        args: &[Value],
        is_new: bool,
        host: &mut dyn Host,
    ) -> R<Value> {
        self.tick()?;
        match function {
            Value::Object(r) => match &self.obj(*r).kind {
                Kind::Function(def, scope) => {
                    let (def, scope) = (def.clone(), *scope);
                    self.call_script(*r, &def, scope, this, args, host)
                }
                Kind::Native(native) => {
                    let native = *native;
                    self.call_native(native, this, args, is_new, host)
                }
                _ => self.error(ErrorKind::Type, "object is not a function"),
            },
            Value::Host(id) => self.call_host(*id, args, host),
            _ => self.error(ErrorKind::Type, "object is not a function"),
        }
    }

    fn call_native(
        &mut self,
        native: NativeFn,
        this: Value,
        args: &[Value],
        is_new: bool,
        host: &mut dyn Host,
    ) -> R<Value> {
        self.enter_call()?;
        self.is_new = is_new;
        let result = native(self, host, &this, args);
        self.exit_call();
        result
    }

    fn call_host(&mut self, id: HostId, args: &[Value], host: &mut dyn Host) -> R<Value> {
        let index = match args.first() {
            Some(value) => self.number(value, host)?,
            None => 0.0,
        };
        Ok(host.index(id, index))
    }

    fn call_script(
        &mut self,
        callee: ObjectRef,
        def: &Arc<FuncDef>,
        parent: Option<u32>,
        this: Value,
        args: &[Value],
        host: &mut dyn Host,
    ) -> R<Value> {
        self.enter_call()?;
        let bound = def.params.len() + def.vars.len() + args.len();
        self.charge(1 + bound as u64 / 8)?;
        let frame = self.new_scope(parent);
        let result = self.call_frame(callee, def, frame, this, args, host);
        self.exit_call();
        self.release_scope(frame);
        result
    }

    fn call_frame(
        &mut self,
        callee: ObjectRef,
        def: &Arc<FuncDef>,
        frame: u32,
        this: Value,
        args: &[Value],
        host: &mut dyn Host,
    ) -> R<Value> {
        self.bind_frame(callee, def, frame, args)?;
        let this = match this {
            Value::Undefined | Value::Null => Value::Object(self.global),
            other => other,
        };
        let saved_scope = self.scope.replace(frame);
        let saved_this = std::mem::replace(&mut self.this, this);
        let saved_track = std::mem::replace(&mut self.track, false);
        let result = self.run_body(def, host);
        self.scope = saved_scope;
        self.this = saved_this;
        self.track = saved_track;
        result
    }

    fn run_body(&mut self, def: &Arc<FuncDef>, host: &mut dyn Host) -> R<Value> {
        self.hoist_funcs(def)?;
        match self.exec_list(&def.body, host)? {
            Flow::Return(value) => Ok(value),
            _ => Ok(Value::Undefined),
        }
    }

    fn bind_frame(
        &mut self,
        callee: ObjectRef,
        def: &Arc<FuncDef>,
        frame: u32,
        args: &[Value],
    ) -> R<()> {
        let arguments = if def.uses_args {
            Some(self.arguments_object(callee, args)?)
        } else {
            None
        };
        let vars = &mut self.scopes[frame as usize].vars;
        if def.self_bind
            && let Some(name) = &def.name
        {
            vars.push((name.clone(), Value::Object(callee)));
        }
        // Backwards, so that of two equal parameter names the last is found.
        for (i, param) in def.params.iter().enumerate().rev() {
            let value = args.get(i).cloned().unwrap_or(Value::Undefined);
            vars.push((param.clone(), value));
        }
        if let Some(arguments) = arguments {
            let name = self.names.get("arguments");
            let vars = &mut self.scopes[frame as usize].vars;
            if !def.params.contains(&name) {
                vars.push((name, Value::Object(arguments)));
            }
        }
        // The parser left these unique and apart from the parameters.
        let vars = &mut self.scopes[frame as usize].vars;
        vars.extend(def.vars.iter().map(|n| (n.clone(), Value::Undefined)));
        Ok(())
    }

    fn arguments_object(&mut self, callee: ObjectRef, args: &[Value]) -> R<ObjectRef> {
        self.mem(args.len() * SLOT_BYTES)?;
        let items = args.iter().cloned().map(Some).collect();
        let arguments = self.alloc(self.protos.object, Kind::Array(items))?;
        self.define(arguments, "callee", Value::Object(callee));
        Ok(arguments)
    }

    pub(super) fn call_method(
        &mut self,
        base: &Value,
        key: &Key,
        args: &[Value],
        host: &mut dyn Host,
    ) -> R<Value> {
        if let (Value::Host(id), Key::Name(name)) = (base, key) {
            return match host.call(*id, name, args) {
                Ok(value) => Ok(value),
                Err(message) => self.error(ErrorKind::Error, &message),
            };
        }
        let function = self.get(base, key, host)?;
        if !matches!(function, Value::Host(_)) && !self.is_callable(&function) {
            let message = format!("'{}' is not a function", key.name());
            return self.error(ErrorKind::Type, &message);
        }
        self.call(&function, base.clone(), args, host)
    }

    pub(super) fn construct(
        &mut self,
        function: &Value,
        args: &[Value],
        host: &mut dyn Host,
    ) -> R<Value> {
        let Value::Object(r) = function else {
            return self.error(ErrorKind::Type, "object is not a constructor");
        };
        match self.obj(*r).kind {
            Kind::Native(_) => self.invoke(function, Value::Undefined, args, true, host),
            Kind::Function(..) => {
                let key = Key::Name(self.names.get("prototype"));
                let proto = match self.obj_get(*r, &key)? {
                    Value::Object(proto) => proto,
                    _ => self.protos.object,
                };
                let object = Value::Object(self.alloc(proto, Kind::Plain)?);
                let result = self.call(function, object.clone(), args, host)?;
                Ok(match result {
                    Value::Object(_) => result,
                    _ => object,
                })
            }
            _ => self.error(ErrorKind::Type, "object is not a constructor"),
        }
    }

    fn eval_args(&mut self, args: &[Expr], host: &mut dyn Host) -> R<Vec<Value>> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg, host)?);
        }
        Ok(values)
    }

    // Expressions. `eval` handles leaves itself and sends the rest through a
    // dispatcher to one small function per kind, which keeps unoptimised
    // stack frames small enough for `MAX_NATIVE_DEPTH`.

    #[inline]
    pub(super) fn tick(&mut self) -> R<()> {
        self.charge(1)
    }

    pub(super) fn eval(&mut self, e: &Expr, host: &mut dyn Host) -> R<Value> {
        self.tick()?;
        match e {
            Expr::Num(n) => Ok(Value::Number(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::This => Ok(self.this.clone()),
            Expr::Ident(name) => self.get_name(name, host),
            _ => {
                self.enter()?;
                let result = self.eval_compound(e, host);
                self.depth -= 1;
                result
            }
        }
    }

    fn eval_compound(&mut self, e: &Expr, host: &mut dyn Host) -> R<Value> {
        match e {
            Expr::Array(items) => self.eval_array(items, host),
            Expr::Object(items) => self.eval_object(items, host),
            Expr::Func(def) => self.eval_function(def),
            Expr::Regex(source, flags) => self.make_regex(source, flags),
            Expr::Chain(head, links) => self.eval_chain(head, links, host),
            Expr::New(callee, args) => self.eval_new(callee, args, host),
            Expr::Unary(op, operand) => self.eval_unary(*op, operand, host),
            Expr::Update {
                prefix,
                delta,
                target,
            } => self.eval_update(*prefix, *delta, target, host),
            Expr::Binary(first, rest) => self.eval_binary(first, rest, host),
            Expr::Cond(parts) => self.eval_cond(parts, host),
            Expr::Assign(op, target, value) => self.eval_assign(*op, target, value, host),
            Expr::Seq(list) => self.eval_seq(list, host),
            // Leaves are answered in `eval`.
            _ => Ok(Value::Undefined),
        }
    }

    fn eval_function(&mut self, def: &Arc<FuncDef>) -> R<Value> {
        Ok(Value::Object(self.make_closure(def)?))
    }

    fn eval_new(&mut self, callee: &Expr, args: &[Expr], host: &mut dyn Host) -> R<Value> {
        let function = self.eval(callee, host)?;
        let values = self.eval_args(args, host)?;
        self.construct(&function, &values, host)
    }

    fn eval_cond(&mut self, parts: &[Expr; 3], host: &mut dyn Host) -> R<Value> {
        let [test, yes, no] = parts;
        let test = self.eval(test, host)?;
        self.eval(if to_bool(&test) { yes } else { no }, host)
    }

    fn eval_seq(&mut self, list: &[Expr], host: &mut dyn Host) -> R<Value> {
        let mut value = Value::Undefined;
        for item in list {
            value = self.eval(item, host)?;
        }
        Ok(value)
    }

    fn eval_array(&mut self, items: &[Option<Expr>], host: &mut dyn Host) -> R<Value> {
        self.mem(items.len() * SLOT_BYTES)?;
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(match item {
                Some(e) => Some(self.eval(e, host)?),
                None => None,
            });
        }
        Ok(Value::Object(
            self.alloc(self.protos.array, Kind::Array(values))?,
        ))
    }

    fn eval_object(&mut self, items: &[(Name, Expr)], host: &mut dyn Host) -> R<Value> {
        let object = self.alloc(self.protos.object, Kind::Plain)?;
        for (name, e) in items {
            let value = self.eval(e, host)?;
            self.obj_set(object, &Key::Name(name.clone()), value, host)?;
        }
        Ok(Value::Object(object))
    }

    fn link_key(&mut self, link: &Link, host: &mut dyn Host) -> R<Key> {
        match link {
            Link::Member(name) => Ok(Key::Name(name.clone())),
            Link::Index(e) => {
                let value = self.eval(e, host)?;
                self.key_of(&value, host)
            }
            Link::Call(_) => Ok(Key::Idx(0)),
        }
    }

    fn eval_chain(&mut self, head: &Expr, links: &[Link], host: &mut dyn Host) -> R<Value> {
        if let (Expr::Ident(name), Some(Link::Call(args))) = (head, links.first()) {
            let value = self.call_name(name, args, host)?;
            return self.walk_links(value, links.get(1..).unwrap_or(&[]), host);
        }
        let value = self.eval(head, host)?;
        self.walk_links(value, links, host)
    }

    fn walk_links(&mut self, mut value: Value, links: &[Link], host: &mut dyn Host) -> R<Value> {
        let mut i = 0;
        while let Some(link) = links.get(i) {
            self.tick()?;
            if let Link::Call(args) = link {
                value = self.call_link(&value, args, host)?;
                i += 1;
                continue;
            }
            let key = self.link_key(link, host)?;
            if let Some(Link::Call(args)) = links.get(i + 1) {
                value = self.method_link(&value, &key, args, host)?;
                i += 2;
            } else {
                value = self.get(&value, &key, host)?;
                i += 1;
            }
        }
        Ok(value)
    }

    fn call_link(&mut self, function: &Value, args: &[Expr], host: &mut dyn Host) -> R<Value> {
        let values = self.eval_args(args, host)?;
        let this = Value::Object(self.global);
        self.call(function, this, &values, host)
    }

    fn method_link(
        &mut self,
        base: &Value,
        key: &Key,
        args: &[Expr],
        host: &mut dyn Host,
    ) -> R<Value> {
        let values = self.eval_args(args, host)?;
        self.call_method(base, key, &values, host)
    }

    fn place(&mut self, target: &Expr, host: &mut dyn Host) -> R<Place> {
        match target {
            Expr::Ident(name) => Ok(Place::Name(name.clone())),
            Expr::Chain(head, links) => {
                let Some((last, before)) = links.split_last() else {
                    return self.error(ErrorKind::Reference, "invalid assignment target");
                };
                let base = self.eval_chain(head, before, host)?;
                let key = self.link_key(last, host)?;
                Ok(Place::Prop(base, key))
            }
            _ => self.error(ErrorKind::Reference, "invalid assignment target"),
        }
    }

    fn read_place(&mut self, place: &Place, host: &mut dyn Host) -> R<Value> {
        match place {
            Place::Name(name) => self.get_name(name, host),
            Place::Prop(base, key) => self.get(base, key, host),
        }
    }

    fn write_place(&mut self, place: &Place, value: Value, host: &mut dyn Host) -> R<()> {
        match place {
            Place::Name(name) => self.set_name(name, value, host),
            Place::Prop(base, key) => self.set(base, key, value, host),
        }
    }

    fn eval_assign(
        &mut self,
        op: Option<BinOp>,
        target: &Expr,
        value: &Expr,
        host: &mut dyn Host,
    ) -> R<Value> {
        let place = self.place(target, host)?;
        let result = match op {
            None => self.eval(value, host)?,
            Some(op) => {
                let old = self.read_place(&place, host)?;
                let right = self.eval(value, host)?;
                self.binary_op(op, &old, &right, host)?
            }
        };
        self.write_place(&place, result.clone(), host)?;
        Ok(result)
    }

    fn eval_update(
        &mut self,
        prefix: bool,
        delta: f64,
        target: &Expr,
        host: &mut dyn Host,
    ) -> R<Value> {
        let place = self.place(target, host)?;
        let old = self.read_place(&place, host)?;
        let old = self.number(&old, host)?;
        let new = old + delta;
        self.write_place(&place, Value::Number(new), host)?;
        Ok(Value::Number(if prefix { new } else { old }))
    }

    fn type_name(&self, v: &Value) -> &'static str {
        match v {
            Value::Undefined => "undefined",
            Value::Null | Value::Host(_) => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::Str(_) => "string",
            Value::Object(_) if self.is_callable(v) => "function",
            Value::Object(_) => "object",
        }
    }

    fn eval_unary(&mut self, op: UnOp, operand: &Expr, host: &mut dyn Host) -> R<Value> {
        match op {
            UnOp::Typeof => {
                let value = match operand {
                    Expr::Ident(name) => self.lookup(name, host)?.unwrap_or(Value::Undefined),
                    _ => self.eval(operand, host)?,
                };
                Ok(Value::Str(self.names.get(self.type_name(&value))))
            }
            UnOp::Delete => self.eval_delete(operand, host),
            UnOp::Void => {
                self.eval(operand, host)?;
                Ok(Value::Undefined)
            }
            UnOp::Not => {
                let value = self.eval(operand, host)?;
                Ok(Value::Bool(!to_bool(&value)))
            }
            UnOp::Neg | UnOp::Plus | UnOp::BitNot => {
                let value = self.eval(operand, host)?;
                let n = self.number(&value, host)?;
                Ok(Value::Number(match op {
                    UnOp::Neg => -n,
                    UnOp::BitNot => f64::from(!to_int32(n)),
                    _ => n,
                }))
            }
        }
    }

    fn eval_delete(&mut self, operand: &Expr, host: &mut dyn Host) -> R<Value> {
        let deleted = match operand {
            Expr::Ident(name) => match self.resolve(name, host)? {
                Slot::Global(at) => {
                    let global = self.global;
                    self.charge(1 + self.obj(global).props.list.len() as u64 / 8)?;
                    self.obj_mut(global).props.remove(at);
                    true
                }
                Slot::Holder(holder) => self.delete_key(&holder, &Key::Name(name.clone()))?,
                Slot::Missing => true,
                _ => false,
            },
            Expr::Chain(_, links) if !matches!(links.last(), Some(Link::Call(_))) => {
                match self.place(operand, host)? {
                    Place::Prop(base, key) => self.delete_key(&base, &key)?,
                    Place::Name(_) => false,
                }
            }
            _ => {
                self.eval(operand, host)?;
                true
            }
        };
        Ok(Value::Bool(deleted))
    }

    fn eval_binary(
        &mut self,
        first: &Expr,
        rest: &[(BinOp, Expr)],
        host: &mut dyn Host,
    ) -> R<Value> {
        let mut left = self.eval(first, host)?;
        for (op, right) in rest {
            match op {
                BinOp::And | BinOp::Or => {
                    if to_bool(&left) == (*op == BinOp::Or) {
                        return Ok(left);
                    }
                    left = self.eval(right, host)?;
                }
                _ => {
                    let value = self.eval(right, host)?;
                    left = self.binary_op(*op, &left, &value, host)?;
                }
            }
        }
        Ok(left)
    }

    pub(super) fn concat(&mut self, a: &str, b: &str) -> R<Value> {
        let len = a.len().saturating_add(b.len());
        if len > MAX_STRING_BYTES {
            return Err(Abort::Memory);
        }
        self.charge(len as u64 / BYTES_PER_STEP)?;
        self.mem(len)?;
        let mut text = String::with_capacity(len);
        text.push_str(a);
        text.push_str(b);
        Ok(Value::Str(Arc::from(text)))
    }

    fn add(&mut self, a: &Value, b: &Value, host: &mut dyn Host) -> R<Value> {
        let a = self.primitive(a, Hint::Default, host)?;
        let b = self.primitive(b, Hint::Default, host)?;
        if matches!(a, Value::Str(_)) || matches!(b, Value::Str(_)) {
            let a = self.string_of(&a, host)?;
            let b = self.string_of(&b, host)?;
            return self.concat(&a, &b);
        }
        Ok(Value::Number(
            self.number(&a, host)? + self.number(&b, host)?,
        ))
    }

    fn compare(
        &mut self,
        a: &Value,
        b: &Value,
        host: &mut dyn Host,
    ) -> R<Option<std::cmp::Ordering>> {
        let a = self.primitive(a, Hint::Number, host)?;
        let b = self.primitive(b, Hint::Number, host)?;
        if let (Value::Str(x), Value::Str(y)) = (&a, &b) {
            self.charge(x.len().min(y.len()) as u64 / 64)?;
            return Ok(Some(x.cmp(y)));
        }
        let x = self.number(&a, host)?;
        let y = self.number(&b, host)?;
        Ok(x.partial_cmp(&y))
    }

    pub(super) fn loose_eq(&mut self, a: &Value, b: &Value, host: &mut dyn Host) -> R<bool> {
        use Value::{Bool, Host as H, Null, Number, Object, Str, Undefined};
        Ok(match (a, b) {
            (Undefined | Null, Undefined | Null) => true,
            (Undefined | Null, _) | (_, Undefined | Null) => false,
            (Number(_), Str(_)) | (Str(_), Number(_)) => {
                self.number(a, host)? == self.number(b, host)?
            }
            (Bool(x), other) | (other, Bool(x)) if !matches!(other, Bool(_)) => {
                let n = Number(f64::from(u8::from(*x)));
                return self.loose_eq(&n, other, host);
            }
            (Object(_), H(_)) | (H(_), Object(_)) => false,
            (Object(_), Object(_)) | (H(_), H(_)) => a == b,
            (Object(_), _) | (H(_), _) => {
                let primitive = self.primitive(a, Hint::Default, host)?;
                return self.loose_eq(&primitive, b, host);
            }
            (_, Object(_)) | (_, H(_)) => {
                let primitive = self.primitive(b, Hint::Default, host)?;
                return self.loose_eq(a, &primitive, host);
            }
            _ => a == b,
        })
    }

    fn binary_op(&mut self, op: BinOp, a: &Value, b: &Value, host: &mut dyn Host) -> R<Value> {
        use std::cmp::Ordering::{Greater, Less};
        if let (Value::Number(x), Value::Number(y)) = (a, b) {
            match op {
                BinOp::Add => return Ok(Value::Number(x + y)),
                BinOp::Sub => return Ok(Value::Number(x - y)),
                BinOp::Mul => return Ok(Value::Number(x * y)),
                BinOp::Lt => return Ok(Value::Bool(x < y)),
                _ => {}
            }
        }
        Ok(match op {
            BinOp::Add => return self.add(a, b, host),
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let x = self.number(a, host)?;
                let y = self.number(b, host)?;
                Value::Number(match op {
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    BinOp::Div => x / y,
                    _ => x % y,
                })
            }
            BinOp::Shl
            | BinOp::Shr
            | BinOp::UShr
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor => {
                let x = self.number(a, host)?;
                let y = self.number(b, host)?;
                let (i, j) = (to_int32(x), to_int32(y));
                let shift = to_uint32(y) & 31;
                Value::Number(match op {
                    BinOp::Shl => f64::from(i.wrapping_shl(shift)),
                    BinOp::Shr => f64::from(i.wrapping_shr(shift)),
                    BinOp::UShr => f64::from(to_uint32(x).wrapping_shr(shift)),
                    BinOp::BitAnd => f64::from(i & j),
                    BinOp::BitOr => f64::from(i | j),
                    _ => f64::from(i ^ j),
                })
            }
            BinOp::Lt => Value::Bool(self.compare(a, b, host)? == Some(Less)),
            BinOp::Gt => Value::Bool(self.compare(a, b, host)? == Some(Greater)),
            BinOp::Le => Value::Bool(matches!(
                self.compare(a, b, host)?,
                Some(o) if o != Greater
            )),
            BinOp::Ge => Value::Bool(matches!(
                self.compare(a, b, host)?,
                Some(o) if o != Less
            )),
            BinOp::Eq => Value::Bool(self.loose_eq(a, b, host)?),
            BinOp::Ne => Value::Bool(!self.loose_eq(a, b, host)?),
            BinOp::StrictEq => Value::Bool(a == b),
            BinOp::StrictNe => Value::Bool(a != b),
            BinOp::In => {
                let key = self.key_of(a, host)?;
                match b {
                    Value::Object(r) => Value::Bool(self.has_property(*r, &key)?),
                    Value::Host(id) => Value::Bool(host.has(*id, &key.name())),
                    _ => return self.error(ErrorKind::Type, "'in' needs an object"),
                }
            }
            BinOp::InstanceOf => return self.instance_of(a, b),
            BinOp::And | BinOp::Or => Value::Undefined,
        })
    }

    fn instance_of(&mut self, a: &Value, b: &Value) -> R<Value> {
        if !self.is_callable(b) {
            return self.error(ErrorKind::Type, "'instanceof' needs a function");
        }
        let (Value::Object(r), Value::Object(f)) = (a, b) else {
            return Ok(Value::Bool(false));
        };
        let key = Key::Name(self.names.get("prototype"));
        let Value::Object(proto) = self.obj_get(*f, &key)? else {
            return Ok(Value::Bool(false));
        };
        let mut current = self.obj(*r).proto;
        while let Some(c) = current {
            self.tick()?;
            if c == proto {
                return Ok(Value::Bool(true));
            }
            current = self.obj(c).proto;
        }
        Ok(Value::Bool(false))
    }

    // Statements.

    pub(super) fn exec_list(&mut self, list: &[Stmt], host: &mut dyn Host) -> R<Flow> {
        for stmt in list {
            match self.exec(stmt, host)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec(&mut self, stmt: &Stmt, host: &mut dyn Host) -> R<Flow> {
        self.tick()?;
        self.enter()?;
        let result = self.exec_inner(stmt, host);
        self.depth -= 1;
        result
    }

    fn exec_inner(&mut self, stmt: &Stmt, host: &mut dyn Host) -> R<Flow> {
        match stmt {
            Stmt::Empty => Ok(Flow::Normal),
            Stmt::Expr(e) => self.exec_expr(e, host),
            Stmt::Var(list) => self.exec_var(list, host),
            Stmt::Block(list) => self.exec_list(list, host),
            Stmt::If(arms, otherwise) => self.exec_if(arms, otherwise.as_deref(), host),
            Stmt::Loop { .. } => self.exec_loop(stmt, host),
            Stmt::ForIn {
                labels,
                target,
                object,
                body,
            } => self.exec_for_in(labels, target, object, body, host),
            Stmt::Switch(subject, cases) => self.exec_switch(subject, cases, host),
            Stmt::Break(label) => Ok(Flow::Break(label.clone())),
            Stmt::Continue(label) => Ok(Flow::Continue(label.clone())),
            Stmt::Return(value) => self.exec_return(value.as_ref(), host),
            Stmt::With(object, body) => self.exec_with(object, body, host),
            Stmt::Try {
                block,
                handler,
                finalizer,
            } => self.exec_try(block, handler.as_ref(), finalizer.as_deref(), host),
            Stmt::Throw(e) => self.exec_throw(e, host),
            Stmt::Labeled(label, body) => self.exec_labeled(label, body, host),
        }
    }

    fn exec_expr(&mut self, e: &Expr, host: &mut dyn Host) -> R<Flow> {
        let value = self.eval(e, host)?;
        if self.track {
            self.last = value;
        }
        Ok(Flow::Normal)
    }

    fn exec_var(&mut self, list: &[(Name, Option<Expr>)], host: &mut dyn Host) -> R<Flow> {
        for (name, init) in list {
            if let Some(init) = init {
                let value = self.eval(init, host)?;
                self.set_name(name, value, host)?;
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_if(
        &mut self,
        arms: &[(Expr, Stmt)],
        otherwise: Option<&Stmt>,
        host: &mut dyn Host,
    ) -> R<Flow> {
        for (cond, body) in arms {
            let test = self.eval(cond, host)?;
            if to_bool(&test) {
                return self.exec(body, host);
            }
        }
        match otherwise {
            Some(body) => self.exec(body, host),
            None => Ok(Flow::Normal),
        }
    }

    fn exec_return(&mut self, value: Option<&Expr>, host: &mut dyn Host) -> R<Flow> {
        Ok(Flow::Return(match value {
            Some(e) => self.eval(e, host)?,
            None => Value::Undefined,
        }))
    }

    fn exec_with(&mut self, object: &Expr, body: &Stmt, host: &mut dyn Host) -> R<Flow> {
        let value = self.eval(object, host)?;
        if matches!(value, Value::Undefined | Value::Null) {
            return self.error(ErrorKind::Type, "object expected");
        }
        let frame = self.new_scope(self.scope);
        self.scopes[frame as usize].with = Some(value);
        let saved = self.scope.replace(frame);
        let result = self.exec(body, host);
        self.scope = saved;
        self.release_scope(frame);
        result
    }

    fn exec_throw(&mut self, e: &Expr, host: &mut dyn Host) -> R<Flow> {
        Err(Abort::Throw(self.eval(e, host)?))
    }

    fn exec_labeled(&mut self, label: &Name, body: &Stmt, host: &mut dyn Host) -> R<Flow> {
        Ok(match self.exec(body, host)? {
            Flow::Break(Some(l)) if l == *label => Flow::Normal,
            flow => flow,
        })
    }

    /// Whether a loop carries on after its body, or else what leaves it.
    fn after_body(flow: Flow, labels: &[Name]) -> Result<(), Flow> {
        match flow {
            Flow::Normal | Flow::Continue(None) => Ok(()),
            Flow::Break(None) => Err(Flow::Normal),
            Flow::Continue(Some(l)) if labels.contains(&l) => Ok(()),
            Flow::Break(Some(l)) if labels.contains(&l) => Err(Flow::Normal),
            other => Err(other),
        }
    }

    fn exec_loop(&mut self, stmt: &Stmt, host: &mut dyn Host) -> R<Flow> {
        let Stmt::Loop {
            labels,
            init,
            cond,
            update,
            body,
            test_first,
        } = stmt
        else {
            return Ok(Flow::Normal);
        };
        if let Some(init) = init {
            self.exec(init, host)?;
        }
        let mut test = *test_first;
        loop {
            self.tick()?;
            if test && let Some(cond) = cond {
                let value = self.eval(cond, host)?;
                if !to_bool(&value) {
                    return Ok(Flow::Normal);
                }
            }
            test = true;
            let flow = self.exec(body, host)?;
            if let Err(leaving) = Self::after_body(flow, labels) {
                return Ok(leaving);
            }
            if let Some(update) = update {
                self.eval(update, host)?;
            }
        }
    }

    /// Every enumerable key, nearest object first, shadowed ones once.
    fn enumerable_keys(&mut self, start: ObjectRef) -> R<Vec<Value>> {
        let mut keys = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = Some(start);
        while let Some(c) = current {
            self.tick()?;
            let obj = self.obj(c);
            let mut found = Vec::new();
            if let Kind::Array(items) = &obj.kind {
                let present = items.iter().enumerate().filter(|(_, v)| v.is_some());
                found.extend(present.map(|(i, _)| Arc::<str>::from(i.to_string())));
            }
            for prop in &obj.props.list {
                if prop.hidden {
                    seen.insert(prop.key.clone());
                } else {
                    found.push(prop.key.clone());
                }
            }
            current = obj.proto;
            self.charge(found.len() as u64)?;
            self.mem(found.len() * SLOT_BYTES)?;
            for key in found {
                if seen.insert(key.clone()) {
                    keys.push(Value::Str(key));
                }
            }
        }
        Ok(keys)
    }

    fn exec_for_in(
        &mut self,
        labels: &[Name],
        target: &Expr,
        object: &Expr,
        body: &Stmt,
        host: &mut dyn Host,
    ) -> R<Flow> {
        let subject = self.eval(object, host)?;
        let Value::Object(r) = subject else {
            return Ok(Flow::Normal);
        };
        for key in self.enumerable_keys(r)? {
            self.tick()?;
            let still_there = {
                let k = self.key_of(&key, host)?;
                self.has_property(r, &k)?
            };
            if !still_there {
                continue;
            }
            let place = self.place(target, host)?;
            self.write_place(&place, key, host)?;
            let flow = self.exec(body, host)?;
            if let Err(leaving) = Self::after_body(flow, labels) {
                return Ok(leaving);
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_switch(
        &mut self,
        subject: &Expr,
        cases: &[(Option<Expr>, Vec<Stmt>)],
        host: &mut dyn Host,
    ) -> R<Flow> {
        let value = self.eval(subject, host)?;
        let mut start = None;
        for (i, (test, _)) in cases.iter().enumerate() {
            if let Some(test) = test
                && self.eval(test, host)? == value
            {
                start = Some(i);
                break;
            }
        }
        let start = start.or_else(|| cases.iter().position(|(test, _)| test.is_none()));
        let Some(start) = start else {
            return Ok(Flow::Normal);
        };
        for (_, body) in cases.get(start..).unwrap_or(&[]) {
            match self.exec_list(body, host)? {
                Flow::Normal => {}
                Flow::Break(None) => return Ok(Flow::Normal),
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_catch(
        &mut self,
        name: &Name,
        value: Value,
        body: &[Stmt],
        host: &mut dyn Host,
    ) -> R<Flow> {
        let frame = self.new_scope(self.scope);
        self.mem(OBJECT_BYTES / 4)?;
        self.scopes[frame as usize].vars.push((name.clone(), value));
        let saved = self.scope.replace(frame);
        let result = self.exec_list(body, host);
        self.scope = saved;
        self.release_scope(frame);
        result
    }

    fn exec_try(
        &mut self,
        block: &[Stmt],
        handler: Option<&(Name, Vec<Stmt>)>,
        finalizer: Option<&[Stmt]>,
        host: &mut dyn Host,
    ) -> R<Flow> {
        let mut result = self.exec_list(block, host);
        if let Some((name, body)) = handler {
            result = match result {
                Err(Abort::Throw(value)) => self.exec_catch(name, value, body, host),
                other => other,
            };
        }
        if let Some(finalizer) = finalizer {
            if matches!(result, Err(Abort::Budget | Abort::Memory | Abort::Depth)) {
                return result;
            }
            match self.exec_list(finalizer, host)? {
                Flow::Normal => {}
                flow => return Ok(flow),
            }
        }
        result
    }
}
