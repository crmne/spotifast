//! How much native stack a script has used.
//!
//! Overflowing the stack ends the process, so no skin may be able to. Counting
//! nesting cannot promise that: a frame's size depends on the build, the
//! compiler and the platform, and an unoptimised frame can be ten times an
//! optimised one. So every recursive function asks how far the stack really
//! is from where the script came in, by the address of a local, and gives up
//! past [`MAX_STACK_BYTES`]. The same bytes hold in every build. Scripts run
//! on the window's thread, which is why the allowance is small enough to leave
//! room on the 1 MiB main thread Windows gives a program, and on the 512 KiB
//! of a secondary thread on macOS, with what called the script already there.

use std::cell::Cell;

/// What a script may use. Real skins nest a dozen calls at most, and a call
/// takes about 12 KiB unoptimised and far less in a release build.
pub(super) const MAX_STACK_BYTES: usize = 160 * 1024;

thread_local! {
    /// Where the script came in. Stacks do not move, and a script runs to
    /// its end on the thread that started it.
    static BASE: Cell<usize> = const { Cell::new(0) };
}

#[inline(always)]
fn here() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker as *const u8) as usize
}

/// Notes that a script starts here.
pub(super) fn mark() {
    BASE.with(|base| base.set(here()));
}

/// Whether this call is past the allowance.
#[inline]
pub(super) fn exceeded() -> bool {
    let start = BASE.with(Cell::get);
    // Stacks grow down on every platform this runs on; the distance serves
    // either way. Code that never marked has no script above it to limit.
    start != 0 && start.abs_diff(here()) > MAX_STACK_BYTES
}
