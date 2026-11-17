//! Headless episode-list benchmark using offline demo data.
//!
//! cargo run --locked --no-default-features --features demo --example performance
//! Use --release for production timing. Counts and time include egui layout
//! and painting commands, but not GPU rendering, audio, or network requests.

#[cfg(not(feature = "demo"))]
fn main() {
    eprintln!("Run with --features demo");
}

#[cfg(feature = "demo")]
fn main() {
    benchmark::run();
}

#[cfg(feature = "demo")]
mod benchmark {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::hash::{Hash, Hasher};
    use std::hint::black_box;
    use std::time::Instant;

    use spotifast::app::{App, AppOptions};
    use spotifast::backend::Waker;
    use spotifast::model::Page;
    use spotifast::paths::AppDirs;
    use spotifast::settings::Settings;

    // Count only allocations on the drawing thread, leaving runtime work out.
    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
        static BYTES: Cell<usize> = const { Cell::new(0) };
    }

    struct CountingAllocator;

    fn record(bytes: usize) {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get().saturating_add(1)));
            let _ = BYTES.try_with(|count| count.set(count.get().saturating_add(bytes)));
        }
    }

    // Every operation forwards the caller's original layout and pointers to
    // System. The thread-local counters never allocate.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            record(size);
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn frame(ctx: &egui::Context, app: &mut App, podcast: bool) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if podcast {
                        spotifast::ui::show::show(app, ui, "sh0");
                    } else {
                        spotifast::ui::library::show(app, ui, Page::Episodes);
                    }
                });
            },
        );
        // Headless drawing deliberately has no renderer to apply texture deltas.
        output.textures_delta.clear();
        black_box(output)
    }

    pub fn run() {
        let root = std::env::current_dir()
            .unwrap()
            .join(".cache")
            .join(format!("performance-demo-{}", std::process::id()));
        let ctx = egui::Context::default();
        let waker = Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        app.attach(&ctx);
        spotifast::demo::populate(&mut app);
        // Use the same episode payload in each row and both views. Keep all
        // artwork absent so this measures UI work without decoding or fetching.
        let mut saved = app.library.episodes.items[0].clone();
        saved.episode.images.clear();
        if let Some(show) = &mut saved.episode.show {
            show.images.clear();
        }
        app.show_pages
            .get_mut("sh0")
            .unwrap()
            .show
            .get_mut()
            .unwrap()
            .images
            .clear();
        println!(
            "view,episodes,median_us_per_frame,allocations_per_frame,allocated_bytes_per_frame,paint_hash"
        );
        for count in [100, 1000, 10_000] {
            app.library.episodes.items = vec![saved.clone(); count];
            app.library.episodes.total = Some(count as u32);
            let page = app.show_pages.get_mut("sh0").unwrap();
            page.episodes.items = vec![saved.episode.clone(); count];
            page.episodes.total = Some(count as u32);
            for podcast in [false, true] {
                for _ in 0..10 {
                    frame(&ctx, &mut app, podcast);
                }
                // Hash the drawing commands outside the timed region so the
                // baseline and candidate can also be checked for visual parity.
                let output = frame(&ctx, &mut app, podcast);
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                format!("{:?}", output.shapes).hash(&mut hash);
                let paint_hash = hash.finish();
                drop(output);
                let mut times = Vec::new();
                let mut allocations = Vec::new();
                let mut bytes = Vec::new();
                for _ in 0..51 {
                    let start = Instant::now();
                    frame(&ctx, &mut app, podcast);
                    times.push(start.elapsed().as_secs_f64() * 1e6);
                }
                // Keep counter updates out of the timing measurements.
                for _ in 0..5 {
                    ALLOCATIONS.with(|count| count.set(0));
                    BYTES.with(|count| count.set(0));
                    COUNTING.with(|count| count.set(true));
                    frame(&ctx, &mut app, podcast);
                    COUNTING.with(|count| count.set(false));
                    allocations.push(ALLOCATIONS.with(Cell::get));
                    bytes.push(BYTES.with(Cell::get));
                }
                times.sort_by(f64::total_cmp);
                allocations.sort_unstable();
                bytes.sort_unstable();
                let name = if podcast { "podcast" } else { "saved" };
                println!(
                    "{name},{count},{:.3},{},{},{paint_hash:016x}",
                    times[25], allocations[2], bytes[2]
                );
            }
        }
        app.backend.shutdown();
        drop(app);
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
