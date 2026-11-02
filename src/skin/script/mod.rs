//! A small sandboxed interpreter for the Microsoft JScript that Windows Media
//! Player skins are written in.
//!
//! Skin files are untrusted, so the interpreter is built around what it must
//! not do: it has no filesystem, network or process access, and every entry
//! point runs under a step budget, a heap cap and depth limits. Going over any
//! of them returns an error from that entry point and leaves the engine usable.
//! The application reaches scripts, and scripts reach the application, only
//! through [`Host`].
//!
//! Names are matched as real skins expect rather than as the language says:
//! a bare name that is not found exactly falls back to a script global of any
//! case, and a missing property to one of any case on the object or its
//! prototypes. Skins written against JScript's error tolerance depend on it.

mod ast;
mod builtins;
mod convert;
mod heap;
mod interp;
mod lexer;
mod parser;
mod regex;
mod stack;
mod strings;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use ast::FuncDef;
use heap::{Obj, Scope};
use lexer::Interner;

type Name = Arc<str>;

/// Largest script accepted by `load` or `compile`, in bytes.
const MAX_SOURCE: usize = 8 << 20;

pub type HostId = u64;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Str(Arc<str>),
    Object(ObjectRef),
    Host(HostId),
}

/// A script object, array or function in the engine's heap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectRef(u32);

pub trait Host {
    /// A bare identifier not found in script scope, e.g. `player`, an element
    /// id, `psPlaying`. None means ReferenceError (for typeof, "undefined").
    fn global(&mut self, name: &str) -> Option<Value>;
    /// Whether host object `object` has member `name` (used to resolve bare
    /// names through a handler's scope object).
    fn has(&mut self, object: HostId, name: &str) -> bool;
    fn get(&mut self, object: HostId, name: &str) -> Value;
    fn set(&mut self, object: HostId, name: &str, value: Value);
    /// A method call `object.name(args)`. Err is a script-catchable error message.
    fn call(&mut self, object: HostId, name: &str, args: &[Value]) -> Result<Value, String>;
    /// A host object indexed `object[i]` or called directly `object(i)`.
    fn index(&mut self, object: HostId, index: f64) -> Value {
        let _ = (object, index);
        Value::Undefined
    }
}

pub struct Limits {
    pub steps: u64,
    pub heap_bytes: usize,
    /// Nested script and builtin calls.
    pub max_depth: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            steps: 400_000,
            heap_bytes: 16 << 20,
            max_depth: 96,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScriptError {
    Syntax { line: u32, message: String },
    Thrown(String),
    Budget,
    Memory,
    Depth,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptError::Syntax { line, message } => write!(f, "line {line}: {message}"),
            ScriptError::Thrown(message) => f.write_str(message),
            ScriptError::Budget => f.write_str("script ran for too long"),
            ScriptError::Memory => f.write_str("script used too much memory"),
            ScriptError::Depth => f.write_str("script nested too deeply"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// A compiled handler or expression, owned by the [`Engine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Code(u32);

/// The prototypes builtins need after a script has had the chance to delete
/// the constructors they hang from.
struct Protos {
    object: ObjectRef,
    function: ObjectRef,
    array: ObjectRef,
    string: ObjectRef,
    number: ObjectRef,
    boolean: ObjectRef,
    date: ObjectRef,
    regexp: ObjectRef,
    errors: [ObjectRef; 7],
}

pub struct Engine {
    limits: Limits,
    objs: Vec<Obj>,
    free_objs: Vec<u32>,
    scopes: Vec<Scope>,
    free_scopes: Vec<u32>,
    /// Estimated bytes: exact after a collection, an upper bound between them.
    bytes: usize,
    /// Answers for a reference that does not belong to this heap.
    dead: Obj,
    global: ObjectRef,
    protos: Protos,
    /// Lower-cased name of each script-defined global to its real name.
    /// Builtins stay out, so a skin's element `date` is not taken for `Date`.
    folded: HashMap<String, Name>,
    steps: u64,
    /// Native recursion, weighted; see `interp::MAX_NATIVE_DEPTH`.
    depth: u32,
    calls: u32,
    scope: Option<u32>,
    this: Value,
    /// Completion value, tracked only in handler and `eval` code.
    last: Value,
    track: bool,
    /// Whether the running native was reached through `new`.
    is_new: bool,
    names: Interner,
    codes: Vec<Arc<FuncDef>>,
    code_cache: HashMap<String, Code>,
    regex_cache: HashMap<(Name, Name), Arc<regex::Program>>,
    rng: u64,
}

fn strip_prefix(source: &str) -> &str {
    let trimmed = source.trim_start();
    match trimmed.get(..8) {
        Some(prefix) if prefix.eq_ignore_ascii_case("jscript:") => trimmed.get(8..).unwrap_or(""),
        _ => source,
    }
}

impl Engine {
    pub fn new(limits: Limits) -> Self {
        builtins::create(limits)
    }

    /// Runs a script file's top level in the global scope. `var`s and
    /// functions become globals. Skins carry bugs that JScript shrugged off,
    /// so a thrown error does not stop the file: every function is already
    /// hoisted, the next top-level statement runs, and the first error is
    /// returned at the end. A syntax error changes nothing. Running out of a
    /// limit stops the file.
    pub fn load(&mut self, source: &str, host: &mut dyn Host) -> Result<(), ScriptError> {
        if source.len() > MAX_SOURCE {
            return Err(ScriptError::Memory);
        }
        let def = parser::parse(source, &mut self.names, parser::MAX_NESTING)?;
        self.begin();
        let result = self.run_program(&def, host);
        self.finish(result.map(|_| Value::Undefined), host)
            .map(|_| ())
    }

    /// Compiles handler text (statements as written in an attribute, perhaps
    /// prefixed `jscript:`) or a single expression. Cached by source text.
    pub fn compile(&mut self, source: &str) -> Result<Code, ScriptError> {
        stack::mark();
        if source.len() > MAX_SOURCE {
            return Err(ScriptError::Memory);
        }
        if let Some(code) = self.code_cache.get(source) {
            return Ok(*code);
        }
        let def = parser::parse(strip_prefix(source), &mut self.names, parser::MAX_NESTING)?;
        let code = Code(self.codes.len() as u32);
        self.codes.push(def);
        self.code_cache.insert(source.to_string(), code);
        Ok(code)
    }

    /// Runs compiled code as an event handler. Bare names resolve through
    /// `args`, then `scope`'s members, then script globals, then
    /// [`Host::global`]. A `var` in the text is local. Returns the value of
    /// the last expression statement.
    pub fn run(
        &mut self,
        code: Code,
        scope: Option<HostId>,
        args: &[(&str, Value)],
        host: &mut dyn Host,
    ) -> Result<Value, ScriptError> {
        let Some(def) = self.codes.get(code.0 as usize).cloned() else {
            return Err(ScriptError::Thrown("unknown code".to_string()));
        };
        self.begin();
        let result = self.run_handler(&def, scope, args, host);
        self.finish(result, host)
    }

    /// Calls a global function by name; a name that is not one is an error.
    pub fn call_global(
        &mut self,
        name: &str,
        args: &[Value],
        host: &mut dyn Host,
    ) -> Result<Value, ScriptError> {
        self.begin();
        let function = self.global_value(name);
        if !self.is_callable(&function) {
            return Err(ScriptError::Thrown(format!("'{name}' is not a function")));
        }
        let this = self.this.clone();
        let result = self.call(&function, this, args, host);
        self.finish(result, host)
    }

    pub fn call_value(
        &mut self,
        function: &Value,
        args: &[Value],
        host: &mut dyn Host,
    ) -> Result<Value, ScriptError> {
        self.begin();
        let this = self.this.clone();
        let result = self.call(function, this, args, host);
        self.finish(result, host)
    }

    /// ECMAScript ToString for the host. Never fails: an object whose
    /// conversion throws or runs out of a limit reads as `[object Object]`.
    pub fn to_str(&mut self, value: &Value, host: &mut dyn Host) -> Arc<str> {
        self.begin();
        self.string_of(value, host)
            .unwrap_or_else(|_| Arc::from("[object Object]"))
    }

    /// ECMAScript ToNumber for the host. Never fails: see [`Self::to_str`].
    pub fn to_number(&mut self, value: &Value, host: &mut dyn Host) -> f64 {
        self.begin();
        self.number(value, host).unwrap_or(f64::NAN)
    }

    pub fn heap_bytes(&self) -> usize {
        self.bytes
    }
}

pub fn to_bool(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => *n != 0.0 && !n.is_nan(),
        Value::Str(s) => !s.is_empty(),
        Value::Object(_) | Value::Host(_) => true,
    }
}
