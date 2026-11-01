//! The standard library, apart from strings and regular expressions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::convert::{
    number_to_string, parse_float, parse_int, relative, to_exponential, to_fixed, to_integer,
    to_precision, to_radix,
};
use super::heap::{Kind, Obj, SLOT_BYTES};
use super::interp::{Abort, BYTES_PER_STEP, ErrorKind, Key, MAX_STRING_BYTES, NativeFn, R};
use super::lexer::Interner;
use super::strings::new_array;
use super::{Engine, Host, Limits, ObjectRef, Protos, Value};

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undefined)
}

fn num_arg(e: &mut Engine, h: &mut dyn Host, args: &[Value], i: usize) -> R<f64> {
    let value = arg(args, i);
    e.number(&value, h)
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

pub(super) fn create(limits: Limits) -> Engine {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0x9e37, |d| d.as_nanos() as u64);
    let zero = ObjectRef(0);
    let mut e = Engine {
        limits,
        objs: Vec::new(),
        free_objs: Vec::new(),
        scopes: Vec::new(),
        free_scopes: Vec::new(),
        bytes: 0,
        dead: Obj::new(None, Kind::Free),
        global: zero,
        protos: Protos {
            object: zero,
            function: zero,
            array: zero,
            string: zero,
            number: zero,
            boolean: zero,
            date: zero,
            regexp: zero,
            errors: [zero; 7],
        },
        folded: HashMap::new(),
        steps: 0,
        depth: 0,
        calls: 0,
        scope: None,
        this: Value::Undefined,
        last: Value::Undefined,
        track: false,
        is_new: false,
        names: Interner::default(),
        codes: Vec::new(),
        code_cache: HashMap::new(),
        regex_cache: HashMap::new(),
        rng: seed | 1,
    };
    let object = e.alloc_raw(Obj::new(None, Kind::Plain));
    let plain = |e: &mut Engine, kind| e.alloc_raw(Obj::new(Some(object), kind));
    e.protos.object = object;
    e.protos.function = plain(&mut e, Kind::Plain);
    e.protos.array = plain(&mut e, Kind::Array(Vec::new()));
    e.protos.string = plain(&mut e, Kind::Boxed(Value::Str(Arc::from(""))));
    e.protos.number = plain(&mut e, Kind::Boxed(Value::Number(0.0)));
    e.protos.boolean = plain(&mut e, Kind::Boxed(Value::Bool(false)));
    e.protos.date = plain(&mut e, Kind::Date(f64::NAN));
    e.protos.regexp = plain(&mut e, Kind::Plain);
    e.global = plain(&mut e, Kind::Plain);
    e.install_object();
    e.install_function();
    e.install_array();
    e.install_strings();
    e.install_number();
    e.install_errors();
    e.install_math();
    e.install_date();
    e.install_globals();
    e.begin();
    e
}

fn error_ctor<const KIND: usize>(
    e: &mut Engine,
    h: &mut dyn Host,
    _: &Value,
    a: &[Value],
) -> R<Value> {
    // JScript also takes `new Error(number, description)`.
    let (number, text) = match a {
        [Value::Number(n), text, ..] => (*n, text.clone()),
        [text, ..] => (0.0, text.clone()),
        [] => (0.0, Value::Undefined),
    };
    let message = match text {
        Value::Undefined => e.names.get(""),
        other => e.string_of(&other, h)?,
    };
    let error = e.new_error(KIND, &message)?;
    e.define(error, "number", Value::Number(number));
    Ok(Value::Object(error))
}

impl Engine {
    pub(super) fn method(&mut self, object: ObjectRef, name: &str, f: NativeFn) -> ObjectRef {
        let proto = self.protos.function;
        let function = self.alloc_raw(Obj::new(Some(proto), Kind::Native(f)));
        self.define(object, name, Value::Object(function));
        function
    }

    /// A global constructor and the links between it and its prototype.
    pub(super) fn constructor(&mut self, name: &str, proto: ObjectRef, f: NativeFn) -> ObjectRef {
        let ctor = self.method(self.global, name, f);
        self.define(ctor, "prototype", Value::Object(proto));
        self.define(proto, "constructor", Value::Object(ctor));
        ctor
    }

    fn install_object(&mut self) {
        let p = self.protos.object;
        self.constructor("Object", p, |e, _, _, a| match a.first() {
            Some(value @ Value::Object(_)) => Ok(value.clone()),
            Some(value @ (Value::Str(_) | Value::Number(_) | Value::Bool(_))) => {
                let proto = match value {
                    Value::Str(_) => e.protos.string,
                    Value::Number(_) => e.protos.number,
                    _ => e.protos.boolean,
                };
                Ok(Value::Object(e.alloc(proto, Kind::Boxed(value.clone()))?))
            }
            Some(value @ Value::Host(_)) => Ok(value.clone()),
            _ => Ok(Value::Object(e.alloc(e.protos.object, Kind::Plain)?)),
        });
        for name in ["toString", "toLocaleString"] {
            self.method(p, name, |e, _, t, _| {
                let class = match t {
                    Value::Object(r) => match &e.obj(*r).kind {
                        Kind::Array(_) => "Array",
                        Kind::Function(..) | Kind::Native(_) => "Function",
                        Kind::Error => "Error",
                        Kind::Date(_) => "Date",
                        Kind::Regex(_) => "RegExp",
                        Kind::Boxed(Value::Str(_)) => "String",
                        Kind::Boxed(Value::Number(_)) => "Number",
                        Kind::Boxed(_) => "Boolean",
                        _ => "Object",
                    },
                    _ => "Object",
                };
                e.string_value(&format!("[object {class}]"))
            });
        }
        self.method(p, "valueOf", |_, _, t, _| Ok(t.clone()));
        self.method(p, "hasOwnProperty", |e, h, t, a| {
            let key = e.key_of(&arg(a, 0), h)?;
            Ok(Value::Bool(match t {
                Value::Object(r) => e.has_own(*r, &key),
                _ => false,
            }))
        });
        self.method(p, "propertyIsEnumerable", |e, h, t, a| {
            let key = e.key_of(&arg(a, 0), h)?;
            let Value::Object(r) = t else {
                return Ok(Value::Bool(false));
            };
            let props = &e.obj(*r).props;
            let hidden = props.find(&key.name()).and_then(|i| props.list.get(i));
            Ok(Value::Bool(
                e.has_own(*r, &key) && !hidden.is_some_and(|p| p.hidden),
            ))
        });
        self.method(p, "isPrototypeOf", |e, _, t, a| {
            let (Value::Object(this), Value::Object(r)) = (t, arg(a, 0)) else {
                return Ok(Value::Bool(false));
            };
            let mut current = e.obj(r).proto;
            while let Some(c) = current {
                e.tick()?;
                if c == *this {
                    return Ok(Value::Bool(true));
                }
                current = e.obj(c).proto;
            }
            Ok(Value::Bool(false))
        });
    }

    fn install_function(&mut self) {
        let p = self.protos.function;
        self.constructor("Function", p, |e, h, _, a| {
            let mut parts = Vec::with_capacity(a.len());
            for value in a {
                parts.push(e.string_of(value, h)?.to_string());
            }
            let body = parts.pop().unwrap_or_default();
            let source = format!("(function anonymous({}) {{\n{body}\n}})", parts.join(","));
            let saved = e.scope.take();
            let result = e.eval_source(&source, h);
            e.scope = saved;
            result
        });
        self.method(p, "toString", |e, _, t, _| {
            let name = match t {
                Value::Object(r) => match &e.obj(*r).kind {
                    Kind::Function(def, _) => def.name.clone(),
                    _ => None,
                },
                _ => None,
            };
            let name = name.as_deref().unwrap_or("");
            e.string_value(&format!("function {name}() {{ [native code] }}"))
        });
        self.method(p, "call", |e, h, t, a| {
            let this = arg(a, 0);
            e.call(t, this, a.get(1..).unwrap_or(&[]), h)
        });
        self.method(p, "apply", |e, h, t, a| {
            let args = match arg(a, 1) {
                Value::Object(r) => match &e.obj(r).kind {
                    Kind::Array(items) => {
                        let items: Vec<_> = items
                            .iter()
                            .map(|v| v.clone().unwrap_or(Value::Undefined))
                            .collect();
                        e.charge(items.len() as u64 / 8)?;
                        items
                    }
                    _ => return e.error(ErrorKind::Type, "apply needs an array"),
                },
                Value::Undefined | Value::Null => Vec::new(),
                _ => return e.error(ErrorKind::Type, "apply needs an array"),
            };
            e.call(t, arg(a, 0), &args, h)
        });
    }

    fn this_array(&mut self, this: &Value) -> R<ObjectRef> {
        if let Value::Object(r) = this
            && matches!(self.obj(*r).kind, Kind::Array(_))
        {
            return Ok(*r);
        }
        self.error(ErrorKind::Type, "array method called on another object")
    }

    fn items(&mut self, r: ObjectRef) -> R<Vec<Option<Value>>> {
        self.charge(self.array_len(r) as u64 / 8)?;
        Ok(match &self.obj(r).kind {
            Kind::Array(items) => items.clone(),
            _ => Vec::new(),
        })
    }

    fn with_items<T>(
        &mut self,
        r: ObjectRef,
        f: impl FnOnce(&mut Vec<Option<Value>>) -> T,
    ) -> Option<T> {
        match &mut self.obj_mut(r).kind {
            Kind::Array(items) => Some(f(items)),
            _ => None,
        }
    }

    fn store_items(&mut self, r: ObjectRef, items: Vec<Option<Value>>) -> R<()> {
        self.with_items(r, |slot| *slot = items);
        Ok(())
    }

    fn join(&mut self, r: ObjectRef, separator: &str, h: &mut dyn Host) -> R<Value> {
        let items = self.items(r)?;
        let mut out = String::new();
        for (i, item) in items.iter().enumerate() {
            self.tick()?;
            if i > 0 {
                out.push_str(separator);
            }
            match item {
                None | Some(Value::Undefined | Value::Null) => {}
                Some(value) => {
                    let text = self.string_of(value, h)?;
                    if out.len() + text.len() > MAX_STRING_BYTES {
                        return Err(Abort::Memory);
                    }
                    self.charge(text.len() as u64 / BYTES_PER_STEP)?;
                    out.push_str(&text);
                }
            }
            if out.len() > MAX_STRING_BYTES {
                return Err(Abort::Memory);
            }
        }
        self.string_value(&out)
    }

    /// Whether `a` sorts after `b`. `undefined` and holes go last.
    fn sort_after(
        &mut self,
        a: &Option<Value>,
        b: &Option<Value>,
        f: &Value,
        h: &mut dyn Host,
    ) -> R<bool> {
        self.tick()?;
        let (a, b) = match (a, b) {
            (Some(Value::Undefined) | None, _) => {
                return Ok(!matches!(b, Some(Value::Undefined) | None));
            }
            (_, Some(Value::Undefined) | None) => return Ok(false),
            (Some(a), Some(b)) => (a, b),
        };
        if *f == Value::Undefined {
            let x = self.string_of(a, h)?;
            let y = self.string_of(b, h)?;
            self.charge(x.len().min(y.len()) as u64 / 64)?;
            return Ok(x > y);
        }
        let result = self.call(f, Value::Undefined, &[a.clone(), b.clone()], h)?;
        Ok(self.number(&result, h)? > 0.0)
    }

    /// A bottom-up merge sort. Comparators are script code and can answer
    /// anything, which a fallible hand-written sort survives where the
    /// standard library's sorts may panic.
    fn merge_sort(&mut self, items: &mut Vec<Option<Value>>, f: &Value, h: &mut dyn Host) -> R<()> {
        let n = items.len();
        let mut width = 1;
        while width < n {
            let mut merged = Vec::with_capacity(n);
            let mut start = 0;
            while start < n {
                let middle = (start + width).min(n);
                let end = (start + 2 * width).min(n);
                let (mut i, mut j) = (start, middle);
                while i < middle && j < end {
                    let (a, b) = (&items[i], &items[j]);
                    if self.sort_after(a, b, f, h)? {
                        merged.push(b.clone());
                        j += 1;
                    } else {
                        merged.push(a.clone());
                        i += 1;
                    }
                }
                merged.extend_from_slice(&items[i..middle]);
                merged.extend_from_slice(&items[j..end]);
                start = end;
            }
            self.charge(n as u64 / 8)?;
            *items = merged;
            width *= 2;
        }
        Ok(())
    }

    fn install_array(&mut self) {
        let p = self.protos.array;
        self.constructor("Array", p, |e, h, _, a| {
            if let [Value::Number(n)] = a {
                if *n < 0.0 || n.fract() != 0.0 || *n >= 4_294_967_296.0 || n.is_nan() {
                    return e.error(ErrorKind::Range, "invalid array length");
                }
                let array = new_array(e, Vec::new())?;
                let key = Key::Name(e.names.get("length"));
                e.obj_set(array, &key, Value::Number(*n), h)?;
                return Ok(Value::Object(array));
            }
            Ok(Value::Object(new_array(e, a.to_vec())?))
        });
        self.method(p, "toString", |e, h, t, _| {
            let r = e.this_array(t)?;
            e.join(r, ",", h)
        });
        self.method(p, "toLocaleString", |e, h, t, _| {
            let r = e.this_array(t)?;
            e.join(r, ",", h)
        });
        self.method(p, "join", |e, h, t, a| {
            let r = e.this_array(t)?;
            let separator = match a.first() {
                None | Some(Value::Undefined) => e.names.get(","),
                Some(value) => e.string_of(value, h)?,
            };
            e.join(r, &separator, h)
        });
        self.method(p, "push", |e, _, t, a| {
            let r = e.this_array(t)?;
            let len = e.array_len(r);
            e.array_resize(r, len + a.len())?;
            e.charge(a.len() as u64 / 8)?;
            e.with_items(r, |items| {
                for (slot, value) in items.iter_mut().skip(len).zip(a) {
                    *slot = Some(value.clone());
                }
            });
            Ok(Value::Number(e.array_len(r) as f64))
        });
        self.method(p, "pop", |e, _, t, _| {
            let r = e.this_array(t)?;
            let last = e.with_items(r, Vec::pop).flatten().flatten();
            Ok(last.unwrap_or(Value::Undefined))
        });
        self.method(p, "shift", |e, _, t, _| {
            let r = e.this_array(t)?;
            e.charge(e.array_len(r) as u64 / 16)?;
            let first = e.with_items(r, |items| {
                if items.is_empty() {
                    None
                } else {
                    items.remove(0)
                }
            });
            Ok(first.flatten().unwrap_or(Value::Undefined))
        });
        self.method(p, "unshift", |e, _, t, a| {
            let r = e.this_array(t)?;
            let mut items = e.items(r)?;
            e.array_resize(r, items.len() + a.len())?;
            items.splice(0..0, a.iter().cloned().map(Some));
            let len = items.len();
            e.store_items(r, items)?;
            Ok(Value::Number(len as f64))
        });
        self.method(p, "slice", |e, h, t, a| {
            let r = e.this_array(t)?;
            let items = e.items(r)?;
            let from = relative(num_arg(e, h, a, 0)?, items.len());
            let to = match a.get(1) {
                None | Some(Value::Undefined) => items.len(),
                Some(value) => relative(e.number(value, h)?, items.len()),
            };
            let part = items.get(from..to.max(from)).unwrap_or(&[]).to_vec();
            e.mem(part.len() * SLOT_BYTES)?;
            Ok(Value::Object(e.alloc(e.protos.array, Kind::Array(part))?))
        });
        self.method(p, "splice", |e, h, t, a| {
            let r = e.this_array(t)?;
            let mut items = e.items(r)?;
            let from = relative(num_arg(e, h, a, 0)?, items.len());
            let count = to_integer(num_arg(e, h, a, 1)?).clamp(0, (items.len() - from) as i64);
            let new = a.get(2..).unwrap_or(&[]);
            e.array_resize(r, items.len() + new.len())?;
            let replacement = new.iter().cloned().map(Some);
            let removed: Vec<_> = items
                .splice(from..from + count as usize, replacement)
                .collect();
            e.store_items(r, items)?;
            e.mem(removed.len() * SLOT_BYTES)?;
            Ok(Value::Object(
                e.alloc(e.protos.array, Kind::Array(removed))?,
            ))
        });
        self.method(p, "concat", |e, _, t, a| {
            let r = e.this_array(t)?;
            let mut out = e.items(r)?;
            for value in a {
                match value {
                    Value::Object(other) if matches!(e.obj(*other).kind, Kind::Array(_)) => {
                        out.extend(e.items(*other)?);
                    }
                    other => out.push(Some(other.clone())),
                }
                if out.len() > super::interp::MAX_ARRAY_LEN {
                    return Err(Abort::Memory);
                }
            }
            e.mem(out.len() * SLOT_BYTES)?;
            Ok(Value::Object(e.alloc(e.protos.array, Kind::Array(out))?))
        });
        self.method(p, "reverse", |e, _, t, _| {
            let r = e.this_array(t)?;
            e.charge(e.array_len(r) as u64 / 8)?;
            e.with_items(r, |items| items.reverse());
            Ok(t.clone())
        });
        self.method(p, "sort", |e, h, t, a| {
            let r = e.this_array(t)?;
            let mut items = e.items(r)?;
            let comparator = arg(a, 0);
            if comparator != Value::Undefined && !e.is_callable(&comparator) {
                return e.error(ErrorKind::Type, "sort needs a function");
            }
            e.merge_sort(&mut items, &comparator, h)?;
            e.store_items(r, items)?;
            Ok(t.clone())
        });
    }

    fn this_number(&mut self, this: &Value) -> R<f64> {
        match this {
            Value::Number(n) => Ok(*n),
            Value::Object(r) => match self.obj(*r).kind {
                Kind::Boxed(Value::Number(n)) => Ok(n),
                _ => self.error(ErrorKind::Type, "not a number"),
            },
            _ => self.error(ErrorKind::Type, "not a number"),
        }
    }

    fn install_number(&mut self) {
        let p = self.protos.number;
        let ctor = self.constructor("Number", p, |e, h, _, a| {
            let is_new = e.is_new;
            let n = if a.is_empty() {
                0.0
            } else {
                num_arg(e, h, a, 0)?
            };
            if !is_new {
                return Ok(Value::Number(n));
            }
            let boxed = Kind::Boxed(Value::Number(n));
            Ok(Value::Object(e.alloc(e.protos.number, boxed)?))
        });
        for (name, value) in [
            ("MAX_VALUE", f64::MAX),
            ("MIN_VALUE", 5e-324),
            ("NaN", f64::NAN),
            ("POSITIVE_INFINITY", f64::INFINITY),
            ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ] {
            self.define(ctor, name, Value::Number(value));
        }
        self.method(p, "toString", |e, h, t, a| {
            let n = e.this_number(t)?;
            let radix = match a.first() {
                None | Some(Value::Undefined) => 10.0,
                Some(value) => e.number(value, h)?,
            };
            if !(2.0..=36.0).contains(&radix) {
                return e.error(ErrorKind::Range, "radix out of range");
            }
            let text = if radix == 10.0 {
                number_to_string(n)
            } else {
                to_radix(n, radix as u32)
            };
            e.string_value(&text)
        });
        self.method(p, "toLocaleString", |e, _, t, _| {
            let n = e.this_number(t)?;
            e.string_value(&number_to_string(n))
        });
        self.method(p, "valueOf", |e, _, t, _| {
            Ok(Value::Number(e.this_number(t)?))
        });
        fn digits(e: &mut Engine, h: &mut dyn Host, a: &[Value], min: f64) -> R<usize> {
            let d = to_integer(num_arg(e, h, a, 0)?) as f64;
            if !(min..=20.0).contains(&d) {
                return e.error(ErrorKind::Range, "digits out of range");
            }
            e.charge(32)?;
            Ok(d as usize)
        }
        self.method(p, "toFixed", |e, h, t, a| {
            let n = e.this_number(t)?;
            let d = digits(e, h, a, 0.0)?;
            e.string_value(&to_fixed(n, d))
        });
        self.method(p, "toExponential", |e, h, t, a| {
            let n = e.this_number(t)?;
            let d = digits(e, h, a, 0.0)?;
            e.string_value(&to_exponential(n, d))
        });
        self.method(p, "toPrecision", |e, h, t, a| {
            let n = e.this_number(t)?;
            if matches!(a.first(), None | Some(Value::Undefined)) {
                return e.string_value(&number_to_string(n));
            }
            let d = digits(e, h, a, 1.0)?;
            e.string_value(&to_precision(n, d))
        });

        let p = self.protos.boolean;
        self.constructor("Boolean", p, |e, _, _, a| {
            let b = a.first().is_some_and(super::to_bool);
            if !e.is_new {
                return Ok(Value::Bool(b));
            }
            let boxed = Kind::Boxed(Value::Bool(b));
            Ok(Value::Object(e.alloc(e.protos.boolean, boxed)?))
        });
        fn this_bool(e: &Engine, t: &Value) -> bool {
            match t {
                Value::Bool(b) => *b,
                Value::Object(r) => matches!(e.obj(*r).kind, Kind::Boxed(Value::Bool(true))),
                _ => false,
            }
        }
        self.method(p, "toString", |e, _, t, _| {
            let text = if this_bool(e, t) { "true" } else { "false" };
            Ok(Value::Str(e.names.get(text)))
        });
        self.method(p, "valueOf", |e, _, t, _| Ok(Value::Bool(this_bool(e, t))));
    }

    fn install_errors(&mut self) {
        const NAMES: [&str; 7] = [
            "Error",
            "TypeError",
            "ReferenceError",
            "RangeError",
            "SyntaxError",
            "EvalError",
            "URIError",
        ];
        const CTORS: [NativeFn; 7] = [
            error_ctor::<0>,
            error_ctor::<1>,
            error_ctor::<2>,
            error_ctor::<3>,
            error_ctor::<4>,
            error_ctor::<5>,
            error_ctor::<6>,
        ];
        let object = self.protos.object;
        let base = self.alloc_raw(Obj::new(Some(object), Kind::Plain));
        for (i, (name, ctor)) in NAMES.iter().zip(CTORS).enumerate() {
            let proto = if i == 0 {
                base
            } else {
                self.alloc_raw(Obj::new(Some(base), Kind::Plain))
            };
            self.protos.errors[i] = proto;
            self.constructor(name, proto, ctor);
            self.define(proto, "name", Value::Str(Arc::from(*name)));
            self.define(proto, "message", Value::Str(Arc::from("")));
        }
        self.method(base, "toString", |e, h, t, _| {
            let name = Key::Name(e.names.get("name"));
            let message = Key::Name(e.names.get("message"));
            let name = e.get(t, &name, h)?;
            let name = e.string_of(&name, h)?;
            let message = e.get(t, &message, h)?;
            let message = e.string_of(&message, h)?;
            if message.is_empty() {
                return Ok(Value::Str(name));
            }
            e.string_value(&format!("{name}: {message}"))
        });
    }

    fn install_math(&mut self) {
        let proto = self.protos.object;
        let math = self.alloc_raw(Obj::new(Some(proto), Kind::Plain));
        self.define(self.global, "Math", Value::Object(math));
        for (name, value) in [
            ("E", std::f64::consts::E),
            ("LN2", std::f64::consts::LN_2),
            ("LN10", std::f64::consts::LN_10),
            ("LOG2E", std::f64::consts::LOG2_E),
            ("LOG10E", std::f64::consts::LOG10_E),
            ("PI", std::f64::consts::PI),
            ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
            ("SQRT2", std::f64::consts::SQRT_2),
        ] {
            self.define(math, name, Value::Number(value));
        }
        macro_rules! math1 {
            ($($name:literal => $f:expr),* $(,)?) => {$(
                self.method(math, $name, |e, h, _, a| {
                    let f: fn(f64) -> f64 = $f;
                    Ok(Value::Number(f(num_arg(e, h, a, 0)?)))
                });
            )*};
        }
        math1! {
            "abs" => f64::abs, "acos" => f64::acos, "asin" => f64::asin, "atan" => f64::atan,
            "ceil" => f64::ceil, "cos" => f64::cos, "exp" => f64::exp, "floor" => f64::floor,
            "log" => f64::ln, "sin" => f64::sin, "sqrt" => f64::sqrt, "tan" => f64::tan,
            "round" => |x| (x + 0.5).floor(),
        }
        self.method(math, "atan2", |e, h, _, a| {
            let y = num_arg(e, h, a, 0)?;
            Ok(Value::Number(y.atan2(num_arg(e, h, a, 1)?)))
        });
        self.method(math, "pow", |e, h, _, a| {
            let x = num_arg(e, h, a, 0)?;
            let y = num_arg(e, h, a, 1)?;
            // powf(1, NaN) and powf(-1, inf) are 1 in C but NaN in ECMAScript.
            let undefined = y.is_nan() || (x.abs() == 1.0 && y.is_infinite());
            Ok(Value::Number(if undefined { f64::NAN } else { x.powf(y) }))
        });
        fn extreme(
            e: &mut Engine,
            h: &mut dyn Host,
            a: &[Value],
            start: f64,
            max: bool,
        ) -> R<Value> {
            let mut best = start;
            for value in a {
                let n = e.number(value, h)?;
                if n.is_nan() {
                    return Ok(Value::Number(n));
                }
                if (max && n > best) || (!max && n < best) {
                    best = n;
                }
            }
            Ok(Value::Number(best))
        }
        self.method(math, "max", |e, h, _, a| {
            extreme(e, h, a, f64::NEG_INFINITY, true)
        });
        self.method(math, "min", |e, h, _, a| {
            extreme(e, h, a, f64::INFINITY, false)
        });
        self.method(math, "random", |e, _, _, _| {
            // Xorshift64: skins want motion, not cryptography.
            let mut x = e.rng;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            e.rng = x;
            Ok(Value::Number((x >> 11) as f64 / (1u64 << 53) as f64))
        });
    }

    fn install_date(&mut self) {
        let p = self.protos.date;
        let ctor = self.constructor("Date", p, |e, h, _, a| {
            if !e.is_new {
                return e.string_value(&date_string(now_ms()));
            }
            let time = match a {
                [] => now_ms(),
                [Value::Str(s)] => parse_date(s),
                [one] => {
                    let primitive = e.primitive(one, super::interp::Hint::Default, h)?;
                    match &primitive {
                        Value::Str(s) => parse_date(s),
                        other => time_clip(e.number(other, h)?),
                    }
                }
                _ => {
                    let mut f = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                    for (slot, value) in f.iter_mut().zip(a) {
                        *slot = e.number(value, h)?;
                    }
                    if (0.0..=99.0).contains(&f[0]) {
                        f[0] += 1900.0;
                    }
                    time_clip(make_time(f))
                }
            };
            Ok(Value::Object(e.alloc(e.protos.date, Kind::Date(time))?))
        });
        self.method(ctor, "parse", |e, h, _, a| {
            let text = e.string_of(&arg(a, 0), h)?;
            Ok(Value::Number(parse_date(&text)))
        });
        fn time_of(e: &mut Engine, t: &Value) -> R<f64> {
            if let Value::Object(r) = t
                && let Kind::Date(time) = e.obj(*r).kind
            {
                return Ok(time);
            }
            e.error(ErrorKind::Type, "not a date")
        }
        for name in ["toString", "toLocaleString", "toUTCString", "toGMTString"] {
            self.method(p, name, |e, _, t, _| {
                let time = time_of(e, t)?;
                e.string_value(&date_string(time))
            });
        }
        for name in ["getTime", "valueOf"] {
            self.method(p, name, |e, _, t, _| Ok(Value::Number(time_of(e, t)?)));
        }
        self.method(p, "getTimezoneOffset", |_, _, _, _| Ok(Value::Number(0.0)));
        self.method(p, "getYear", |e, _, t, _| {
            let time = time_of(e, t)?;
            Ok(Value::Number(
                fields(time).map_or(f64::NAN, |f| f[0] - 1900.0),
            ))
        });
        macro_rules! date_fields {
            ($($field:literal: $get:literal $get_utc:literal $set:literal $set_utc:literal),* $(,)?) => {$(
                for name in [$get, $get_utc] {
                    self.method(p, name, |e, _, t, _| {
                        let time = time_of(e, t)?;
                        Ok(Value::Number(fields(time).map_or(f64::NAN, |f| f[$field])))
                    });
                }
                for name in [$set, $set_utc] {
                    self.method(p, name, |e, h, t, a| {
                        let time = time_of(e, t)?;
                        let mut f = fields(time).unwrap_or([1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
                        f[$field] = num_arg(e, h, a, 0)?;
                        let time = time_clip(make_time(f));
                        if let Value::Object(r) = t {
                            e.obj_mut(*r).kind = Kind::Date(time);
                        }
                        Ok(Value::Number(time))
                    });
                }
            )*};
        }
        date_fields! {
            0: "getFullYear" "getUTCFullYear" "setFullYear" "setUTCFullYear",
            1: "getMonth" "getUTCMonth" "setMonth" "setUTCMonth",
            2: "getDate" "getUTCDate" "setDate" "setUTCDate",
            3: "getHours" "getUTCHours" "setHours" "setUTCHours",
            4: "getMinutes" "getUTCMinutes" "setMinutes" "setUTCMinutes",
            5: "getSeconds" "getUTCSeconds" "setSeconds" "setUTCSeconds",
            6: "getMilliseconds" "getUTCMilliseconds" "setMilliseconds" "setUTCMilliseconds",
        }
        for name in ["getDay", "getUTCDay"] {
            self.method(p, name, |e, _, t, _| {
                let time = time_of(e, t)?;
                Ok(Value::Number(if time.is_nan() {
                    f64::NAN
                } else {
                    weekday(time) as f64
                }))
            });
        }
        self.method(p, "setTime", |e, h, t, a| {
            time_of(e, t)?;
            let time = time_clip(num_arg(e, h, a, 0)?);
            if let Value::Object(r) = t {
                e.obj_mut(*r).kind = Kind::Date(time);
            }
            Ok(Value::Number(time))
        });
    }

    fn install_globals(&mut self) {
        let g = self.global;
        self.define(g, "undefined", Value::Undefined);
        self.define(g, "NaN", Value::Number(f64::NAN));
        self.define(g, "Infinity", Value::Number(f64::INFINITY));
        self.method(g, "ActiveXObject", |e, _, _, _| {
            e.error(ErrorKind::Error, "Automation server can't create object")
        });
        self.method(g, "eval", |e, h, _, a| match a.first() {
            Some(Value::Str(source)) => e.eval_source(&source.clone(), h),
            Some(other) => Ok(other.clone()),
            None => Ok(Value::Undefined),
        });
        self.method(g, "parseInt", |e, h, _, a| {
            let text = e.string_of(&arg(a, 0), h)?;
            e.charge(text.len() as u64 / 16)?;
            let radix = if a.len() > 1 {
                num_arg(e, h, a, 1)?
            } else {
                0.0
            };
            Ok(Value::Number(parse_int(&text, radix)))
        });
        self.method(g, "parseFloat", |e, h, _, a| {
            let text = e.string_of(&arg(a, 0), h)?;
            e.charge(text.len() as u64 / 16)?;
            Ok(Value::Number(parse_float(&text)))
        });
        self.method(g, "isNaN", |e, h, _, a| {
            Ok(Value::Bool(num_arg(e, h, a, 0)?.is_nan()))
        });
        self.method(g, "isFinite", |e, h, _, a| {
            Ok(Value::Bool(num_arg(e, h, a, 0)?.is_finite()))
        });
        self.method(g, "escape", |e, h, _, a| {
            let text = e.string_of(&arg(a, 0), h)?;
            e.charge(text.len() as u64 / 4)?;
            e.mem(text.len() * 6)?;
            let mut out = String::new();
            for unit in text.encode_utf16() {
                let plain = u8::try_from(unit)
                    .ok()
                    .filter(|b| b.is_ascii_alphanumeric() || b"@*_+-./".contains(b));
                match plain {
                    Some(b) => out.push(char::from(b)),
                    None if unit < 256 => out.push_str(&format!("%{unit:02X}")),
                    None => out.push_str(&format!("%u{unit:04X}")),
                }
            }
            e.string_value(&out)
        });
        self.method(g, "unescape", |e, h, _, a| {
            let text = e.string_of(&arg(a, 0), h)?;
            e.charge(text.len() as u64 / 4)?;
            let input: Vec<u16> = text.encode_utf16().collect();
            let hex = |from: usize, count: usize| -> Option<u16> {
                let mut value = 0u32;
                for unit in input.get(from..from + count)? {
                    value = value * 16 + char::from_u32(u32::from(*unit))?.to_digit(16)?;
                }
                Some(value as u16)
            };
            let mut out = Vec::with_capacity(input.len());
            let mut i = 0;
            while let Some(unit) = input.get(i).copied() {
                let percent = unit == u16::from(b'%');
                let wide = percent && input.get(i + 1) == Some(&u16::from(b'u'));
                let long = if wide { hex(i + 2, 4) } else { None };
                let short = if percent { hex(i + 1, 2) } else { None };
                if let Some(code) = long {
                    out.push(code);
                    i += 6;
                } else if let Some(code) = short {
                    out.push(code);
                    i += 3;
                } else {
                    out.push(unit);
                    i += 1;
                }
            }
            e.string_value(&String::from_utf16_lossy(&out))
        });
        const URI_MARKS: &[u8] = b"-_.!~*'()";
        const URI_RESERVED: &[u8] = b";/?:@&=+$,#";
        fn encode(e: &mut Engine, h: &mut dyn Host, a: &[Value], keep_reserved: bool) -> R<Value> {
            let text = e.string_of(&arg(a, 0), h)?;
            e.charge(text.len() as u64 / 4)?;
            e.mem(text.len() * 3)?;
            let mut out = String::new();
            for b in text.bytes() {
                let keep = b.is_ascii_alphanumeric()
                    || URI_MARKS.contains(&b)
                    || (keep_reserved && URI_RESERVED.contains(&b));
                if keep {
                    out.push(char::from(b));
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            e.string_value(&out)
        }
        fn decode(e: &mut Engine, h: &mut dyn Host, a: &[Value], keep_reserved: bool) -> R<Value> {
            let text = e.string_of(&arg(a, 0), h)?;
            e.charge(text.len() as u64 / 4)?;
            let bytes = text.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while let Some(b) = bytes.get(i).copied() {
                if b != b'%' {
                    out.push(b);
                    i += 1;
                    continue;
                }
                let high = bytes.get(i + 1).and_then(|c| char::from(*c).to_digit(16));
                let low = bytes.get(i + 2).and_then(|c| char::from(*c).to_digit(16));
                let (Some(high), Some(low)) = (high, low) else {
                    return e.error(ErrorKind::Uri, "malformed URI sequence");
                };
                let decoded = (high * 16 + low) as u8;
                if keep_reserved && URI_RESERVED.contains(&decoded) {
                    out.extend_from_slice(bytes.get(i..i + 3).unwrap_or(&[]));
                } else {
                    out.push(decoded);
                }
                i += 3;
            }
            match String::from_utf8(out) {
                Ok(text) => e.string_value(&text),
                Err(_) => e.error(ErrorKind::Uri, "malformed URI sequence"),
            }
        }
        self.method(g, "encodeURIComponent", |e, h, _, a| encode(e, h, a, false));
        self.method(g, "encodeURI", |e, h, _, a| encode(e, h, a, true));
        self.method(g, "decodeURIComponent", |e, h, _, a| decode(e, h, a, false));
        self.method(g, "decodeURI", |e, h, _, a| decode(e, h, a, true));
    }
}

const MS_PER_DAY: f64 = 86_400_000.0;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn time_clip(time: f64) -> f64 {
    if time.abs() > 8.64e15 {
        return f64::NAN;
    }
    time.trunc() + 0.0
}

/// Days since the epoch of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month < 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let shifted = (month + 10) % 12;
    let doy = (153 * shifted + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = (mp + 2) % 12;
    let year = yoe + era * 400 + i64::from(month < 2);
    (year, month, day)
}

/// Year, month, day, hours, minutes, seconds, milliseconds. Local time is
/// treated as UTC: a sandbox has no business reading the time zone.
fn fields(time: f64) -> Option<[f64; 7]> {
    if !time.is_finite() {
        return None;
    }
    let days = (time / MS_PER_DAY).floor();
    let ms = time - days * MS_PER_DAY;
    let (year, month, day) = civil_from_days(days as i64);
    Some([
        year as f64,
        month as f64,
        day as f64,
        (ms / 3_600_000.0).floor(),
        (ms / 60_000.0).floor() % 60.0,
        (ms / 1000.0).floor() % 60.0,
        ms % 1000.0,
    ])
}

/// The inverse of [`fields`]; out-of-range fields carry, as in `setDate(40)`.
fn make_time(f: [f64; 7]) -> f64 {
    if f.iter().any(|x| !x.is_finite() || x.abs() > 1e12) {
        return f64::NAN;
    }
    let [year, month, day, hours, minutes, seconds, ms] = f.map(f64::trunc);
    let year = year + (month / 12.0).floor();
    if year.abs() > 400_000.0 {
        return f64::NAN;
    }
    let month = month.rem_euclid(12.0) as i64;
    let days = days_from_civil(year as i64, month, 1) as f64 + day - 1.0;
    days * MS_PER_DAY + hours * 3_600_000.0 + minutes * 60_000.0 + seconds * 1000.0 + ms
}

fn weekday(time: f64) -> usize {
    let days = (time / MS_PER_DAY).floor();
    (days + 4.0).rem_euclid(7.0) as usize
}

/// JScript's format: `Mon Jan 2 03:04:05 UTC 2006`.
fn date_string(time: f64) -> String {
    let Some(f) = fields(time) else {
        return "NaN".to_string();
    };
    format!(
        "{} {} {} {:02}:{:02}:{:02} UTC {}",
        WEEKDAYS.get(weekday(time)).unwrap_or(&""),
        MONTHS.get(f[1] as usize).unwrap_or(&""),
        f[2],
        f[3],
        f[4],
        f[5],
        f[0]
    )
}

/// `Date.parse` for the shapes scripts write: `2006/1/2`, `1/2/2006`, or a
/// month name, each with an optional `hh:mm:ss`.
fn parse_date(text: &str) -> f64 {
    let (mut numbers, mut time, mut month) = (Vec::new(), None, None);
    let cleaned = text.replace(',', " ");
    for word in cleaned.split_whitespace() {
        if word.contains(':') {
            let mut parts = word.split(':').map(str::parse::<f64>);
            let mut part = || parts.next().and_then(Result::ok).unwrap_or(0.0);
            time = Some(part() * 3_600_000.0 + part() * 60_000.0 + part() * 1000.0);
        } else if word.contains('/') || word.contains('-') {
            numbers.extend(word.split(['/', '-']).filter_map(|p| p.parse::<f64>().ok()));
        } else if let Ok(n) = word.parse::<f64>() {
            numbers.push(n);
        } else if let Some(prefix) = word.get(..3) {
            let name = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(prefix));
            month = month.or(name);
        }
    }
    let (year, month, day) = match (month, numbers.as_slice()) {
        (Some(m), [a, b, ..]) if *a > 31.0 => (*a, m as f64, *b),
        (Some(m), [a, b, ..]) => (*b, m as f64, *a),
        (None, [a, b, c, ..]) if *a > 31.0 => (*a, b - 1.0, *c),
        (None, [a, b, c, ..]) => (*c, a - 1.0, *b),
        _ => return f64::NAN,
    };
    let date = make_time([year, month, day, 0.0, 0.0, 0.0, 0.0]);
    time_clip(date + time.unwrap_or(0.0))
}
