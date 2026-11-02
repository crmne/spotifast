//! Regular expressions, compiled to a small program for a backtracking
//! matcher over UTF-16 code units. The backtrack stack is a heap vector, so
//! input length cannot reach the native stack, and every instruction costs a
//! step from the caller's budget, so `(a+)+$` simply runs out.

use std::sync::Arc;

/// Group nesting; lookaheads recurse in the matcher, once per level.
const MAX_NESTING: u32 = 32;
/// Program size, which also bounds what `{n,m}` copies can expand to.
const MAX_PROGRAM: usize = 1 << 16;

#[derive(Debug)]
struct Class {
    negate: bool,
    ranges: Vec<(u16, u16)>,
}

impl Class {
    fn has(&self, c: u16) -> bool {
        self.ranges.iter().any(|(a, b)| *a <= c && c <= *b)
    }
}

#[derive(Debug)]
enum Node {
    Empty,
    Char(u16),
    Any,
    Class(Arc<Class>),
    Start,
    End,
    WordBoundary(bool),
    Group(Box<Node>, u32),
    Concat(Vec<Node>),
    Alternate(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
    BackRef(u32),
    Look(Box<Node>, bool),
}

#[derive(Debug)]
enum Inst {
    Char(u16),
    Any,
    Class(Arc<Class>),
    Start,
    End,
    WordBoundary(bool),
    Save(u32),
    /// Try the first target, then the second on backtracking.
    Split(u32, u32),
    Jump(u32),
    /// Remember the position, then fail a loop body that did not advance it.
    Mark(u32),
    Advanced(u32),
    BackRef(u32),
    /// A lookahead over the following instructions, up to `Match`, resuming
    /// at the target.
    Look(bool, u32),
    Match,
}

#[derive(Debug)]
pub(super) struct Program {
    insts: Vec<Inst>,
    /// Capture slots, then loop marks.
    slots: usize,
    pub groups: usize,
    icase: bool,
    multiline: bool,
}

const DIGIT: &[(u16, u16)] = &[(48, 57)];
const WORD: &[(u16, u16)] = &[(48, 57), (65, 90), (95, 95), (97, 122)];
const SPACE: &[(u16, u16)] = &[
    (9, 13),
    (32, 32),
    (160, 160),
    (0x1680, 0x1680),
    (0x2000, 0x200a),
    (0x2028, 0x2029),
    (0x202f, 0x202f),
    (0x205f, 0x205f),
    (0x3000, 0x3000),
    (0xfeff, 0xfeff),
];

fn complement(ranges: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    let mut next = 0u32;
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable();
    for (a, b) in sorted {
        if u32::from(a) > next {
            out.push((next as u16, a - 1));
        }
        next = next.max(u32::from(b) + 1);
    }
    if next <= 0xffff {
        out.push((next as u16, 0xffff));
    }
    out
}

fn is_word(c: u16) -> bool {
    WORD.iter().any(|(a, b)| *a <= c && c <= *b)
}

fn is_line_end(c: u16) -> bool {
    matches!(c, 10 | 13 | 0x2028 | 0x2029)
}

fn map_case(c: u16, upper: bool) -> u16 {
    if c < 128 {
        let b = c as u8;
        return u16::from(if upper {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        });
    }
    let Some(ch) = char::from_u32(u32::from(c)) else {
        return c;
    };
    let mapped: Vec<char> = if upper {
        ch.to_uppercase().collect()
    } else {
        ch.to_lowercase().collect()
    };
    match mapped.as_slice() {
        [one] if (*one as u32) < 0x10000 => *one as u32 as u16,
        _ => c,
    }
}

struct Parser {
    src: Vec<u16>,
    pos: usize,
    groups: u32,
    total_groups: u32,
    depth: u32,
}

type P<T> = Result<T, String>;

fn ch(c: char) -> u16 {
    c as u32 as u16
}

impl Parser {
    fn peek(&self) -> Option<u16> {
        self.src.get(self.pos).copied()
    }

    fn at(&self, c: char) -> bool {
        self.peek() == Some(ch(c))
    }

    fn eat(&mut self, c: char) -> bool {
        let found = self.at(c);
        if found {
            self.pos += 1;
        }
        found
    }

    fn alternation(&mut self) -> P<Node> {
        self.depth += 1;
        if self.depth > MAX_NESTING || super::stack::exceeded() {
            return Err("regular expression nested too deeply".into());
        }
        let mut arms = vec![self.sequence()?];
        while self.eat('|') {
            arms.push(self.sequence()?);
        }
        self.depth -= 1;
        Ok(if arms.len() == 1 {
            arms.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alternate(arms)
        })
    }

    fn sequence(&mut self) -> P<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == ch('|') || c == ch(')') {
                break;
            }
            let atom = self.atom()?;
            items.push(self.quantifier(atom)?);
        }
        Ok(Node::Concat(items))
    }

    fn number(&mut self) -> Option<u32> {
        let from = self.pos;
        let mut value = 0u32;
        while let Some(d) = self.peek().filter(|c| (48..58).contains(c)) {
            value = value.saturating_mul(10).saturating_add(u32::from(d - 48));
            self.pos += 1;
        }
        (self.pos > from).then_some(value)
    }

    /// `{n}`, `{n,}` or `{n,m}`. Anything else is a literal brace.
    fn braces(&mut self) -> Option<(u32, Option<u32>)> {
        let mark = self.pos;
        self.pos += 1;
        let parsed = (|| {
            let min = self.number()?;
            let max = if self.eat(',') {
                if self.at('}') {
                    None
                } else {
                    Some(self.number()?)
                }
            } else {
                Some(min)
            };
            self.eat('}').then_some((min, max))
        })();
        if parsed.is_none() {
            self.pos = mark;
        }
        parsed
    }

    fn quantifier(&mut self, atom: Node) -> P<Node> {
        let (min, max) = if self.eat('*') {
            (0, None)
        } else if self.eat('+') {
            (1, None)
        } else if self.eat('?') {
            (0, Some(1))
        } else if self.at('{') {
            match self.braces() {
                Some(bounds) => bounds,
                None => return Ok(atom),
            }
        } else {
            return Ok(atom);
        };
        if max.is_some_and(|m| m < min) {
            return Err("numbers out of order in quantifier".into());
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy: !self.eat('?'),
        })
    }

    fn atom(&mut self) -> P<Node> {
        let Some(c) = self.peek() else {
            return Ok(Node::Empty);
        };
        self.pos += 1;
        match char::from_u32(u32::from(c)).unwrap_or('\u{fffd}') {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '(' => self.group(),
            '[' => self.class(),
            '\\' => self.escape(),
            '*' | '+' | '?' => Err("nothing to repeat".into()),
            _ => Ok(Node::Char(c)),
        }
    }

    fn group(&mut self) -> P<Node> {
        let look = if self.eat('?') {
            if self.eat(':') {
                Some(None)
            } else if self.eat('=') {
                Some(Some(false))
            } else if self.eat('!') {
                Some(Some(true))
            } else {
                return Err("invalid group".into());
            }
        } else {
            None
        };
        let index = if look.is_none() {
            self.groups += 1;
            self.groups
        } else {
            0
        };
        let inner = self.alternation()?;
        if !self.eat(')') {
            return Err("missing )".into());
        }
        Ok(match look {
            None => Node::Group(Box::new(inner), index),
            Some(None) => inner,
            Some(Some(negate)) => Node::Look(Box::new(inner), negate),
        })
    }

    fn hex(&mut self, count: usize) -> Option<u16> {
        let digits = self.src.get(self.pos..self.pos + count)?;
        let mut value = 0u32;
        for d in digits {
            value = value * 16 + char::from_u32(u32::from(*d))?.to_digit(16)?;
        }
        self.pos += count;
        Some(value as u16)
    }

    /// One escaped code unit, after any class escape was ruled out.
    fn char_escape(&mut self, c: u16) -> u16 {
        match char::from_u32(u32::from(c)).unwrap_or('\u{fffd}') {
            'n' => 10,
            'r' => 13,
            't' => 9,
            'v' => 11,
            'f' => 12,
            '0' => 0,
            'x' => self.hex(2).unwrap_or(c),
            'u' => self.hex(4).unwrap_or(c),
            'c' => match self.peek() {
                Some(l) if l < 128 && (l as u8).is_ascii_alphabetic() => {
                    self.pos += 1;
                    l % 32
                }
                _ => c,
            },
            _ => c,
        }
    }

    fn class_escape(c: u16) -> Option<Vec<(u16, u16)>> {
        Some(match char::from_u32(u32::from(c))? {
            'd' => DIGIT.to_vec(),
            'D' => complement(DIGIT),
            'w' => WORD.to_vec(),
            'W' => complement(WORD),
            's' => SPACE.to_vec(),
            'S' => complement(SPACE),
            _ => return None,
        })
    }

    fn escape(&mut self) -> P<Node> {
        let Some(c) = self.peek() else {
            return Err("\\ at end of pattern".into());
        };
        self.pos += 1;
        if c == ch('b') {
            return Ok(Node::WordBoundary(true));
        }
        if c == ch('B') {
            return Ok(Node::WordBoundary(false));
        }
        if let Some(ranges) = Self::class_escape(c) {
            let class = Class {
                negate: false,
                ranges,
            };
            return Ok(Node::Class(Arc::new(class)));
        }
        if (49..58).contains(&c) {
            self.pos -= 1;
            let mark = self.pos;
            let n = self.number().unwrap_or(0);
            if n <= self.total_groups {
                return Ok(Node::BackRef(n));
            }
            self.pos = mark + 1;
            return Ok(Node::Char(c));
        }
        Ok(Node::Char(self.char_escape(c)))
    }

    fn class_atom(&mut self) -> P<Result<u16, Vec<(u16, u16)>>> {
        let Some(c) = self.peek() else {
            return Err("missing ]".into());
        };
        self.pos += 1;
        if c != ch('\\') {
            return Ok(Ok(c));
        }
        let Some(e) = self.peek() else {
            return Err("missing ]".into());
        };
        self.pos += 1;
        if e == ch('b') {
            return Ok(Ok(8));
        }
        Ok(match Self::class_escape(e) {
            Some(ranges) => Err(ranges),
            None => Ok(self.char_escape(e)),
        })
    }

    fn class(&mut self) -> P<Node> {
        let negate = self.eat('^');
        let mut ranges = Vec::new();
        loop {
            if self.eat(']') {
                break;
            }
            let low = match self.class_atom()? {
                Ok(c) => c,
                Err(set) => {
                    ranges.extend(set);
                    continue;
                }
            };
            let dash = self.at('-') && self.src.get(self.pos + 1).is_some_and(|c| *c != ch(']'));
            if !dash {
                ranges.push((low, low));
                continue;
            }
            self.pos += 1;
            match self.class_atom()? {
                Ok(high) if low <= high => ranges.push((low, high)),
                Ok(_) => return Err("range out of order in class".into()),
                Err(set) => {
                    ranges.push((low, low));
                    ranges.push((ch('-'), ch('-')));
                    ranges.extend(set);
                }
            }
        }
        Ok(Node::Class(Arc::new(Class { negate, ranges })))
    }
}

/// Capturing groups in the source, needed before parsing to tell a
/// backreference from a literal digit escape.
fn count_groups(src: &[u16]) -> u32 {
    let mut count = 0;
    let mut i = 0;
    let mut in_class = false;
    while let Some(c) = src.get(i).copied() {
        if c == ch('\\') {
            i += 1;
        } else if c == ch('[') {
            in_class = true;
        } else if c == ch(']') {
            in_class = false;
        } else if c == ch('(') && !in_class && src.get(i + 1) != Some(&ch('?')) {
            count += 1;
        }
        i += 1;
    }
    count
}

struct Emitter {
    insts: Vec<Inst>,
    marks: u32,
    icase: bool,
    /// Nodes walked: `(?:){1000000000}` emits nothing but still has to stop.
    walked: usize,
}

impl Emitter {
    fn push(&mut self, inst: Inst) -> P<usize> {
        if self.insts.len() >= MAX_PROGRAM {
            return Err("regular expression too big".into());
        }
        self.insts.push(inst);
        Ok(self.insts.len() - 1)
    }

    fn here(&self) -> u32 {
        self.insts.len() as u32
    }

    fn patch(&mut self, at: usize, inst: Inst) {
        if let Some(slot) = self.insts.get_mut(at) {
            *slot = inst;
        }
    }

    fn split(a: u32, b: u32, greedy: bool) -> Inst {
        if greedy {
            Inst::Split(a, b)
        } else {
            Inst::Split(b, a)
        }
    }

    fn emit(&mut self, node: &Node) -> P<()> {
        self.walked += 1;
        if self.walked > MAX_PROGRAM || super::stack::exceeded() {
            return Err("regular expression too big".into());
        }
        match node {
            Node::Empty => {}
            Node::Char(c) => {
                let c = if self.icase { map_case(*c, true) } else { *c };
                self.push(Inst::Char(c))?;
            }
            Node::Any => {
                self.push(Inst::Any)?;
            }
            Node::Class(class) => {
                self.push(Inst::Class(class.clone()))?;
            }
            Node::Start => {
                self.push(Inst::Start)?;
            }
            Node::End => {
                self.push(Inst::End)?;
            }
            Node::WordBoundary(expected) => {
                self.push(Inst::WordBoundary(*expected))?;
            }
            Node::Group(inner, index) => {
                self.push(Inst::Save(index * 2))?;
                self.emit(inner)?;
                self.push(Inst::Save(index * 2 + 1))?;
            }
            Node::Concat(items) => {
                for item in items {
                    self.emit(item)?;
                }
            }
            Node::Alternate(arms) => {
                let mut exits = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    if i + 1 == arms.len() {
                        self.emit(arm)?;
                        break;
                    }
                    let split = self.push(Inst::Match)?;
                    self.emit(arm)?;
                    exits.push(self.push(Inst::Match)?);
                    let next = self.here();
                    self.patch(split, Inst::Split(split as u32 + 1, next));
                }
                let end = self.here();
                for exit in exits {
                    self.patch(exit, Inst::Jump(end));
                }
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => {
                for _ in 0..*min {
                    self.emit(node)?;
                }
                match max {
                    None => self.emit_star(node, *greedy)?,
                    Some(max) => {
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            splits.push(self.push(Inst::Match)?);
                            self.emit(node)?;
                        }
                        let end = self.here();
                        for split in splits {
                            let inst = Self::split(split as u32 + 1, end, *greedy);
                            self.patch(split, inst);
                        }
                    }
                }
            }
            Node::BackRef(n) => {
                self.push(Inst::BackRef(*n))?;
            }
            Node::Look(inner, negate) => {
                let look = self.push(Inst::Match)?;
                self.emit(inner)?;
                self.push(Inst::Match)?;
                let end = self.here();
                self.patch(look, Inst::Look(*negate, end));
            }
        }
        Ok(())
    }

    fn emit_star(&mut self, node: &Node, greedy: bool) -> P<()> {
        let mark = self.marks;
        self.marks += 1;
        let start = self.push(Inst::Match)?;
        self.push(Inst::Mark(mark))?;
        self.emit(node)?;
        self.push(Inst::Advanced(mark))?;
        self.push(Inst::Jump(start as u32))?;
        let end = self.here();
        self.patch(start, Self::split(start as u32 + 1, end, greedy));
        Ok(())
    }
}

pub(super) fn compile(source: &str, flags: &str) -> Result<Program, String> {
    let (mut icase, mut multiline) = (false, false);
    for flag in flags.chars() {
        match flag {
            'i' => icase = true,
            'm' => multiline = true,
            'g' => {}
            _ => return Err("invalid regular expression flag".into()),
        }
    }
    let src: Vec<u16> = source.encode_utf16().collect();
    let total_groups = count_groups(&src);
    let mut parser = Parser {
        src,
        pos: 0,
        groups: 0,
        total_groups,
        depth: 0,
    };
    let root = parser.alternation()?;
    if parser.pos < parser.src.len() {
        return Err("unmatched )".into());
    }
    let mut emitter = Emitter {
        insts: Vec::new(),
        marks: 0,
        icase,
        walked: 0,
    };
    emitter.emit(&root)?;
    emitter.push(Inst::Match)?;
    let groups = total_groups as usize + 1;
    Ok(Program {
        insts: emitter.insts,
        slots: groups * 2 + emitter.marks as usize,
        groups,
        icase,
        multiline,
    })
}

/// The step budget ran out.
pub(super) struct OutOfSteps;

enum Back {
    Branch(u32, u32),
    Restore(u32, i32),
}

struct Matcher<'a> {
    program: &'a Program,
    input: &'a [u16],
    slots: Vec<i32>,
    steps: &'a mut u64,
    groups: usize,
}

impl Matcher<'_> {
    fn word_at(&self, i: usize) -> bool {
        self.input.get(i).is_some_and(|c| is_word(*c))
    }

    fn class_has(&self, class: &Class, c: u16) -> bool {
        let found = class.has(c)
            || (self.program.icase
                && (class.has(map_case(c, true)) || class.has(map_case(c, false))));
        found != class.negate
    }

    fn set_slot(&mut self, slot: usize, value: i32, stack: &mut Vec<Back>) {
        if let Some(cell) = self.slots.get_mut(slot) {
            stack.push(Back::Restore(slot as u32, *cell));
            *cell = value;
        }
    }

    /// Runs from `pc` at `pos` to `Match`; returns the end position.
    fn run(&mut self, pc: usize, pos: usize) -> Result<Option<usize>, OutOfSteps> {
        if super::stack::exceeded() {
            return Err(OutOfSteps);
        }
        let mut stack = Vec::new();
        let (mut pc, mut pos) = (pc, pos);
        loop {
            if *self.steps == 0 {
                return Err(OutOfSteps);
            }
            *self.steps -= 1;
            let matched = match self.program.insts.get(pc) {
                None | Some(Inst::Match) => return Ok(Some(pos)),
                Some(Inst::Char(c)) => match self.input.get(pos) {
                    Some(x) if x == c || (self.program.icase && map_case(*x, true) == *c) => {
                        pos += 1;
                        true
                    }
                    _ => false,
                },
                Some(Inst::Any) => match self.input.get(pos) {
                    Some(x) if !is_line_end(*x) => {
                        pos += 1;
                        true
                    }
                    _ => false,
                },
                Some(Inst::Class(class)) => {
                    let cost = class.ranges.len() as u64 / 16;
                    *self.steps = self.steps.saturating_sub(cost);
                    match self.input.get(pos) {
                        Some(x) if self.class_has(class, *x) => {
                            pos += 1;
                            true
                        }
                        _ => false,
                    }
                }
                Some(Inst::Start) => {
                    pos == 0
                        || (self.program.multiline
                            && self.input.get(pos - 1).is_some_and(|c| is_line_end(*c)))
                }
                Some(Inst::End) => {
                    pos == self.input.len()
                        || (self.program.multiline
                            && self.input.get(pos).is_some_and(|c| is_line_end(*c)))
                }
                Some(Inst::WordBoundary(expected)) => {
                    let before = pos > 0 && self.word_at(pos - 1);
                    (before != self.word_at(pos)) == *expected
                }
                Some(Inst::Save(slot)) => {
                    self.set_slot(*slot as usize, pos as i32, &mut stack);
                    true
                }
                Some(Inst::Mark(mark)) => {
                    let slot = self.groups * 2 + *mark as usize;
                    self.set_slot(slot, pos as i32, &mut stack);
                    true
                }
                Some(Inst::Advanced(mark)) => {
                    let slot = self.groups * 2 + *mark as usize;
                    self.slots.get(slot).copied() != Some(pos as i32)
                }
                Some(Inst::Split(a, b)) => {
                    stack.push(Back::Branch(*b, pos as u32));
                    pc = *a as usize;
                    continue;
                }
                Some(Inst::Jump(target)) => {
                    pc = *target as usize;
                    continue;
                }
                Some(Inst::BackRef(n)) => self.back_reference(*n as usize, &mut pos)?,
                Some(Inst::Look(negate, end)) => {
                    let (negate, end) = (*negate, *end as usize);
                    let saved = self.slots.clone();
                    *self.steps = self.steps.saturating_sub(saved.len() as u64 / 8);
                    let found = self.run(pc + 1, pos)?.is_some();
                    if found && !negate {
                        // Captures made inside stay, but backtracking past
                        // here must undo them.
                        for (slot, old) in saved.iter().enumerate() {
                            if self.slots.get(slot) != Some(old) {
                                stack.push(Back::Restore(slot as u32, *old));
                            }
                        }
                    } else {
                        self.slots = saved;
                    }
                    if found == negate {
                        false
                    } else {
                        pc = end;
                        continue;
                    }
                }
            };
            if matched {
                pc += 1;
                continue;
            }
            loop {
                match stack.pop() {
                    None => return Ok(None),
                    Some(Back::Restore(slot, old)) => {
                        if let Some(cell) = self.slots.get_mut(slot as usize) {
                            *cell = old;
                        }
                    }
                    Some(Back::Branch(target, at)) => {
                        pc = target as usize;
                        pos = at as usize;
                        break;
                    }
                }
            }
        }
    }

    fn back_reference(&mut self, n: usize, pos: &mut usize) -> Result<bool, OutOfSteps> {
        let start = self.slots.get(n * 2).copied().unwrap_or(-1);
        let end = self.slots.get(n * 2 + 1).copied().unwrap_or(-1);
        if start < 0 || end < start {
            return Ok(true);
        }
        let (start, end) = (start as usize, end as usize);
        let Some(text) = self.input.get(start..end) else {
            return Ok(true);
        };
        let Some(here) = self.input.get(*pos..*pos + text.len()) else {
            return Ok(false);
        };
        if (*self.steps as usize) < text.len() / 16 {
            return Err(OutOfSteps);
        }
        *self.steps -= text.len() as u64 / 16;
        let equal = text.iter().zip(here).all(|(a, b)| {
            a == b || (self.program.icase && map_case(*a, true) == map_case(*b, true))
        });
        if equal {
            *pos += text.len();
        }
        Ok(equal)
    }
}

impl Program {
    /// Instructions compiled, which is what compiling this cost.
    pub fn size(&self) -> usize {
        self.insts.len()
    }

    /// The first match at or after `start`: slot pairs, -1 for an unset group.
    pub fn exec(
        &self,
        input: &[u16],
        start: usize,
        steps: &mut u64,
    ) -> Result<Option<Vec<i32>>, OutOfSteps> {
        let mut matcher = Matcher {
            program: self,
            input,
            slots: vec![-1; self.slots],
            steps,
            groups: self.groups,
        };
        let reset_cost = 1 + self.slots as u64 / 32;
        for from in start..=input.len() {
            if *matcher.steps < reset_cost {
                return Err(OutOfSteps);
            }
            *matcher.steps -= reset_cost;
            matcher.slots.fill(-1);
            if let Some(end) = matcher.run(0, from)? {
                let mut slots = matcher.slots;
                slots.truncate(self.groups * 2);
                if let [first, second, ..] = slots.as_mut_slice() {
                    *first = from as i32;
                    *second = end as i32;
                }
                return Ok(Some(slots));
            }
        }
        Ok(None)
    }
}
