//! The image worker: renditions are made on a thread of their own, so the
//! event loop never waits for a decode.
//!
//! Jobs go to the thread over one channel and results come back over
//! another; the event loop picks them up on its next turn (it polls more
//! often while jobs are out, [`super::WORK_POLL`]). Every job carries the
//! epoch it was asked in: after a new layout ([`Worker::bump`]) the jobs
//! still queued are skipped, not made, and reported as such.
//!
//! The thread keeps the decoded images it used last ([`Pixels`]), so the
//! renditions of one image (blocks, then a pixel protocol, then slices of
//! it while it is partly visible) decode it once; an SVG is drawn once per
//! box size. Each job runs inside [`guarded`]: a panic loses that
//! rendition only. The thread starts with the first job and ends when the
//! pager drops the worker. If no thread can be started, or it is gone (its
//! jobs are then reported skipped, so they are asked for again), jobs run
//! in the event loop instead, one per turn.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::gfx::Rgba;
use crate::gfx::store::{self, Made, Source, StoreOptions};
use crate::ir::ImageId;
use crate::panic::guarded;

use super::cache::{Key, StoreId};

/// Decoded pixels kept by the worker (plan §3).
const DECODED_BUDGET: usize = 64 << 20;

/// A rendition to make.
pub(crate) struct Job {
    id: u64,
    epoch: u64,
    key: Key,
    source: Source,
    opts: StoreOptions,
}

/// What came of a job.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The rendition (`None`: it cannot be made).
    Made(Option<Made>),
    /// Not made: it was asked before the last [`Worker::bump`] (or lost).
    Skipped,
}

/// A finished job.
#[derive(Debug)]
pub(crate) struct Done {
    /// The job's number ([`Worker::submit`]).
    pub(crate) job: u64,
    pub(crate) key: Key,
    pub(crate) outcome: Outcome,
}

/// Where jobs run.
enum Backend {
    /// Nothing asked yet: the thread starts with the first job.
    Idle,
    /// The worker thread.
    Thread {
        jobs: Sender<Job>,
        done: Receiver<Done>,
    },
    /// No thread: jobs run in the event loop.
    Inline {
        queue: VecDeque<Job>,
        pixels: Pixels,
    },
}

/// The next result, if any.
enum Next {
    Done(Done),
    Nothing,
    /// The thread is gone.
    Gone,
}

/// Makes renditions off the event loop; see the module docs.
pub(crate) struct Worker {
    backend: Backend,
    epoch: Arc<AtomicU64>,
    next_job: u64,
    /// Jobs asked for and not done yet.
    out: HashMap<u64, Key>,
    /// Results not handed out yet (jobs lost with the thread).
    ready: Vec<Done>,
}

impl Worker {
    /// A worker whose thread starts with the first job.
    pub(crate) fn new() -> Worker {
        Worker {
            backend: Backend::Idle,
            epoch: Arc::new(AtomicU64::new(0)),
            next_job: 1,
            out: HashMap::new(),
            ready: Vec::new(),
        }
    }

    /// A worker that runs every job in the event loop.
    #[cfg(all(test, feature = "images"))]
    pub(crate) fn inline() -> Worker {
        Worker {
            backend: inline_backend(),
            ..Worker::new()
        }
    }

    /// Ask for `key`, made from `source` with `opts`; returns the job's
    /// number.
    pub(crate) fn submit(&mut self, key: Key, source: Source, opts: StoreOptions) -> u64 {
        let id = self.next_job;
        self.next_job += 1;
        let job = Job {
            id,
            epoch: self.epoch.load(Ordering::SeqCst),
            key: key.clone(),
            source,
            opts,
        };
        if matches!(self.backend, Backend::Idle) {
            self.backend = spawn(Arc::clone(&self.epoch));
        }
        let unsent = match &self.backend {
            Backend::Thread { jobs, .. } => jobs.send(job).err().map(|mpsc::SendError(j)| j),
            Backend::Idle | Backend::Inline { .. } => Some(job),
        };
        if let Some(job) = unsent {
            if matches!(self.backend, Backend::Thread { .. }) {
                self.lost();
            }
            if let Backend::Inline { queue, .. } = &mut self.backend {
                queue.push_back(job);
            }
        }
        self.out.insert(id, key);
        id
    }

    /// A new epoch: jobs asked before it are skipped when their turn comes.
    pub(crate) fn bump(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether jobs are out.
    pub(crate) fn busy(&self) -> bool {
        !self.out.is_empty()
    }

    /// The jobs done since the last call; without a thread, one job is run
    /// first.
    pub(crate) fn take(&mut self) -> Vec<Done> {
        let mut done = std::mem::take(&mut self.ready);
        let inline = matches!(self.backend, Backend::Inline { .. });
        loop {
            match self.next(None) {
                Next::Done(d) => {
                    done.push(d);
                    if inline {
                        break;
                    }
                }
                Next::Nothing => break,
                Next::Gone => {
                    self.lost();
                    break;
                }
            }
        }
        self.finish(&done);
        done
    }

    /// Wait until every job is done, but no longer than `limit`, and return
    /// what was done.
    pub(crate) fn wait(&mut self, limit: Duration) -> Vec<Done> {
        let until = Instant::now() + limit;
        let mut done = std::mem::take(&mut self.ready);
        self.finish(&done);
        while !self.out.is_empty() {
            match self.next(Some(until)) {
                Next::Done(d) => {
                    self.out.remove(&d.job);
                    done.push(d);
                }
                Next::Nothing => break,
                Next::Gone => {
                    self.lost();
                    let lost = std::mem::take(&mut self.ready);
                    self.finish(&lost);
                    done.extend(lost);
                }
            }
        }
        done
    }

    /// The next result: waiting until `until` for the thread, or running
    /// one job without it.
    fn next(&mut self, until: Option<Instant>) -> Next {
        match &mut self.backend {
            Backend::Idle => Next::Nothing,
            Backend::Thread { done, .. } => {
                let got = match until {
                    None => done.try_recv().map_err(|e| e == TryRecvError::Disconnected),
                    Some(t) => done
                        .recv_timeout(t.saturating_duration_since(Instant::now()))
                        .map_err(|e| e == RecvTimeoutError::Disconnected),
                };
                match got {
                    Ok(d) => Next::Done(d),
                    Err(true) => Next::Gone,
                    Err(false) => Next::Nothing,
                }
            }
            Backend::Inline { queue, pixels } => match queue.pop_front() {
                Some(job) => Next::Done(run(job, pixels, &self.epoch)),
                None => Next::Nothing,
            },
        }
    }

    /// Jobs `done` are no longer out.
    fn finish(&mut self, done: &[Done]) {
        for d in done {
            self.out.remove(&d.job);
        }
    }

    /// The thread is gone with the jobs it had: they are reported skipped
    /// (so they are asked for again), and jobs run in the event loop from
    /// now on.
    fn lost(&mut self) {
        self.backend = inline_backend();
        let lost = self.out.iter().map(|(&job, key)| Done {
            job,
            key: key.clone(),
            outcome: Outcome::Skipped,
        });
        self.ready.extend(lost);
    }
}

/// Jobs run in the event loop.
fn inline_backend() -> Backend {
    Backend::Inline {
        queue: VecDeque::new(),
        pixels: Pixels::new(DECODED_BUDGET),
    }
}

/// Start the worker thread (or, without one, run jobs inline).
fn spawn(epoch: Arc<AtomicU64>) -> Backend {
    let (jobs, inbox) = mpsc::channel::<Job>();
    let (outbox, done) = mpsc::channel::<Done>();
    let started = std::thread::Builder::new()
        .name("emde-images".into())
        .spawn(move || serve(&inbox, &outbox, &epoch));
    match started {
        Ok(_) => Backend::Thread { jobs, done },
        Err(_) => inline_backend(),
    }
}

/// The worker thread: make renditions until the pager hangs up.
fn serve(inbox: &Receiver<Job>, outbox: &Sender<Done>, epoch: &AtomicU64) {
    let mut pixels = Pixels::new(DECODED_BUDGET);
    for job in inbox {
        if outbox.send(run(job, &mut pixels, epoch)).is_err() {
            return;
        }
    }
}

/// Make one job's rendition (or skip it: asked before the current epoch).
fn run(job: Job, pixels: &mut Pixels, epoch: &AtomicU64) -> Done {
    let outcome = if job.epoch < epoch.load(Ordering::SeqCst) {
        Outcome::Skipped
    } else {
        let made = guarded(|| {
            let at = job
                .source
                .decode_size(job.key.cols, job.key.rows, &job.opts);
            let image = (job.key.store, job.key.image, at);
            let mut decode = || pixels.get(image, &job.source, job.opts.max_pixels);
            store::make(
                &job.source,
                &job.key.make,
                job.key.cols,
                job.key.rows,
                &job.opts,
                &mut decode,
            )
        });
        Outcome::Made(made.flatten())
    };
    Done {
        job: job.id,
        key: job.key,
        outcome,
    }
}

/// Which decoded image: a store's image, and for an SVG the size it was
/// drawn at ([`Source::decode_size`]).
type Decoded = (StoreId, ImageId, Option<(u32, u32)>);

/// Decoded images, most recently used last, within a byte budget (the
/// last one is kept whatever its size, so the renditions of one large
/// image decode it once). Failures are kept too: a broken file is not
/// decoded again for every rendition.
struct Pixels {
    items: Vec<(Decoded, Option<Arc<Rgba>>)>,
    budget: usize,
}

impl Pixels {
    fn new(budget: usize) -> Pixels {
        Pixels {
            items: Vec::new(),
            budget,
        }
    }

    /// The decoded image `key`, decoding `source` on a miss.
    fn get(&mut self, key: Decoded, source: &Source, max: u64) -> Option<Arc<Rgba>> {
        let item = match self.items.iter().position(|(k, _)| *k == key) {
            Some(i) => self.items.remove(i),
            None => (key, source.decode_at(max, key.2).map(Arc::new)),
        };
        let image = item.1.clone();
        self.items.push(item);
        self.trim();
        image
    }

    /// Drop the least recently used images beyond the budget.
    fn trim(&mut self) {
        let size = |i: &Option<Arc<Rgba>>| i.as_ref().map_or(0, |r| r.pixels.len());
        let mut total: usize = self.items.iter().map(|(_, i)| size(i)).sum();
        while total > self.budget && self.items.len() > 1 {
            let (_, gone) = self.items.remove(0);
            total -= size(&gone);
        }
    }
}

#[cfg(all(test, feature = "images"))]
mod tests {
    use super::*;
    use crate::gfx::Passthrough;
    use crate::gfx::store::{ImageStore, Make};
    use crate::parse::{ParseOptions, parse};
    use crate::style::Rgb;
    use crate::term::{BlockGlyphSet, ColorDepth, Graphics};
    use crate::theme::Variant;

    /// A store of one data-URI figure: an 8×4 red PNG.
    fn store() -> (ImageStore, ImageId) {
        let red = Rgba::filled(8, 4, [255, 0, 0, 255]).unwrap();
        let png = crate::gfx::png::encode(&red).unwrap();
        let md = format!(
            "![red](data:image/png;base64,{})",
            crate::gfx::b64::encode_string(&png)
        );
        let doc = parse(&md, &ParseOptions::default());
        let opts = StoreOptions {
            graphics: Graphics::Blocks,
            glyphs: BlockGlyphSet::Half,
            depth: ColorDepth::TrueColor,
            background: Some(Rgb(0, 0, 0)),
            page: Rgb(0, 0, 0),
            cell_px: Some((8, 16)),
            passthrough: Passthrough::Direct,
            max_pixels: 1_000_000,
            remote: false,
            variant: Variant::Dark,
            blocks: true,
        };
        let id = store::figure_images(&doc)[0];
        (ImageStore::load_figures(&doc, opts), id)
    }

    fn key(image: ImageId, make: Make) -> Key {
        Key {
            store: StoreId(1),
            image,
            cols: 2,
            rows: 1,
            make,
        }
    }

    #[test]
    fn the_thread_makes_renditions() {
        let (store, image) = store();
        let mut w = Worker::new();
        assert!(!w.busy());
        assert!(w.take().is_empty());
        let job = w.submit(
            key(image, Make::Blocks),
            store.source(image).unwrap(),
            store.options().clone(),
        );
        assert!(w.busy());
        let done = w.wait(Duration::from_secs(10));
        assert!(!w.busy());
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].job, job);
        let Outcome::Made(Some(Made::Blocks(raster))) = &done[0].outcome else {
            panic!("{:?}", done[0].outcome);
        };
        assert_eq!((raster.cols, raster.rows), (2, 1));
        assert!(w.take().is_empty());
    }

    #[test]
    fn stale_jobs_are_skipped() {
        let (store, image) = store();
        for mut w in [Worker::new(), Worker::inline()] {
            let src = || store.source(image).unwrap();
            let opts = store.options().clone();
            let old = w.submit(key(image, Make::Blocks), src(), opts.clone());
            w.bump();
            let new = w.submit(key(image, Make::Iterm(0..1)), src(), opts);
            let mut done = w.wait(Duration::from_secs(10));
            done.sort_by_key(|d| d.job);
            assert_eq!(done.len(), 2);
            assert_eq!(done[0].job, old);
            // The thread may have taken the old job before the bump; inline
            // it is always skipped.
            if matches!(w.backend, Backend::Inline { .. }) {
                assert!(matches!(done[0].outcome, Outcome::Skipped));
            }
            assert_eq!(done[1].job, new);
            assert!(matches!(
                done[1].outcome,
                Outcome::Made(Some(Made::Pixels(_)))
            ));
            assert!(!w.busy());
        }
    }

    #[test]
    fn inline_jobs_run_one_per_turn() {
        let (store, image) = store();
        let mut w = Worker::inline();
        for make in [Make::Blocks, Make::Kitty] {
            w.submit(
                key(image, make),
                store.source(image).unwrap(),
                store.options().clone(),
            );
        }
        assert_eq!(w.take().len(), 1);
        assert!(w.busy());
        assert_eq!(w.take().len(), 1);
        assert!(!w.busy());
        assert!(w.take().is_empty());
    }

    #[test]
    fn decoded_images_are_kept_within_the_budget() {
        let (store, image) = store();
        let src = store.source(image).unwrap();
        // 8×4 RGBA is 128 bytes: a budget of 200 keeps one.
        let mut p = Pixels::new(200);
        let a = p.get((StoreId(1), image, None), &src, 1000).unwrap();
        let again = p.get((StoreId(1), image, None), &src, 1000).unwrap();
        assert!(Arc::ptr_eq(&a, &again), "decoded once");
        p.get((StoreId(2), image, None), &src, 1000);
        assert_eq!(p.items.len(), 1, "over budget: the older one goes");
        // Too many pixels: a failure, kept as one.
        let mut small = Pixels::new(200);
        assert!(small.get((StoreId(1), image, None), &src, 4).is_none());
        assert_eq!(small.items.len(), 1);
    }

    #[test]
    fn a_panic_loses_only_its_rendition() {
        // Zero cells make no rendition, and nothing panics; a panic inside
        // a job is caught the same way (`guarded`).
        let (store, image) = store();
        let mut w = Worker::inline();
        let mut bad = key(image, Make::Blocks);
        bad.cols = 0;
        w.submit(bad, store.source(image).unwrap(), store.options().clone());
        let done = w.wait(Duration::from_secs(1));
        assert!(matches!(done[0].outcome, Outcome::Made(None)));
    }
}
