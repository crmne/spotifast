//! The arena of objects and scopes, and the mark-sweep collector. Everything
//! refers to everything else by index, so cycles cost nothing and the engine
//! stays `Send`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::ast::FuncDef;
use super::interp::{Abort, NativeFn, R};
use super::regex::Program;
use super::{Engine, HostId, Name, ObjectRef, Value};

/// Estimated cost of an object, a property and an array slot.
pub(super) const OBJECT_BYTES: usize = 96;
pub(super) const PROP_BYTES: usize = 64;
pub(super) const SLOT_BYTES: usize = 24;
const SCOPE_BYTES: usize = 64;

/// Strings shared by many references are counted once from this size up;
/// below it hashing each one would cost more than the error is worth.
const SHARED_STRING_BYTES: usize = 256;

/// Past this many properties an object gets a hash index.
const LINEAR_PROPS: usize = 12;

pub(super) struct Prop {
    pub key: Name,
    pub value: Value,
    pub hidden: bool,
}

/// Properties in insertion order, which is the order `for in` shows.
#[derive(Default)]
pub(super) struct Props {
    pub list: Vec<Prop>,
    index: Option<HashMap<Name, u32>>,
}

impl Props {
    pub fn find(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(key).map(|i| *i as usize),
            None => self.list.iter().position(|p| &*p.key == key),
        }
    }

    pub fn insert(&mut self, key: Name, value: Value, hidden: bool) {
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.list.len() as u32);
        }
        self.list.push(Prop { key, value, hidden });
        if self.index.is_none() && self.list.len() > LINEAR_PROPS {
            self.reindex();
        }
    }

    pub fn remove(&mut self, at: usize) {
        if at < self.list.len() {
            self.list.remove(at);
            if self.index.is_some() {
                self.reindex();
            }
        }
    }

    fn reindex(&mut self) {
        let pairs = self.list.iter().enumerate();
        self.index = Some(pairs.map(|(i, p)| (p.key.clone(), i as u32)).collect());
    }
}

pub(super) enum Kind {
    Free,
    Plain,
    Array(Vec<Option<Value>>),
    Function(Arc<FuncDef>, Option<u32>),
    Native(NativeFn),
    Error,
    Date(f64),
    Regex(Arc<Program>),
    Boxed(Value),
}

pub(super) struct Obj {
    pub proto: Option<ObjectRef>,
    pub kind: Kind,
    pub props: Props,
}

impl Obj {
    pub fn new(proto: Option<ObjectRef>, kind: Kind) -> Self {
        Obj {
            proto,
            kind,
            props: Props::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct Scope {
    pub parent: Option<u32>,
    pub vars: Vec<(Name, Value)>,
    /// The object of a `with` statement.
    pub with: Option<Value>,
    /// A handler's scope object.
    pub host: Option<HostId>,
    /// A closure holds this scope, so it outlives its call.
    pub captured: bool,
}

pub(super) fn find_var(vars: &[(Name, Value)], name: &Name) -> Option<usize> {
    vars.iter()
        .position(|(k, _)| Arc::ptr_eq(k, name) || k == name)
}

impl Engine {
    pub(super) fn obj(&self, r: ObjectRef) -> &Obj {
        self.objs.get(r.0 as usize).unwrap_or(&self.dead)
    }

    pub(super) fn obj_mut(&mut self, r: ObjectRef) -> &mut Obj {
        match self.objs.get_mut(r.0 as usize) {
            Some(obj) => obj,
            None => &mut self.dead,
        }
    }

    /// Adds to the heap estimate, failing once it passes the limit. Callers
    /// charge before they allocate.
    pub(super) fn mem(&mut self, bytes: usize) -> R<()> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > self.limits.heap_bytes {
            return Err(Abort::Memory);
        }
        Ok(())
    }

    /// An allocation that cannot fail, for the builtins made before any
    /// script runs.
    pub(super) fn alloc_raw(&mut self, obj: Obj) -> ObjectRef {
        self.bytes = self.bytes.saturating_add(OBJECT_BYTES);
        match self.free_objs.pop() {
            Some(i) => {
                if let Some(slot) = self.objs.get_mut(i as usize) {
                    *slot = obj;
                }
                ObjectRef(i)
            }
            None => {
                self.objs.push(obj);
                ObjectRef(self.objs.len() as u32 - 1)
            }
        }
    }

    pub(super) fn alloc(&mut self, proto: ObjectRef, kind: Kind) -> R<ObjectRef> {
        self.mem(OBJECT_BYTES)?;
        Ok(self.alloc_raw(Obj::new(Some(proto), kind)))
    }

    pub(super) fn new_scope(&mut self, parent: Option<u32>) -> u32 {
        match self.free_scopes.pop() {
            Some(i) => {
                self.scopes[i as usize].parent = parent;
                i
            }
            None => {
                self.scopes.push(Scope {
                    parent,
                    ..Scope::default()
                });
                self.scopes.len() as u32 - 1
            }
        }
    }

    /// Returns a scope to the pool unless a closure kept it. Calls that make
    /// no closures therefore leave no garbage, whatever a loop does.
    pub(super) fn release_scope(&mut self, i: u32) {
        let scope = &mut self.scopes[i as usize];
        if !scope.captured {
            scope.vars.clear();
            scope.with = None;
            scope.host = None;
            scope.parent = None;
            self.free_scopes.push(i);
        }
    }

    /// Marks a scope chain as held by a closure.
    pub(super) fn capture_scopes(&mut self, from: Option<u32>) -> R<()> {
        let mut current = from;
        while let Some(i) = current {
            let scope = &mut self.scopes[i as usize];
            if scope.captured {
                break;
            }
            scope.captured = true;
            current = scope.parent;
            let bytes = SCOPE_BYTES + scope.vars.len() * PROP_BYTES;
            self.mem(bytes)?;
        }
        Ok(())
    }

    fn protos_list(&self) -> Vec<ObjectRef> {
        let p = &self.protos;
        let mut list = vec![
            self.global,
            p.object,
            p.function,
            p.array,
            p.string,
            p.number,
            p.boolean,
            p.date,
            p.regexp,
        ];
        list.extend_from_slice(&p.errors);
        list
    }

    /// Frees unreachable objects. Called between handlers, never during one.
    /// The worklist is explicit, so object graph depth cannot reach the stack.
    pub fn collect(&mut self, roots: &[Value]) {
        enum Work {
            Object(ObjectRef),
            Scope(u32),
        }
        fn visit(value: &Value, work: &mut Vec<Work>) {
            if let Value::Object(r) = value {
                work.push(Work::Object(*r));
            }
        }
        let mut work: Vec<Work> = self.protos_list().into_iter().map(Work::Object).collect();
        roots.iter().for_each(|v| visit(v, &mut work));
        let mut live_objs = vec![false; self.objs.len()];
        let mut live_scopes = vec![false; self.scopes.len()];
        while let Some(item) = work.pop() {
            match item {
                Work::Object(r) => {
                    let Some(seen) = live_objs.get_mut(r.0 as usize) else {
                        continue;
                    };
                    if std::mem::replace(seen, true) {
                        continue;
                    }
                    let obj = self.obj(r);
                    work.extend(obj.proto.map(Work::Object));
                    obj.props
                        .list
                        .iter()
                        .for_each(|p| visit(&p.value, &mut work));
                    match &obj.kind {
                        Kind::Array(items) => {
                            items.iter().flatten().for_each(|v| visit(v, &mut work))
                        }
                        Kind::Function(_, scope) => work.extend(scope.map(Work::Scope)),
                        Kind::Boxed(v) => visit(v, &mut work),
                        _ => {}
                    }
                }
                Work::Scope(i) => {
                    let Some(seen) = live_scopes.get_mut(i as usize) else {
                        continue;
                    };
                    if std::mem::replace(seen, true) {
                        continue;
                    }
                    let scope = &self.scopes[i as usize];
                    work.extend(scope.parent.map(Work::Scope));
                    scope.vars.iter().for_each(|(_, v)| visit(v, &mut work));
                    scope.with.iter().for_each(|v| visit(v, &mut work));
                }
            }
        }
        self.sweep(&live_objs, &live_scopes);
    }

    fn sweep(&mut self, live_objs: &[bool], live_scopes: &[bool]) {
        let mut bytes = 0usize;
        let mut seen = HashSet::new();
        let mut string = |value: &Value| {
            if let Value::Str(s) = value
                && (s.len() < SHARED_STRING_BYTES || seen.insert(s.as_ptr() as usize))
            {
                return s.len();
            }
            0
        };
        self.free_objs.clear();
        for (i, obj) in self.objs.iter_mut().enumerate() {
            if !live_objs.get(i).copied().unwrap_or(false) {
                if !matches!(obj.kind, Kind::Free) {
                    *obj = Obj::new(None, Kind::Free);
                }
                self.free_objs.push(i as u32);
                continue;
            }
            bytes += OBJECT_BYTES + obj.props.list.len() * PROP_BYTES;
            for prop in &obj.props.list {
                bytes += prop.key.len() + string(&prop.value);
            }
            match &obj.kind {
                Kind::Array(items) => {
                    bytes += items.len() * SLOT_BYTES;
                    bytes += items.iter().flatten().map(&mut string).sum::<usize>();
                }
                Kind::Boxed(value) => bytes += string(value),
                _ => {}
            }
        }
        self.free_scopes.clear();
        for (i, scope) in self.scopes.iter_mut().enumerate() {
            if live_scopes.get(i).copied().unwrap_or(false) {
                bytes += SCOPE_BYTES + scope.vars.len() * PROP_BYTES;
                bytes += scope.vars.iter().map(|(_, v)| string(v)).sum::<usize>();
            } else {
                *scope = Scope::default();
                self.free_scopes.push(i as u32);
            }
        }
        self.bytes = bytes;
        let globals = &self.objs[self.global.0 as usize].props;
        self.folded.retain(|_, name| globals.find(name).is_some());
    }
}
